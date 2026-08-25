//! A2 —— 缓存目录本身近期无写入。
//!
//! 这道门是 A1 的**弱代理**，不单独作为放行依据：进程刚退出但产物还没落盘、
//! 或构建脚本在两次编译之间的空档，都可能骗过它。真正承重的是 A1。
//!
//! 只 stat 顶层若干层并早退——遍历一个 40GB 的 target 找最新 mtime 是不可接受的开销。

use crate::gates::{Gate, GateCtx, cachesafe};
use crate::model::{Cause, GateDetail, GateId, GateStatus};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub struct RecentGate {
    pub dir: String,
}

/// B1 —— 整个 worktree 已达到配置的空闲时长。
///
/// 它只是 Busy 的时间侧补充，不能单独证明 worktree 可删除；后续 Dirty、Landed 等
/// 门禁仍需全部通过。与缓存的 `cache_quiet` 分开，避免 24 小时阈值阻塞缓存回收。
pub struct IdleGate;

/// 只看这么深。构建工具写产物时一定会同时更新浅层目录的 mtime，
/// 深挖并不会更准，只会更慢。
const MAX_DEPTH: usize = 3;

impl Gate for RecentGate {
    fn id(&self) -> GateId {
        GateId::Recent
    }

    fn evaluate(&self, ctx: &GateCtx<'_>) -> GateStatus {
        let target = ctx.worktree.join(&self.dir);
        evaluate_recency(target, ctx.cfg.cache_quiet, ctx.clock.now())
    }
}

impl Gate for IdleGate {
    fn id(&self) -> GateId {
        GateId::Idle
    }

    fn evaluate(&self, ctx: &GateCtx<'_>) -> GateStatus {
        if ctx.cfg.idle.is_zero() {
            return GateStatus::Pass;
        }
        let target = ctx.worktree.to_path_buf();
        let mut filesystem = match newest_mtime(&target, MAX_DEPTH) {
            Ok(value) => value,
            Err(e) => {
                return GateStatus::Unknown(Cause::Io {
                    path: target,
                    msg: e.to_string(),
                });
            }
        };
        // 从 worktree 根只走三层会少看一层缓存内部结构；每个已知缓存再以自身为根
        // 复查一次，既覆盖 target/debug/deps 等常见写入，又不全量遍历几十 GB 产物。
        for dir in cachesafe::candidates(ctx.worktree, ctx.cfg) {
            let cache = ctx.worktree.join(dir);
            let cache_newest = match newest_mtime(&cache, MAX_DEPTH) {
                Ok(value) => value,
                Err(e) => {
                    return GateStatus::Unknown(Cause::Io {
                        path: cache,
                        msg: e.to_string(),
                    });
                }
            };
            filesystem = newer(filesystem, cache_newest);
        }
        let head = match head_commit_mtime(ctx) {
            Ok(value) => value,
            Err(cause) => return GateStatus::Unknown(cause),
        };
        evaluate_newest(newer(filesystem, Some(head)), ctx.cfg.idle, ctx.clock.now())
    }
}

fn evaluate_recency(target: PathBuf, quiet: Duration, now: SystemTime) -> GateStatus {
    match newest_mtime(&target, MAX_DEPTH) {
        Err(e) => GateStatus::Unknown(Cause::Io {
            path: target,
            msg: e.to_string(),
        }),
        Ok(newest) => evaluate_newest(newest, quiet, now),
    }
}

fn head_commit_mtime(ctx: &GateCtx<'_>) -> Result<(PathBuf, SystemTime), Cause> {
    let out = ctx
        .git
        .run_ok(ctx.worktree, &["show", "-s", "--format=%ct", "HEAD"])?;
    let raw = out.stdout_utf8();
    let text = raw.trim();
    let seconds = text.parse::<i64>().map_err(|e| Cause::CommandFailed {
        cmd: "git show -s --format=%ct HEAD".into(),
        code: Some(0),
        stderr: format!("HEAD 提交时间无法解析（{e}）：{text}"),
    })?;
    let time = if seconds >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds as u64))
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(seconds.unsigned_abs()))
    }
    .ok_or_else(|| Cause::CommandFailed {
        cmd: "git show -s --format=%ct HEAD".into(),
        code: Some(0),
        stderr: format!("HEAD 提交时间超出系统可表示范围：{text}"),
    })?;
    Ok((ctx.worktree.join(".git"), time))
}

fn newer(
    left: Option<(PathBuf, SystemTime)>,
    right: Option<(PathBuf, SystemTime)>,
) -> Option<(PathBuf, SystemTime)> {
    match (left, right) {
        (Some(a), Some(b)) if b.1 > a.1 => Some(b),
        (Some(a), Some(_)) => Some(a),
        (None, value) | (value, None) => value,
    }
}

fn evaluate_newest(
    newest: Option<(PathBuf, SystemTime)>,
    quiet: Duration,
    now: SystemTime,
) -> GateStatus {
    let Some((path, mtime)) = newest else {
        return GateStatus::Pass;
    };
    match now.duration_since(mtime) {
        Ok(age) if age >= quiet => GateStatus::Pass,
        Ok(age) => GateStatus::Blocked(GateDetail::RecentlyModified {
            newest_path: path,
            age_secs: age.as_secs(),
        }),
        // mtime 在未来（时钟回拨、跨机器同步）——不敢判，交给人
        Err(e) => GateStatus::Unknown(Cause::Io {
            path,
            msg: format!("修改时间晚于当前时间，差 {:?}", e.duration()),
        }),
    }
}

fn newest_mtime(root: &Path, depth: usize) -> std::io::Result<Option<(PathBuf, SystemTime)>> {
    let root_meta = std::fs::symlink_metadata(root)?;
    let mut best = Some((root.to_path_buf(), root_meta.modified()?));
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return Ok(best);
    }
    let mut stack = vec![(root.to_path_buf(), 0usize)];

    while let Some((dir, d)) = stack.pop() {
        let entries = std::fs::read_dir(&dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            // 不跟随软链接：IdleGate 会从 worktree 根遍历，跟随目录链接会越界扫描。
            let meta = std::fs::symlink_metadata(&path)?;
            let m = meta.modified()?;
            if best.as_ref().is_none_or(|(_, b)| m > *b) {
                best = Some((entry.path(), m));
            }
            if meta.is_dir() && !meta.file_type().is_symlink() && d + 1 < depth {
                stack.push((path, d + 1));
            }
        }
    }
    Ok(best)
}

/// 供测试与 CLI 复用的固定时钟。
pub struct FixedClock(pub SystemTime);
impl crate::gates::Clock for FixedClock {
    fn now(&self) -> SystemTime {
        self.0
    }
}

/// 便于构造「N 秒前」的时刻。
pub fn ago(d: Duration) -> SystemTime {
    SystemTime::now() - d
}
