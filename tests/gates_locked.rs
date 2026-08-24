#![allow(clippy::expect_used)]
//! B6 Locked 门禁的回归测试。
//!
//! `git worktree lock` 是用户显式说过「别动这个」的地方。这里的每个用例都在问
//! 同一件事：**在这种情况下，工具会不会自作主张放行？**

use std::path::{Path, PathBuf};
use std::time::SystemTime;
use wtgc::config::ScanConfig;
use wtgc::gates::locked::LockedGate;
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

/// 总是失败的 git —— 用来断言 git 挂掉时**没有**任何 worktree 被判可删。
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

/// 造一个有一次提交的仓，省掉每个用例的重复样板。
fn repo_with_commit() -> TempRepo {
    let r = TempRepo::new();
    r.write("a.txt", "x");
    r.commit("init");
    r
}

/// 跑一次 B6，git 用真的。
fn evaluate_locked(r: &TempRepo, wt: &Path) -> GateStatus {
    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(r, wt, &head, &cfg, &git, &procs, &clock, &forge);
    LockedGate.evaluate(&c)
}

/// 没锁的普通 worktree：B6 不该拦。
#[test]
fn unlocked_worktree_passes() {
    let r = repo_with_commit();
    let wt = r.worktree("wt", &r.head());

    assert_eq!(
        evaluate_locked(&r, &wt),
        GateStatus::Pass,
        "未加锁的 worktree 应通过 B6"
    );
}

/// `git worktree lock`（不带理由）：必须拦下，reason 报 None 而不是空串。
#[test]
fn locked_worktree_is_blocked() {
    let r = repo_with_commit();
    let wt = r.worktree("wt", &r.head());
    r.git(&["worktree", "lock", wt.to_str().expect("worktree 路径")]);

    match evaluate_locked(&r, &wt) {
        GateStatus::Blocked(GateDetail::WorktreeLocked { reason }) => {
            assert_eq!(reason, None, "未给理由的锁定应报 None，不该是空串");
        }
        other => panic!("被 git worktree lock 锁定的 worktree 必须拦下，实际 {other:?}"),
    }
}

/// `git worktree lock --reason "外置盘"`：拦下，且理由要原样带出给人看。
///
/// 非 ASCII 理由在 porcelain 里会被 git C 风格转义成 `"\345\244\226..."`，
/// 直接透传就是一串乱码，这里锚定的正是解码结果。
#[test]
fn lock_reason_is_reported_verbatim() {
    let r = repo_with_commit();
    let wt = r.worktree("wt", &r.head());
    r.git(&[
        "worktree",
        "lock",
        "--reason",
        "外置盘",
        wt.to_str().expect("worktree 路径"),
    ]);

    match evaluate_locked(&r, &wt) {
        GateStatus::Blocked(GateDetail::WorktreeLocked { reason }) => {
            assert_eq!(reason.as_deref(), Some("外置盘"), "锁定理由应原样带出");
        }
        other => panic!("带理由锁定的 worktree 必须拦下，实际 {other:?}"),
    }
}

/// 纯 ASCII 理由 git 不加引号，走的是另一条分支，别把它当转义串解坏了。
#[test]
fn ascii_lock_reason_is_not_mangled() {
    let r = repo_with_commit();
    let wt = r.worktree("wt", &r.head());
    r.git(&[
        "worktree",
        "lock",
        "--reason",
        "on external drive",
        wt.to_str().expect("worktree 路径"),
    ]);

    match evaluate_locked(&r, &wt) {
        GateStatus::Blocked(GateDetail::WorktreeLocked { reason }) => {
            assert_eq!(
                reason.as_deref(),
                Some("on external drive"),
                "未被转义的理由应原样带出"
            );
        }
        other => panic!("带理由锁定的 worktree 必须拦下，实际 {other:?}"),
    }
}

/// 锁的是**别人**：不能因为注册表里存在 locked 记录就连坐拦下自己。
///
/// 反过来说，这也是在验证匹配确实按路径走，而不是「看到 locked 就拦」。
#[test]
fn lock_on_another_worktree_does_not_leak() {
    let r = repo_with_commit();
    let mine = r.worktree("mine", &r.head());
    let theirs = r.worktree("theirs", &r.head());
    r.git(&["worktree", "lock", theirs.to_str().expect("worktree 路径")]);

    assert_eq!(
        evaluate_locked(&r, &mine),
        GateStatus::Pass,
        "别的 worktree 上的锁不该影响这一个"
    );
    assert!(
        matches!(
            evaluate_locked(&r, &theirs),
            GateStatus::Blocked(GateDetail::WorktreeLocked { .. })
        ),
        "被锁的那个仍应被拦下"
    );
}

/// 传入一个真实存在、但没在 git 注册过的目录：状态不一致，判不准。
///
/// **这里返回 Pass 就是最危险的形状** —— 一个我们根本不了解的目录被判成「可删」。
#[test]
fn unregistered_path_is_unknown_not_pass() {
    let r = repo_with_commit();
    let stray = r.root.join("not-a-worktree");
    std::fs::create_dir_all(&stray).expect("建目录");
    let stray = stray.canonicalize().expect("canonicalize");

    let st = evaluate_locked(&r, &stray);
    assert!(
        matches!(st, GateStatus::Unknown(Cause::Io { .. })),
        "未注册的路径应判 Unknown（绝不能是 Pass），实际 {st:?}"
    );
}

/// 路径压根不存在：canonicalize 失败也走 Unknown，不静默当没锁。
#[test]
fn missing_path_is_unknown_not_pass() {
    let r = repo_with_commit();
    let gone = r.root.join("vanished");

    let st = evaluate_locked(&r, &gone);
    assert!(
        matches!(st, GateStatus::Unknown(Cause::Io { .. })),
        "不存在的路径应判 Unknown，实际 {st:?}"
    );
}

/// git 子命令失败：拿不到注册表就是判不准，绝不 fail-open（D7）。
#[test]
fn git_failure_is_unknown_not_pass() {
    let r = repo_with_commit();
    let wt = r.worktree("wt", &r.head());

    let cfg = ScanConfig::default();
    let git = BrokenGit;
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    let st = LockedGate.evaluate(&c);
    assert!(
        matches!(st, GateStatus::Unknown(Cause::CommandFailed { .. })),
        "git worktree list 失败时必须判 Unknown 而非 Pass，实际 {st:?}"
    );
}

/// 主工作区（仓库根）本身没被锁，也要能在注册表里认出自己。
#[test]
fn main_worktree_is_found_and_passes() {
    let r = repo_with_commit();
    let root: PathBuf = r.root.clone();

    assert_eq!(
        evaluate_locked(&r, &root),
        GateStatus::Pass,
        "主工作区应被认出且未加锁"
    );
}
