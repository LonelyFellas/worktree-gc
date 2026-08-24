//! A2 —— 缓存目录本身近期无写入。
//!
//! 这道门是 A1 的**弱代理**，不单独作为放行依据：进程刚退出但产物还没落盘、
//! 或构建脚本在两次编译之间的空档，都可能骗过它。真正承重的是 A1。
//!
//! 只 stat 顶层若干层并早退——遍历一个 40GB 的 target 找最新 mtime 是不可接受的开销。

use crate::gates::{Gate, GateCtx};
use crate::model::{Cause, GateDetail, GateId, GateStatus};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub struct RecentGate {
    pub dir: String,
}

/// 只看这么深。构建工具写产物时一定会同时更新浅层目录的 mtime，
/// 深挖并不会更准，只会更慢。
const MAX_DEPTH: usize = 3;

impl Gate for RecentGate {
    fn id(&self) -> GateId {
        GateId::Recent
    }

    fn evaluate(&self, ctx: &GateCtx<'_>) -> GateStatus {
        let target = ctx.worktree.join(&self.dir);
        let now = ctx.clock.now();
        let quiet = ctx.cfg.cache_quiet;

        match newest_mtime(&target, MAX_DEPTH) {
            Err(e) => GateStatus::Unknown(Cause::Io { path: target, msg: e.to_string() }),
            Ok(None) => GateStatus::Pass, // 空目录，没什么可判的
            Ok(Some((path, mtime))) => match now.duration_since(mtime) {
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
            },
        }
    }
}

fn newest_mtime(root: &Path, depth: usize) -> std::io::Result<Option<(PathBuf, SystemTime)>> {
    let mut best: Option<(PathBuf, SystemTime)> = None;
    let mut stack = vec![(root.to_path_buf(), 0usize)];

    while let Some((dir, d)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue, // 单个条目读不到不影响整体判断
            };
            if let Ok(m) = meta.modified()
                && best.as_ref().is_none_or(|(_, b)| m > *b)
            {
                best = Some((entry.path(), m));
            }
            if meta.is_dir() && d + 1 < depth {
                stack.push((entry.path(), d + 1));
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
