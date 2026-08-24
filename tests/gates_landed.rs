#![allow(clippy::expect_used)]
//! B2 Landed 门禁的回归测试。
//!
//! 全部用真 git 造仓：这道门的每一个坑都是 git 行为的意外（squash 之后 oid 全变、
//! `git cherry` 在多提交 squash 时两条都标 `+`、base 前进后三点 diff 失效）。
//! mock 掉 git 就是测自己编的故事。
//!
//! 只有 forge 是替身——不打真网络。

use std::path::{Path, PathBuf};
use std::time::SystemTime;
use wtgc::config::ScanConfig;
use wtgc::gates::landed::LandedGate;
use wtgc::gates::recent::FixedClock;
use wtgc::gates::{Gate, GateCtx, MergeStatusProvider, ProcInfo, ProcessTable};
use wtgc::git::{GitExec, GitRunner};
use wtgc::model::{Baseline, BaselineSource, Cause, GateDetail, GateStatus};
use wtgc::testkit::{TempRepo, test_git};

// ---------- 替身 ----------

/// 这道门不看进程，给个恒空实现占位。
struct NoProcs;
impl ProcessTable for NoProcs {
    fn processes_under(&self, _dir: &Path) -> Result<Vec<ProcInfo>, Cause> {
        Ok(vec![])
    }
}

/// forge 说「没有已合入的 PR」。
struct NoForge;
impl MergeStatusProvider for NoForge {
    fn merged_pr(&self, _repo: &Path, _branch: &str, _oid: &str) -> Result<Option<u64>, Cause> {
        Ok(None)
    }
}

/// forge 说「已合入 PR #7」。
struct MergedForge;
impl MergeStatusProvider for MergedForge {
    fn merged_pr(&self, _repo: &Path, _branch: &str, _oid: &str) -> Result<Option<u64>, Cause> {
        Ok(Some(7))
    }
}

/// forge 查不通（断网 / 未鉴权 / 没装 gh）。
struct BrokenForge;
impl MergeStatusProvider for BrokenForge {
    fn merged_pr(&self, _repo: &Path, _branch: &str, _oid: &str) -> Result<Option<u64>, Cause> {
        Err(Cause::ForgeUnavailable {
            detail: "网络不可达".into(),
        })
    }
}

/// 总是失败的 git —— 用于 D7 否定性断言。
struct FailGit;
impl GitRunner for FailGit {
    fn exec(&self, cwd: &Path, args: &[&str]) -> Result<GitExec, Cause> {
        Err(Cause::Io {
            path: cwd.to_path_buf(),
            msg: format!("git 起不来: {}", args.join(" ")),
        })
    }
}

// ---------- 布景 ----------

/// 一次求值所需的全部上下文。git 是真的，只有 forge 可替换。
struct Scene {
    repo: TempRepo,
    wt: PathBuf,
    head: String,
    cfg: ScanConfig,
    git: Box<dyn GitRunner>,
    procs: NoProcs,
    clock: FixedClock,
}

impl Scene {
    /// 在 `committish` 上开一个 worktree 作为被判定对象。
    fn at(repo: TempRepo, committish: &str) -> Scene {
        Scene::at_with(repo, committish, Box::new(test_git()))
    }

    fn at_with(repo: TempRepo, committish: &str, git: Box<dyn GitRunner>) -> Scene {
        let wt = repo.worktree("wt", committish);
        Scene {
            repo,
            wt,
            head: committish.to_string(),
            cfg: ScanConfig::default(),
            git,
            procs: NoProcs,
            clock: FixedClock(SystemTime::now()),
        }
    }

    fn ctx<'a>(
        &'a self,
        branch: Option<&'a str>,
        baseline: Option<&'a Baseline>,
        forge: &'a dyn MergeStatusProvider,
    ) -> GateCtx<'a> {
        GateCtx {
            repo: &self.repo.root,
            worktree: &self.wt,
            branch,
            head_oid: &self.head,
            baseline,
            cfg: &self.cfg,
            git: self.git.as_ref(),
            procs: &self.procs,
            clock: &self.clock,
            forge,
        }
    }
}

