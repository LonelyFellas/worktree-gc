//! 领域模型。
//!
//! 这里最重要的类型是 [`GateStatus`] 与 [`Verdict`]：整个工具的安全性建立在
//! 「判不准就说判不准」之上，所以状态是**三态**而非布尔。见 docs/design.md 的 D7。

use serde::Serialize;
use std::path::PathBuf;

/// 门禁编号。A 组管「回收构建缓存」，B 组管「删除整个 worktree」。
///
/// 两组分开不是为了整齐——原型阶段的致命缺陷正是缓存回收不受任何门禁约束，
/// 对那些因为「有未提交改动」「有进程占用」而被保留的 worktree 直接删了 target。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum GateId {
    /// A1：目录当前无进程占用。
    Busy,
    /// A2：缓存目录本身近期无写入。
    Recent,
    /// A3：目标确实是可重建的构建产物（被忽略、无 tracked 文件、在根内、非 symlink）。
    CacheSafe,
    /// B1：整个 worktree 已达到配置的空闲时长。
    Idle,
    /// B2：无未提交改动。
    Dirty,
    /// B3：工作已进主干。
    Landed,
    /// B4：无独有的敏感文件。
    Precious,
    /// B5：内部没有嵌套的其它 worktree / git 仓。
    Nested,
    /// B6：不处于 rebase / merge / cherry-pick / bisect 中间态。
    InProgress,
    /// B7：未被 `git worktree lock` 锁定。
    Locked,
}

impl GateId {
    /// A 组：回收构建缓存所需的门禁。
    pub const CACHE: [GateId; 3] = [GateId::Busy, GateId::Recent, GateId::CacheSafe];

    /// B 组：删除整个 worktree 在 Busy 之外额外需要的门禁。
    pub const REMOVE: [GateId; 7] = [
        GateId::Idle,
        GateId::Dirty,
        GateId::Landed,
        GateId::Precious,
        GateId::Nested,
        GateId::InProgress,
        GateId::Locked,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            GateId::Busy => "busy",
            GateId::Recent => "recent",
            GateId::CacheSafe => "cache-safe",
            GateId::Idle => "idle",
            GateId::Dirty => "dirty",
            GateId::Landed => "landed",
            GateId::Precious => "precious",
            GateId::Nested => "nested",
            GateId::InProgress => "in-progress",
            GateId::Locked => "locked",
        }
    }
}

/// 门禁未通过的具体原因。**结构化枚举，不是拼好的字符串**——
/// CLI / JSON / HTML / 未来的 GUI 各自渲染，快照测试也才稳定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum GateDetail {
    /// 有进程正在使用该目录。
    ProcessesActive { pids: Vec<u32>, sample: Vec<String> },
    /// 近期仍有写入。
    RecentlyModified { newest_path: PathBuf, age_secs: u64 },
    /// 目标不是纯粹的可重建产物。
    NotPureCache { reason: CacheUnsafeReason },
    /// 有未提交改动。
    UncommittedChanges { count: usize, sample: Vec<String> },
    /// 工作尚未进入主干。
    NotLanded { ahead: usize, baseline: String },
    /// 含有主仓没有、或内容不同的敏感文件。
    PreciousFiles { paths: Vec<PathBuf> },
    /// 内部嵌套了其它 git 工作区。
    NestedWorktrees { paths: Vec<PathBuf> },
    /// 处于 rebase / merge 等中间态。
    OperationInProgress { kind: &'static str },
    /// 被显式锁定。
    WorktreeLocked { reason: Option<String> },
}

/// A3 门禁拒绝的具体理由。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum CacheUnsafeReason {
    /// 缺少生态佐证文件，无法证明同名目录真的是该生态的构建产物。
    MissingMarker { expected: Vec<String> },
    /// 目录未被 gitignore —— 说明它可能是源码而非产物。
    NotIgnored,
    /// 目录内存在被 git 跟踪的文件（比如入库的 `dist/`），删了就是永久损失。
    ContainsTrackedFiles { sample: Vec<String> },
    /// 是符号链接，删它可能波及链接目标之外的东西。
    IsSymlink,
    /// canonicalize 之后不在 worktree 根之下。
    EscapesWorktree { resolved: PathBuf },
    /// 不匹配任何已知的构建缓存规则。
    NoMatchingRule,
}

/// 判定为 `Unknown` 的成因。**能力缺失、命令失败、超时一律落这里**，
/// 绝不允许静默当成 `Pass`——那正是原型阶段最危险的 fail-open 形状。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Cause {
    /// 子命令非零退出。stderr 原样保留用于展示与归因。
    CommandFailed {
        cmd: String,
        code: Option<i32>,
        stderr: String,
    },
    /// 子命令超时。
    Timeout { cmd: String, secs: u64 },
    /// 本平台不具备这项能力（如 Windows 上拿不到进程 cwd）。
    Unsupported {
        what: &'static str,
        platform: &'static str,
    },
    /// 依赖的外部程序不存在（如未装 gh）。
    ToolMissing { tool: &'static str },
    /// 网络不可达或未鉴权，判据被迫降级。
    ForgeUnavailable { detail: String },
    /// 文件系统错误。
    Io { path: PathBuf, msg: String },
}

