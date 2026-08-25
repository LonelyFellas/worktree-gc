//! 门禁框架。
//!
//! 两组门禁对应两个破坏性等级完全不同的动作：
//! - **A 组**（[`GateId::CACHE`]）：回收构建缓存，只丢可重建的产物。
//! - **B 组**（[`GateId::REMOVE`]）：删除整个 worktree，在 Busy 之上再加七道。
//!
//! 拆开不是为了整齐。原型阶段的致命缺陷正是缓存回收**完全不受门禁约束**，
//! 会对正在构建的 worktree 抽走 target、对入库的 `dist/` 直接 rm -rf。

// A 组：回收构建缓存
pub mod busy;
pub mod cachesafe;
pub mod recent;

// B 组：删除整个 worktree（Busy 之外的额外门禁）
pub mod dirty;
pub mod inprogress;
pub mod landed;
pub mod locked;
pub mod nested;
pub mod precious;

use crate::config::ScanConfig;
use crate::git::GitRunner;
use crate::model::{Baseline, GateId, GateStatus};
use std::path::Path;
use std::time::SystemTime;

/// 进程表。抽象出来既为跨平台，也为在测试里造 busy 状态。
pub trait ProcessTable: Send + Sync {
    /// 找出工作目录位于 `dir` 之下的进程。
    ///
    /// **必须同时看 cwd 与命令行**：实测 `pgrep -f <path>` 对 `cargo build`
    /// 这类「cwd 在目录内但 argv 不含路径」的进程 100% 假阴性（D11）。
    /// 平台拿不到 cwd 时返回 `Err`，由调用方落成 `Unknown`——绝不返回空集当作「没占用」。
    fn processes_under(&self, dir: &Path) -> Result<Vec<ProcInfo>, crate::model::Cause>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
}

/// 时钟。抽象出来是为了让 idle 阈值可测。
pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// 合入状态查询（GitHub / GitLab）。网络不可达时返回 `Err` → `Unknown`。
pub trait MergeStatusProvider: Send + Sync {
    /// 查询该分支是否已有被合入的 PR。
    ///
    /// 实现必须做两件事，否则会误判（D6）：
    /// 1. 显式指定仓库（`gh` 按 cwd 解析仓库，跨仓查询会永远查不到）
    /// 2. 用 `owner:branch` 限定并比对 headRefOid，否则会匹配到任意 fork 的同名分支
    fn merged_pr(
        &self,
        repo: &Path,
        branch: &str,
        head_oid: &str,
    ) -> Result<Option<u64>, crate::model::Cause>;
}

/// 门禁求值所需的全部上下文。
pub struct GateCtx<'a> {
    pub repo: &'a Path,
    pub worktree: &'a Path,
    pub branch: Option<&'a str>,
    pub head_oid: &'a str,
    pub baseline: Option<&'a Baseline>,
    pub cfg: &'a ScanConfig,
    pub git: &'a dyn GitRunner,
    pub procs: &'a dyn ProcessTable,
    pub clock: &'a dyn Clock,
    pub forge: &'a dyn MergeStatusProvider,
}

/// 一道门禁。
pub trait Gate {
    fn id(&self) -> GateId;

    /// 求值。**任何无法判断的情况都必须返回 [`GateStatus::Unknown`]**，
    /// 不许吞掉错误后返回 `Pass`——那是 D7 描述的 fail-open，是这个工具最危险的失效形态。
    fn evaluate(&self, ctx: &GateCtx<'_>) -> GateStatus;
}
