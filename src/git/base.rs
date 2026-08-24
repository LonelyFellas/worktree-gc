//! 主干基线探测。
//!
//! 原型把基线写死成 `origin/main`，退化到本地 `main`。这在 gitflow 仓
//! （main 是发布分支）或有陈旧本地 main 时会让祖先判定跑在错误的 ref 上；
//! 更糟的是「找不到基线」被静默跳过，用户以为「没东西可清」而不是「根本没扫」（D12）。

use crate::config::BaseBranchPolicy;
use crate::git::GitRunner;
use crate::model::{Baseline, BaselineSource, Cause};
use std::path::Path;

const COMMON_NAMES: &[&str] = &["main", "master", "trunk", "develop", "development"];

/// 远端的优先顺序。**不能按 `git remote` 的字母序取第一个。**
///
/// 实测代价：一个仓同时挂着 `gitee` 与 `origin`，字母序让 `gitee` 胜出，
/// 而那个镜像停在 13 天前 —— 于是「领先主干 1 个提交」被判成「领先 100 个提交」，
/// 每个 worktree 都显示未合入，Landed 门禁整个沦为噪音。
///
/// 约定俗成里 `origin` 就是主远端，`upstream` 是 fork 场景的上游。按约定排，
/// 剩下的才按 git 给的顺序兜底。
const REMOTE_PRIORITY: &[&str] = &["origin", "upstream"];

/// 按约定优先级排序远端。
fn ordered_remotes(raw: &str) -> Vec<String> {
    let all: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let mut out: Vec<String> = Vec::new();
    for want in REMOTE_PRIORITY {
        if let Some(hit) = all.iter().find(|r| r.as_str() == *want) {
            out.push(hit.clone());
        }
    }
    for r in all {
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// 五级探测。任何一级命中即返回；全部失败返回 `Cause`，调用方据此**整仓标红**。
pub fn detect(
    git: &dyn GitRunner,
    repo: &Path,
    policy: &BaseBranchPolicy,
) -> Result<Baseline, Cause> {
    // 1) 用户显式指定 —— 最高优先级，但仍要验证 ref 真实存在
    if let BaseBranchPolicy::Explicit { remote, branch } = policy {
        let b = Baseline {
            remote: remote.clone(),
            branch: branch.clone(),
            source: BaselineSource::Explicit,
        };
        return verify(git, repo, b);
    }

    // 2) 各远端的 HEAD 符号引用（权威来源）
    if let Ok(out) = git.run_ok(repo, &["remote"]) {
        let remotes = ordered_remotes(&out.stdout_utf8());

        for remote in &remotes {
            let r = format!("refs/remotes/{remote}/HEAD");
            if let Ok(o) = git.run_ok(repo, &["symbolic-ref", "--short", &r])
                && let Some(short) = o.stdout_utf8().trim().rsplit('/').next()
                && !short.is_empty()
            {
                let b = Baseline {
                    remote: Some(remote.clone()),
                    branch: short.to_string(),
                    source: BaselineSource::RemoteHead,
                };
                if let Ok(v) = verify(git, repo, b) {
                    return Ok(v);
                }
            }
        }

        // 3) 远端上的常见分支名
        for remote in &remotes {
            for name in COMMON_NAMES {
                let b = Baseline {
                    remote: Some(remote.clone()),
                    branch: (*name).to_string(),
                    source: BaselineSource::Guessed,
                };
                if let Ok(v) = verify(git, repo, b) {
                    return Ok(v);
                }
            }
        }
    }

    // 4) init.defaultBranch 配置的本地分支
    if let Ok(o) = git.run_ok(repo, &["config", "--get", "init.defaultBranch"]) {
        let name = o.stdout_utf8().trim().to_string();
        if !name.is_empty() {
            let b = Baseline {
                remote: None,
                branch: name,
                source: BaselineSource::Guessed,
            };
            if let Ok(v) = verify(git, repo, b) {
                return Ok(v);
            }
        }
    }

    // 5) 本地常见分支名
    for name in COMMON_NAMES {
        let b = Baseline {
            remote: None,
            branch: (*name).to_string(),
            source: BaselineSource::Guessed,
        };
        if let Ok(v) = verify(git, repo, b) {
            return Ok(v);
        }
    }

    Err(Cause::CommandFailed {
        cmd: "detect baseline".into(),
        code: None,
        stderr: "未能识别主干分支：远端 HEAD、常见分支名、init.defaultBranch 均未命中".into(),
    })
}

fn verify(git: &dyn GitRunner, repo: &Path, b: Baseline) -> Result<Baseline, Cause> {
    let refname = b.refname();
    let out = git.exec(repo, &["rev-parse", "--verify", "--quiet", &refname])?;
    if out.code == Some(0) {
        Ok(b)
    } else {
        Err(Cause::CommandFailed {
            cmd: format!("git rev-parse --verify {refname}"),
            code: out.code,
            stderr: format!("ref 不存在: {refname}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_wins_over_alphabetically_earlier_remotes() {
        // git remote 按字母序输出，gitee 会排在 origin 前面。
        // 取字母序第一个的代价是拿一个 13 天前的镜像当主干（实测）。
        assert_eq!(ordered_remotes("gitee\norigin\n"), vec!["origin", "gitee"]);
    }

    #[test]
    fn upstream_ranks_after_origin_but_before_others() {
        assert_eq!(
            ordered_remotes("aaa\nupstream\norigin\nzzz\n"),
            vec!["origin", "upstream", "aaa", "zzz"]
        );
    }

    #[test]
    fn unknown_remotes_keep_git_order() {
        assert_eq!(ordered_remotes("beta\nalpha\n"), vec!["beta", "alpha"]);
    }

    #[test]
    fn handles_no_remotes() {
        assert!(ordered_remotes("").is_empty());
    }
}
