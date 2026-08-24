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

// ───────────────────────── 记录型替身 ─────────────────────────
//
// 这个工具最重要的一类测试是**否定性的**：证明某件事**没有发生**。
// dry-run 下一次删除都不该发出、`worktree remove` 失败后不该有任何 fallback。
// 断言"没发生"只能靠记录所有调用再检查记录，普通的 mock 做不到。

use std::sync::{Arc, Mutex};

/// 记录全部 git 调用；可配置每条命令的返回。
pub struct RecordingGit {
    /// 用 Arc 而非裸 Mutex：`Env` 持有 `Box<dyn GitRunner>`，装进去就拿不回来了，
    /// 而否定性断言恰恰要在 apply 之后检查记录。日志句柄必须能先克隆一份留在测试手里。
    pub calls: Arc<Mutex<Vec<Vec<String>>>>,
    /// 命中该子串的命令返回的退出码。默认 0。
    pub fail_matching: Option<(String, i32)>,
    pub stdout: Vec<u8>,
}

impl RecordingGit {
    pub fn new() -> Self {
        Self { calls: Arc::new(Mutex::new(Vec::new())), fail_matching: None, stdout: Vec::new() }
    }

    pub fn failing(substr: &str, code: i32) -> Self {
        Self { fail_matching: Some((substr.to_string(), code)), ..Self::new() }
    }

    /// 克隆一份日志句柄，供 apply 之后检查。
    pub fn log(&self) -> Arc<Mutex<Vec<Vec<String>>>> {
        Arc::clone(&self.calls)
    }

    pub fn recorded(&self) -> Vec<String> {
        match self.calls.lock() {
            Ok(g) => g.iter().map(|c| c.join(" ")).collect(),
            Err(p) => p.into_inner().iter().map(|c| c.join(" ")).collect(),
        }
    }
}

impl Default for RecordingGit {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::git::GitRunner for RecordingGit {
    fn exec(&self, _cwd: &Path, args: &[&str]) -> Result<crate::git::GitExec, crate::model::Cause> {
        let joined = args.join(" ");
        if let Ok(mut g) = self.calls.lock() {
            g.push(args.iter().map(|s| s.to_string()).collect());
        }
        let code = match &self.fail_matching {
            Some((sub, c)) if joined.contains(sub.as_str()) => Some(*c),
            _ => Some(0),
        };
        Ok(crate::git::GitExec { code, stdout: self.stdout.clone(), stderr: String::new() })
    }
}

/// 记录全部文件系统写操作，但**什么都不做**。
pub struct SpyFs {
    pub removed: Mutex<Vec<PathBuf>>,
    pub fail: bool,
}

impl SpyFs {
    pub fn new() -> Self {
        Self { removed: Mutex::new(Vec::new()), fail: false }
    }

    pub fn removals(&self) -> Vec<PathBuf> {
        match self.removed.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }
}

impl Default for SpyFs {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::fsops::FsOps for SpyFs {
    fn remove_dir_all(&self, path: &Path) -> Result<(), crate::model::Cause> {
        if let Ok(mut g) = self.removed.lock() {
            g.push(path.to_path_buf());
        }
        if self.fail {
            Err(crate::model::Cause::Io { path: path.to_path_buf(), msg: "spy 故意失败".into() })
        } else {
            Ok(())
        }
    }
    fn create_dir_all(&self, _p: &Path) -> Result<(), crate::model::Cause> {
        Ok(())
    }
    fn copy_file(&self, _a: &Path, _b: &Path) -> Result<(), crate::model::Cause> {
        Ok(())
    }
    fn exists(&self, p: &Path) -> bool {
        std::fs::symlink_metadata(p).is_ok()
    }
}

/// 想报告什么就报告什么的进程表。
pub struct FakeProcs(pub Vec<crate::gates::ProcInfo>);

impl crate::gates::ProcessTable for FakeProcs {
    fn processes_under(
        &self,
        _dir: &Path,
    ) -> Result<Vec<crate::gates::ProcInfo>, crate::model::Cause> {
        Ok(self.0.clone())
    }
}
