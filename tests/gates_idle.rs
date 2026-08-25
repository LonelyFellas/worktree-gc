#![allow(clippy::expect_used)]
//! worktree 空闲阈值只约束整树删除，不影响可重建缓存的回收。

use std::time::{Duration, SystemTime};
use wtgc::config::ScanConfig;
use wtgc::gates::recent::FixedClock;
use wtgc::git::{GitExec, GitRunner, RealGit};
use wtgc::model::{Cause, GateDetail, GateId, GateStatus, Verdict};
use wtgc::scan::{Env, scan};
use wtgc::testkit::{FakeProcs, TempRepo, test_git};

fn scan_at(cfg: &ScanConfig, now: SystemTime) -> wtgc::model::ScanReport {
    scan_at_with_git(cfg, now, test_git())
}

fn scan_at_with_git(
    cfg: &ScanConfig,
    now: SystemTime,
    git: impl GitRunner + 'static,
) -> wtgc::model::ScanReport {
    let env = Env {
        git: Box::new(git),
        forge: Box::new(wtgc::forge::Offline),
        clock: Box::new(FixedClock(now)),
        procs: Box::new(FakeProcs(Vec::new())),
    };
    scan(cfg, &env)
}

struct HeadTimeGit {
    inner: RealGit,
    epoch_secs: u64,
}

impl GitRunner for HeadTimeGit {
    fn exec(&self, cwd: &std::path::Path, args: &[&str]) -> Result<GitExec, Cause> {
        if args == ["show", "-s", "--format=%ct", "HEAD"] {
            return Ok(GitExec {
                code: Some(0),
                stdout: format!("{}\n", self.epoch_secs).into_bytes(),
                stderr: String::new(),
            });
        }
        self.inner.exec(cwd, args)
    }
}

#[test]
fn fresh_worktree_blocks_removal_but_not_cache_reclamation() {
    let repo = TempRepo::new();
    repo.write(".gitignore", "target/\n");
    repo.write(
        "Cargo.toml",
        "[package]\nname='idle-test'\nversion='0.1.0'\n",
    );
    let head = repo.commit("init");
    let worktree = repo.worktree("wt", &head);
    std::fs::create_dir_all(worktree.join("target/debug")).expect("建 target");
    std::fs::write(worktree.join("target/debug/artifact"), "cache").expect("写缓存");

    let cfg = ScanConfig {
        repos: vec![repo.root.clone()],
        idle: Duration::from_secs(24 * 3600),
        cache_quiet: Duration::ZERO,
        ..ScanConfig::default()
    };
    let report = scan_at(&cfg, SystemTime::now());
    let wt = report.repos[0]
        .worktrees
        .iter()
        .find(|wt| wt.path == worktree)
        .expect("找到 linked worktree");

    assert!(
        matches!(
            wt.outcomes.iter().find(|outcome| outcome.id == GateId::Idle),
            Some(outcome) if matches!(
                outcome.status,
                GateStatus::Blocked(GateDetail::RecentlyModified { .. })
            )
        ),
        "刚创建的 worktree 应被 idle 门禁拦下"
    );
    assert!(
        matches!(wt.verdict, Verdict::Blocked { ref by } if by.contains(&GateId::Idle)),
        "idle 未满足时不能删除整个 worktree，实际 {:?}",
        wt.verdict
    );
    assert!(
        wt.caches[0]
            .outcomes
            .iter()
            .all(|outcome| outcome.status.is_pass()),
        "idle 不应阻止 cache_quiet 已满足的缓存回收"
    );
}

#[test]
fn worktree_becomes_removable_after_idle_threshold() {
    let repo = TempRepo::new();
    repo.write("a.txt", "x");
    let head = repo.commit("init");
    let worktree = repo.worktree("wt", &head);

    let cfg = ScanConfig {
        repos: vec![repo.root.clone()],
        idle: Duration::from_secs(24 * 3600),
        ..ScanConfig::default()
    };
    let report = scan_at(&cfg, SystemTime::now() + Duration::from_secs(25 * 3600));
    let wt = report.repos[0]
        .worktrees
        .iter()
        .find(|wt| wt.path == worktree)
        .expect("找到 linked worktree");

    assert_eq!(
        wt.verdict,
        Verdict::Removable,
        "空闲满 24 小时后应通过删除门禁"
    );
}

