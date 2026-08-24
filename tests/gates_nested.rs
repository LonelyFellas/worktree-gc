#![allow(clippy::expect_used)]
//! B4 Nested 门禁的回归测试。
//!
//! 对应 docs/design.md 的 D5：Claude Code 把 worktree 建在 `<repo>/.claude/worktrees/`，
//! 而 `.claude/` 常被 gitignore —— 外层看着干干净净，删下去连内层的未提交改动一起没。
//! 这里全部用真 git 造仓：嵌套 worktree 的 `.git` 是文件不是目录，mock 编不出这种细节。

use std::path::Path;
use std::time::SystemTime;
use wtgc::config::ScanConfig;
use wtgc::gates::nested::NestedGate;
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

/// git 全线不可用。用来断言门禁落 `Unknown` 而不是 `Pass`（D7 的 fail-open）。
struct FailingGit;
impl GitRunner for FailingGit {
    fn exec(&self, cwd: &Path, _args: &[&str]) -> Result<GitExec, Cause> {
        Err(Cause::Io {
            path: cwd.to_path_buf(),
            msg: "模拟 git 不可用".into(),
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

/// 造一个带一次提交的仓，并从中开出名为 `wt` 的 worktree。
fn repo_with_worktree() -> (TempRepo, std::path::PathBuf) {
    let r = TempRepo::new();
    r.write("README.md", "hello");
    r.write("Cargo.toml", "[package]\nname='outer'\n");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    (r, wt)
}

/// 普通 worktree：只有自己的 `.git`，注册表里也没有别人在它下面 → 放行。
#[test]
fn plain_worktree_passes() {
    let (r, wt) = repo_with_worktree();

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_eq!(
        NestedGate.evaluate(&c),
        GateStatus::Pass,
        "worktree 根自己的 .git 不是嵌套，不该被拦"
    );
}

/// 内部只有普通目录（哪怕深过扫描上限）→ 放行。没有 `.git` 就没有别人的工作区。
#[test]
fn ordinary_subdirectories_pass() {
    let (r, wt) = repo_with_worktree();
    std::fs::create_dir_all(wt.join("src/a/b/c/d/e/f/g")).expect("建深层普通目录");
    std::fs::write(wt.join("src/a/b/lib.rs"), "fn main() {}").expect("写源码");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_eq!(
        NestedGate.evaluate(&c),
        GateStatus::Pass,
        "普通子目录不含 .git，不该被当成嵌套工作区"
    );
}

/// **D5 本体**：`.claude/worktrees/` 下注册了一个内层 worktree。
///
/// 内层的 `.git` 是**文件**不是目录 —— 只判目录的实现会在这里静默放行，
/// 然后把别人未提交的改动一起删掉。
#[test]
fn registered_nested_worktree_blocks() {
    let (r, wt) = repo_with_worktree();

    let inner = wt.join(".claude/worktrees/agent-1");
    let head = r.head();
    r.git_in(
        &wt,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            inner.to_str().expect("路径"),
            &head,
        ],
    );
    let inner = inner.canonicalize().expect("canonicalize 内层 worktree");

    assert!(
        inner.join(".git").is_file(),
        "前提校验：linked worktree 的 .git 应当是文件"
    );

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    match NestedGate.evaluate(&c) {
        GateStatus::Blocked(GateDetail::NestedWorktrees { paths }) => {
            assert_eq!(
                paths,
                vec![inner.clone()],
                "应当且只应当报出内层 worktree 的根路径"
            );
        }
        other => panic!("内嵌注册过的 worktree 必须拦下，实际 {other:?}"),
    }
}

/// 内部是一个 `git init` 出来的独立仓：注册表里查不到，只有文件系统扫描能发现。
#[test]
fn independent_nested_repo_blocks() {
    let (r, wt) = repo_with_worktree();

    let inner = wt.join("vendor/some-lib");
    std::fs::create_dir_all(&inner).expect("建内层仓目录");
    r.git_in(&inner, &["init", "--template=", "-q", "-b", "main", "."]);
    std::fs::write(inner.join("notes.txt"), "别人的未提交改动").expect("写文件");
    let inner = inner.canonicalize().expect("canonicalize 内层仓");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    match NestedGate.evaluate(&c) {
        GateStatus::Blocked(GateDetail::NestedWorktrees { paths }) => {
            assert_eq!(paths, vec![inner.clone()], "应报出独立内层仓的根路径");
        }
        other => panic!("内嵌的独立 git 仓必须拦下（它不在注册表里），实际 {other:?}"),
    }
}

/// 根仓是 Rust 项目，不代表任意深层同名 `target/` 都是它的构建缓存。
#[test]
fn independent_repo_in_unmarked_nested_target_blocks() {
    let (r, wt) = repo_with_worktree();

    let inner = wt.join("data/target");
    std::fs::create_dir_all(&inner).expect("建同名资料目录");
    r.git_in(&inner, &["init", "--template=", "-q", "-b", "main", "."]);
    std::fs::write(inner.join("notes.txt"), "独立仓内容").expect("写文件");
    let inner = inner.canonicalize().expect("canonicalize 内层仓");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    match NestedGate.evaluate(&c) {
        GateStatus::Blocked(GateDetail::NestedWorktrees { paths }) => {
            assert_eq!(
                paths,
                vec![inner],
                "缺少同级 Cargo.toml 时不得跳过深层 target"
            );
        }
        other => panic!("同名目录里的独立 git 仓必须拦下，实际 {other:?}"),
    }
}

/// 内层 worktree 藏在被跳过的 `target/` 里：文件系统扫描故意不进去，
/// 必须由注册表这条判据兜住，否则就是一个可被构造出来的 fail-open。
#[test]
fn nested_worktree_inside_skipped_dir_still_blocks() {
    let (r, wt) = repo_with_worktree();

    let inner = wt.join("target/scratch/agent-2");
    let head = r.head();
    r.git_in(
        &wt,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            inner.to_str().expect("路径"),
            &head,
        ],
    );
    let inner = inner.canonicalize().expect("canonicalize 内层 worktree");

    let cfg = ScanConfig::default();
    assert!(
        cfg.precious.disposable_dirs.iter().any(|d| d == "target"),
        "前提校验：target 应在跳过表里，否则这个用例测不到想测的路径"
    );

    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    match NestedGate.evaluate(&c) {
        GateStatus::Blocked(GateDetail::NestedWorktrees { paths }) => {
            assert_eq!(
                paths,
                vec![inner.clone()],
                "跳过表内的注册 worktree 应由判据一报出"
            );
        }
        other => panic!("跳过表挡不住注册表，内层 worktree 仍须拦下，实际 {other:?}"),
    }
}

/// 兄弟 worktree 名字有共同前缀（`wt` 与 `wt-2`）：按字符串前缀比会误判成嵌套。
#[test]
fn sibling_worktree_with_shared_prefix_passes() {
    let (r, wt) = repo_with_worktree();
    let _sibling = r.worktree("wt-2", &r.head());

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_eq!(
        NestedGate.evaluate(&c),
        GateStatus::Pass,
        "wt-2 是 wt 的兄弟而非子目录，路径比较必须按分量而不是按字符串前缀"
    );
}

/// D7：git 拿不到结果时**绝不能**当成「没有嵌套」放行。
#[test]
fn git_failure_is_unknown_not_pass() {
    let (r, wt) = repo_with_worktree();

    let cfg = ScanConfig::default();
    let git = FailingGit;
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert!(
        matches!(
            NestedGate.evaluate(&c),
            GateStatus::Unknown(Cause::Io { .. })
        ),
        "git worktree list 失败时必须落 Unknown，绝不能 fail-open 成 Pass"
    );
}

/// worktree 目录本身已经不存在：canonicalize 失败 → `Unknown`，不是 `Pass`。
#[test]
fn missing_worktree_dir_is_unknown() {
    let (r, wt) = repo_with_worktree();
    std::fs::remove_dir_all(&wt).expect("删掉 worktree 目录");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert!(
        matches!(
            NestedGate.evaluate(&c),
            GateStatus::Unknown(Cause::Io { .. })
        ),
        "worktree 路径解析不了就是判不准，必须落 Unknown"
    );
}
