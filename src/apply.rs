//! 执行计划。**这是整个工具唯一会破坏数据的地方。**
//!
//! 四条不可让步的规矩：
//!
//! 1. **执行前重算指纹**。扫描到执行之间可能过了几十秒（CLI）到几分钟（GUI），
//!    足够一个 agent 重新开工。指纹对不上就跳过并上报，不猜（D14）。
//! 2. **删除 worktree 一律 `git worktree remove` 且不带 `--force`**，
//!    失败就是失败，**任何情况下不得退化成 `rm -rf`**。git 自己的校验
//!    （submodule、损坏的 .git）是最后一层保险，绕过它等于亲手拆掉（D15）。
//! 3. **删除路径必须 canonicalize 后仍在预期的根之下**。符号链接、`..`、
//!    竞态换目录都可能让一个看起来安全的相对路径指到别处。
//! 4. **回收量报实测差值**，不报 du 的估算。APFS 写时复制会让估算系统性偏高，
//!    数字对不上就会丢掉用户的信任。

use crate::fsops::FsOps;
use crate::model::Cause;
use crate::plan::{Action, Plan};
use crate::platform::disk;
use crate::scan::Env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    /// 只演练不动手。**默认值就是它**。
    pub dry_run: bool,
    /// 审计日志落盘位置。None 表示不记。
    pub audit_log: Option<PathBuf>,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self { dry_run: true, audit_log: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done { freed_estimate: u64 },
    /// 指纹对不上——状态在扫描之后变了。
    Stale { what: String },
    Failed(Cause),
    /// dry-run 下的占位。
    Simulated,
}

