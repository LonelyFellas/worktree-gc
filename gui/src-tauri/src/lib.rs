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

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use wtgc::apply::{apply, ApplyOptions, Outcome};
use wtgc::config::ScanConfig;
use wtgc::fsops::RealFs;
use wtgc::gates::SystemClock;
use wtgc::model::ScanReport;
use wtgc::plan::{force_remove, plan, Plan, Selection};
use wtgc::scan::{scan, Env};
use wtgc::shared_cache::{self, RepoCacheProfile, RepoCacheSettings, SharedCacheError};
use wtgc::{discover, forge, git, platform};

mod daily;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum UiLanguage {
    Zh,
    En,
}

impl UiLanguage {
    fn code(self) -> &'static str {
        match self {
            Self::Zh => "zh",
            Self::En => "en",
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct LocalizedError {
    zh: String,
    en: String,
}

impl LocalizedError {
    fn new(zh: impl Into<String>, en: impl Into<String>) -> Self {
        Self {
            zh: zh.into(),
            en: en.into(),
        }
    }

    fn text(&self, language: UiLanguage) -> &str {
        match language {
            UiLanguage::Zh => &self.zh,
            UiLanguage::En => &self.en,
        }
    }
}

/// 扫描给定仓库。**只读**，不做任何破坏性动作。
#[tauri::command]
async fn scan_repos(
    store: tauri::State<'_, ScanStore>,
    repos: Vec<String>,
    offline: bool,
) -> Result<ScanEnvelope, LocalizedError> {
    let report = tauri::async_runtime::spawn_blocking(move || run_scan(repos, offline))
        .await
        .map_err(|e| {
            LocalizedError::new(
                format!("扫描任务异常终止：{e}"),
                format!("The scan task terminated unexpectedly: {e}"),
            )
        })??;
    let (scan_id, remove_targets) = store.insert(report.clone(), Instant::now())?;
    Ok(ScanEnvelope {
        scan_id,
        report,
        remove_targets,
    })
}

fn run_scan(repos: Vec<String>, offline: bool) -> Result<ScanReport, LocalizedError> {
    let git_exe = git::exe::resolve("git").ok_or_else(|| {
        LocalizedError::new(
            "找不到 git。它是硬依赖，请先安装。",
            "Git was not found. Install Git before scanning.",
        )
    })?;

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

fn write_repos(list: &[String]) -> Result<(), LocalizedError> {
    let path = repos_file().ok_or_else(|| {
        LocalizedError::new(
            "找不到 HOME，无法定位配置文件",
            "HOME is not available, so the repository configuration file could not be located.",
        )
    })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| {
            LocalizedError::new(
                format!("创建配置目录失败：{e}"),
                format!("Could not create the configuration directory: {e}"),
            )
        })?;
    }
    // 保留说明性注释：这个文件用户也会手动编辑，写回时把来龙去脉留下
    std::fs::write(&path, render_repos(list)).map_err(|e| {
        LocalizedError::new(
            format!("写入 {} 失败：{e}", path.display()),
            format!("Could not write {}: {e}", path.display()),
        )
    })
}

fn render_repos(list: &[String]) -> String {
    format!(
        "# wtgc 要扫的仓库，一行一个。# 开头是注释。\n# GUI 和每日体检读的是同一份清单，改哪边都一样。\n# 只需列「不在已知 agent 落点下」的仓库——~/.codex/worktrees 之类会被自动发现。\n{}\n",
        list.join("\n")
    )
}

#[tauri::command]
fn default_repos() -> Vec<String> {
    read_repos()
}

/// 加一个仓库。**先验证再写入**——把一个非 git 目录静默加进去，
/// 结果是每次扫描都多一条「未识别主干」的噪音，而用户不知道为什么。
#[tauri::command]
fn add_repo(path: String) -> Result<Vec<String>, LocalizedError> {
    let p = PathBuf::from(&path);
    let canonical = p
        .canonicalize()
        .map_err(|e| {
            LocalizedError::new(
                format!("路径不可用：{e}"),
                format!("The selected path is unavailable: {e}"),
            )
        })?
        .to_string_lossy()
        .into_owned();

    let git_exe = git::exe::resolve("git")
        .ok_or_else(|| LocalizedError::new("找不到 git", "Git was not found."))?;
    let runner = git::RealGit::new(git_exe, Duration::from_secs(10));

    // 用 --show-toplevel 而不是判断 .git 是否存在：
    // 用户很可能选中的是仓库里的某个子目录，这条能直接把它归位到仓库根。
    let out = {
        use wtgc::git::GitRunner;
        runner
            .exec(
                &PathBuf::from(&canonical),
                &["rev-parse", "--show-toplevel"],
            )
            .map_err(|e| {
                LocalizedError::new(
                    format!("Git 仓库检查失败：{e:?}"),
                    format!("The Git repository check failed: {e:?}"),
                )
            })?
    };
    if out.code != Some(0) {
        return Err(LocalizedError::new(
            format!("{canonical} 不是 Git 仓库"),
            format!("{canonical} is not a Git repository."),
        ));
    }
    let root = out.stdout_utf8().trim().to_string();
    if root.is_empty() {
        return Err(LocalizedError::new(
            "无法确定仓库根目录",
            "The repository root could not be determined.",
        ));
    }

    let mut list = read_repos();
    if list.iter().any(|r| r == &root) {
        return Err(LocalizedError::new(
            format!("{root} 已经在清单里了"),
            format!("{root} is already monitored."),
        ));
    }
    list.push(root);
    write_repos(&list)?;
    Ok(list)
}

#[tauri::command]
fn remove_repo(path: String) -> Result<Vec<String>, LocalizedError> {
    let mut list = read_repos();
    list.retain(|r| r != &path);
    write_repos(&list)?;
    Ok(list)
}

/// 只接受现有仓库的排列，避免前端绕过 `add_repo` 的 git 仓库校验。
#[tauri::command]
fn reorder_repos(repos: Vec<String>) -> Result<Vec<String>, LocalizedError> {
    let current = read_repos();
    if !same_repo_items(&current, &repos) {
        return Err(LocalizedError::new(
            "排序列表与当前监控的仓库不一致，请刷新后重试",
            "The repository list changed while it was being reordered. Refresh and try again.",
        ));
    }
    write_repos(&repos)?;
    Ok(repos)
}

fn same_repo_items(left: &[String], right: &[String]) -> bool {
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort();
    right.sort();
    left == right
}

/// 读取每个监控仓库的共享缓存状态。外部工具探测可能起子进程，必须放到
/// `spawn_blocking`，否则 pnpm/uv 状态查询会卡住 Tauri async runtime。
#[tauri::command]
async fn shared_cache_profiles(repos: Vec<String>) -> Result<Vec<RepoCacheProfile>, LocalizedError> {
    if !same_repo_items(&read_repos(), &repos) {
        return Err(LocalizedError::new(
            "仓库列表已变化，请刷新后重试",
            "The repository list changed. Refresh and try again.",
        ));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let config = shared_cache::load().map_err(localize_shared_cache_error)?;
        repos
            .into_iter()
            .map(|repo| {
                let root = canonical_repo(&repo)?;
                let settings = config.settings_for(&root);
                Ok(shared_cache::inspect_repo(&root, settings))
            })
            .collect()
    })
    .await
    .map_err(|error| {
        LocalizedError::new(
            format!("共享缓存状态检查异常终止：{error}"),
            format!("The shared-cache check terminated unexpectedly: {error}"),
        )
    })?
}

