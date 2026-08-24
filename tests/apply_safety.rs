#![allow(clippy::expect_used)]
//! 破坏性路径的安全护栏。
//!
//! **这个文件里几乎全是否定性断言**——证明某件事没有发生。
//! 一个删文件的工具，"该删的删了"只是及格线，"不该删的一个都没碰"才是它的价值。

use std::collections::HashSet;
use std::path::PathBuf;
use wtgc::apply::{ApplyOptions, Outcome, apply};
use wtgc::gates::{ProcInfo, SystemClock};
use wtgc::model::*;
use wtgc::plan::{Action, Selection, plan};
use wtgc::scan::Env;
use wtgc::testkit::{FakeProcs, RecordingGit, SpyFs};

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
    Env {
        git: Box::new(git),
        forge: Box::new(wtgc::forge::Offline),
        clock: Box::new(SystemClock),
        procs: Box::new(FakeProcs(procs)),
    }
}

// ───────────────────────── plan 层：勾选不是授权 ─────────────────────────

#[test]
fn selecting_a_blocked_worktree_does_not_plan_it() {
    let r = report_with(vec![wt(
        "/repo/wt",
        Verdict::Blocked { by: vec![GateId::Dirty] },
        fingerprint(3, vec![]),
    )]);
    let sel = Selection { remove: HashSet::from([PathBuf::from("/repo/wt")]), ..Default::default() };

    let p = plan(&r, &sel);
    assert!(p.is_empty(), "被拦下的 worktree 即使勾选也不该进入计划");
    assert_eq!(p.rejected.len(), 1, "落选必须上报，默默少做比不做更糟");
}

#[test]
fn selecting_a_needs_attention_worktree_does_not_plan_it() {
    // 判不准比判定拒绝更危险——它意味着我们不知道那里有什么
    let r = report_with(vec![wt(
        "/repo/wt",
        Verdict::NeedsAttention { unknown: vec![GateId::Busy] },
        fingerprint(0, vec![]),
    )]);
    let sel = Selection { remove: HashSet::from([PathBuf::from("/repo/wt")]), ..Default::default() };

    assert!(plan(&r, &sel).is_empty(), "判不准的绝不放行");
}

#[test]
fn everything_allowed_never_picks_up_blocked_items() {
    let r = report_with(vec![
        wt("/repo/ok", Verdict::Removable, fingerprint(0, vec![])),
        wt("/repo/bad", Verdict::Blocked { by: vec![GateId::Precious] }, fingerprint(0, vec![])),
    ]);
    let sel = Selection::everything_allowed(&r);
    assert!(sel.remove.contains(&PathBuf::from("/repo/ok")));
    assert!(!sel.remove.contains(&PathBuf::from("/repo/bad")), "--yes 也不该碰被拦下的");
}

// ───────────────────────── apply 层：dry-run 必须真的什么都不做 ─────────────────────────

#[test]
fn dry_run_emits_no_destructive_command_at_all() {
    let r = report_with(vec![wt("/repo/wt", Verdict::Removable, fingerprint(0, vec![]))]);
    let sel = Selection { remove: HashSet::from([PathBuf::from("/repo/wt")]), ..Default::default() };
    let p = plan(&r, &sel);
    assert!(!p.is_empty(), "前提：计划里确实有东西，否则本测试是假绿的");

    let git = RecordingGit::new();
    let fs = SpyFs::new();
    let env = env_with(git, vec![]);
    let out = apply(&p, &ApplyOptions { dry_run: true, audit_log: None }, &env, &fs);

    assert!(fs.removals().is_empty(), "dry-run 不该发出任何删除");
    assert!(matches!(out.results[0].outcome, Outcome::Simulated));
}

// ───────────────────────── apply 层：TOCTOU 复检 ─────────────────────────

#[test]
fn a_worktree_that_became_busy_is_skipped_not_deleted() {
    // 扫描时空闲，执行时 agent 又开工了——这是 GUI 场景下几分钟窗口的真实情形
    let r = report_with(vec![wt("/repo/wt", Verdict::Removable, fingerprint(0, vec![]))]);
    let sel = Selection { remove: HashSet::from([PathBuf::from("/repo/wt")]), ..Default::default() };
    let p = plan(&r, &sel);

    let git = RecordingGit::new();
    let fs = SpyFs::new();
    let env = env_with(git, vec![ProcInfo { pid: 999, name: "cargo".into() }]);
    let out = apply(&p, &ApplyOptions { dry_run: false, audit_log: None }, &env, &fs);

    assert!(
        matches!(out.results[0].outcome, Outcome::Stale { .. }),
        "状态变了必须跳过，实际 {:?}",
        out.results[0].outcome
    );
    assert_eq!(out.stale_count(), 1);
}

