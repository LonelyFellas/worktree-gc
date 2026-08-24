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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use wtgc::config::ScanConfig;
use wtgc::gates::SystemClock;
use wtgc::model::ScanReport;
use wtgc::apply::{ApplyOptions, Outcome, apply};
use wtgc::fsops::RealFs;
use wtgc::plan::{Plan, Selection, plan};
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
    if cfg.repos.is_empty() {
        cfg.seeds.extend(discover::default_seeds());
    }

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

/// 仓库清单的位置。**与 CLI 的每日体检读同一份文件**——
/// 两个入口各存一份配置，迟早会出现「GUI 里有、定时任务里没有」的沉默偏差。
fn repos_file() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".claude/skills/worktree-gc/repos.txt"))
}

fn read_repos() -> Vec<String> {
    repos_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn write_repos(list: &[String]) -> Result<(), String> {
    let path = repos_file().ok_or("找不到 HOME，无法定位配置文件")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败: {e}"))?;
    }
    // 保留说明性注释：这个文件用户也会手动编辑，写回时把来龙去脉留下
    let body = format!(
        "# wtgc 要扫的仓库，一行一个。# 开头是注释。\n         # GUI 和每日体检读的是同一份清单，改哪边都一样。\n         # 只需列「不在已知 agent 落点下」的仓库——~/.codex/worktrees 之类会被自动发现。\n{}\n",
        list.join("\n")
    );
    std::fs::write(&path, body).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

#[tauri::command]
fn default_repos() -> Vec<String> {
    read_repos()
}

/// 加一个仓库。**先验证再写入**——把一个非 git 目录静默加进去，
/// 结果是每次扫描都多一条「未识别主干」的噪音，而用户不知道为什么。
#[tauri::command]
fn add_repo(path: String) -> Result<Vec<String>, String> {
    let p = PathBuf::from(&path);
    let canonical = p
        .canonicalize()
        .map_err(|e| format!("路径不可用: {e}"))?
        .to_string_lossy()
        .into_owned();

    let git_exe = git::exe::resolve("git").ok_or("找不到 git")?;
    let runner = git::RealGit::new(git_exe, Duration::from_secs(10));

    // 用 --show-toplevel 而不是判断 .git 是否存在：
    // 用户很可能选中的是仓库里的某个子目录，这条能直接把它归位到仓库根。
    let out = {
        use wtgc::git::GitRunner;
        runner
            .exec(&PathBuf::from(&canonical), &["rev-parse", "--show-toplevel"])
            .map_err(|e| format!("{e:?}"))?
    };
    if out.code != Some(0) {
        return Err(format!("{canonical} 不是 git 仓库"));
    }
    let root = out.stdout_utf8().trim().to_string();
    if root.is_empty() {
        return Err("无法确定仓库根目录".into());
    }

    let mut list = read_repos();
    if list.iter().any(|r| r == &root) {
        return Err(format!("{root} 已经在清单里了"));
    }
    list.push(root);
    list.sort();
    write_repos(&list)?;
    Ok(list)
}

#[tauri::command]
fn remove_repo(path: String) -> Result<Vec<String>, String> {
    let mut list = read_repos();
    list.retain(|r| r != &path);
    write_repos(&list)?;
    Ok(list)
}

/// 已创建但尚未执行的计划。
///
/// **前端只拿到 id，永远拿不到也传不回路径。** 这不是 UI 礼貌，是两个硬保证：
/// 一个被攻陷或有 bug 的 WebView 无法让后端去删任意目录；
/// 以及计划里冻结的指纹能在执行前复检，挡住扫描到点击之间的状态漂移。
#[derive(Default)]
struct PlanStore(Mutex<HashMap<String, Plan>>);

#[derive(serde::Serialize)]
struct PlanSummary {
    id: String,
    /// 只回传展示所需的最小信息，不含可用于构造删除请求的原始路径。
    items: Vec<PlanItem>,
    estimated_bytes: u64,
    rejected: Vec<String>,
}

#[derive(serde::Serialize)]
struct PlanItem {
    label: String,
    bytes: u64,
}

#[derive(serde::Serialize)]
struct ApplySummary {
    done: usize,
    stale: usize,
    failed: usize,
    /// 执行前后实测的可用空间差。**不报 du 的估算**——
    /// APFS 写时复制会让估算系统性偏高，数字对不上就会丢掉信任。
    measured_freed: i64,
    lines: Vec<String>,
}

/// 从一次扫描结果构造计划。`kind` 为 "reclaim" 或 "remove"。
#[tauri::command]
async fn create_plan(
    store: tauri::State<'_, PlanStore>,
    repos: Vec<String>,
    kind: String,
    include_main: bool,
) -> Result<PlanSummary, String> {
    let report = tauri::async_runtime::spawn_blocking(move || run_scan(repos, false))
        .await
        .map_err(|e| format!("扫描任务异常终止: {e}"))??;

    let all = Selection::everything_allowed(&report, include_main);
    let sel = Selection {
        reclaim: if kind == "reclaim" { all.reclaim } else { Default::default() },
        remove: if kind == "remove" { all.remove } else { Default::default() },
        prune: kind == "remove",
    };
    let p = plan(&report, &sel);

    // id 由内容派生而非随机：同一份计划重复创建不会在 store 里堆积
    let id = format!("{kind}-{}-{}", p.actions.len(), p.estimated_bytes());
    let summary = PlanSummary {
        id: id.clone(),
        items: p
            .actions
            .iter()
            .map(|a| PlanItem {
                label: shorten(a.target()),
                bytes: a.estimated_bytes(),
            })
            .collect(),
        estimated_bytes: p.estimated_bytes(),
        rejected: p.rejected.iter().map(|(path, why)| format!("{}：{why}", shorten(path))).collect(),
    };

    if let Ok(mut g) = store.0.lock() {
        g.insert(id, p);
    }
    Ok(summary)
}

/// 执行一个已创建的计划。计划**单次使用**，执行后即从 store 移除——
/// 防止一次确认被重放成多次删除。
#[tauri::command]
async fn apply_plan(
    store: tauri::State<'_, PlanStore>,
    id: String,
) -> Result<ApplySummary, String> {
    let p = {
        let mut g = store.0.lock().map_err(|_| "计划存储被污染".to_string())?;
        g.remove(&id).ok_or("计划已失效，请重新扫描")?
    };

    tauri::async_runtime::spawn_blocking(move || {
        let git_exe = git::exe::resolve("git").ok_or("找不到 git")?;
        let env = Env {
            git: Box::new(git::RealGit::new(git_exe, Duration::from_secs(30))),
            forge: Box::new(forge::Offline),
            clock: Box::new(SystemClock),
            procs: Box::new(platform::procs::SysinfoProcs),
        };
        let cfg = ScanConfig::default();
        let out = apply(
            &p,
            &ApplyOptions { dry_run: false, audit_log: None },
            &cfg,
            &env,
            &RealFs,
        );

        let mut lines = Vec::new();
        let mut failed = 0;
        for r in &out.results {
            let name = shorten(r.action.target());
            match &r.outcome {
                Outcome::Done { .. } => lines.push(format!("✅ {name}")),
                Outcome::Stale { what } => lines.push(format!("⏭ {name}：{what}")),
                Outcome::Failed(c) => {
                    failed += 1;
                    lines.push(format!("⚠️ {name}：{c:?}"));
                }
                Outcome::Simulated => {}
            }
        }
        Ok(ApplySummary {
            done: out.done_count(),
            stale: out.stale_count(),
            failed,
            measured_freed: out.measured_freed,
            lines,
        })
    })
    .await
    .map_err(|e| format!("执行任务异常终止: {e}"))?
}

/// 末两段路径。多个 agent worktree 常同名，只显示 basename 分不清。
fn shorten(p: &std::path::Path) -> String {
    let parts: Vec<_> = p.components().rev().take(3).collect();
    parts
        .into_iter()
        .rev()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(PlanStore::default())
        .invoke_handler(tauri::generate_handler![
            scan_repos,
            default_repos,
            add_repo,
            remove_repo,
            create_plan,
            apply_plan
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
