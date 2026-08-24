//! 从扫描报告生成执行计划。
//!
//! 这一层的唯一职责是**把"用户选了什么"和"判定允许什么"求交集**。
//! 选择来自人（或 GUI 的复选框），判定来自门禁——两者不一致时一律以判定为准。
//!
//! 换句话说：**勾选不是授权。** 用户可以勾一个被拦下的 worktree，
//! 但它不会进入计划。想强行删除得走另一条明确的路（`--i-know-what-im-doing`），
//! 而那条路也绕不过抢救备份。

use crate::model::*;
use std::collections::HashSet;
use std::path::PathBuf;

/// 用户的选择。路径是唯一标识。
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// 要回收的缓存目录（`CacheDir::path`）。
    pub reclaim: HashSet<PathBuf>,
    /// 要删除的 worktree（`WorktreeReport::path`）。
    pub remove: HashSet<PathBuf>,
    /// 是否清理目录已消失的注册记录。
    pub prune: bool,
}

impl Selection {
    /// 选中所有判定允许的项。这是 `--yes` 的语义，**不是默认行为**——
    /// 默认是什么都不选，只报告。
    ///
    /// `include_main` 决定要不要连主工作区的构建缓存一起选上，**默认不选**。
    /// 主工作区是人天天在用的那个，它的缓存命中率最高、重建代价也最实在
    /// （实测一个 378 个依赖的 Rust 项目冷编译要 5–15 分钟），
    /// 不该和 agent 用完即弃的 worktree 同等对待。要清得显式说。
    pub fn everything_allowed(report: &ScanReport, include_main: bool) -> Self {
        let mut sel = Selection {
            prune: true,
            ..Default::default()
        };
        for repo in &report.repos {
            for wt in &repo.worktrees {
                if matches!(wt.verdict, Verdict::Removable) {
                    sel.remove.insert(wt.path.clone());
                }
                if wt.is_main && !include_main {
                    continue;
                }
                for c in &wt.caches {
                    if c.outcomes.iter().all(|o| o.status.is_pass()) {
                        sel.reclaim.insert(c.path.clone());
                    }
                }
            }
        }
        sel
    }
}

/// 一条待执行的动作。每条都带着判定时刻的指纹，apply 前要重算比对。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    ReclaimCache {
        worktree: PathBuf,
        cache: PathBuf,
        kind: CacheKind,
        bytes: u64,
        expect: Fingerprint,
    },
    RemoveWorktree {
        repo: PathBuf,
        worktree: PathBuf,
        bytes: u64,
        expect: Fingerprint,
    },
    /// 清理注册表里目录已消失的条目。
    ///
    /// `confirmed_missing` 是执行前又确认过一次确实不存在的路径——
    /// 外置盘临时没挂载时目录也"不存在"，无条件 prune 会把人家的注册记录删掉（D13）。
    PruneAdmin {
        repo: PathBuf,
        confirmed_missing: Vec<PathBuf>,
    },
}

impl Action {
    /// 这条动作预计能腾出多少。**是上界**：APFS 写时复制与跨树硬链接都会让它偏高，
    /// 真实数字以 apply 前后的可用空间差为准。
    pub fn estimated_bytes(&self) -> u64 {
        match self {
            Action::ReclaimCache { bytes, .. } | Action::RemoveWorktree { bytes, .. } => *bytes,
            Action::PruneAdmin { .. } => 0,
        }
    }

    pub fn target(&self) -> &std::path::Path {
        match self {
            Action::ReclaimCache { cache, .. } => cache,
            Action::RemoveWorktree { worktree, .. } => worktree,
            Action::PruneAdmin { repo, .. } => repo,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub actions: Vec<Action>,
    /// 被用户勾选、但判定不允许而落选的项，连同落选理由。
    ///
    /// 必须报给用户：默默少做一件他勾了的事，比不做更糟。
    pub rejected: Vec<(PathBuf, String)>,
}

impl Plan {
    pub fn estimated_bytes(&self) -> u64 {
        self.actions.iter().map(Action::estimated_bytes).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

/// 求交集：用户选了什么 ∩ 判定允许什么。
pub fn plan(report: &ScanReport, sel: &Selection) -> Plan {
    let mut out = Plan::default();

    for repo in &report.repos {
        for wt in &repo.worktrees {
            // 缓存回收：只要该缓存的 A 组三道全过
            for c in &wt.caches {
                if !sel.reclaim.contains(&c.path) {
                    continue;
                }
                match c.outcomes.iter().find(|o| !o.status.is_pass()) {
                    None => out.actions.push(Action::ReclaimCache {
                        worktree: wt.path.clone(),
                        cache: c.path.clone(),
                        kind: c.kind.clone(),
                        bytes: c.bytes,
                        expect: wt.fingerprint.clone(),
                    }),
                    Some(blocker) => out.rejected.push((
                        c.path.clone(),
                        format!("门禁 {} 未通过", blocker.id.as_str()),
                    )),
                }
            }

            // 整个 worktree 删除：只接受 Removable，其余一律落选
            if sel.remove.contains(&wt.path) {
                match &wt.verdict {
                    Verdict::Removable => out.actions.push(Action::RemoveWorktree {
                        repo: repo.root.clone(),
                        worktree: wt.path.clone(),
                        bytes: wt.bytes,
                        expect: wt.fingerprint.clone(),
                    }),
                    v => out.rejected.push((wt.path.clone(), describe_verdict(v))),
                }
            }
        }

        if sel.prune && !repo.prunable.is_empty() {
            out.actions.push(Action::PruneAdmin {
                repo: repo.root.clone(),
                confirmed_missing: repo.prunable.clone(),
            });
        }
    }

    out
}

fn describe_verdict(v: &Verdict) -> String {
    match v {
        Verdict::Blocked { by } => format!(
            "被门禁拦下：{}",
            by.iter().map(|g| g.as_str()).collect::<Vec<_>>().join("、")
        ),
        Verdict::NeedsAttention { unknown } => format!(
            "判不准（{}），不放行",
            unknown
                .iter()
                .map(|g| g.as_str())
                .collect::<Vec<_>>()
                .join("、")
        ),
        Verdict::Protected { why } => format!("受保护：{why}"),
        Verdict::CacheReclaimable => "只能回收缓存，不能整个删除".into(),
        Verdict::Removable => unreachable_removable(),
    }
}

/// `Removable` 在上面已被匹配掉，走到这里说明控制流写错了。
/// 不用 `unreachable!()` 是因为 panic 在一个删文件的工具里代价太高。
fn unreachable_removable() -> String {
    "内部状态异常，保守起见不执行".into()
}
