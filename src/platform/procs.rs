//! 进程表实现。
//!
//! 这是 A 组唯一真正承重的门禁，也是原型里唯一被实测证伪的那道。
//!
//! 原型用 `pgrep -f <worktree路径>`，只匹配命令行。而 `cargo build` / `pnpm dev`
//! 的 argv 里**根本不含 worktree 路径**——它们只是 cwd 在里面。实测这类进程
//! 100% 假阴性，会把正在构建的 worktree 判成空闲，然后抽走它的 target。

use crate::gates::{ProcInfo, ProcessTable};
use crate::model::Cause;
use std::path::Path;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub struct SysinfoProcs;

impl ProcessTable for SysinfoProcs {
    fn processes_under(&self, dir: &Path) -> Result<Vec<ProcInfo>, Cause> {
        // Windows 上拿不到进程 cwd，这道门无法成立 —— 明说不知道，而不是返回空集。
        if cfg!(windows) {
            return Err(Cause::Unsupported {
                what: "进程工作目录（判断 worktree 是否正被占用）",
                platform: "windows",
            });
        }

        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cwd(UpdateKind::Always)
                .with_exe(UpdateKind::Always)
                .with_cmd(UpdateKind::Always),
        );

        let me = std::process::id();
        let mut hits = Vec::new();
        let mut cwd_readable = 0usize;

        for (pid, proc_) in sys.processes() {
            let pid_u32 = pid.as_u32();
            if pid_u32 == me {
                continue; // 别把自己算成占用
            }

            // 判据一：工作目录落在 dir 之下（抓 cargo build / pnpm dev 这类）
            let by_cwd = match proc_.cwd() {
                Some(cwd) => {
                    cwd_readable += 1;
                    cwd.starts_with(dir)
                }
                None => false,
            };

            // 判据二：可执行文件或命令行里出现该路径（抓 `git -C <path>` 这类）
            let by_cmd = proc_.exe().is_some_and(|e| e.starts_with(dir))
                || proc_.cmd().iter().any(|a| Path::new(a).starts_with(dir));

            if by_cwd || by_cmd {
                hits.push(ProcInfo {
                    pid: pid_u32,
                    name: proc_.name().to_string_lossy().into_owned(),
                });
            }
        }

        // 一个 cwd 都读不到，说明这台机器上这条判据根本不成立（权限、沙箱、平台限制）。
        // 此时「没找到占用进程」不是证据，必须报不知道。
        if cwd_readable == 0 {
            return Err(Cause::Unsupported {
                what: "进程工作目录（全部进程的 cwd 均不可读）",
                platform: std::env::consts::OS,
            });
        }

        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use std::process::{Command, Stdio};

    /// 复刻 D11：进程的 cwd 在目录内，但命令行完全不含该路径。
    /// 原型的 pgrep -f 对这种 100% 漏检。
    #[test]
    fn detects_process_by_cwd_when_argv_has_no_path() {
        let dir = tempfile::tempdir().expect("建临时目录");
        let path = dir.path().canonicalize().expect("canonicalize");

        let mut child = Command::new("sleep")
            .arg("30")
            .current_dir(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("起 sleep");

        std::thread::sleep(std::time::Duration::from_millis(300));
        let found = SysinfoProcs.processes_under(&path);
        let _ = child.kill();
        let _ = child.wait(); // 回收，别留僵尸进程

        let found = found.expect("本平台应能读到 cwd");
        assert!(
            found.iter().any(|p| p.name.contains("sleep")),
            "cwd 在目录内的进程必须被检出，实际: {found:?}"
        );
    }

    #[test]
    fn empty_dir_has_no_processes() {
        let dir = tempfile::tempdir().expect("建临时目录");
        let path = dir.path().canonicalize().expect("canonicalize");
        let found = SysinfoProcs.processes_under(&path).expect("应能判断");
        assert!(found.is_empty(), "空目录不该有占用进程，实际: {found:?}");
    }
}