/// 保存一个仓库的缓存设置。只写 worktree-gc 自己的配置文件；不会写入仓库、
/// `~/.cargo/config.toml` 或 Gradle/pnpm 全局配置。
#[tauri::command]
async fn save_shared_cache_settings(
    repo: String,
    settings: RepoCacheSettings,
) -> Result<RepoCacheProfile, LocalizedError> {
    if !read_repos().iter().any(|current| current == &repo) {
        return Err(LocalizedError::new(
            "该仓库不在当前监控清单里，请刷新后重试",
            "This repository is no longer monitored. Refresh and try again.",
        ));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let root = canonical_repo(&repo)?;
        settings
            .validate(&root)
            .map_err(localize_shared_cache_error)?;
        let mut config = shared_cache::load().map_err(localize_shared_cache_error)?;
        config.set(root.clone(), settings.clone());
        shared_cache::save(&config).map_err(localize_shared_cache_error)?;
        Ok(shared_cache::inspect_repo(&root, settings))
    })
    .await
    .map_err(|error| {
        LocalizedError::new(
            format!("共享缓存设置保存异常终止：{error}"),
            format!("Saving shared-cache settings terminated unexpectedly: {error}"),
        )
    })?
}

fn canonical_repo(repo: &str) -> Result<PathBuf, LocalizedError> {
    PathBuf::from(repo).canonicalize().map_err(|error| {
        LocalizedError::new(
            format!("仓库路径不可用：{error}"),
            format!("The repository path is unavailable: {error}"),
        )
    })
}

