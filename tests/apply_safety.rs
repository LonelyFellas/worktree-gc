#![allow(clippy::expect_used)]
//! 破坏性路径的安全护栏。
//!
//! **这个文件里几乎全是否定性断言**——证明某件事没有发生。
//! 一个删文件的工具，"该删的删了"只是及格线，"不该删的一个都没碰"才是它的价值。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use wtgc::apply::{ApplyOptions, Outcome, apply};
use wtgc::config::ScanConfig;
use wtgc::gates::{ProcInfo, SystemClock};
use wtgc::git::{GitExec, GitRunner, RealGit};
use wtgc::model::*;
use wtgc::plan::{Action, Selection, plan};
use wtgc::scan::Env;
use wtgc::testkit::{FakeProcs, RecordingGit, SpyFs, TempRepo, test_git};

fn fingerprint(dirty: usize, pids: Vec<u32>) -> Fingerprint {
    Fingerprint {
        head_oid: "abc123".into(),
        dirty_count: dirty,
        busy_pids: pids,
        precious_digest: String::new(),
    }
}

fn wt(path: &str, verdict: Verdict, fp: Fingerprint) -> WorktreeReport {
    WorktreeReport {
        path: PathBuf::from(path),
        branch: Some("feat/x".into()),
        head_oid: "abc123".into(),
        is_main: false,
        bytes: 1000,
        caches: Vec::new(),
        outcomes: Vec::new(),
        verdict,
        fingerprint: fp,
    }
}

fn report_with(worktrees: Vec<WorktreeReport>) -> ScanReport {
    ScanReport {
        repos: vec![RepoReport {
            root: PathBuf::from("/repo"),
            baseline: None,
            baseline_error: None,
            worktrees,
            prunable: Vec::new(),
        }],
        available_bytes: 0,
        tools: Vec::new(),
    }
}

fn env_with(git: RecordingGit, procs: Vec<ProcInfo>) -> Env {
    env_with_runner(git, procs)
}

fn env_with_runner(git: impl GitRunner + 'static, procs: Vec<ProcInfo>) -> Env {
    Env {
        git: Box::new(git),
        forge: Box::new(wtgc::forge::Offline),
        clock: Box::new(SystemClock),
        procs: Box::new(FakeProcs(procs)),
    }
}

struct RecordingRealGit {
    inner: RealGit,
    calls: Arc<Mutex<Vec<String>>>,
    fail_remove: bool,
}

impl RecordingRealGit {
    fn new(fail_remove: bool) -> (Self, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                inner: test_git(),
                calls: Arc::clone(&calls),
                fail_remove,
            },
            calls,
        )
    }
}

impl GitRunner for RecordingRealGit {
    fn exec(&self, cwd: &std::path::Path, args: &[&str]) -> Result<GitExec, Cause> {
        let joined = args.join(" ");
        self.calls.lock().expect("调用日志锁").push(joined.clone());
        if self.fail_remove && joined.starts_with("worktree remove ") {
            return Ok(GitExec {
                code: Some(128),
                stdout: Vec::new(),
                stderr: "模拟 git 拒绝删除".into(),
            });
        }
        self.inner.exec(cwd, args)
    }
}

fn removal_plan(repo: &TempRepo, worktree: PathBuf, head: String) -> wtgc::plan::Plan {
    wtgc::plan::Plan {
        actions: vec![Action::RemoveWorktree {
            repo: repo.root.clone(),
            worktree,
            expect: Fingerprint {
                head_oid: head,
                dirty_count: 0,
                busy_pids: Vec::new(),
                precious_digest: String::new(),
            },
            bytes: 100,
        }],
        rejected: Vec::new(),
    }
}

// ───────────────────────── plan 层：勾选不是授权 ─────────────────────────

#[test]
fn selecting_a_blocked_worktree_does_not_plan_it() {
    let r = report_with(vec![wt(
        "/repo/wt",
        Verdict::Blocked {
            by: vec![GateId::Dirty],
        },
        fingerprint(3, vec![]),
    )]);
    let sel = Selection {
        remove: HashSet::from([PathBuf::from("/repo/wt")]),
        ..Default::default()
    };

    let p = plan(&r, &sel);
    assert!(p.is_empty(), "被拦下的 worktree 即使勾选也不该进入计划");
    assert_eq!(p.rejected.len(), 1, "落选必须上报，默默少做比不做更糟");
}

#[test]
fn selecting_a_needs_attention_worktree_does_not_plan_it() {
    // 判不准比判定拒绝更危险——它意味着我们不知道那里有什么
    let r = report_with(vec![wt(
        "/repo/wt",
        Verdict::NeedsAttention {
            unknown: vec![GateId::Busy],
        },
        fingerprint(0, vec![]),
    )]);
    let sel = Selection {
        remove: HashSet::from([PathBuf::from("/repo/wt")]),
        ..Default::default()
    };

    assert!(plan(&r, &sel).is_empty(), "判不准的绝不放行");
}

