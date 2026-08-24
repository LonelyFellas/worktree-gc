#![allow(clippy::expect_used)]
//! B5 门禁的回归测试 —— 对应 docs/design.md 的 D9。
//!
//! 全部用真 git 造中间态。这道门的判据就是「git 把标记落在哪个目录、叫什么名字」，
//! mock 掉 git 等于把判据抄一遍再自证，测不出任何东西。

use std::path::Path;
use std::process::Command;
use std::time::SystemTime;
use wtgc::config::ScanConfig;
use wtgc::gates::inprogress::InProgressGate;
use wtgc::gates::recent::FixedClock;
use wtgc::gates::{Clock, Gate, GateCtx, MergeStatusProvider, ProcInfo, ProcessTable};
use wtgc::git::{GitExec, GitRunner};
use wtgc::model::{Cause, GateDetail, GateStatus};
use wtgc::testkit::{TempRepo, test_git};

/// 测试替身：想让它报告什么就报告什么。
struct FakeProcs(Result<Vec<ProcInfo>, Cause>);
impl ProcessTable for FakeProcs {
    fn processes_under(&self, _dir: &Path) -> Result<Vec<ProcInfo>, Cause> {
        self.0.clone()
    }
}
struct NoForge;
impl MergeStatusProvider for NoForge {
    fn merged_pr(&self, _repo: &Path, _branch: &str, _oid: &str) -> Result<Option<u64>, Cause> {
        Ok(None)
    }
}

/// 恒失败的 git：用来断言 git 全线不可用时**没有任何** worktree 被判可删（D7）。
struct BrokenGit;
impl GitRunner for BrokenGit {
    fn exec(&self, _cwd: &Path, _args: &[&str]) -> Result<GitExec, Cause> {
        Ok(GitExec {
            code: Some(128),
            stdout: Vec::new(),
            stderr: "fatal: not a git repository".into(),
        })
    }
}

#[allow(clippy::too_many_arguments)] // 测试辅助函数，参数即依赖注入
fn ctx<'a>(
    repo: &'a TempRepo,
    wt: &'a Path,
    head: &'a str,
    cfg: &'a ScanConfig,
    git: &'a dyn GitRunner,
    procs: &'a dyn ProcessTable,
    clock: &'a dyn Clock,
    forge: &'a dyn MergeStatusProvider,
) -> GateCtx<'a> {
    GateCtx {
        repo: &repo.root,
        worktree: wt,
        branch: None,
        head_oid: head,
        baseline: None,
        cfg,
        git,
        procs,
        clock,
        forge,
    }
}

/// 造中间态必须让 git 以非零退出（冲突就是要它失败），而 `TempRepo::git` 会
/// assert 成功，所以这里直接用 Command，并反过来断言「它确实失败了」——
/// 否则冲突没造出来，后面的断言就成了空跑。
fn git_expect_conflict(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("跑 git");
    assert!(
        !out.status.success(),
        "夹具期望 git {args:?} 因冲突而失败，实际却成功了 —— 中间态没造出来，测试无效"
    );
}

/// 建一个 main / topic 对同一文件做不同修改的仓，必冲突。
fn repo_with_conflicting_branches() -> TempRepo {
    let r = TempRepo::new();
    r.write("f.txt", "base\n");
    r.commit("init");

    r.git(&["checkout", "-q", "-b", "topic"]);
    r.write("f.txt", "topic\n");
    r.commit("topic 侧改动");

    r.git(&["checkout", "-q", "main"]);
    r.write("f.txt", "main\n");
    r.commit("main 侧改动");

    r
}

/// 取该 worktree 自己的 git 目录（linked worktree 是 `<main>/.git/worktrees/<name>`）。
fn git_dir_of(r: &TempRepo, wt: &Path) -> std::path::PathBuf {
    std::path::PathBuf::from(r.git_in(wt, &["rev-parse", "--absolute-git-dir"]).trim())
}

/// 没在中间态的普通 worktree → 放行。
#[test]
fn clean_worktree_passes() {
    let r = TempRepo::new();
    r.write("a.txt", "x");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_eq!(
        InProgressGate.evaluate(&c),
        GateStatus::Pass,
        "干净的 worktree 没有任何中间态标记，应放行"
    );
}

/// D9 的正主：rebase 冲突中断在 detached HEAD 上，工作树看着干净，但 sequencer
/// 状态与该 worktree 独有的 HEAD reflog 一删就没。
#[test]
fn interrupted_rebase_is_blocked() {
    let r = repo_with_conflicting_branches();
    let wt = r.worktree("wt", "topic");

    git_expect_conflict(&wt, &["rebase", "main"]);

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    match InProgressGate.evaluate(&c) {
        GateStatus::Blocked(GateDetail::OperationInProgress { kind }) => {
            assert_eq!(kind, "rebase", "中断的 rebase 应报成 rebase");
        }
        other => panic!("rebase 中途的 worktree 必须被拦下（D9），实际 {other:?}"),
    }
}