#[test]
fn new_uncommitted_changes_since_scan_abort_the_removal() {
    let r = report_with(vec![wt("/repo/wt", Verdict::Removable, fingerprint(0, vec![]))]);
    let sel = Selection { remove: HashSet::from([PathBuf::from("/repo/wt")]), ..Default::default() };
    let p = plan(&r, &sel);

    // status 现在吐出两条改动，而指纹记的是 0
    let mut git = RecordingGit::new();
    git.stdout = b" M a.txt\0 M b.txt\0".to_vec();
    let fs = SpyFs::new();
    let env = env_with(git, vec![]);
    let out = apply(&p, &ApplyOptions { dry_run: false, audit_log: None }, &env, &fs);

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
    let r = report_with(vec![wt("/repo/wt", Verdict::Removable, fingerprint(0, vec![]))]);
    let sel = Selection { remove: HashSet::from([PathBuf::from("/repo/wt")]), ..Default::default() };
    let p = plan(&r, &sel);

    let git = RecordingGit::failing("worktree remove", 128);
    let fs = SpyFs::new();
    let env = env_with(git, vec![]);
    let out = apply(&p, &ApplyOptions { dry_run: false, audit_log: None }, &env, &fs);

    assert!(matches!(out.results[0].outcome, Outcome::Failed(_)), "应如实报告失败");
    assert!(fs.removals().is_empty(), "绝不能退化成自己 rm -rf");
}

#[test]
fn removal_command_never_carries_force() {
    let r = report_with(vec![wt("/repo/wt", Verdict::Removable, fingerprint(0, vec![]))]);
    let sel = Selection { remove: HashSet::from([PathBuf::from("/repo/wt")]), ..Default::default() };
    let p = plan(&r, &sel);

    let git = RecordingGit::new();
    let log = git.log(); // 先把日志句柄留在手里，Box 进 Env 之后就取不回来了
    let env = env_with(git, vec![]);
    let _ = apply(&p, &ApplyOptions { dry_run: false, audit_log: None }, &env, &SpyFs::new());

    let calls: Vec<String> = match log.lock() {
        Ok(g) => g.iter().map(|c| c.join(" ")).collect(),
        Err(e) => e.into_inner().iter().map(|c| c.join(" ")).collect(),
    };
    assert!(
        calls.iter().any(|c| c.contains("worktree remove")),
        "前提：确实发出了删除命令，否则本测试是假绿的。实际记录: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.contains("--force") || c.contains("-f")),
        "删除命令绝不能带 --force，实际记录: {calls:?}"
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
            kind: CacheKind { name: "target".into(), ecosystem: "rust".into() },
            bytes: 100,
            expect: fingerprint(0, vec![]),
        }],
        rejected: Vec::new(),
    };

    let fs = SpyFs::new();
    let env = env_with(RecordingGit::new(), vec![]);
    let out = apply(&p, &ApplyOptions { dry_run: false, audit_log: None }, &env, &fs);

    assert!(matches!(out.results[0].outcome, Outcome::Failed(_)), "越界路径必须拒绝");
    assert!(fs.removals().is_empty(), "越界路径一次删除都不该发出");
}

// ───────────────────────── 重建线索 ─────────────────────────

#[test]
fn every_removal_carries_a_restore_hint() {
    // 删了才想起来要重建就晚了，所以线索在计划阶段就要算好
    let r = report_with(vec![wt("/repo/wt", Verdict::Removable, fingerprint(0, vec![]))]);
    let sel = Selection { remove: HashSet::from([PathBuf::from("/repo/wt")]), ..Default::default() };
    let p = plan(&r, &sel);

    let fs = SpyFs::new();
    let env = env_with(RecordingGit::new(), vec![]);
    let out = apply(&p, &ApplyOptions::default(), &env, &fs);

    let hint = out.results[0].restore_hint.as_deref().unwrap_or_default();
    assert!(hint.contains("worktree add"), "应给出可直接执行的重建命令，实际: {hint}");
    assert!(hint.contains("abc123"), "应带上具体的 commit");
}