#[test]
fn everything_allowed_never_picks_up_blocked_items() {
    let r = report_with(vec![
        wt("/repo/ok", Verdict::Removable, fingerprint(0, vec![])),
        wt(
            "/repo/bad",
            Verdict::Blocked {
                by: vec![GateId::Precious],
            },
            fingerprint(0, vec![]),
        ),
    ]);
    let sel = Selection::everything_allowed(&r, true);
    assert!(sel.remove.contains(&PathBuf::from("/repo/ok")));
    assert!(
        !sel.remove.contains(&PathBuf::from("/repo/bad")),
        "--yes 也不该碰被拦下的"
    );
}

// ───────────────────────── apply 层：dry-run 必须真的什么都不做 ─────────────────────────

#[test]
fn dry_run_emits_no_destructive_command_at_all() {
    let r = report_with(vec![wt(
        "/repo/wt",
        Verdict::Removable,
        fingerprint(0, vec![]),
    )]);
    let sel = Selection {
        remove: HashSet::from([PathBuf::from("/repo/wt")]),
        ..Default::default()
    };
    let p = plan(&r, &sel);
    assert!(!p.is_empty(), "前提：计划里确实有东西，否则本测试是假绿的");

    let git = RecordingGit::new();
    let fs = SpyFs::new();
    let env = env_with(git, vec![]);
    let out = apply(
        &p,
        &ApplyOptions {
            dry_run: true,
            audit_log: None,
        },
        &ScanConfig::default(),
        &env,
        &fs,
    );

    assert!(fs.removals().is_empty(), "dry-run 不该发出任何删除");
    assert!(matches!(out.results[0].outcome, Outcome::Simulated));
}

// ───────────────────────── apply 层：TOCTOU 复检 ─────────────────────────

#[test]
fn a_worktree_that_became_busy_is_skipped_not_deleted() {
    // 扫描时空闲，执行时 agent 又开工了——这是 GUI 场景下几分钟窗口的真实情形
    let r = report_with(vec![wt(
        "/repo/wt",
        Verdict::Removable,
        fingerprint(0, vec![]),
    )]);
    let sel = Selection {
        remove: HashSet::from([PathBuf::from("/repo/wt")]),
        ..Default::default()
    };
    let p = plan(&r, &sel);

    let git = RecordingGit::new();
    let fs = SpyFs::new();
    let env = env_with(
        git,
        vec![ProcInfo {
            pid: 999,
            name: "cargo".into(),
        }],
    );
    let out = apply(
        &p,
        &ApplyOptions {
            dry_run: false,
            audit_log: None,
        },
        &ScanConfig::default(),
        &env,
        &fs,
    );

    assert!(
        matches!(out.results[0].outcome, Outcome::Stale { .. }),
        "状态变了必须跳过，实际 {:?}",
        out.results[0].outcome
    );
    assert_eq!(out.stale_count(), 1);
}

#[test]
fn new_uncommitted_changes_since_scan_abort_the_removal() {
    let r = report_with(vec![wt(
        "/repo/wt",
        Verdict::Removable,
        fingerprint(0, vec![]),
    )]);
    let sel = Selection {
        remove: HashSet::from([PathBuf::from("/repo/wt")]),
        ..Default::default()
    };
    let p = plan(&r, &sel);

    // status 现在吐出两条改动，而指纹记的是 0
    let mut git = RecordingGit::new();
    git.stdout = b" M a.txt\0 M b.txt\0".to_vec();
    let fs = SpyFs::new();
    let env = env_with(git, vec![]);
    let out = apply(
        &p,
        &ApplyOptions {
            dry_run: false,
            audit_log: None,
        },
        &ScanConfig::default(),
        &env,
        &fs,
    );

    assert!(
        matches!(out.results[0].outcome, Outcome::Stale { .. }),
        "扫描后新增的改动必须中止删除，实际 {:?}",
        out.results[0].outcome
    );
}

// ───────────────────────── apply 层：绝不退化 ─────────────────────────

