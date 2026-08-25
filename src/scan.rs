//! 扫描编排：把十道门禁串成对整机仓库的判定。
//!
//! 这一层只读，不做任何破坏性动作。它的产出 [`ScanReport`] 是后续 plan/apply 的唯一输入。
//!
//! 两个层级的门禁不能混为一谈：
//! - **worktree 级**：Busy + B 组七道 → 决定整个 worktree 能不能删
//! - **缓存目录级**：Busy（继承自 worktree）+ Recent + CacheSafe → 决定这个 target/ 能不能回收
//!
//! 一个 worktree 完全可能"不能删、但缓存能回收"——这正是最常见也最有价值的那种情形。

use crate::config::ScanConfig;
use crate::discover::{self, Discovered};
use crate::facts::{self, WorktreeFacts};
use crate::gates::{
    Clock, Gate, GateCtx, MergeStatusProvider, ProcessTable, busy, cachesafe, dirty, inprogress,
    landed, locked, nested, precious, recent,
};
use crate::git::{GitRunner, base, exe, porcelain::WorktreeEntry};
use crate::model::*;
use crate::platform::disk;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;

const MAX_SCAN_THREADS: usize = 4;

/// 全部副作用依赖集中注入。这是可测性的支点，也是否定性测试的入口——
/// 注入一个「总是失败」的 GitRunner，就能断言没有任何 worktree 被判为可清理。
pub struct Env {
    pub git: Box<dyn GitRunner>,
    pub forge: Box<dyn MergeStatusProvider>,
    pub clock: Box<dyn Clock>,
    pub procs: Box<dyn ProcessTable>,
}

/// 扫描全部配置到的仓库。
///
/// 单个仓库出问题不会让整次扫描失败——它会以 `baseline_error` 或某道门禁的 `Unknown`
/// 出现在报告里。**静默跳过是不允许的**：用户会把"没扫到"误读成"没东西可清"。
pub fn scan(cfg: &ScanConfig, env: &Env) -> ScanReport {
    let discovered = discover::discover(cfg, env.git.as_ref());
    let process_paths: Vec<_> = discovered
        .iter()
        .flat_map(|repo| repo.worktrees.iter().map(|entry| entry.path.clone()))
        .collect();
    let process_snapshot = ScanProcessTable::capture(env.procs.as_ref(), &process_paths);
    let scan_all = || {
        discovered
            .par_iter()
            .map(|d| scan_repo(d, cfg, env, &process_snapshot))
            .collect()
    };
    let repos = match rayon::ThreadPoolBuilder::new()
        .num_threads(MAX_SCAN_THREADS)
        .thread_name(|index| format!("wtgc-scan-{index}"))
        .build()
    {
        Ok(pool) => pool.install(scan_all),
        // 线程池初始化失败时退回顺序扫描，不能让性能设施改变安全判定的可用性。
        Err(_) => discovered
            .iter()
            .map(|d| scan_repo(d, cfg, env, &process_snapshot))
            .collect(),
    };

    // 可用空间在扫描时刻取一次，apply 之后再取一次，用差值报**实测**回收量。
    // du 的口径在 APFS 上会因写时复制系统性高估，不能拿它当交付数字。
    let available_bytes = std::env::current_dir()
        .ok()
        .and_then(|p| disk::available_bytes(&p).ok())
        .unwrap_or(0);

    ScanReport {
        repos,
        available_bytes,
        tools: vec![
            exe::probe("git", "--version"),
            exe::probe("gh", "--version"),
        ],
    }
}

/// 一轮扫描只刷新一次系统进程表。门禁仍通过 ProcessTable 查询，apply 可继续使用
/// 实时平台实现；扫描阶段则从这份不可变快照读取，保证同轮结果自洽。
struct ScanProcessTable {
    by_path: HashMap<std::path::PathBuf, Result<Vec<crate::gates::ProcInfo>, Cause>>,
}

impl ScanProcessTable {
    fn capture(source: &dyn ProcessTable, paths: &[std::path::PathBuf]) -> Self {
        let results = source.processes_under_many(paths);
        let mut by_path = HashMap::with_capacity(paths.len());
        for (index, path) in paths.iter().enumerate() {
            let result = results.get(index).cloned().unwrap_or_else(|| {
                Err(Cause::CommandFailed {
                    cmd: "snapshot process table".into(),
                    code: None,
                    stderr: format!(
                        "批量进程查询返回 {} 项，但需要 {} 项",
                        results.len(),
                        paths.len()
                    ),
                })
            });
            by_path.insert(path.clone(), result);
        }
        Self { by_path }
    }
}