fn localize_shared_cache_error(error: SharedCacheError) -> LocalizedError {
    match error {
        SharedCacheError::ConfigPathUnavailable => LocalizedError::new(
            "无法确定共享缓存配置文件位置",
            "The shared-cache configuration path is unavailable.",
        ),
        SharedCacheError::InvalidCachePath { path, reason } => LocalizedError::new(
            format!("缓存路径 {} 无效：{reason}", path.display()),
            format!(
                "The cache path {} is invalid. Use an absolute path outside the repository.",
                path.display()
            ),
        ),
        SharedCacheError::UnsupportedVersion { version } => LocalizedError::new(
            format!("不支持共享缓存配置版本 {version}"),
            format!("Shared-cache configuration version {version} is not supported."),
        ),
        SharedCacheError::Read { path, source } => LocalizedError::new(
            format!("读取 {} 失败：{source}", path.display()),
            format!("Could not read {}: {source}", path.display()),
        ),
        SharedCacheError::Parse { path, source } => LocalizedError::new(
            format!("解析 {} 失败：{source}", path.display()),
            format!("Could not parse {}: {source}", path.display()),
        ),
        SharedCacheError::Write { path, source } => LocalizedError::new(
            format!("写入 {} 失败：{source}", path.display()),
            format!("Could not write {}: {source}", path.display()),
        ),
        SharedCacheError::EmptyCommand | SharedCacheError::MissingSccache => LocalizedError::new(
            error.to_string(),
            "The shared-cache command configuration is invalid.",
        ),
    }
}

/// GUI 最近一次扫描的短期快照。计划只能引用这里的报告，不能从 WebView 回传路径。
const SCAN_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_STORED_SCANS: usize = 8;

#[derive(Default)]
struct ScanStore {
    reports: Mutex<HashMap<String, StoredScan>>,
    next_id: AtomicU64,
}

struct StoredScan {
    created_at: Instant,
    report: ScanReport,
    remove_targets: Vec<RemoveTarget>,
}

#[derive(Serialize)]
struct ScanEnvelope {
    scan_id: String,
    report: ScanReport,
    remove_targets: Vec<RemoveTarget>,
}

#[derive(Clone, Debug, Serialize)]
struct RemoveTarget {
    id: String,
    path: PathBuf,
    confirmation: String,
}

