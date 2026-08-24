#![allow(clippy::expect_used)]
//! A 组门禁的回归测试。
//!
//! 每个用例对应 docs/design.md 里的一条数据丢失场景。这些不是「覆盖率测试」，
//! 是护栏：原型阶段每一条都真的丢过东西。

use std::time::{Duration, SystemTime};
use wtgc::config::ScanConfig;
use wtgc::gates::cachesafe::CacheSafeGate;
use wtgc::gates::recent::{FixedClock, RecentGate};
use wtgc::gates::{Clock, Gate, GateCtx, MergeStatusProvider, ProcInfo, ProcessTable};
use wtgc::model::{CacheUnsafeReason, Cause, GateDetail, GateStatus};
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

#[allow(clippy::too_many_arguments)] // 测试辅助函数，参数即依赖注入
fn ctx<'a>(
    repo: &'a TempRepo,
    wt: &'a std::path::Path,
    head: &'a str,
    cfg: &'a ScanConfig,
    git: &'a dyn wtgc::git::GitRunner,
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

/// D8：入库的 `dist/` 长得跟构建产物一模一样，删了就是永久损失。
#[test]
fn tracked_files_block_cache_reclamation() {
    let r = TempRepo::new();
    r.write(".gitignore", "target/\n");
    r.write("package.json", "{\"name\":\"x\"}\n");
    r.write("dist/bundle.js", "// 这是入库的产物，不是缓存");
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

    let st = CacheSafeGate { dir: "dist".into() }.evaluate(&c);
    match st {
        GateStatus::Blocked(GateDetail::NotPureCache { reason }) => {
            // 先撞上「未被忽略」这条也算拦住了——关键是不放行
            assert!(
                matches!(
                    reason,
                    CacheUnsafeReason::ContainsTrackedFiles { .. } | CacheUnsafeReason::NotIgnored
                ),
                "应因含 tracked 文件或未被忽略而拒绝，实际 {reason:?}"
            );
        }
        other => panic!("入库的 dist/ 必须被拦下，实际 {other:?}"),
    }
}

/// 正常的 Rust target：被忽略、无 tracked 文件、非 symlink、在根内 → 放行。
#[test]
fn plain_ignored_target_passes() {
    let r = TempRepo::new();
    r.write(".gitignore", "target/\n");
    r.write("Cargo.toml", "[package]\nname='x'\n");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::create_dir_all(wt.join("target/debug")).expect("建 target");
    std::fs::write(wt.join("target/debug/bin"), "artifact").expect("写产物");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_eq!(
        CacheSafeGate {
            dir: "target".into()
        }
        .evaluate(&c),
        GateStatus::Pass,
        "纯忽略产物应放行"
    );
}

/// 同名目录和 gitignore 都不是充分证据：没有 Cargo.toml 的 `target/` 可能是用户资料。
#[test]
fn ignored_target_without_rust_marker_is_blocked() {
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

    assert!(
        matches!(
            CacheSafeGate {
                dir: "target".into()
            }
            .evaluate(&c),
            GateStatus::Blocked(GateDetail::NotPureCache {
                reason: CacheUnsafeReason::MissingMarker { .. }
            })
        ),
        "缺少 Cargo.toml 时不能把 target 当作 Rust 缓存"
    );
}

/// 符号链接：删它可能波及链接目标之外的东西。
#[cfg(unix)]
#[test]
fn symlinked_cache_is_blocked() {
    let r = TempRepo::new();
    r.write(".gitignore", "target/\n");
    r.write("Cargo.toml", "[package]\nname='x'\n");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());

    let elsewhere = r.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("建目录");
    std::os::unix::fs::symlink(&elsewhere, wt.join("target")).expect("建软链");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert!(
        matches!(
            CacheSafeGate {
                dir: "target".into()
            }
            .evaluate(&c),
            GateStatus::Blocked(GateDetail::NotPureCache {
                reason: CacheUnsafeReason::IsSymlink
            })
        ),
        "符号链接必须被拦下"
    );
}

/// 刚写过的缓存目录 → A2 拦下（构建可能还在跑）。
#[test]
fn freshly_written_cache_is_blocked_by_recency() {
    let r = TempRepo::new();
    r.write(".gitignore", "target/\n");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::create_dir_all(wt.join("target")).expect("建 target");
    std::fs::write(wt.join("target/fresh"), "刚写的").expect("写文件");

    let cfg = ScanConfig::default(); // cache_quiet 默认 10 分钟
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now());
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert!(
        matches!(
            RecentGate {
                dir: "target".into()
            }
            .evaluate(&c),
            GateStatus::Blocked(GateDetail::RecentlyModified { .. })
        ),
        "刚写过的缓存不该放行"
    );
}

/// 同一个目录，把时钟拨到 1 小时后 → 放行。
#[test]
fn quiet_cache_passes_recency() {
    let r = TempRepo::new();
    r.write(".gitignore", "target/\n");
    r.commit("init");
    let wt = r.worktree("wt", &r.head());
    std::fs::create_dir_all(wt.join("target")).expect("建 target");
    std::fs::write(wt.join("target/old"), "旧的").expect("写文件");

    let cfg = ScanConfig::default();
    let git = test_git();
    let procs = FakeProcs(Ok(vec![]));
    let clock = FixedClock(SystemTime::now() + Duration::from_secs(3600));
    let forge = NoForge;
    let head = r.head();
    let c = ctx(&r, &wt, &head, &cfg, &git, &procs, &clock, &forge);

    assert_eq!(
        RecentGate {
            dir: "target".into()
        }
        .evaluate(&c),
        GateStatus::Pass,
        "安静足够久的缓存应放行"
    );
}