#[test]
fn failed_worktree_remove_never_falls_back_to_force_or_rm() {
    // git 拒绝删除时，它的理由通常是我们的门禁没覆盖到的（submodule、损坏的 .git）。
    // 那是最后一层保险，绕过它等于亲手拆掉。
    let repo = TempRepo::new();
    repo.write("a.txt", "x");
    let head = repo.commit("init");
    let worktree = repo.worktree("wt", &head);
    let p = removal_plan(&repo, worktree.clone(), head);

    let (git, calls) = RecordingRealGit::new(true);
    let fs = SpyFs::new();
    let env = env_with_runner(git, vec![]);
    let out = apply(
        &p,
        &ApplyOptions {
            dry_run: false,
            audit_log: None,
        },
        &ScanConfig::default(),
        &env,
        &fs,
    );

    assert!(
        matches!(out.results[0].outcome, Outcome::Failed(_)),
        "应如实报告失败"
    );
    assert!(
        calls
            .lock()
            .expect("调用日志锁")
            .iter()
            .any(|c| c.starts_with("worktree remove ")),
        "前提：复检通过后确实尝试了 git worktree remove"
    );
    assert!(worktree.exists(), "git 拒绝后 worktree 必须仍然存在");
    assert!(fs.removals().is_empty(), "绝不能退化成自己 rm -rf");
}

#[test]
fn removal_command_never_carries_force() {
    let repo = TempRepo::new();
    repo.write("a.txt", "x");
    let head = repo.commit("init");
    let worktree = repo.worktree("wt", &head);
    let p = removal_plan(&repo, worktree.clone(), head);

    let (git, calls) = RecordingRealGit::new(false);
    let env = env_with_runner(git, vec![]);
    let out = apply(
        &p,
        &ApplyOptions {
            dry_run: false,
            audit_log: None,
        },
        &ScanConfig::default(),
        &env,
        &SpyFs::new(),
    );

    assert!(
        matches!(out.results[0].outcome, Outcome::Done { .. }),
        "复检应通过并执行删除"
    );
    let calls = calls.lock().expect("调用日志锁").clone();
    let removal = calls
        .iter()
        .find(|c| c.starts_with("worktree remove "))
        .unwrap_or_else(|| panic!("前提：确实发出了删除命令。实际记录: {calls:?}"));
    assert!(
        !removal
            .split_whitespace()
            .any(|arg| arg == "--force" || arg == "-f"),
        "删除命令绝不能带 --force，实际: {removal}"
    );
}

// ───────────────────────── apply 层：路径必须在预期的根之下 ─────────────────────────

#[test]
fn a_cache_path_outside_its_worktree_is_refused() {
    let tmp = tempfile::tempdir().expect("临时目录");
    let wt_dir = tmp.path().join("wt");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&wt_dir).expect("建 wt");
    std::fs::create_dir_all(&outside).expect("建 outside");

    let p = wtgc::plan::Plan {
        actions: vec![Action::ReclaimCache {
            worktree: wt_dir.clone(),
            cache: outside.clone(), // ← 根本不在 worktree 里
            kind: CacheKind {
                name: "target".into(),
                ecosystem: "rust".into(),
            },
            bytes: 100,
            expect: fingerprint(0, vec![]),
        }],
        rejected: Vec::new(),
    };

    let fs = SpyFs::new();
    let env = env_with(RecordingGit::new(), vec![]);
    let out = apply(
        &p,
        &ApplyOptions {
            dry_run: false,
            audit_log: None,
        },
        &ScanConfig::default(),
        &env,
        &fs,
    );

    assert!(
        matches!(out.results[0].outcome, Outcome::Failed(_)),
        "越界路径必须拒绝"
    );
    assert!(fs.removals().is_empty(), "越界路径一次删除都不该发出");
}

// ───────────────────────── 重建线索 ─────────────────────────

#[test]
fn every_removal_carries_a_restore_hint() {
    // 删了才想起来要重建就晚了，所以线索在计划阶段就要算好
    let r = report_with(vec![wt(
        "/repo/wt",
        Verdict::Removable,
        fingerprint(0, vec![]),
    )]);
    let sel = Selection {
        remove: HashSet::from([PathBuf::from("/repo/wt")]),
        ..Default::default()
    };
    let p = plan(&r, &sel);

    let fs = SpyFs::new();
    let env = env_with(RecordingGit::new(), vec![]);
    let out = apply(
        &p,
        &ApplyOptions::default(),
        &ScanConfig::default(),
        &env,
        &fs,
    );

    let hint = out.results[0].restore_hint.as_deref().unwrap_or_default();
    assert!(
        hint.contains("worktree add"),
        "应给出可直接执行的重建命令，实际: {hint}"
    );
    assert!(hint.contains("abc123"), "应带上具体的 commit");
}

// ───────────────────────── 两种动作的复检范围不同 ─────────────────────────

