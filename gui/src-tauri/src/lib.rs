//! worktree-gc 的 Tauri 外壳。
//!
//! 这一层**只做搬运**：判定逻辑全在 `wtgc` core 里，GUI 与 CLI 共用同一套门禁。
//! 两个界面对「什么能安全删」给出不同答案是不可接受的。
//!
//! 两个必须守住的点：
//!
//! 1. **扫描必须 `spawn_blocking`。** 一次全量扫描要跑几十秒的 git 子进程和
//!    目录遍历，直接在 command 里同步跑会把 async runtime 整个卡死，
//!    表现是界面假死而不是报错。
//! 2. **不要创建 `permissions/` 目录。** Tauri v2 一旦发现 app 侧的 ACL manifest，
//!    就会把「本地来源的自定义命令免 ACL」这条豁免整体关掉，之后每个
//!    `#[tauri::command]` 都要显式授权，否则全部 `not allowed by ACL`。

use std::path::PathBuf;
use std::time::Duration;
use wtgc::config::ScanConfig;
use wtgc::gates::SystemClock;
use wtgc::model::ScanReport;
use wtgc::scan::{Env, scan};
use wtgc::{discover, forge, git, platform};

/// 扫描给定仓库。**只读**，不做任何破坏性动作。
#[tauri::command]
async fn scan_repos(repos: Vec<String>, offline: bool) -> Result<ScanReport, String> {
    tauri::async_runtime::spawn_blocking(move || run_scan(repos, offline))
        .await
        .map_err(|e| format!("扫描任务异常终止: {e}"))?
}

fn run_scan(repos: Vec<String>, offline: bool) -> Result<ScanReport, String> {
    let git_exe = git::exe::resolve("git")
        .ok_or_else(|| "找不到 git。它是硬依赖，请先安装。".to_string())?;

    let mut cfg = ScanConfig {
        repos: repos.into_iter().map(PathBuf::from).collect(),
        idle: Duration::from_secs(24 * 3600),
        cache_quiet: Duration::from_secs(600),
        ..ScanConfig::default()
    };
    cfg.seeds.extend(discover::default_seeds());

    // 打包后的 .app 从 Finder 启动时 PATH 里没有 homebrew，gh 会找不到。
    // wtgc 的 exe::resolve 会兜底探常见目录，但找不到时必须走离线判定而不是崩掉。
    let forge: Box<dyn wtgc::gates::MergeStatusProvider> = if offline {
        Box::new(forge::Offline)
    } else {
        match forge::github::GhCli::detect() {
            Some(gh) => Box::new(gh),
            None => Box::new(forge::Offline),
        }
    };

    let env = Env {
        git: Box::new(git::RealGit::new(git_exe, cfg.git_timeout)),
        forge,
        clock: Box::new(SystemClock),
        procs: Box::new(platform::procs::SysinfoProcs),
    };

    Ok(scan(&cfg, &env))
}

/// 默认要扫的仓库。与 CLI 的每日体检读同一份清单，避免两边各说各话。
#[tauri::command]
fn default_repos() -> Vec<String> {
    let list = std::env::var("HOME")
        .map(|h| PathBuf::from(h).join(".claude/skills/worktree-gc/repos.txt"))
        .ok();

    let from_file = list.and_then(|p| std::fs::read_to_string(p).ok()).map(|s| {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect::<Vec<_>>()
    });

    from_file.unwrap_or_default()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![scan_repos, default_repos])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
