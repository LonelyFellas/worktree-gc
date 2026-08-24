#![allow(clippy::expect_used)]
//! B1 Dirty 门的回归测试。
//!
//! 这道门有两条能被彻底击穿的路径（D3 / D4），且**两条同时也会击穿
//! `git worktree remove` 自己的保护**——它拒绝删除的依据同样是 status。
//! 所以这里的每个用例都不是覆盖率，是护栏：漏一条就是永久丢数据。

use std::time::SystemTime;
use wtgc::config::ScanConfig;
use wtgc::gates::dirty::DirtyGate;
use wtgc::gates::recent::FixedClock;
use wtgc::gates::{Clock, Gate, GateCtx, MergeStatusProvider, ProcInfo, ProcessTable};
use wtgc::git::{GitExec, GitRunner};
use wtgc::model::{Cause, GateDetail, GateStatus};
use wtgc::testkit::{TempRepo, test_git};

/// 测试替身：想让它报告什么就报告什么。
struct FakeProcs(Result<Vec<ProcInfo>, Cause>);
impl ProcessTable for FakeProcs {
    fn processes_under(&self, _dir: &std::path::Path) -> Result<Vec<ProcInfo>, Cause> {
        self.0.clone()
    }
}
struct NoForge;
impl MergeStatusProvider for NoForge {
    fn merged_pr(
        &self,
        _repo: &std::path::Path,
        _branch: &str,
        _oid: &str,
    ) -> Result<Option<u64>, Cause> {
        Ok(None)
    }
}