/// 本地 `main` 作基线（测试仓没有远端，refname 就是 `main`）。
fn main_baseline() -> Baseline {
    Baseline {
        remote: None,
        branch: "main".into(),
        source: BaselineSource::Guessed,
    }
}

/// 主干一个初始提交 + feature 分支两个提交（改了两个不同文件）。返回分支尖端 oid。
/// 返回时仓已切回 main。
fn feature_with_two_commits(r: &TempRepo) -> String {
    r.write("a.txt", "base\n");
    r.commit("init");
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("b.txt", "b1\n");
    r.commit("feat b");
    r.write("c.txt", "c1\n");
    let tip = r.commit("feat c");
    r.git(&["checkout", "-q", "main"]);
    tip
}

// ---------- 用例 ----------

/// HEAD 就是基线本身——最平凡的已落地形态。
#[test]
fn head_equal_to_baseline_passes() {
    let r = TempRepo::new();
    r.write("a.txt", "base\n");
    let head = r.commit("init");
    let s = Scene::at(r, &head);

    let b = main_baseline();
    let forge = NoForge;
    assert_eq!(
        LandedGate.evaluate(&s.ctx(None, Some(&b), &forge)),
        GateStatus::Pass,
        "HEAD 与基线同一个提交时必须判为已落地"
    );
}

/// merge commit 合入：分支尖端是 merge 的父提交，祖先判定直接成立。
#[test]
fn merge_commit_passes_by_ancestry() {
    let r = TempRepo::new();
    let tip = feature_with_two_commits(&r);
    r.git(&["merge", "--no-ff", "-q", "-m", "merge feature", "feature"]);
    let s = Scene::at(r, &tip);

    let b = main_baseline();
    let forge = NoForge;
    assert_eq!(
        LandedGate.evaluate(&s.ctx(None, Some(&b), &forge)),
        GateStatus::Pass,
        "merge commit 合入后分支尖端是主干的祖先，应判为已落地"
    );
}

/// squash 单提交合入：oid 对不上，只能靠内容判据。
#[test]
fn squashed_single_commit_passes() {
    let r = TempRepo::new();
    r.write("a.txt", "base\n");
    r.commit("init");
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("b.txt", "b1\n");
    let tip = r.commit("feat b");
    r.git(&["checkout", "-q", "main"]);
    r.git(&["merge", "--squash", "-q", "feature"]);
    r.git(&["commit", "-q", "-m", "squash feature"]);
    let s = Scene::at(r, &tip);

    let b = main_baseline();
    let forge = NoForge;
    assert_eq!(
        LandedGate.evaluate(&s.ctx(None, Some(&b), &forge)),
        GateStatus::Pass,
        "squash 单提交合入后内容已在主干里，应判为已落地"
    );
}

/// squash **多提交**合入：`git cherry` 会把两条都标 `+`，识别不出——本级判据的存在理由。
#[test]
fn squashed_multiple_commits_passes() {
    let r = TempRepo::new();
    let tip = feature_with_two_commits(&r);
    r.git(&["merge", "--squash", "-q", "feature"]);
    r.git(&["commit", "-q", "-m", "squash feature"]);
    let s = Scene::at(r, &tip);

    let b = main_baseline();
    let forge = NoForge;
    assert_eq!(
        LandedGate.evaluate(&s.ctx(None, Some(&b), &forge)),
        GateStatus::Pass,
        "squash 多提交合入后内容已在主干里，应判为已落地（git cherry 在这里识别不出）"
    );
}