/// 单道门禁的结果。三态外加 `Skipped`（该门禁在当前动作下不适用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum GateStatus {
    Pass,
    Blocked(GateDetail),
    Unknown(Cause),
    Skipped,
}

impl GateStatus {
    /// **唯一允许放行的判断**。注意 `Unknown` 与 `Skipped` 都不算通过——
    /// 想加一个「Unknown 也放行」的分支时，请先重读 docs/design.md 的 D7。
    pub fn is_pass(&self) -> bool {
        matches!(self, GateStatus::Pass)
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, GateStatus::Unknown(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateOutcome {
    pub id: GateId,
    pub status: GateStatus,
}

/// 对一个 worktree 的最终判定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Verdict {
    /// A 组全过：可以回收它的构建缓存（源码与改动保留）。
    CacheReclaimable,
    /// A + B 全过：整个 worktree 可以删除。
    Removable,
    /// 有门禁明确拒绝。
    Blocked { by: Vec<GateId> },
    /// 有门禁判不准。**任何一个 Unknown 都落这里，永不放行。**
    NeedsAttention { unknown: Vec<GateId> },
    /// 主工作区，或被用户显式锁定——工具永不触碰。
    Protected { why: &'static str },
}

impl Verdict {
    /// 从门禁结果推出判定。顺序很重要：
    /// **`Unknown` 优先于 `Blocked`**——判不准比判定拒绝更需要人看一眼。
    pub fn from_outcomes(cache: &[GateOutcome], remove: Option<&[GateOutcome]>) -> Verdict {
        let all: Vec<&GateOutcome> = cache.iter().chain(remove.unwrap_or(&[]).iter()).collect();

        let unknown: Vec<GateId> = all
            .iter()
            .filter(|o| o.status.is_unknown())
            .map(|o| o.id)
            .collect();
        if !unknown.is_empty() {
            return Verdict::NeedsAttention { unknown };
        }

        let blocked: Vec<GateId> = all
            .iter()
            .filter(|o| matches!(o.status, GateStatus::Blocked(_)))
            .map(|o| o.id)
            .collect();
        if !blocked.is_empty() {
            return Verdict::Blocked { by: blocked };
        }

        match remove {
            Some(_) => Verdict::Removable,
            None => Verdict::CacheReclaimable,
        }
    }
}

/// 执行前重算并比对的指纹，用于防 TOCTOU（扫描到执行之间 agent 重新开工）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fingerprint {
    pub head_oid: String,
    pub dirty_count: usize,
    pub busy_pids: Vec<u32>,
    pub precious_digest: String,
}

/// 已识别的构建缓存类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheKind {
    pub name: String,
    /// 与 config::CacheRule 同为 String——规则表要能从配置文件反序列化出来。
    pub ecosystem: String,
}

/// 一个缓存目录及其体积。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheDir {
    pub path: PathBuf,
    pub kind: CacheKind,
    pub bytes: u64,
    pub outcomes: Vec<GateOutcome>,
}

/// 一个 worktree 的完整报告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeReport {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head_oid: String,
    pub is_main: bool,
    pub bytes: u64,
    pub caches: Vec<CacheDir>,
    pub outcomes: Vec<GateOutcome>,
    pub verdict: Verdict,
    pub fingerprint: Fingerprint,
}

impl WorktreeReport {
    /// 可零代码损失回收的字节数。**只统计判定放行的缓存**。
    pub fn reclaimable_bytes(&self) -> u64 {
        self.caches
            .iter()
            .filter(|c| c.outcomes.iter().all(|o| o.status.is_pass()))
            .map(|c| c.bytes)
            .sum()
    }
}

/// 主干基线，表达为 `(remote, branch)` 二元组。
///
/// 不写死 `origin`：实测本机的 tsz 仓同时挂着 origin(GitHub) 与 gitee 两个远端。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Baseline {
    pub remote: Option<String>,
    pub branch: String,
    /// 是怎么探测出来的，报告里要显示——用户需要知道判定跑在哪个 ref 上。
    pub source: BaselineSource,
}

impl Baseline {
    /// git 可用的 ref 名，如 `origin/main` 或 `main`。
    pub fn refname(&self) -> String {
        match &self.remote {
            Some(r) => format!("{r}/{}", self.branch),
            None => self.branch.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BaselineSource {
    /// 用户在配置里显式指定。
    Explicit,
    /// `git symbolic-ref refs/remotes/<remote>/HEAD`。
    RemoteHead,
    /// `git config init.defaultBranch` 或常见名探测。
    Guessed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoReport {
    pub root: PathBuf,
    /// 基线探测失败时为 None —— 此时整仓标红，**不静默跳过**（见 D12）。
    pub baseline: Option<Baseline>,
    pub baseline_error: Option<Cause>,
    pub worktrees: Vec<WorktreeReport>,
    /// 目录已消失、可 prune 的注册记录。
    pub prunable: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanReport {
    pub repos: Vec<RepoReport>,
    /// 扫描时刻各卷的可用空间，用于执行后报实测差值（而非 du 的估算值）。
    pub available_bytes: u64,
    /// 实际用到的 git / gh 路径与版本，打进报告便于排查 PATH 问题。
    pub tools: Vec<ToolInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolInfo {
    pub name: &'static str,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
}