impl ScanStore {
    fn insert(
        &self,
        report: ScanReport,
        now: Instant,
    ) -> Result<(String, Vec<RemoveTarget>), LocalizedError> {
        let mut reports = self.reports.lock().map_err(|_| {
            LocalizedError::new(
                "扫描结果存储不可用，请重新启动应用",
                "The scan result store is unavailable. Restart the app and try again.",
            )
        })?;
        Self::discard_expired(&mut reports, now);
        while reports.len() >= MAX_STORED_SCANS {
            let Some(oldest) = reports
                .iter()
                .min_by_key(|(_, stored)| stored.created_at)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            reports.remove(&oldest);
        }
        let id = format!("scan-{:016x}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let remove_targets = report
            .repos
            .iter()
            .enumerate()
            .flat_map(|(repo_index, repo)| {
                let scan_id = &id;
                repo.worktrees
                    .iter()
                    .enumerate()
                    .filter(|(_, wt)| !wt.is_main)
                    .map(move |(worktree_index, wt)| RemoveTarget {
                        id: format!("{scan_id}-remove-{repo_index}-{worktree_index}"),
                        path: wt.path.clone(),
                        confirmation: wt.branch.clone().unwrap_or_else(|| {
                            wt.path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "DETACHED".into())
                        }),
                    })
            })
            .collect::<Vec<_>>();
        reports.insert(
            id.clone(),
            StoredScan {
                created_at: now,
                report,
                remove_targets: remove_targets.clone(),
            },
        );
        Ok((id, remove_targets))
    }

    fn get(&self, id: &str, now: Instant) -> Result<ScanReport, LocalizedError> {
        let mut reports = self.reports.lock().map_err(|_| {
            LocalizedError::new(
                "扫描结果存储不可用，请重新启动应用",
                "The scan result store is unavailable. Restart the app and try again.",
            )
        })?;
        Self::discard_expired(&mut reports, now);
        reports
            .get(id)
            .map(|stored| stored.report.clone())
            .ok_or_else(|| {
                LocalizedError::new(
                    "扫描结果已失效，请重新扫描",
                    "This scan result has expired. Rescan and try again.",
                )
            })
    }

    fn removal_target(
        &self,
        scan_id: &str,
        target_id: &str,
        confirmation: &str,
        now: Instant,
    ) -> Result<PathBuf, LocalizedError> {
        let mut reports = self.reports.lock().map_err(|_| {
            LocalizedError::new(
                "扫描结果存储不可用，请重新启动应用",
                "The scan result store is unavailable. Restart the app and try again.",
            )
        })?;
        Self::discard_expired(&mut reports, now);
        let target = reports
            .get(scan_id)
            .and_then(|stored| {
                stored
                    .remove_targets
                    .iter()
                    .find(|target| target.id == target_id)
            })
            .ok_or_else(|| {
                LocalizedError::new(
                    "删除目标已失效，请重新扫描",
                    "This removal target has expired. Rescan and try again.",
                )
            })?;
        if target.confirmation != confirmation {
            return Err(LocalizedError::new(
                "分支名不匹配，未创建删除计划",
                "The branch name does not match, so no removal plan was created.",
            ));
        }
        Ok(target.path.clone())
    }