/// merge 冲突未解决：`MERGE_HEAD` 还在，删掉就丢了「正在合并谁」这个信息。
#[test]
fn conflicted_merge_is_blocked() {
    let r = repo_with_conflicting_branches();
    let wt = r.worktree("wt", "main");

    git_expect_conflict(&wt, &["merge", "topic"]);

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    match InProgressGate.evaluate(&c) {
        GateStatus::Blocked(GateDetail::OperationInProgress { kind }) => {
            assert_eq!(kind, "merge", "未解决的 merge 应报成 merge");
        }
        other => panic!("merge 冲突中的 worktree 必须被拦下，实际 {other:?}"),
    }
}

/// cherry-pick 中断：`CHERRY_PICK_HEAD` 还在。
#[test]
fn interrupted_cherry_pick_is_blocked() {
    let r = repo_with_conflicting_branches();
    let wt = r.worktree("wt", "main");

    git_expect_conflict(&wt, &["cherry-pick", "topic"]);

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    match InProgressGate.evaluate(&c) {
        GateStatus::Blocked(GateDetail::OperationInProgress { kind }) => {
            assert_eq!(kind, "cherry-pick", "中断的 cherry-pick 应报成 cherry-pick");
        }
        other => panic!("cherry-pick 中途的 worktree 必须被拦下，实际 {other:?}"),
    }
}

/// revert / bisect / 多提交 sequencer 三种中间态。
///
/// 这里直接在 worktree 自己的 git 目录里放标记 —— 位置和名字与 git 落盘的一模一样，
/// 但不必为了造 bisect 状态而编一段二分历史。
#[test]
fn revert_bisect_and_sequencer_markers_are_blocked() {
    for (marker, is_dir, expect_kind) in [
        ("REVERT_HEAD", false, "revert"),
        ("BISECT_LOG", false, "bisect"),
        ("sequencer", true, "sequencer"),
    ] {
        let r = TempRepo::new();
        r.write("a.txt", "x");
        r.commit("init");
        let wt = r.worktree("wt", &r.head());
        let gd = git_dir_of(&r, &wt);

        if is_dir {
            std::fs::create_dir_all(gd.join(marker)).expect("建标记目录");
        } else {
            std::fs::write(gd.join(marker), "").expect("写标记文件");
        }

        let cfg = ScanConfig::default();
        let git = test_git();
        let procs = FakeProcs(Ok(vec![]));
        let clock = FixedClock(SystemTime::now());
        let forge = NoForge;
        let head = r.head();
        let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

        match InProgressGate.evaluate(&c) {
            GateStatus::Blocked(GateDetail::OperationInProgress { kind }) => {
                assert_eq!(kind, expect_kind, "标记 {marker} 应报成 {expect_kind}");
            }
            other => panic!("存在 {marker} 标记时必须拦下，实际 {other:?}"),
        }
    }
}

/// linked worktree 的中间态不在 `<wt>/.git`（那是个文件）里。
///
/// 主工作区正在合并时，挂在同一仓上的 linked worktree 并没有中间态，不该被牵连；
/// 反过来主工作区自己必须被拦。判错方向说明 git 目录解析错了。
#[test]
fn state_is_resolved_per_worktree_not_from_dot_git_path() {
    let r = repo_with_conflicting_branches();
    let wt = r.worktree("wt", "main");

    git_expect_conflict(&r.root, &["merge", "topic"]);

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();

    let c_wt = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);
    assert_eq!(
        InProgressGate.evaluate(&c_wt),
        GateStatus::Pass,
        "主工作区在合并中不代表 linked worktree 也在合并中，应放行"
    );

    let root = r.root.clone();
    let c_main = ctx(&r, &root, &head, &cfg, &git, &procs, &clock, &forge);
    assert!(
        matches!(
            InProgressGate.evaluate(&c_main),
            GateStatus::Blocked(GateDetail::OperationInProgress { kind: "merge" })
        ),
        "正在合并的主工作区必须被拦下"
    );
}

/// D7：git 问不出 git 目录时，只能说判不准，绝不能当成「没有中间态」放行。
#[test]
fn broken_git_yields_unknown_not_pass() {
    let r = TempRepo::new();
    r.write("a.txt", "x");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());

    let cfg = ScanConfig::default();
    let git = BrokenGit;
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    match InProgressGate.evaluate(&c) {
        GateStatus::Unknown(Cause::CommandFailed { .. }) => {}
        other => panic!("git 失败时必须落 Unknown，绝不能放行，实际 {other:?}"),
    }
}

/// worktree 目录已经消失（注册记录陈旧）时同样判不准，而不是「干净」。
#[test]
fn missing_worktree_yields_unknown_not_pass() {
    let r = TempRepo::new();
    r.write("a.txt", "x");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::remove_dir_all(&wt).expect("删掉 worktree 目录");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert!(
        InProgressGate.evaluate(&c).is_unknown(),
        "worktree 目录不存在时无从判断，必须落 Unknown"
    );
}
