//! 可执行文件路径解析。
//!
//! **不能依赖继承来的 PATH。** 实测：`launchctl getenv PATH` 为空，
//! 而 `gh` 只装在 `/opt/homebrew/bin`、不在系统默认 PATH 里——
//! 从 launchd 或 GUI 启动时会直接找不到，squash-merge 判定静默降级。
//! 本机还同时存在 `/usr/bin/git 2.50.1` 与 `/opt/homebrew/bin/git 2.53.0`，
//! 用哪个必须写进报告。

use crate::model::ToolInfo;
use std::path::PathBuf;

/// 除继承的 PATH 外，额外探查的常见安装位置。
const EXTRA_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/opt/local/bin",
];

/// 找到某个外部程序的实际路径。先看 PATH，再兜底常见目录。
pub fn resolve(name: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(name);
            if is_executable(&p) {
                return Some(p);
            }
        }
    }
    for d in EXTRA_DIRS {
        let p = PathBuf::from(d).join(name);
        if is_executable(&p) {
            return Some(p);
        }
    }
    None
}

fn is_executable(p: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// 探测程序版本，用于写进报告。
pub fn probe(name: &'static str, version_arg: &str) -> ToolInfo {
    let path = resolve(name);
    let version = path.as_ref().and_then(|p| {
        std::process::Command::new(p)
            .arg(version_arg)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    });
    ToolInfo { name, path, version }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn finds_git_somewhere() {
        // git 是本工具的硬依赖，找不到说明环境有问题
        assert!(resolve("git").is_some(), "git 应当可解析");
    }

    #[test]
    fn reports_missing_tool_as_none() {
        assert!(resolve("definitely-not-a-real-binary-xyzzy").is_none());
    }

    #[test]
    fn probe_records_version() {
        let info = probe("git", "--version");
        assert_eq!(info.name, "git");
        assert!(info.version.is_some_and(|v| v.contains("git version")));
    }
}
