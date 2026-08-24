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
        for remote in out.stdout_utf8().lines().map(str::trim).filter(|s| !s.is_empty()) {
            let r = format!("refs/remotes/{remote}/HEAD");
            if let Ok(o) = git.run_ok(repo, &["symbolic-ref", "--short", &r])
                && let Some(short) = o.stdout_utf8().trim().rsplit('/').next()
                && !short.is_empty()
            {
                let b = Baseline {
                    remote: Some(remote.to_string()),
                    branch: short.to_string(),
                    source: BaselineSource::RemoteHead,
                };
                if let Ok(v) = verify(git, repo, b) {
                    return Ok(v);
                }
            }
        }

        // 3) 远端上的常见分支名
        for remote in out.stdout_utf8().lines().map(str::trim).filter(|s| !s.is_empty()) {
            for name in COMMON_NAMES {
                let b = Baseline {
                    remote: Some(remote.to_string()),
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
            let b =
                Baseline { remote: None, branch: name, source: BaselineSource::Guessed };
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