#[test]
fn recent_head_commit_blocks_even_when_shallow_mtimes_are_old() {
    let repo = TempRepo::new();
    repo.write("src/features/deep/existing.rs", "x");
    let head = repo.commit("init");
    let worktree = repo.worktree("wt", &head);
    let now = SystemTime::now() + Duration::from_secs(25 * 3600);
    let recent_head = now - Duration::from_secs(3600);
    let epoch_secs = recent_head
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("时间晚于 epoch")
        .as_secs();

    let cfg = ScanConfig {
        repos: vec![repo.root.clone()],
        idle: Duration::from_secs(24 * 3600),
        ..ScanConfig::default()
    };
    let report = scan_at_with_git(
        &cfg,
        now,
        HeadTimeGit {
            inner: test_git(),
            epoch_secs,
        },
    );
    let wt = report.repos[0]
        .worktrees
        .iter()
        .find(|wt| wt.path == worktree)
        .expect("找到 linked worktree");

    assert!(
        matches!(
            wt.outcomes.iter().find(|outcome| outcome.id == GateId::Idle),
            Some(outcome) if matches!(
                outcome.status,
                GateStatus::Blocked(GateDetail::RecentlyModified { .. })
            )
        ),
        "近期 HEAD 提交必须阻止整树删除"
    );
}

#[test]
fn recent_deep_cache_write_blocks_worktree_removal() {
    use std::fs::{FileTimes, OpenOptions};

    let repo = TempRepo::new();
    repo.write(".gitignore", "target/\n");
    repo.write(
        "Cargo.toml",
        "[package]\nname='idle-cache'\nversion='0.1.0'\n",
    );
    let head = repo.commit("init");
    let worktree = repo.worktree("wt", &head);
    let artifact = worktree.join("target/debug/deps/existing-artifact");
    std::fs::create_dir_all(artifact.parent().expect("缓存父目录")).expect("建缓存目录");
    std::fs::write(&artifact, "cache").expect("写缓存");
    let now = SystemTime::now() + Duration::from_secs(25 * 3600);
    OpenOptions::new()
        .write(true)
        .open(&artifact)
        .expect("打开缓存")
        .set_times(FileTimes::new().set_modified(now - Duration::from_secs(3600)))
        .expect("设置缓存时间");

    let cfg = ScanConfig {
        repos: vec![repo.root.clone()],
        idle: Duration::from_secs(24 * 3600),
        cache_quiet: Duration::ZERO,
        ..ScanConfig::default()
    };
    let report = scan_at(&cfg, now);
    let wt = report.repos[0]
        .worktrees
        .iter()
        .find(|wt| wt.path == worktree)
        .expect("找到 linked worktree");

    assert!(
        matches!(
            wt.outcomes.iter().find(|outcome| outcome.id == GateId::Idle),
            Some(outcome) if matches!(
                outcome.status,
                GateStatus::Blocked(GateDetail::RecentlyModified { .. })
            )
        ),
        "三层缓存内的近期写入必须阻止整树删除"
    );
}

#[cfg(unix)]
#[test]
fn directory_symlink_does_not_import_external_activity() {
    use std::fs::{FileTimes, OpenOptions};
    use std::os::unix::fs::symlink;

    let repo = TempRepo::new();
    repo.write("a.txt", "x");
    let head = repo.commit("init");
    let worktree = repo.worktree("wt", &head);
    let outside = tempfile::tempdir().expect("建外部目录");
    let outside_file = outside.path().join("fresh");
    std::fs::write(&outside_file, "outside").expect("写外部文件");
    let now = SystemTime::now() + Duration::from_secs(25 * 3600);
    OpenOptions::new()
        .write(true)
        .open(&outside_file)
        .expect("打开外部文件")
        .set_times(FileTimes::new().set_modified(now - Duration::from_secs(3600)))
        .expect("设置外部文件时间");
    symlink(outside.path(), worktree.join("external-link")).expect("创建目录软链接");

    let cfg = ScanConfig {
        repos: vec![repo.root.clone()],
        idle: Duration::from_secs(24 * 3600),
        ..ScanConfig::default()
    };
    let report = scan_at(&cfg, now);
    let wt = report.repos[0]
        .worktrees
        .iter()
        .find(|wt| wt.path == worktree)
        .expect("找到 linked worktree");
    let idle = wt
        .outcomes
        .iter()
        .find(|outcome| outcome.id == GateId::Idle)
        .expect("存在 idle 门禁");

    assert_eq!(
        idle.status,
        GateStatus::Pass,
        "不得跟随软链接读取 worktree 外部活动"
    );
}