/// squash 多提交 + 基线又前进了一个无关提交：朴素 diff 与三点 diff 都失效，
/// 只有「限定在分支改过的文件上」才判得准。
#[test]
fn squashed_then_baseline_advanced_passes() {
    let r = TempRepo::new();
    let tip = feature_with_two_commits(&r);
    r.git(&["merge", "--squash", "-q", "feature"]);
    r.git(&["commit", "-q", "-m", "squash feature"]);
    r.write("d.txt", "无关提交\n");
    r.commit("unrelated");
    let s = Scene::at(r, &tip);

    let b = main_baseline();
    let forge = NoForge;
    assert_eq!(
        LandedGate.evaluate(&s.ctx(None, Some(&b), &forge)),
        GateStatus::Pass,
        "基线在 squash 之后又前进了无关提交，仍应判为已落地——判据只该看分支改过的文件"
    );
}

/// 未合入的分支：三级判据全不成立，必须拦下并报出领先提交数。
#[test]
fn unlanded_branch_is_blocked() {
    let r = TempRepo::new();
    let tip = feature_with_two_commits(&r);
    let s = Scene::at(r, &tip);

    let b = main_baseline();
    let forge = NoForge;
    match LandedGate.evaluate(&s.ctx(None, Some(&b), &forge)) {
        GateStatus::Blocked(GateDetail::NotLanded { ahead, baseline }) => {
            assert_eq!(ahead, 2, "应报出分支领先基线的 2 个提交");
            assert_eq!(baseline, "main", "应报出判定所用的基线 ref");
        }
        other => panic!("未合入的分支必须被拦下，实际 {other:?}"),
    }
}

/// 退化陷阱：分支改了又改回去，净变更为零。此时「这些文件已无差异」会平凡成立，
/// 不堵住就会把一个毫无落地的分支放行。
#[test]
fn zero_net_change_branch_is_blocked() {
    let r = TempRepo::new();
    r.write("a.txt", "base\n");
    r.commit("init");
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("a.txt", "改了\n");
    r.commit("改");
    r.write("a.txt", "base\n");
    let tip = r.commit("又改回去");
    r.git(&["checkout", "-q", "main"]);
    let s = Scene::at(r, &tip);

    let b = main_baseline();
    let forge = NoForge;
    match LandedGate.evaluate(&s.ctx(None, Some(&b), &forge)) {
        GateStatus::Blocked(GateDetail::NotLanded { ahead, .. }) => {
            assert_eq!(ahead, 2, "净变更为零但仍有 2 个独有提交，应如实报出");
        }
        other => {
            panic!("净变更为零的分支不得被判为已落地（内容判据在这里平凡成立），实际 {other:?}")
        }
    }
}

/// 基线探测失败 → Unknown。绝不能因为「没基线可比」而放行。
#[test]
fn missing_baseline_is_unknown() {
    let r = TempRepo::new();
    let tip = feature_with_two_commits(&r);
    let s = Scene::at(r, &tip);

    let forge = NoForge;
    let st = LandedGate.evaluate(&s.ctx(None, None, &forge));
    assert!(
        matches!(st, GateStatus::Unknown(_)),
        "基线未知时必须落 Unknown 而不是 Pass，实际 {st:?}"
    );
}

/// forge 说已合入：squash / rebase 之后这是唯一权威答案，优先于离线判据。
#[test]
fn forge_merged_pr_passes() {
    let r = TempRepo::new();
    let tip = feature_with_two_commits(&r);
    let s = Scene::at(r, &tip);

    let b = main_baseline();
    let forge = MergedForge;
    assert_eq!(
        LandedGate.evaluate(&s.ctx(Some("feature"), Some(&b), &forge)),
        GateStatus::Pass,
        "forge 报告 PR 已合入时应判为已落地，哪怕离线判据还看得见差异"
    );
}