impl ProcessTable for ScanProcessTable {
    fn processes_under(&self, dir: &Path) -> Result<Vec<crate::gates::ProcInfo>, Cause> {
        self.by_path.get(dir).cloned().unwrap_or_else(|| {
            Err(Cause::CommandFailed {
                cmd: "query process snapshot".into(),
                code: None,
                stderr: format!("进程快照里没有 {}", dir.display()),
            })
        })
    }
}

fn scan_repo(d: &Discovered, cfg: &ScanConfig, env: &Env, procs: &dyn ProcessTable) -> RepoReport {
    let (baseline, baseline_error) = match base::detect(env.git.as_ref(), &d.root, &cfg.baseline) {
        Ok(b) => (Some(b), None),
        Err(c) => (None, Some(c)),
    };

    let mut worktrees: Vec<WorktreeReport> = d
        .worktrees
        .par_iter()
        .map(|w| scan_worktree(&d.root, w, &d.worktrees, baseline.as_ref(), cfg, env, procs))
        .collect();
    subtract_nested_sizes(&mut worktrees);

    RepoReport {
        root: d.root.clone(),
        baseline,
        baseline_error,
        worktrees,
        prunable: d.prunable.clone(),
    }
}

fn scan_worktree(
    repo: &Path,
    entry: &WorktreeEntry,
    entries: &[WorktreeEntry],
    baseline: Option<&Baseline>,
    cfg: &ScanConfig,
    env: &Env,
    procs: &dyn ProcessTable,
) -> WorktreeReport {
    let head_oid = entry.head.clone().unwrap_or_default();
    let is_main = entry.path == repo;

    let ctx = GateCtx {
        repo,
        worktree: &entry.path,
        branch: entry.branch.as_deref(),
        head_oid: &head_oid,
        baseline,
        cfg,
        git: env.git.as_ref(),
        procs,
        clock: env.clock.as_ref(),
        forge: env.forge.as_ref(),
    };

    // Busy 同时服务两个层级：worktree 能不能删要看它，缓存能不能回收也要看它。
    // 只求值一次，两处共用——重复起进程扫描是这套流程里最贵的一步。
    let busy = GateOutcome {
        id: GateId::Busy,
        status: busy::BusyGate.evaluate(&ctx),
    };

    let cache_dirs = cachesafe::candidates(ctx.worktree, ctx.cfg);
    let facts = facts::collect(ctx.worktree, &cache_dirs, ctx.cfg);
    let caches = scan_caches(&ctx, &busy, &cache_dirs, &facts);
    let bytes = facts.bytes;

    // 主工作区永不触碰。它不是"恰好过不了门禁"，是根本不进入判定。
    if is_main {
        return WorktreeReport {
            path: entry.path.clone(),
            branch: entry.branch.clone(),
            head_oid: head_oid.clone(),
            is_main,
            bytes,
            caches,
            outcomes: vec![busy],
            verdict: Verdict::Protected {
                why: "主工作区"
            },
            fingerprint: Fingerprint {
                head_oid,
                dirty_count: 0,
                busy_pids: Vec::new(),
                precious_digest: String::new(),
            },
        };
    }

    let b_gates: Vec<GateOutcome> = vec![
        GateOutcome {
            id: GateId::Idle,
            status: recent::IdleGate.evaluate_activity(&ctx, facts.activity.clone()),
        },
        GateOutcome {
            id: GateId::Dirty,
            status: dirty::DirtyGate.evaluate(&ctx),
        },
        GateOutcome {
            id: GateId::Landed,
            status: landed::LandedGate.evaluate(&ctx),
        },
        GateOutcome {
            id: GateId::Precious,
            status: precious::PreciousGate.evaluate(&ctx),
        },
        GateOutcome {
            id: GateId::Nested,
            status: nested::NestedGate.evaluate_entries_with_filesystem(
                &ctx,
                entries,
                facts.nested_git.clone(),
            ),
        },
        GateOutcome {
            id: GateId::InProgress,
            status: inprogress::InProgressGate.evaluate(&ctx),
        },
        GateOutcome {
            id: GateId::Locked,
            status: locked::LockedGate.evaluate_entry(entry),
        },
    ];

    let verdict = Verdict::from_outcomes(std::slice::from_ref(&busy), Some(&b_gates));
    let fingerprint = fingerprint_of(&head_oid, &busy, &b_gates);

    let mut outcomes = vec![busy];
    outcomes.extend(b_gates);

    WorktreeReport {
        path: entry.path.clone(),
        branch: entry.branch.clone(),
        head_oid,
        is_main,
        bytes,
        caches,
        outcomes,
        verdict,
        fingerprint,
    }
}

