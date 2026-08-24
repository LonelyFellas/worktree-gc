#![allow(clippy::expect_used)]
//! B3 门禁的回归测试。
//!
//! 这道门守的是 `git worktree remove` 的静默删除行为，所以每个用例都用真 git 造仓：
//! 「哪些文件被忽略」由 git 的 gitignore 语义说了算，mock 掉就等于测自己编的故事。

use std::time::SystemTime;
use wtgc::config::ScanConfig;
use wtgc::gates::precious::PreciousGate;
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

/// 复刻 D7 的形状：非零退出 + 空 stdout。原脚本把它当成「什么都没查到」。
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

#[allow(clippy::too_many_arguments)] // 测试辅助函数，参数即依赖注入
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

/// 断言拦下了，且列出的路径里有以 `needle` 结尾的一条。
fn assert_blocked_with(st: &GateStatus, needle: &str) {
    match st {
        GateStatus::Blocked(GateDetail::PreciousFiles { paths }) => assert!(
            paths.iter().any(|p| p.ends_with(needle)),
            "应把 {needle} 列为独有敏感文件，实际列出 {paths:?}"
        ),
        other => panic!("{needle} 是 worktree 独有的敏感文件，必须拦下，实际 {other:?}"),
    }
}

/// 干净的 worktree：没有任何被忽略的内容 → 放行。
#[test]
fn nothing_ignored_passes() {
    let r = TempRepo::new();
    r.write(".gitignore", "target/\n");
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
        PreciousGate.evaluate(&c),
        GateStatus::Pass,
        "没有任何被忽略的内容时，这道门应放行"
    );
}

/// 只有构建缓存：在可弃名单里，不该拦，也不该为它遍历 30GB。
#[test]
fn disposable_build_cache_alone_passes() {
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
        PreciousGate.evaluate(&c),
        GateStatus::Pass,
        "只有可弃名单内的构建缓存时应放行"
    );
}

/// 可弃目录也必须有生态 marker；否则 PreciousGate 会把同名资料目录整个跳过。
#[test]
fn ignored_target_without_marker_is_precious() {
    let r = TempRepo::new();
    r.write(".gitignore", "target/\n");
    r.write("a.txt", "x");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::create_dir_all(wt.join("target")).expect("建 target");
    std::fs::write(wt.join("target/notes.txt"), "不可重建的资料").expect("写资料");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_blocked_with(&PreciousGate.evaluate(&c), "target/notes.txt");
}

/// 根目录有 Cargo.toml 也不能替深层同名目录作证；marker 必须属于缓存的同级项目。
#[test]
fn nested_target_without_nearby_marker_is_precious() {
    let r = TempRepo::new();
    r.write(".gitignore", "data/target/\n");
    r.write("Cargo.toml", "[package]\nname='outer'\n");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::create_dir_all(wt.join("data/target")).expect("建深层 target");
    std::fs::write(wt.join("data/target/notes.txt"), "不可重建的资料").expect("写资料");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_blocked_with(&PreciousGate.evaluate(&c), "data/target/notes.txt");
}

/// **D1**：`ls-files --directory` 把整个被忽略的目录折叠成一行 `secrets/`，
/// 原型按「不是文件」跳过，于是含 prod.key 的整个目录被静默删掉。
#[test]
fn collapsed_ignored_directory_is_opened_up() {
    let r = TempRepo::new();
    r.write(".gitignore", "secrets/\n");
    r.write("a.txt", "x");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::create_dir_all(wt.join("secrets")).expect("建 secrets");
    std::fs::write(wt.join("secrets/prod.key"), "AKIA-真钥匙").expect("写密钥");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_blocked_with(&PreciousGate.evaluate(&c), "secrets/prod.key");
}

/// 与主工作区逐字节相同的 `.env` 只是副本，删了没有损失——全拦下来这道门就恒假。
#[test]
fn identical_copy_of_main_env_passes() {
    let r = TempRepo::new();
    r.write(".gitignore", ".env\n");
    r.write("a.txt", "x");
    r.commit("init");
    r.write(".env", "API_URL=http://localhost:8383\n");
    let wt = r.worktree("wt", &r.head());
    std::fs::write(wt.join(".env"), "API_URL=http://localhost:8383\n").expect("写 .env");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_eq!(
        PreciousGate.evaluate(&c),
        GateStatus::Pass,
        "与主工作区内容相同的 .env 是纯副本，不该拦"
    );
}

