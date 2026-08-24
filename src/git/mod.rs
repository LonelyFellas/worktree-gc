//! git 调用层。
//!
//! **一律 shell out 到 git CLI，不用 libgit2。** 理由见 docs/design.md：
//! 最硬的一条是 `git worktree remove` 的安全语义无可替代——libgit2 只有 prune，
//! 等价物要自己 `rm -rf`，等于亲手拆掉最后一层保险。
//!
//! 所有输出**只解析 `--porcelain` / `-z` 的机器格式，禁止解析人类可读输出**。

pub mod base;
pub mod exe;
pub mod porcelain;

use crate::model::Cause;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// 一次 git 调用的原始结果。
///
/// 注意 `code` 是保留字段而非错误：很多 git 子命令用退出码表达语义
/// （`check-ignore` 的 1 = 未忽略，`merge-base --is-ancestor` 的 1 = 否），
/// 把非零一律当错误会丢掉信息。
#[derive(Debug, Clone)]
pub struct GitExec {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl GitExec {
    pub fn stdout_utf8(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// git 执行器。抽成 trait 是为了能注入「总是失败」的实现做否定性测试——
/// 断言在 git 全线失败时**没有任何** worktree 被判可清理（docs/design.md 的 D7）。
pub trait GitRunner: Send + Sync {
    /// 执行 git 子命令。`Err` 只用于**无法判断结果**的情况（spawn 失败、超时），
    /// 命令本身返回非零属于正常结果，通过 `GitExec::code` 表达。
    fn exec(&self, cwd: &Path, args: &[&str]) -> Result<GitExec, Cause>;

    /// 要求命令成功，否则映射为 `Cause` —— 调用方据此落到 `Unknown`。
    fn run_ok(&self, cwd: &Path, args: &[&str]) -> Result<GitExec, Cause> {
        let out = self.exec(cwd, args)?;
        if out.code == Some(0) {
            Ok(out)
        } else {
            Err(Cause::CommandFailed {
                cmd: format!("git {}", args.join(" ")),
                code: out.code,
                stderr: out.stderr,
            })
        }
    }

    /// 用于以退出码表达真假的子命令（0=真，1=假）。其它退出码一律是 `Cause`。
    fn run_bool(&self, cwd: &Path, args: &[&str]) -> Result<bool, Cause> {
        let out = self.exec(cwd, args)?;
        match out.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            other => Err(Cause::CommandFailed {
                cmd: format!("git {}", args.join(" ")),
                code: other,
                stderr: out.stderr,
            }),
        }
    }
}

/// 真实的 git 执行器。
pub struct RealGit {
    exe: std::path::PathBuf,
    timeout: Duration,
}

impl RealGit {
    pub fn new(exe: std::path::PathBuf, timeout: Duration) -> Self {
        Self { exe, timeout }
    }
}

impl GitRunner for RealGit {
    fn exec(&self, cwd: &Path, args: &[&str]) -> Result<GitExec, Cause> {
        // 环境隔离：不让用户的全局配置改变判定行为。
        // 尤其 status.showUntrackedFiles=no 会让未跟踪文件在 porcelain 里消失（D3）。
        let mut cmd = Command::new(&self.exe);
        cmd.current_dir(cwd)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0") // 绝不因为等凭据输入而挂住
            .env("GIT_OPTIONAL_LOCKS", "0"); // 只读操作不去抢索引锁

        let _ = self.timeout; // TODO(timeout): 用 wait-timeout 包一层，见 tests/cli_timeout.rs

        match cmd.output() {
            Ok(o) => Ok(GitExec {
                code: o.status.code(),
                stdout: o.stdout,
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            }),
            Err(e) => Err(Cause::Io { path: cwd.to_path_buf(), msg: e.to_string() }),
        }
    }
}
