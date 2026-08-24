//! 测试夹具：**用真 git 造仓，不 mock git**。
//!
//! 这个项目所有真实 bug 都是 git 行为的意外（remove 吃掉 .env.local、
//! squash 后祖先判定失败、--directory 折叠出目录、showUntrackedFiles=no 击穿门禁）。
//! mock 掉 git 等于测自己编的故事。
//!
//! 卫生条件必须写死，否则 CI 与本机行为不一致：
//! 全局/系统配置隔离、空模板（避开用户钩子）、固定时间（oid 可复现）。

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct TempRepo {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

impl TempRepo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("建临时目录");
        let root = dir.path().canonicalize().expect("canonicalize");
        let repo = Self { _dir: dir, root };
        repo.git(&["init", "--template=", "-q", "-b", "main", "."]);
        repo.git(&["config", "user.name", "wtgc-test"]);
        repo.git(&["config", "user.email", "test@example.invalid"]);
        repo
    }

    /// 在仓内执行 git，带上全部隔离环境变量。
    pub fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.root, args)
    }

    pub fn git_in(&self, cwd: &Path, args: &[&str]) -> String {
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
            out.status.success(),
            "git {:?} 失败: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    pub fn write(&self, rel: &str, content: &str) -> PathBuf {
        self.write_in(&self.root, rel, content)
    }

    pub fn write_in(&self, base: &Path, rel: &str, content: &str) -> PathBuf {
        let p = base.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("建父目录");
        }
        std::fs::write(&p, content).expect("写文件");
        p
    }

    pub fn commit(&self, msg: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
        self.git(&["rev-parse", "HEAD"]).trim().to_string()
    }

    /// 建一个 worktree，返回其绝对路径。
    pub fn worktree(&self, name: &str, committish: &str) -> PathBuf {
        let p = self.root.join(name);
        self.git(&["worktree", "add", "-q", "--detach", p.to_str().expect("路径"), committish]);
        p.canonicalize().expect("canonicalize")
    }

    pub fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"]).trim().to_string()
    }
}

impl Default for TempRepo {
    fn default() -> Self {
        Self::new()
    }
}

/// 真实 git 执行器，配好隔离环境，供门禁单测使用。
pub fn test_git() -> crate::git::RealGit {
    crate::git::RealGit::new(
        crate::git::exe::resolve("git").expect("找到 git"),
        std::time::Duration::from_secs(30),
    )
}