fn scan_caches(
    ctx: &GateCtx<'_>,
    busy: &GateOutcome,
    cache_dirs: &[String],
    facts: &WorktreeFacts,
) -> Vec<CacheDir> {
    cache_dirs
        .iter()
        .cloned()
        .map(|dir| {
            let cache_facts = facts.cache(&dir);
            let outcomes = vec![
                busy.clone(),
                GateOutcome {
                    id: GateId::Recent,
                    status: cache_facts.map_or_else(
                        || {
                            GateStatus::Unknown(Cause::Io {
                                path: ctx.worktree.join(&dir),
                                msg: "文件树事实里缺少缓存目录".into(),
                            })
                        },
                        |facts| {
                            recent::RecentGate { dir: dir.clone() }
                                .evaluate_activity(ctx, facts.activity.clone())
                        },
                    ),
                },
                GateOutcome {
                    id: GateId::CacheSafe,
                    status: cachesafe::CacheSafeGate { dir: dir.clone() }.evaluate(ctx),
                },
            ];
            let path = ctx.worktree.join(&dir);
            let bytes = cache_facts.map_or(0, |facts| facts.bytes);
            let ecosystem = ctx
                .cfg
                .cache_rules
                .iter()
                .find(|r| r.dir == dir)
                .map(|r| r.ecosystem.clone())
                .unwrap_or_default();
            CacheDir {
                path,
                kind: CacheKind {
                    name: dir,
                    ecosystem,
                },
                bytes,
                outcomes,
            }
        })
        .collect()
}

/// 扣掉嵌套 worktree 的体积，避免重复计数。
///
/// Claude Code 把 worktree 建在 `<repo>/.claude/worktrees/` 下，遍历主工作区时
/// 会把它们连同各自 30GB 的 target 一起算进去 —— 实测主仓因此显示 113.9G，
/// 而其中 90G 是那三个 worktree 自己的，汇总行会因此严重高估。
///
/// 扣的是**后代的原始体积**：逐层相减会漏掉孙辈（B 的体积已经扣过 C，
/// 再从 A 里减 B 就把 C 漏在 A 里了）。
fn subtract_nested_sizes(wts: &mut [WorktreeReport]) {
    let raw: Vec<(std::path::PathBuf, u64)> =
        wts.iter().map(|w| (w.path.clone(), w.bytes)).collect();
    for w in wts.iter_mut() {
        let nested: u64 = raw
            .iter()
            .filter(|(p, _)| p != &w.path && p.starts_with(&w.path))
            .map(|(_, b)| *b)
            .sum();
        w.bytes = w.bytes.saturating_sub(nested);
    }
}

/// 冻结判定时刻的状态，apply 前重算比对，防 TOCTOU（扫描到执行之间 agent 重新开工）。
fn fingerprint_of(head_oid: &str, busy: &GateOutcome, b: &[GateOutcome]) -> Fingerprint {
    let busy_pids = match &busy.status {
        GateStatus::Blocked(GateDetail::ProcessesActive { pids, .. }) => pids.clone(),
        _ => Vec::new(),
    };
    let dirty_count = b
        .iter()
        .find_map(|o| match &o.status {
            GateStatus::Blocked(GateDetail::UncommittedChanges { count, .. }) => Some(*count),
            _ => None,
        })
        .unwrap_or(0);
    let precious_digest = b
        .iter()
        .find_map(|o| match &o.status {
            GateStatus::Blocked(GateDetail::PreciousFiles { paths }) => Some(
                paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\u{1}"),
            ),
            _ => None,
        })
        .unwrap_or_default();

    Fingerprint {
        head_oid: head_oid.to_string(),
        dirty_count,
        busy_pids,
        precious_digest,
    }
}
