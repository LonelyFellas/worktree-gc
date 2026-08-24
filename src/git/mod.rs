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
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use wait_timeout::ChildExt;

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
            .env("GIT_OPTIONAL_LOCKS", "0") // 只读操作不去抢索引锁
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| Cause::Io {
            path: cwd.to_path_buf(),
            msg: e.to_string(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| Cause::Io {
            path: cwd.to_path_buf(),
            msg: "无法捕获 git stdout".into(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| Cause::Io {
            path: cwd.to_path_buf(),
            msg: "无法捕获 git stderr".into(),
        })?;

        // 输出必须并行排空；先 wait 再读会在子进程写满 pipe 时互相等待。
        let stdout_reader = thread::spawn(move || {
            let mut reader = stdout;
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).map(|_| bytes)
        });
        let stderr_reader = thread::spawn(move || {
            let mut reader = stderr;
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).map(|_| bytes)
        });

        let cmd_text = format!("git {}", args.join(" "));
        let status = match child.wait_timeout(self.timeout) {
            Ok(Some(status)) => status,
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(Cause::Timeout {
                    cmd: cmd_text,
                    secs: self.timeout.as_secs().max(1),
                });
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(Cause::Io {
                    path: cwd.to_path_buf(),
                    msg: e.to_string(),
                });
            }
        };

        let stdout = join_output(stdout_reader, cwd, "stdout")?;
        let stderr = join_output(stderr_reader, cwd, "stderr")?;
        Ok(GitExec {
            code: status.code(),
            stdout,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

fn join_output(
    handle: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    cwd: &Path,
    stream: &str,
) -> Result<Vec<u8>, Cause> {
    match handle.join() {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(e)) => Err(Cause::Io {
            path: cwd.to_path_buf(),
            msg: e.to_string(),
        }),
        Err(_) => Err(Cause::Io {
            path: cwd.to_path_buf(),
            msg: format!("读取 git {stream} 的线程异常终止"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{GitRunner, RealGit};
    use crate::model::Cause;
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    #[test]
    fn command_timeout_is_enforced() {
        let git = RealGit::new("/bin/sh".into(), Duration::from_millis(100));
        let started = Instant::now();
        let result = git.exec(std::path::Path::new("/"), &["-c", "sleep 5"]);

        assert!(
            matches!(result, Err(Cause::Timeout { .. })),
            "实际结果：{result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "超时配置没有生效"
        );
    }
}