/// 同名但内容不同 → 这份配置是 worktree 独有的改动，删了就没了。
#[test]
fn diverged_env_is_blocked() {
    let r = TempRepo::new();
    r.write(".gitignore", ".env\n");
    r.write("a.txt", "x");
    r.commit("init");
    r.write(".env", "API_URL=http://localhost:8383\n");
    let wt = r.worktree("wt", &r.head());
    std::fs::write(
        wt.join(".env"),
        "API_URL=http://staging.internal\nTOKEN=只此一份\n",
    )
    .expect("写 .env");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_blocked_with(&PreciousGate.evaluate(&c), ".env");
}

/// **D2**：白名单语义漏掉的东西之一。terraform state 丢了要重建整套基础设施。
#[test]
fn tfstate_is_blocked() {
    let r = TempRepo::new();
    r.write(".gitignore", "*.tfstate\n");
    r.write("a.txt", "x");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::write(wt.join("foo.tfstate"), "{\"serial\":7}").expect("写 tfstate");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_blocked_with(&PreciousGate.evaluate(&c), "foo.tfstate");
}

/// **D2 的真正形状**：没人列过 `.safetensors`，黑名单语义照样拦得住。
/// 这条用例专门盯着「别人仓里的敏感文件集合是无界的」——它不在任何模式名单里。
#[test]
fn unlisted_file_type_is_still_blocked() {
    let r = TempRepo::new();
    r.write(".gitignore", "*.safetensors\n");
    r.write("a.txt", "x");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::write(wt.join("model.safetensors"), "训了三天的权重").expect("写权重");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_blocked_with(&PreciousGate.evaluate(&c), "model.safetensors");
}

/// 敏感文件嵌在被忽略的目录里：折叠出来的是 `config/`，得递归进去才看得见。
#[test]
fn sensitive_file_nested_in_ignored_dir_is_blocked() {
    let r = TempRepo::new();
    r.write(".gitignore", "config/\n");
    r.write("a.txt", "x");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::create_dir_all(wt.join("config")).expect("建 config");
    std::fs::write(wt.join("config/secrets.json"), "{\"token\":\"独此一份\"}").expect("写配置");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_blocked_with(&PreciousGate.evaluate(&c), "config/secrets.json");
}

/// `always_precious` 是内容比对之上的额外保险：私钥拷贝两处仍然是私钥。
#[test]
fn always_precious_pattern_beats_identical_copy() {
    let r = TempRepo::new();
    r.write(".gitignore", "id_rsa\n");
    r.write("a.txt", "x");
    r.commit("init");
    r.write("id_rsa", "-----BEGIN PRIVATE KEY-----\n");
    let wt = r.worktree("wt", &r.head());
    std::fs::write(wt.join("id_rsa"), "-----BEGIN PRIVATE KEY-----\n").expect("写私钥");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_blocked_with(&PreciousGate.evaluate(&c), "id_rsa");
}

/// 超过限深的目录不是「看不见就当没有」，而是整个交给人确认。
#[test]
fn directory_deeper_than_limit_is_blocked_not_skipped() {
    let r = TempRepo::new();
    r.write(".gitignore", "data/\n");
    r.write("a.txt", "x");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::create_dir_all(wt.join("data/a/b/c/d/e")).expect("建深目录");
    std::fs::write(wt.join("data/a/b/c/d/e/secret.txt"), "埋得很深").expect("写文件");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_blocked_with(&PreciousGate.evaluate(&c), "data/a/b/c/d");
}

/// **D7**：git 非零退出 + 空 stdout，绝不能被读成「没有敏感文件」。
#[test]
fn git_failure_is_unknown_never_pass() {
    let r = TempRepo::new();
    r.write(".gitignore", "secrets/\n");
    r.write("a.txt", "x");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::create_dir_all(wt.join("secrets")).expect("建 secrets");
    std::fs::write(wt.join("secrets/prod.key"), "AKIA-真钥匙").expect("写密钥");

    let cfg = ScanConfig::default();
    let git = BrokenGit;
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert!(
        matches!(
            PreciousGate.evaluate(&c),
            GateStatus::Unknown(Cause::CommandFailed { .. })
        ),
        "git 失败必须落 Unknown；返回 Pass 就是 D7 那种静默放行"
    );
}
