#![allow(clippy::expect_used)]
//! 扫描级快照的编排回归：一次扫描不能为每个 worktree 重刷进程表或注册表。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use wtgc::config::ScanConfig;
use wtgc::gates::recent::FixedClock;
use wtgc::gates::{ProcInfo, ProcessTable};
use wtgc::git::{GitExec, GitRunner, RealGit};
use wtgc::model::Cause;
use wtgc::scan::{Env, scan};
use wtgc::testkit::{TempRepo, test_git};

struct CountingGit {
    inner: RealGit,
    worktree_lists: Arc<AtomicUsize>,
}

impl GitRunner for CountingGit {
    fn exec(&self, cwd: &Path, args: &[&str]) -> Result<GitExec, Cause> {
        if args == ["worktree", "list", "--porcelain"] {
            self.worktree_lists.fetch_add(1, Ordering::Relaxed);
        }
        self.inner.exec(cwd, args)
    }
}

struct CountingProcs {
    single_queries: Arc<AtomicUsize>,
    batch_queries: Arc<AtomicUsize>,
}

impl ProcessTable for CountingProcs {
    fn processes_under(&self, _dir: &Path) -> Result<Vec<ProcInfo>, Cause> {
        self.single_queries.fetch_add(1, Ordering::Relaxed);
        Ok(Vec::new())
    }

    fn processes_under_many(&self, dirs: &[PathBuf]) -> Vec<Result<Vec<ProcInfo>, Cause>> {
        self.batch_queries.fetch_add(1, Ordering::Relaxed);
        dirs.iter().map(|_| Ok(Vec::new())).collect()
    }
}

#[test]
fn one_scan_reuses_one_process_snapshot_and_one_worktree_registry_read() {
    let repo = TempRepo::new();
    repo.write("tracked.txt", "x");
    let head = repo.commit("init");
    repo.worktree("linked", &head);

    let worktree_lists = Arc::new(AtomicUsize::new(0));
    let single_queries = Arc::new(AtomicUsize::new(0));
    let batch_queries = Arc::new(AtomicUsize::new(0));
    let env = Env {
        git: Box::new(CountingGit {
            inner: test_git(),
            worktree_lists: Arc::clone(&worktree_lists),
        }),
        forge: Box::new(wtgc::forge::Offline),
        clock: Box::new(FixedClock(
            std::time::SystemTime::now() + Duration::from_secs(1),
        )),
        procs: Box::new(CountingProcs {
            single_queries: Arc::clone(&single_queries),
            batch_queries: Arc::clone(&batch_queries),
        }),
    };
    let cfg = ScanConfig {
        repos: vec![repo.root.clone()],
        idle: Duration::ZERO,
        ..ScanConfig::default()
    };

    let report = scan(&cfg, &env);

    assert_eq!(report.repos[0].worktrees.len(), 2);
    assert_eq!(batch_queries.load(Ordering::Relaxed), 1);
    assert_eq!(single_queries.load(Ordering::Relaxed), 0);
    assert_eq!(
        worktree_lists.load(Ordering::Relaxed),
        1,
        "discover 读到的注册表必须供 Locked 与 Nested 共用"
    );
}