    fn discard_expired(reports: &mut HashMap<String, StoredScan>, now: Instant) {
        reports.retain(|_, stored| now.saturating_duration_since(stored.created_at) <= SCAN_TTL);
    }
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

#[derive(Serialize)]
struct ApplySummary {
    done: usize,
    stale: usize,
    failed: usize,
    /// 执行前后实测的可用空间差。**不报 du 的估算**——
    /// APFS 写时复制会让估算系统性偏高，数字对不上就会丢掉信任。
    measured_freed: i64,
    lines: Vec<LocalizedLine>,
}

#[derive(Serialize)]
struct LocalizedLine {
    zh: String,
    en: String,
}

/// 从一次扫描结果构造计划。`kind` 为 "reclaim" 或 "remove"。
#[tauri::command]
async fn create_plan(
    store: tauri::State<'_, PlanStore>,
    scans: tauri::State<'_, ScanStore>,
    scan_id: String,
    kind: String,
    include_main: bool,
    target_id: Option<String>,
    confirmation: Option<String>,
) -> Result<PlanSummary, LocalizedError> {
    if kind != "reclaim" && kind != "remove" {
        return Err(LocalizedError::new(
            "未知的计划类型",
            "Unknown reclaim plan type.",
        ));
    }
    let now = Instant::now();
    let report = scans.get(&scan_id, now)?;

    let p = if kind == "reclaim" {
        let all = Selection::everything_allowed(&report, include_main);
        let sel = Selection {
            reclaim: all.reclaim,
            ..Default::default()
        };
        plan(&report, &sel)
    } else {
        let target_id = target_id.as_deref().ok_or_else(|| {
            LocalizedError::new(
                "缺少单项删除目标",
                "A single worktree removal target is required.",
            )
        })?;
        let confirmation = confirmation.as_deref().ok_or_else(|| {
            LocalizedError::new(
                "请输入分支名确认删除",
                "Enter the branch name to confirm removal.",
            )
        })?;
        let target = scans.removal_target(&scan_id, target_id, confirmation, now)?;
        force_remove(&report, &target)
    };

    // 目标 id 绑定扫描快照，同一扫描的重复确认不会在 store 里堆积。
    let id = format!("{kind}-{scan_id}-{}", target_id.as_deref().unwrap_or("all"));
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
        rejected: p
            .rejected
            .iter()
            .map(|(path, why)| format!("{}：{why}", shorten(path)))
            .collect(),
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
) -> Result<ApplySummary, LocalizedError> {
    let p = {
        let mut g = store.0.lock().map_err(|_| {
            LocalizedError::new(
                "计划存储不可用，请重新启动应用",
                "The reclaim plan store is unavailable. Restart the app and try again.",
            )
        })?;
        g.remove(&id).ok_or_else(|| {
            LocalizedError::new(
                "计划已失效，请重新扫描",
                "This reclaim plan has expired. Rescan and try again.",
            )
        })?
    };

    tauri::async_runtime::spawn_blocking(move || -> Result<ApplySummary, LocalizedError> {
        let git_exe = git::exe::resolve("git")
            .ok_or_else(|| LocalizedError::new("找不到 git", "Git was not found."))?;
        let env = Env {
            git: Box::new(git::RealGit::new(git_exe, Duration::from_secs(30))),
            forge: Box::new(forge::Offline),
            clock: Box::new(SystemClock),
            procs: Box::new(platform::procs::SysinfoProcs),
        };
        let cfg = ScanConfig::default();
        let out = apply(
            &p,
            &ApplyOptions {
                dry_run: false,
                audit_log: None,
            },
            &cfg,
            &env,
            &RealFs,
        );

        let mut lines = Vec::new();
        let mut failed = 0;
        for r in &out.results {
            let name = shorten(r.action.target());
            match &r.outcome {
                Outcome::Done { .. } => lines.push(LocalizedLine {
                    zh: format!("✅ {name}"),
                    en: format!("✅ {name}"),
                }),
                Outcome::Stale { what } => lines.push(LocalizedLine {
                    zh: format!("⏭ {name}：{what}"),
                    en: format!("⏭ {name}: skipped because its safety state changed"),
                }),
                Outcome::Failed(c) => {
                    failed += 1;
                    lines.push(LocalizedLine {
                        zh: format!("⚠️ {name}：{c:?}"),
                        en: format!("⚠️ {name}: reclaim failed"),
                    });
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
    .map_err(|e| {
        LocalizedError::new(
            format!("执行任务异常终止：{e}"),
            format!("The reclaim task terminated unexpectedly: {e}"),
        )
    })?
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
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(PlanStore::default())
        .manage(ScanStore::default())
        .invoke_handler(tauri::generate_handler![
            scan_repos,
            default_repos,
            add_repo,
            remove_repo,
            reorder_repos,
            shared_cache_profiles,
            save_shared_cache_settings,
            daily::daily_check_status,
            daily::set_daily_check_enabled,
            daily::set_ui_language,
            create_plan,
            apply_plan
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

pub use daily::run_daily_check;

#[cfg(test)]
mod tests {
    use super::{render_repos, same_repo_items, LocalizedError, ScanStore, UiLanguage, SCAN_TTL};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};
    use wtgc::model::{Fingerprint, RepoReport, ScanReport, Verdict, WorktreeReport};

    fn empty_report(available_bytes: u64) -> ScanReport {
        ScanReport {
            repos: Vec::new(),
            available_bytes,
            tools: Vec::new(),
        }
    }

    fn removal_report() -> ScanReport {
        let worktree = |path: &str, branch: &str, is_main: bool| WorktreeReport {
            path: PathBuf::from(path),
            branch: Some(branch.into()),
            head_oid: "abc123".into(),
            is_main,
            bytes: 100,
            caches: Vec::new(),
            outcomes: Vec::new(),
            verdict: if is_main {
                Verdict::Protected {
                    why: "主工作区"
                }
            } else {
                Verdict::Blocked {
                    by: vec![wtgc::model::GateId::Dirty],
                }
            },
            fingerprint: Fingerprint {
                head_oid: "abc123".into(),
                dirty_count: usize::from(!is_main),
                busy_pids: Vec::new(),
                precious_digest: String::new(),
            },
        };
        ScanReport {
            repos: vec![RepoReport {
                root: PathBuf::from("/repo"),
                baseline: None,
                baseline_error: None,
                worktrees: vec![
                    worktree("/repo", "main", true),
                    worktree("/repo/topic", "topic", false),
                ],
                prunable: Vec::new(),
            }],
            available_bytes: 0,
            tools: Vec::new(),
        }
    }

    #[test]
    fn command_errors_expose_both_languages_to_the_webview() {
        let error = LocalizedError::new("中文错误", "English error");
        let json = serde_json::to_value(&error).expect("序列化错误");

        assert_eq!(json["zh"], "中文错误");
        assert_eq!(json["en"], "English error");
        assert_eq!(error.text(UiLanguage::En), "English error");
    }

    #[test]
    fn repo_order_may_change_but_items_must_not() {
        let current = vec!["/repo/a".into(), "/repo/b".into()];
        assert!(same_repo_items(
            &current,
            &["/repo/b".into(), "/repo/a".into()]
        ));
        assert!(!same_repo_items(
            &current,
            &["/repo/a".into(), "/repo/c".into()]
        ));
    }

    #[test]
    fn repo_file_comments_start_at_column_zero() {
        let rendered = render_repos(&["/repo/b".into(), "/repo/a".into()]);
        assert!(rendered.lines().take(3).all(|line| line.starts_with('#')));
        assert!(rendered.ends_with("/repo/b\n/repo/a\n"));
    }

    #[test]
    fn scan_snapshot_is_reusable_within_ttl() {
        let store = ScanStore::default();
        let now = Instant::now();
        let (id, _) = store.insert(empty_report(42), now).expect("保存扫描");

        let report = store.get(&id, now + SCAN_TTL).expect("TTL 边界内仍可使用");

        assert_eq!(report.available_bytes, 42);
    }

    #[test]
    fn expired_scan_snapshot_cannot_create_a_plan() {
        let store = ScanStore::default();
        let now = Instant::now();
        let (id, _) = store.insert(empty_report(42), now).expect("保存扫描");

        let error = store
            .get(&id, now + SCAN_TTL + Duration::from_millis(1))
            .expect_err("过期扫描必须拒绝");

        assert_eq!(error.text(UiLanguage::Zh), "扫描结果已失效，请重新扫描");
    }

    #[test]
    fn scan_snapshot_only_issues_removal_targets_for_linked_worktrees() {
        let store = ScanStore::default();
        let now = Instant::now();
        let (scan_id, targets) = store.insert(removal_report(), now).expect("保存扫描");

        assert_eq!(targets.len(), 1, "主工作区不得获得删除目标 id");
        assert_eq!(targets[0].path, PathBuf::from("/repo/topic"));
        assert_eq!(targets[0].confirmation, "topic");
        assert_eq!(
            store
                .removal_target(&scan_id, &targets[0].id, "topic", now)
                .expect("分支名匹配"),
            PathBuf::from("/repo/topic")
        );
    }

    #[test]
    fn removal_target_rejects_a_mismatched_branch_confirmation() {
        let store = ScanStore::default();
        let now = Instant::now();
        let (scan_id, targets) = store.insert(removal_report(), now).expect("保存扫描");

        let error = store
            .removal_target(&scan_id, &targets[0].id, "other", now)
            .expect_err("错误分支名不得创建删除计划");

        assert_eq!(error.text(UiLanguage::Zh), "分支名不匹配，未创建删除计划");
    }
}