/// forge 查不通但离线判得出来（squash 形态）→ 仍然放行。
/// 网络不可达不该让整个判定瘫痪。
#[test]
fn forge_error_falls_back_to_offline_pass() {
    let r = TempRepo::new();
    let tip = feature_with_two_commits(&r);
    r.git(&["merge", "--squash", "-q", "feature"]);
    r.git(&["commit", "-q", "-m", "squash feature"]);
    let s = Scene::at(r, &tip);

    let b = main_baseline();
    let forge = BrokenForge;
    assert_eq!(
        LandedGate.evaluate(&s.ctx(Some("feature"), Some(&b), &forge)),
        GateStatus::Pass,
        "forge 查不通时应降级到离线判据，squash 内容已在主干里就该放行"
    );
}

/// forge 查不通且离线也判不出来 → Unknown 而非 Blocked：
/// 「查不到」不等于「没合入」（主干后续动过同一批文件时离线视角照样有差异）。
#[test]
fn forge_error_without_offline_evidence_is_unknown() {
    let r = TempRepo::new();
    let tip = feature_with_two_commits(&r);
    let s = Scene::at(r, &tip);

    let b = main_baseline();
    let forge = BrokenForge;
    let st = LandedGate.evaluate(&s.ctx(Some("feature"), Some(&b), &forge));
    assert!(
        matches!(st, GateStatus::Unknown(Cause::ForgeUnavailable { .. })),
        "forge 不可用且离线无法证明已落地时应落 Unknown（保留 forge 成因），实际 {st:?}"
    );
}

/// 改动路径含 glob 元字符（Next.js 的 `app/[slug]/page.tsx` 这类）时，
/// 判据必须按字面路径跑：匹配不到会读作「无差异」而误判已落地，
/// 匹配过头则会把没改过的文件也拖进来而误拦。
#[test]
fn glob_metachar_paths_are_judged_literally() {
    // 未合入 —— 必须拦下
    let r = TempRepo::new();
    r.write("a.txt", "base\n");
    r.commit("init");
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.write("app/[slug]/page.tsx", "页面\n");
    let tip = r.commit("feat 动态路由");
    r.git(&["checkout", "-q", "main"]);
    let s = Scene::at(r, &tip);

    let b = main_baseline();
    let forge = NoForge;
    assert!(
        matches!(
            LandedGate.evaluate(&s.ctx(None, Some(&b), &forge)),
            GateStatus::Blocked(GateDetail::NotLanded { .. })
        ),
        "路径含 `[slug]` 的未合入分支必须被拦下——匹配不到路径就会读成「无差异」而放行"
    );

    // 同样的路径，squash 合入之后 —— 必须放行
    let r2 = TempRepo::new();
    r2.write("a.txt", "base\n");
    r2.commit("init");
    r2.git(&["checkout", "-q", "-b", "feature"]);
    r2.write("app/[slug]/page.tsx", "页面\n");
    let tip2 = r2.commit("feat 动态路由");
    r2.git(&["checkout", "-q", "main"]);
    r2.git(&["merge", "--squash", "-q", "feature"]);
    r2.git(&["commit", "-q", "-m", "squash feature"]);
    let s2 = Scene::at(r2, &tip2);

    assert_eq!(
        LandedGate.evaluate(&s2.ctx(None, Some(&b), &forge)),
        GateStatus::Pass,
        "路径含 `[slug]` 的分支 squash 合入后应放行——字面量匹配不该把判定弄丢"
    );
}

/// D7 否定性断言：git 全线失败时绝不放行。
#[test]
fn failing_git_never_passes() {
    let r = TempRepo::new();
    let tip = feature_with_two_commits(&r);
    let s = Scene::at_with(r, &tip, Box::new(FailGit));

    let b = main_baseline();
    let forge = NoForge;
    let st = LandedGate.evaluate(&s.ctx(None, Some(&b), &forge));
    assert!(
        matches!(st, GateStatus::Unknown(_)),
        "git 全线失败必须落 Unknown，吞掉错误后放行正是 D7 的 fail-open，实际 {st:?}"
    );
}