/// D7 的原样复刻：`.git` 损坏时 `git status --porcelain` 退出码 128、stdout 为空。
/// 原型脚本靠「输出是否为空」判干净，于是把损坏的仓当成了干净的仓。
struct BrokenGit;
impl GitRunner for BrokenGit {
    fn exec(&self, _cwd: &std::path::Path, _args: &[&str]) -> Result<GitExec, Cause> {
        Ok(GitExec {
            code: Some(128),
            stdout: Vec::new(),
            stderr: "fatal: not a git repository".into(),
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn ctx<'a>(
    repo: &'a TempRepo,
    wt: &'a std::path::Path,
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

/// 从 Blocked 里取出改动列表；不是 Blocked 就带着期望说明炸掉。
fn expect_blocked(st: GateStatus, why: &str) -> (usize, Vec<String>) {
    match st {
        GateStatus::Blocked(GateDetail::UncommittedChanges { count, sample }) => (count, sample),
        other => panic!("{why}；实际拿到 {other:?}"),
    }
}

/// 建一个带一次提交的仓 + 一个 detached worktree。
fn repo_with_worktree() -> (TempRepo, std::path::PathBuf) {
    let r = TempRepo::new();
    r.write("a.txt", "原始内容\n");
    r.write("b.txt", "另一个\n");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    (r, wt)
}

// ---------------------------------------------------------------- 基础三态

#[test]
fn clean_worktree_passes() {
    let (r, wt) = repo_with_worktree();

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_eq!(
        DirtyGate.evaluate(&c),
        GateStatus::Pass,
        "没有任何改动的 worktree 应放行"
    );
}

#[test]
fn untracked_file_blocks() {
    let (r, wt) = repo_with_worktree();
    r.write_in(&wt, "scratch.md", "agent 刚写的草稿，没提交");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    let (count, sample) = expect_blocked(
        DirtyGate.evaluate(&c),
        "未跟踪文件必须拦下：删了就永远找不回来",
    );
    assert_eq!(count, 1, "只有一个未跟踪文件，应计 1 处改动");
    assert!(
        sample.iter().any(|p| p == "scratch.md"),
        "样例里应点名 scratch.md，实际 {sample:?}"
    );
}

#[test]
fn tracked_modification_blocks() {
    let (r, wt) = repo_with_worktree();
    r.write_in(&wt, "a.txt", "改过了，还没提交\n");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    let (_, sample) = expect_blocked(DirtyGate.evaluate(&c), "已跟踪文件的改动必须拦下");
    assert!(
        sample.iter().any(|p| p == "a.txt"),
        "样例里应点名 a.txt，实际 {sample:?}"
    );
}

/// 边界：被 gitignore 的构建产物**不算**未提交改动。
/// 它归 A3 / B3 管；在这道门里判脏会让所有装过依赖的 worktree 全部卡死。
#[test]
fn ignored_build_output_is_not_dirty() {
    let r = TempRepo::new();
    r.write(".gitignore", "target/\n");
    r.write("Cargo.toml", "[package]\nname='x'\n");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::create_dir_all(wt.join("target/debug")).expect("建 target");
    std::fs::write(wt.join("target/debug/bin"), "产物").expect("写产物");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_eq!(
        DirtyGate.evaluate(&c),
        GateStatus::Pass,
        "被忽略的构建产物不该算作未提交改动（那是 B3 Precious 的职责）"
    );
}

// ---------------------------------------------------------------- D3 护栏

/// D3：仓库配了 `status.showUntrackedFiles=no`，新建文件在 porcelain 里完全消失，
/// 而 `git worktree remove` 也不会拒绝——两层保护同时破。
#[test]
fn show_untracked_files_no_cannot_hide_new_files() {
    let (r, wt) = repo_with_worktree();
    r.git(&["config", "status.showUntrackedFiles", "no"]);
    r.write_in(&wt, "scratch.md", "这份草稿会在裸 status 里彻底隐身");

    // 前提校验：确认夹具真的复现了击穿条件，而不是配置没生效导致用例白跑
    let naive = r.git_in(&wt, &["status", "--porcelain=v1"]);
    assert!(
        naive.is_empty(),
        "夹具前提：设了 no 之后裸 status 应看不见新文件，实际 {naive:?}"
    );

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    let (_, sample) = expect_blocked(
        DirtyGate.evaluate(&c),
        "D3：即便仓库配了 showUntrackedFiles=no，门禁也必须自己覆盖回 all 并拦下",
    );
    assert!(
        sample.iter().any(|p| p == "scratch.md"),
        "样例里应点名 scratch.md，实际 {sample:?}"
    );
}

// ---------------------------------------------------------------- D4 护栏

/// D4：`skip-worktree` 让本地修改在 porcelain 里彻底不可见。
#[test]
fn skip_worktree_modification_blocks() {
    let (r, wt) = repo_with_worktree();
    r.git_in(&wt, &["update-index", "--skip-worktree", "a.txt"]);
    r.write_in(&wt, "a.txt", "偷偷改掉的本地配置\n");

    // 前提校验：这正是 status 与 worktree remove 一起失守的位置
    let naive = r.git_in(&wt, &["status", "--porcelain=v1", "-uall"]);
    assert!(
        naive.is_empty(),
        "夹具前提：skip-worktree 的改动应对 status 隐身，实际 {naive:?}"
    );

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    let (_, sample) = expect_blocked(
        DirtyGate.evaluate(&c),
        "D4：skip-worktree 标记的文件被改过，必须逐个比对内容后拦下",
    );
    assert!(
        sample.iter().any(|p| p == "a.txt"),
        "样例里应点名 a.txt，实际 {sample:?}"
    );
}

/// D4 变体：`assume-unchanged`，标记不同、后果一样。
#[test]
fn assume_unchanged_modification_blocks() {
    let (r, wt) = repo_with_worktree();
    r.git_in(&wt, &["update-index", "--assume-unchanged", "b.txt"]);
    r.write_in(&wt, "b.txt", "同样看不见的改动\n");

    let naive = r.git_in(&wt, &["status", "--porcelain=v1", "-uall"]);
    assert!(
        naive.is_empty(),
        "夹具前提：assume-unchanged 的改动应对 status 隐身，实际 {naive:?}"
    );

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    let (_, sample) = expect_blocked(
        DirtyGate.evaluate(&c),
        "D4：assume-unchanged 标记的文件被改过必须拦下",
    );
    assert!(
        sample.iter().any(|p| p == "b.txt"),
        "样例里应点名 b.txt，实际 {sample:?}"
    );
}

/// D4 的窄缝：**同时**打两个标记时 `ls-files -v` 输出的是小写 `s`，
/// 既不是 `S` 也不是 `h`，正好从「只认 S 和 h」的判据中间漏过去（git 2.53 实测）。
#[test]
fn both_marks_lowercase_tag_still_blocks() {
    let (r, wt) = repo_with_worktree();
    r.git_in(&wt, &["update-index", "--skip-worktree", "a.txt"]);
    r.git_in(&wt, &["update-index", "--assume-unchanged", "a.txt"]);
    r.write_in(&wt, "a.txt", "双标记掩护下的改动\n");

    let listing = r.git_in(&wt, &["ls-files", "-v"]);
    assert!(
        listing.lines().any(|l| l.starts_with("s ")),
        "夹具前提：双标记文件的标签应为小写 s，实际 {listing:?}"
    );
    let naive = r.git_in(&wt, &["status", "--porcelain=v1", "-uall"]);
    assert!(
        naive.is_empty(),
        "夹具前提：双标记的改动应对 status 隐身，实际 {naive:?}"
    );

    let (_, sample) = {
        let cfg = ScanConfig::default();
        let git = test_git();
        let procs = FakeProcs(Ok(vec![]));
        let clock = FixedClock(SystemTime::now());
        let forge = NoForge;
        let head = r.head();
        let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);
        expect_blocked(DirtyGate.evaluate(&c), "小写 s 标签同样要收进来，不能漏过")
    };
    assert!(
        sample.iter().any(|p| p == "a.txt"),
        "样例里应点名 a.txt，实际 {sample:?}"
    );
}

/// D4 的反面：标记打了但内容没动 —— 必须仍然放行。
/// 否则「凡有标记就拦」会把这道门做成恒 Blocked，等于没门。
#[test]
fn marked_but_unmodified_file_still_passes() {
    let (r, wt) = repo_with_worktree();
    r.git_in(&wt, &["update-index", "--skip-worktree", "a.txt"]);
    r.git_in(&wt, &["update-index", "--assume-unchanged", "b.txt"]);

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_eq!(
        DirtyGate.evaluate(&c),
        GateStatus::Pass,
        "只是打了标记、内容与索引一致，应当放行（不能做成恒 Blocked）"
    );
}

/// 被标记的文件在工作区里被删掉，同样是未提交改动。
#[test]
fn marked_file_deleted_blocks() {
    let (r, wt) = repo_with_worktree();
    r.git_in(&wt, &["update-index", "--skip-worktree", "a.txt"]);
    std::fs::remove_file(wt.join("a.txt")).expect("删文件");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    let (_, sample) = expect_blocked(
        DirtyGate.evaluate(&c),
        "被标记的文件被删除也是未提交改动，应拦下",
    );
    assert!(
        sample.iter().any(|p| p == "a.txt"),
        "样例里应点名 a.txt，实际 {sample:?}"
    );
}

// ---------------------------------------------------------------- D7 护栏

/// D7：git 调用失败必须落 Unknown。返回 Pass 就是这个工具最危险的失效形态。
#[test]
fn git_failure_is_unknown_never_pass() {
    let (r, wt) = repo_with_worktree();

    let cfg = ScanConfig::default();
    let git = BrokenGit;
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    let st = DirtyGate.evaluate(&c);
    assert!(
        matches!(
            st,
            GateStatus::Unknown(Cause::CommandFailed {
                code: Some(128),
                ..
            })
        ),
        "git 退出 128、stdout 为空时必须判 Unknown（绝不能当成干净），实际 {st:?}"
    );
}