#[test]
fn source_edits_do_not_block_cache_reclamation() {
    // A 组门禁刻意不含 Dirty：未提交的源码改动威胁不到 gitignore 的构建产物。
    // 复检若把这条一视同仁地套在回收上，就会因为无关的源码编辑白白少清几十 GB。
    let tmp = tempfile::tempdir().expect("临时目录");
    let wt_dir = tmp.path().join("wt");
    let cache = wt_dir.join("target");
    std::fs::create_dir_all(&cache).expect("建 target");
    std::fs::write(wt_dir.join("Cargo.toml"), "[package]\nname='x'\n").expect("写 marker");

    let p = wtgc::plan::Plan {
        actions: vec![Action::ReclaimCache {
            worktree: wt_dir.clone(),
            cache: cache.clone(),
            kind: CacheKind {
                name: "target".into(),
                ecosystem: "rust".into(),
            },
            bytes: 100,
            expect: fingerprint(0, vec![]),
        }],
        rejected: Vec::new(),
    };

    // status 现在报 2 处改动，而指纹记的是 0
    let mut git = RecordingGit::new();
    git.stdout = b" M src/a.rs\0 M src/b.rs\0".to_vec();
    let fs = SpyFs::new();
    let env = env_with(git, vec![]);
    let cfg = ScanConfig {
        cache_quiet: std::time::Duration::ZERO,
        ..ScanConfig::default()
    };
    let out = apply(
        &p,
        &ApplyOptions {
            dry_run: false,
            audit_log: None,
        },
        &cfg,
        &env,
        &fs,
    );

    assert!(
        matches!(out.results[0].outcome, Outcome::Done { .. }),
        "源码改动不该挡住缓存回收，实际 {:?}",
        out.results[0].outcome
    );
    assert_eq!(fs.removals(), vec![cache], "该回收的缓存必须真的被回收");
}

#[test]
fn a_busy_worktree_still_blocks_cache_reclamation() {
    // 放宽 dirty 之后，busy 就是回收路径上唯一承重的复检——它必须仍然有效
    let tmp = tempfile::tempdir().expect("临时目录");
    let wt_dir = tmp.path().join("wt");
    let cache = wt_dir.join("target");
    std::fs::create_dir_all(&cache).expect("建 target");

    let p = wtgc::plan::Plan {
        actions: vec![Action::ReclaimCache {
            worktree: wt_dir,
            cache,
            kind: CacheKind {
                name: "target".into(),
                ecosystem: "rust".into(),
            },
            bytes: 100,
            expect: fingerprint(0, vec![]),
        }],
        rejected: Vec::new(),
    };

    let fs = SpyFs::new();
    let env = env_with(
        RecordingGit::new(),
        vec![ProcInfo {
            pid: 42,
            name: "cargo".into(),
        }],
    );
    let out = apply(
        &p,
        &ApplyOptions {
            dry_run: false,
            audit_log: None,
        },
        &ScanConfig::default(),
        &env,
        &fs,
    );

    assert!(
        matches!(out.results[0].outcome, Outcome::Stale { .. }),
        "有进程在用时绝不能回收（会中断构建），实际 {:?}",
        out.results[0].outcome
    );
    assert!(fs.removals().is_empty(), "一次删除都不该发出");
}

#[test]
fn main_worktree_cache_is_not_selected_by_default() {
    // 主工作区是人天天在用的那个，缓存命中率最高、重建最贵。
    // 它可以被回收，但不该在 --yes 的默认语义里被顺手带走。
    let cache = CacheDir {
        path: PathBuf::from("/repo/target"),
        kind: CacheKind {
            name: "target".into(),
            ecosystem: "rust".into(),
        },
        bytes: 22_000_000_000,
        outcomes: vec![GateOutcome {
            id: GateId::CacheSafe,
            status: GateStatus::Pass,
        }],
    };
    let mut main_wt = wt(
        "/repo",
        Verdict::Protected {
            why: "主工作区"
        },
        fingerprint(0, vec![]),
    );
    main_wt.is_main = true;
    main_wt.caches = vec![cache.clone()];

    let mut agent_wt = wt("/repo/wt", Verdict::Removable, fingerprint(0, vec![]));
    agent_wt.caches = vec![CacheDir {
        path: PathBuf::from("/repo/wt/target"),
        ..cache
    }];

    let r = report_with(vec![main_wt, agent_wt]);

    let default_sel = Selection::everything_allowed(&r, false);
    assert!(
        !default_sel.reclaim.contains(&PathBuf::from("/repo/target")),
        "默认不该选中主工作区的缓存"
    );
    assert!(
        default_sel
            .reclaim
            .contains(&PathBuf::from("/repo/wt/target")),
        "agent worktree 的缓存照选"
    );

    let opted_in = Selection::everything_allowed(&r, true);
    assert!(
        opted_in.reclaim.contains(&PathBuf::from("/repo/target")),
        "显式要求时才带上主工作区"
    );
}