#[derive(Debug, Clone)]
pub struct ActionResult {
    pub action: Action,
    pub outcome: Outcome,
    /// 删除后如何重建。**删了才想起来要重建就晚了，所以扫描时就算好。**
    pub restore_hint: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ApplyReport {
    pub results: Vec<ActionResult>,
    /// 执行前后实测的可用空间差。这才是交付数字。
    pub measured_freed: i64,
    pub estimated_freed: u64,
}

impl ApplyReport {
    pub fn done_count(&self) -> usize {
        self.results.iter().filter(|r| matches!(r.outcome, Outcome::Done { .. })).count()
    }
    pub fn stale_count(&self) -> usize {
        self.results.iter().filter(|r| matches!(r.outcome, Outcome::Stale { .. })).count()
    }
}

pub fn apply(plan: &Plan, opts: &ApplyOptions, env: &Env, fs: &dyn FsOps) -> ApplyReport {
    let before = probe_available();
    let mut report = ApplyReport {
        estimated_freed: plan.estimated_bytes(),
        ..Default::default()
    };

    for action in &plan.actions {
        let hint = restore_hint(action);
        let outcome = if opts.dry_run {
            Outcome::Simulated
        } else {
            run_action(action, env, fs)
        };
        report.results.push(ActionResult {
            action: action.clone(),
            outcome,
            restore_hint: hint,
        });
    }

    let after = probe_available();
    report.measured_freed = match (before, after) {
        (Some(b), Some(a)) => a as i64 - b as i64,
        _ => 0,
    };

    if let Some(path) = &opts.audit_log
        && !opts.dry_run
    {
        // 审计失败不能让已完成的删除"看起来没发生"，所以只警告不中断
        if let Err(e) = write_audit(path, &report, fs) {
            eprintln!("警告：审计日志写入失败（删除已执行）: {e:?}");
        }
    }

    report
}

fn run_action(action: &Action, env: &Env, fs: &dyn FsOps) -> Outcome {
    match action {
        Action::ReclaimCache { worktree, cache, expect, bytes, .. } => {
            // 规矩 3：解析后必须仍在这个 worktree 之下
            if let Err(o) = ensure_within(cache, worktree) {
                return o;
            }
            // 规矩 1：状态可能已经变了
            if let Some(o) = recheck(worktree, expect, env) {
                return o;
            }
            match fs.remove_dir_all(cache) {
                Ok(()) => Outcome::Done { freed_estimate: *bytes },
                Err(c) => Outcome::Failed(c),
            }
        }

        Action::RemoveWorktree { repo, worktree, expect, bytes } => {
            if let Some(o) = recheck(worktree, expect, env) {
                return o;
            }
            // 规矩 2：不带 --force，失败就失败
            let args = ["worktree", "remove", &*worktree.to_string_lossy()];
            match env.git.exec(repo, &args) {
                Ok(out) if out.code == Some(0) => Outcome::Done { freed_estimate: *bytes },
                Ok(out) => Outcome::Failed(Cause::CommandFailed {
                    cmd: format!("git worktree remove {}", worktree.display()),
                    code: out.code,
                    stderr: out.stderr,
                }),
                Err(c) => Outcome::Failed(c),
            }
        }

        Action::PruneAdmin { repo, confirmed_missing } => {
            // D13：外置盘临时没挂载时目录也"不存在"，无条件 prune 会删掉本该保留的注册记录。
            // 所以执行前再确认一次，任何一条又出现了就整体放弃。
            if let Some(back) = confirmed_missing.iter().find(|p| fs.exists(p)) {
                return Outcome::Stale {
                    what: format!("{} 又出现了（外置盘挂回来了？），本次不 prune", back.display()),
                };
            }
            match env.git.exec(repo, &["worktree", "prune"]) {
                Ok(out) if out.code == Some(0) => Outcome::Done { freed_estimate: 0 },
                Ok(out) => Outcome::Failed(Cause::CommandFailed {
                    cmd: "git worktree prune".into(),
                    code: out.code,
                    stderr: out.stderr,
                }),
                Err(c) => Outcome::Failed(c),
            }
        }
    }
}

/// 重算指纹并与扫描时刻比对。返回 `Some` 表示不该继续。
fn recheck(worktree: &Path, expect: &crate::model::Fingerprint, env: &Env) -> Option<Outcome> {
    // 只复检最便宜且最致命的两项：有没有人在用、有没有新的未提交改动。
    // 复检本身要快——它挡在每一次删除前面。
    match env.procs.processes_under(worktree) {
        Ok(ps) => {
            let now: Vec<u32> = ps.iter().map(|p| p.pid).collect();
            if !now.is_empty() && now != expect.busy_pids {
                return Some(Outcome::Stale {
                    what: format!("现在有 {} 个进程在使用它", now.len()),
                });
            }
        }
        // 扫描时能判断、现在判不准了，同样不能继续
        Err(c) => return Some(Outcome::Failed(c)),
    }

    let args = ["-c", "status.showUntrackedFiles=all", "status", "--porcelain=v1", "-z", "-uall"];
    match env.git.exec(worktree, &args) {
        Ok(out) if out.code == Some(0) => {
            let n = crate::git::porcelain::split_z(&out.stdout).len();
            if n != expect.dirty_count {
                return Some(Outcome::Stale {
                    what: format!("未提交改动从 {} 处变成了 {n} 处", expect.dirty_count),
                });
            }
            None
        }
        Ok(out) => Some(Outcome::Failed(Cause::CommandFailed {
            cmd: "git status（执行前复检）".into(),
            code: out.code,
            stderr: out.stderr,
        })),
        Err(c) => Some(Outcome::Failed(c)),
    }
}

/// 解析后必须仍在 `root` 之下。挡符号链接、`..`、以及竞态换目录。
fn ensure_within(target: &Path, root: &Path) -> Result<(), Outcome> {
    let (rt, rr) = match (target.canonicalize(), root.canonicalize()) {
        (Ok(a), Ok(b)) => (a, b),
        _ => {
            return Err(Outcome::Failed(Cause::Io {
                path: target.to_path_buf(),
                msg: "路径无法解析，拒绝删除".into(),
            }));
        }
    };
    if rt.starts_with(&rr) && rt != rr {
        Ok(())
    } else {
        Err(Outcome::Failed(Cause::Io {
            path: rt,
            msg: format!("解析后不在 {} 之下，拒绝删除", rr.display()),
        }))
    }
}

/// 删除后怎么把它变回来。worktree 删了分支还在，所以总是可重建的。
fn restore_hint(action: &Action) -> Option<String> {
    match action {
        Action::RemoveWorktree { repo, worktree, expect, .. } => Some(format!(
            "git -C {} worktree add {} {}",
            repo.display(),
            worktree.display(),
            if expect.head_oid.is_empty() { "<commit>" } else { &expect.head_oid }
        )),
        Action::ReclaimCache { worktree, kind, .. } => {
            Some(format!("在 {} 重新构建即可恢复 {}/", worktree.display(), kind.name))
        }
        Action::PruneAdmin { .. } => Some("git worktree repair".into()),
    }
}

fn probe_available() -> Option<u64> {
    std::env::current_dir().ok().and_then(|p| disk::available_bytes(&p).ok())
}

fn write_audit(path: &Path, report: &ApplyReport, fs: &dyn FsOps) -> Result<(), Cause> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        fs.create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| Cause::Io { path: path.to_path_buf(), msg: e.to_string() })?;
    for r in &report.results {
        let line = format!(
            "{{\"target\":{:?},\"outcome\":\"{:?}\",\"restore\":{:?}}}\n",
            r.action.target().display().to_string(),
            r.outcome,
            r.restore_hint.clone().unwrap_or_default()
        );
        f.write_all(line.as_bytes())
            .map_err(|e| Cause::Io { path: path.to_path_buf(), msg: e.to_string() })?;
    }
    Ok(())
}
