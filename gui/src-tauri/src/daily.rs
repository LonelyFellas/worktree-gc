use super::{LocalizedError, UiLanguage};
use serde::Serialize;
#[cfg(target_os = "macos")]
use std::ffi::OsString;
#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Command, Output, Stdio};
#[cfg(target_os = "macos")]
use wtgc::model::{ScanReport, Verdict};

#[cfg(target_os = "macos")]
const LABEL: &str = "com.darwish.worktree-gc";
const SCHEDULE: &str = "09:30";
#[cfg(target_os = "macos")]
const RECLAIMABLE_NOTIFY_BYTES: u64 = 1024 * 1024 * 1024;
#[cfg(target_os = "macos")]
const LOW_DISK_BYTES: u64 = 50 * 1024 * 1024 * 1024;
#[cfg(target_os = "macos")]
const LANGUAGE_FILE: &str = "language.txt";

#[derive(Serialize)]
pub struct DailyCheckStatus {
    supported: bool,
    enabled: bool,
    schedule: &'static str,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Default, PartialEq, Eq)]
struct DailyMetrics {
    repositories: usize,
    worktrees: usize,
    reclaimable_caches: usize,
    reclaimable_bytes: u64,
    removable_worktrees: usize,
    needs_attention: usize,
}

#[tauri::command]
pub(crate) fn daily_check_status() -> Result<DailyCheckStatus, LocalizedError> {
    #[cfg(target_os = "macos")]
    {
        let uid = current_uid()?;
        if service_is_loaded(&uid)? && !launch_agent_is_current()? {
            enable(&uid)?;
        }
        Ok(DailyCheckStatus {
            supported: true,
            enabled: service_is_loaded(&uid)?,
            schedule: SCHEDULE,
        })
    }

    #[cfg(not(target_os = "macos"))]
    Ok(DailyCheckStatus {
        supported: false,
        enabled: false,
        schedule: SCHEDULE,
    })
}

#[tauri::command]
pub(crate) fn set_daily_check_enabled(enabled: bool) -> Result<DailyCheckStatus, LocalizedError> {
    #[cfg(target_os = "macos")]
    {
        let uid = current_uid()?;
        if enabled {
            enable(&uid)?;
        } else {
            disable(&uid)?;
        }
        daily_check_status()
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = enabled;
        Err(LocalizedError::new(
            "每日自动体检目前仅支持 macOS",
            "Daily health checks are currently available on macOS only.",
        ))
    }
}

#[tauri::command]
pub(crate) fn set_ui_language(language: UiLanguage) -> Result<(), LocalizedError> {
    #[cfg(target_os = "macos")]
    {
        let home = home_dir()?;
        let data_dir = data_dir(&home);
        fs::create_dir_all(&data_dir).map_err(|e| {
            LocalizedError::new(
                format!("创建配置目录失败：{e}"),
                format!("Could not create the configuration directory: {e}"),
            )
        })?;
        fs::write(data_dir.join(LANGUAGE_FILE), language.code()).map_err(|e| {
            LocalizedError::new(
                format!("保存语言设置失败：{e}"),
                format!("Could not save the language preference: {e}"),
            )
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = language;
        Ok(())
    }
}

/// launchd 调用的无界面入口。只扫描、写报告并按阈值通知，不执行任何回收动作。
#[cfg(not(target_os = "macos"))]
pub fn run_daily_check() -> Result<(), String> {
    Err("Daily health checks are currently available on macOS only.".into())
}

/// launchd 调用的无界面入口。只扫描、写报告并按阈值通知，不执行任何回收动作。
#[cfg(target_os = "macos")]
pub fn run_daily_check() -> Result<(), String> {
    let fallback_language = system_language();
    let home = home_dir().map_err(|error| error.text(fallback_language).to_string())?;
    let data_dir = data_dir(&home);
    let language = preferred_language(&data_dir);
    fs::create_dir_all(&data_dir).map_err(|e| {
        selected_error(
            language,
            format!("创建报告目录失败：{e}"),
            format!("Could not create the report directory: {e}"),
        )
    })?;

    let report = super::run_scan(super::read_repos(), false)
        .map_err(|error| error.text(language).to_string())?;
    let metrics = daily_metrics(&report);
    let rendered = render_daily_report(&report, &metrics, language)
        .map_err(|error| error.text(language).to_string())?;
    let stamp = timestamp(language);
    let entry = format!("===== {stamp} =====\n{rendered}\n");

    fs::write(data_dir.join("last-report.txt"), &entry).map_err(|e| {
        selected_error(
            language,
            format!("写入 last-report.txt 失败：{e}"),
            format!("Could not write last-report.txt: {e}"),
        )
    })?;
    let json = wtgc::report::json::render(&report).map_err(|e| {
        selected_error(
            language,
            format!("生成 JSON 报告失败：{e}"),
            format!("Could not generate the JSON report: {e}"),
        )
    })?;
    fs::write(data_dir.join("last-report.json"), json).map_err(|e| {
        selected_error(
            language,
            format!("写入 last-report.json 失败：{e}"),
            format!("Could not write last-report.json: {e}"),
        )
    })?;
    append_history(&data_dir.join("history.log"), &entry)
        .map_err(|error| error.text(language).to_string())?;

    if should_notify(&metrics, report.available_bytes) {
        let available = wtgc::platform::disk::human_bytes(report.available_bytes);
        let (title, body) = notification_content(&metrics, &available, language);
        let script = format!(
            "display notification {} with title {}",
            apple_script_string(&body),
            apple_script_string(&title),
        );
        let status = Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .stdout(Stdio::null())
            .status()
            .map_err(|e| {
                selected_error(
                    language,
                    format!("无法发送每日体检通知：{e}"),
                    format!("Could not send the daily health-check notification: {e}"),
                )
            })?;
        if !status.success() {
            return Err(selected_error(
                language,
                "每日体检通知发送失败",
                "The daily health-check notification failed.",
            ));
        }
    }

    println!("{rendered}");
    Ok(())
}

#[cfg(target_os = "macos")]
fn daily_metrics(report: &ScanReport) -> DailyMetrics {
    let mut metrics = DailyMetrics {
        repositories: report.repos.len(),
        ..DailyMetrics::default()
    };
    for worktree in report.repos.iter().flat_map(|repo| &repo.worktrees) {
        metrics.worktrees += 1;
        if matches!(worktree.verdict, Verdict::Removable) {
            metrics.removable_worktrees += 1;
        }
        if matches!(worktree.verdict, Verdict::NeedsAttention { .. }) {
            metrics.needs_attention += 1;
        }
        if worktree.is_main {
            continue;
        }
        for cache in &worktree.caches {
            if cache
                .outcomes
                .iter()
                .all(|outcome| outcome.status.is_pass())
            {
                metrics.reclaimable_caches += 1;
                metrics.reclaimable_bytes = metrics.reclaimable_bytes.saturating_add(cache.bytes);
            }
        }
    }
    metrics
}

#[cfg(target_os = "macos")]
fn should_notify(metrics: &DailyMetrics, available_bytes: u64) -> bool {
    metrics.removable_worktrees > 0
        || metrics.reclaimable_bytes >= RECLAIMABLE_NOTIFY_BYTES
        || available_bytes < LOW_DISK_BYTES
}

#[cfg(target_os = "macos")]
fn notification_content(
    metrics: &DailyMetrics,
    available: &str,
    language: UiLanguage,
) -> (String, String) {
    let reclaimable = wtgc::platform::disk::human_bytes(metrics.reclaimable_bytes);
    match language {
        UiLanguage::Zh => (
            "worktree-gc 每日体检".into(),
            format!(
                "可删除 {} 个、可回收缓存 {}（{} 处），剩余 {available}",
                metrics.removable_worktrees, reclaimable, metrics.reclaimable_caches
            ),
        ),
        UiLanguage::En => (
            "worktree-gc daily check".into(),
            format!(
                "{} removable, {} reclaimable across {} caches, {available} available",
                metrics.removable_worktrees, reclaimable, metrics.reclaimable_caches
            ),
        ),
    }
}

#[cfg(target_os = "macos")]
fn render_daily_report(
    report: &ScanReport,
    metrics: &DailyMetrics,
    language: UiLanguage,
) -> Result<String, LocalizedError> {
    let reclaimable = wtgc::platform::disk::human_bytes(metrics.reclaimable_bytes);
    let available = wtgc::platform::disk::human_bytes(report.available_bytes);
    match language {
        UiLanguage::Zh => {
            let mut rendered = Vec::new();
            wtgc::report::human::render(report, &mut rendered).map_err(|e| {
                LocalizedError::new(
                    format!("生成人类可读报告失败：{e}"),
                    format!("Could not generate the human-readable report: {e}"),
                )
            })?;
            String::from_utf8(rendered).map_err(|e| {
                LocalizedError::new(
                    format!("报告不是有效 UTF-8：{e}"),
                    format!("The report is not valid UTF-8: {e}"),
                )
            })
        }
        UiLanguage::En => Ok(format!(
            "Scan complete: {} repositories, {} worktrees\nSafely reclaimable build cache: {reclaimable} across {} caches (main worktrees excluded)\nRemovable worktrees: {}\nNeeds attention: {}\nDisk space available: {available}",
            metrics.repositories,
            metrics.worktrees,
            metrics.reclaimable_caches,
            metrics.removable_worktrees,
            metrics.needs_attention,
        )),
    }
}

#[cfg(target_os = "macos")]
fn selected_error(language: UiLanguage, zh: impl Into<String>, en: impl Into<String>) -> String {
    LocalizedError::new(zh, en).text(language).to_string()
}

#[cfg(target_os = "macos")]
fn apple_script_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(target_os = "macos")]
fn enable(uid: &str) -> Result<(), LocalizedError> {
    let home = home_dir()?;
    let launch_agents = home.join("Library/LaunchAgents");
    let data_dir = data_dir(&home);
    fs::create_dir_all(&launch_agents).map_err(|e| {
        LocalizedError::new(
            format!("创建 LaunchAgents 目录失败：{e}"),
            format!("Could not create the LaunchAgents directory: {e}"),
        )
    })?;
    fs::create_dir_all(&data_dir).map_err(|e| {
        LocalizedError::new(
            format!("创建报告目录失败：{e}"),
            format!("Could not create the report directory: {e}"),
        )
    })?;

    let executable = std::env::current_exe().map_err(|e| {
        LocalizedError::new(
            format!("定位应用程序失败：{e}"),
            format!("Could not locate the application executable: {e}"),
        )
    })?;
    if executable_is_translocated(&executable) {
        return Err(LocalizedError::new(
            "请先把 worktree-gc 移到“应用程序”文件夹，再开启每日自动体检",
            "Move worktree-gc to the Applications folder before enabling daily health checks.",
        ));
    }
    let plist = launch_agents.join(format!("{LABEL}.plist"));
    let previous = fs::read(&plist).ok();
    let body = render_launch_agent(
        &executable,
        &data_dir.join("launchd.out.log"),
        &data_dir.join("launchd.err.log"),
    );
    let was_loaded = service_is_loaded(uid)?;
    if was_loaded && previous.as_deref() == Some(body.as_bytes()) {
        return Ok(());
    }
    let bootstrap_args = vec![
        OsString::from("bootstrap"),
        OsString::from(format!("gui/{uid}")),
        plist.as_os_str().to_owned(),
    ];
    if was_loaded {
        run_launchctl(&[
            OsString::from("bootout"),
            OsString::from(service_target(uid)),
        ])?;
    }
    if let Err(e) = fs::write(&plist, &body) {
        if was_loaded {
            if let Some(contents) = &previous {
                let _ = fs::write(&plist, contents);
                let _ = run_launchctl(&bootstrap_args);
            }
        }
        return Err(LocalizedError::new(
            format!("写入 {} 失败：{e}", plist.display()),
            format!("Could not write {}: {e}", plist.display()),
        ));
    }
    if let Err(error) = run_launchctl(&bootstrap_args) {
        if let Some(contents) = &previous {
            let _ = fs::write(&plist, contents);
            if was_loaded {
                let _ = run_launchctl(&bootstrap_args);
            }
        } else {
            let _ = fs::remove_file(&plist);
        }
        return Err(error);
    }

    if !service_is_loaded(uid)? {
        return Err(LocalizedError::new(
            "launchd 没有加载每日体检任务",
            "launchd did not load the daily health-check task.",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn disable(uid: &str) -> Result<(), LocalizedError> {
    if service_is_loaded(uid)? {
        let args = vec![
            OsString::from("bootout"),
            OsString::from(service_target(uid)),
        ];
        run_launchctl(&args)?;
    }

    let plist = home_dir()?.join(format!("Library/LaunchAgents/{LABEL}.plist"));
    match fs::remove_file(&plist) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(LocalizedError::new(
            format!("删除 {} 失败：{e}", plist.display()),
            format!("Could not remove {}: {e}", plist.display()),
        )),
    }
}

#[cfg(target_os = "macos")]
fn home_dir() -> Result<PathBuf, LocalizedError> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        LocalizedError::new(
            "找不到 HOME，无法定位用户配置目录",
            "HOME is not available, so the user configuration directory could not be located.",
        )
    })
}

#[cfg(target_os = "macos")]
fn data_dir(home: &Path) -> PathBuf {
    home.join(".claude/skills/worktree-gc")
}

#[cfg(target_os = "macos")]
fn preferred_language(data_dir: &Path) -> UiLanguage {
    match fs::read_to_string(data_dir.join(LANGUAGE_FILE)) {
        Ok(language) if language.trim() == "zh" => UiLanguage::Zh,
        Ok(language) if language.trim() == "en" => UiLanguage::En,
        _ => system_language(),
    }
}

#[cfg(target_os = "macos")]
fn system_language() -> UiLanguage {
    if let Ok(output) = Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
    {
        if output.status.success() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let tag = line
                    .trim()
                    .trim_matches(|c| matches!(c, '(' | ')' | '"' | ','));
                if !tag.is_empty() {
                    return language_from_tag(tag);
                }
            }
        }
    }
    std::env::var("LANG")
        .ok()
        .map_or(UiLanguage::En, |tag| language_from_tag(&tag))
}

#[cfg(target_os = "macos")]
fn language_from_tag(tag: &str) -> UiLanguage {
    if tag.to_ascii_lowercase().starts_with("zh") {
        UiLanguage::Zh
    } else {
        UiLanguage::En
    }
}

#[cfg(target_os = "macos")]
fn current_uid() -> Result<String, LocalizedError> {
    let output = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(|e| {
            LocalizedError::new(
                format!("获取当前用户 ID 失败：{e}"),
                format!("Could not determine the current user ID: {e}"),
            )
        })?;
    if !output.status.success() {
        return Err(LocalizedError::new(
            "获取当前用户 ID 失败",
            "Could not determine the current user ID.",
        ));
    }
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if uid.is_empty() {
        Err(LocalizedError::new(
            "当前用户 ID 为空",
            "The current user ID is empty.",
        ))
    } else {
        Ok(uid)
    }
}

#[cfg(target_os = "macos")]
fn service_target(uid: &str) -> String {
    format!("gui/{uid}/{LABEL}")
}

#[cfg(target_os = "macos")]
fn service_is_loaded(uid: &str) -> Result<bool, LocalizedError> {
    let args = vec![OsString::from("print"), OsString::from(service_target(uid))];
    let output = launchctl_output(&args)?;
    Ok(output.status.success())
}

#[cfg(target_os = "macos")]
fn run_launchctl(args: &[OsString]) -> Result<(), LocalizedError> {
    let output = launchctl_output(args)?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if detail.is_empty() {
        LocalizedError::new(
            format!("launchctl 执行失败：{:?}", output.status.code()),
            format!("launchctl failed with status {:?}.", output.status.code()),
        )
    } else {
        LocalizedError::new(
            format!("launchctl 执行失败：{detail}"),
            format!("launchctl failed: {detail}"),
        )
    })
}

#[cfg(target_os = "macos")]
fn launchctl_output(args: &[OsString]) -> Result<Output, LocalizedError> {
    Command::new("/bin/launchctl")
        .args(args)
        .output()
        .map_err(|e| {
            LocalizedError::new(
                format!("无法运行 launchctl：{e}"),
                format!("Could not run launchctl: {e}"),
            )
        })
}

#[cfg(target_os = "macos")]
fn launch_agent_is_current() -> Result<bool, LocalizedError> {
    let home = home_dir()?;
    let data_dir = data_dir(&home);
    let executable = std::env::current_exe().map_err(|e| {
        LocalizedError::new(
            format!("定位应用程序失败：{e}"),
            format!("Could not locate the application executable: {e}"),
        )
    })?;
    let expected = render_launch_agent(
        &executable,
        &data_dir.join("launchd.out.log"),
        &data_dir.join("launchd.err.log"),
    );
    let plist = home.join(format!("Library/LaunchAgents/{LABEL}.plist"));
    Ok(fs::read(plist).is_ok_and(|contents| launch_agent_contents_match(&contents, &expected)))
}

#[cfg(target_os = "macos")]
fn launch_agent_contents_match(contents: &[u8], expected: &str) -> bool {
    contents == expected.as_bytes()
}

#[cfg(target_os = "macos")]
fn executable_is_translocated(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "AppTranslocation")
}

#[cfg(target_os = "macos")]
fn render_launch_agent(executable: &Path, stdout: &Path, stderr: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
    <string>--daily-check</string>
  </array>
  <key>RunAtLoad</key>
  <false/>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key>
    <integer>9</integer>
    <key>Minute</key>
    <integer>30</integer>
  </dict>
  <key>ProcessType</key>
  <string>Background</string>
  <key>StandardOutPath</key>
  <string>{stdout}</string>
  <key>StandardErrorPath</key>
  <string>{stderr}</string>
</dict>
</plist>
"#,
        label = LABEL,
        executable = xml_escape(&executable.to_string_lossy()),
        stdout = xml_escape(&stdout.to_string_lossy()),
        stderr = xml_escape(&stderr.to_string_lossy()),
    )
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "macos")]
fn timestamp(language: UiLanguage) -> String {
    match Command::new("/bin/date").arg("+%Y-%m-%d %H:%M").output() {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => match language {
            UiLanguage::Zh => "时间未知".into(),
            UiLanguage::En => "Unknown time".into(),
        },
    }
}

#[cfg(target_os = "macos")]
fn append_history(path: &Path, entry: &str) -> Result<(), LocalizedError> {
    let mut history = fs::read_to_string(path).unwrap_or_default();
    history.push_str(entry);
    let lines: Vec<&str> = history.lines().collect();
    if lines.len() > 2000 {
        history = lines[lines.len() - 2000..].join("\n");
        history.push('\n');
    }
    fs::write(path, history).map_err(|e| {
        LocalizedError::new(
            format!("写入 history.log 失败：{e}"),
            format!("Could not write history.log: {e}"),
        )
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use wtgc::model::{
        CacheDir, CacheKind, Fingerprint, GateId, GateOutcome, GateStatus, RepoReport,
        WorktreeReport,
    };

    fn report_with_caches(main_bytes: u64, secondary_bytes: u64) -> ScanReport {
        let worktree = |name: &str, is_main: bool, bytes: u64| WorktreeReport {
            path: PathBuf::from(format!("/repo/{name}")),
            branch: Some("main".into()),
            head_oid: "abc123".into(),
            is_main,
            bytes,
            caches: vec![CacheDir {
                path: PathBuf::from(format!("/repo/{name}/target")),
                kind: CacheKind {
                    name: "target".into(),
                    ecosystem: "rust".into(),
                },
                bytes,
                outcomes: vec![GateOutcome {
                    id: GateId::Busy,
                    status: GateStatus::Pass,
                }],
            }],
            outcomes: Vec::new(),
            verdict: if is_main {
                Verdict::Protected {
                    why: "main worktree",
                }
            } else {
                Verdict::CacheReclaimable
            },
            fingerprint: Fingerprint {
                head_oid: "abc123".into(),
                dirty_count: 0,
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
                    worktree("main", true, main_bytes),
                    worktree("secondary", false, secondary_bytes),
                ],
                prunable: Vec::new(),
            }],
            available_bytes: LOW_DISK_BYTES,
            tools: Vec::new(),
        }
    }

    #[test]
    fn launch_agent_runs_headless_at_fixed_time_and_escapes_paths() {
        let plist = render_launch_agent(
            Path::new("/Applications/Worktree & GC.app/Contents/MacOS/worktree-gc"),
            Path::new("/tmp/out.log"),
            Path::new("/tmp/err.log"),
        );

        assert!(plist.contains("<string>--daily-check</string>"));
        assert!(plist.contains("<integer>9</integer>"));
        assert!(plist.contains("<integer>30</integer>"));
        assert!(plist.contains("Worktree &amp; GC.app"));
        assert!(plist.contains("<false/>"));
    }

    #[test]
    fn launch_agent_path_change_requires_reload() {
        let old = render_launch_agent(
            Path::new("/Users/test/Downloads/worktree-gc.app/Contents/MacOS/worktree-gc"),
            Path::new("/tmp/out.log"),
            Path::new("/tmp/err.log"),
        );
        let current = render_launch_agent(
            Path::new("/Applications/worktree-gc.app/Contents/MacOS/worktree-gc"),
            Path::new("/tmp/out.log"),
            Path::new("/tmp/err.log"),
        );

        assert!(launch_agent_contents_match(current.as_bytes(), &current));
        assert!(!launch_agent_contents_match(old.as_bytes(), &current));
    }

    #[test]
    fn translocated_app_is_not_safe_for_a_persistent_launch_agent() {
        assert!(executable_is_translocated(Path::new(
            "/private/var/folders/x/AppTranslocation/id/d/worktree-gc.app/Contents/MacOS/worktree-gc"
        )));
        assert!(!executable_is_translocated(Path::new(
            "/Applications/worktree-gc.app/Contents/MacOS/worktree-gc"
        )));
    }

    #[test]
    fn notification_uses_non_main_reclaimable_bytes() {
        let report = report_with_caches(8 * RECLAIMABLE_NOTIFY_BYTES, RECLAIMABLE_NOTIFY_BYTES / 2);
        let metrics = daily_metrics(&report);

        assert_eq!(metrics.reclaimable_caches, 1);
        assert_eq!(metrics.reclaimable_bytes, RECLAIMABLE_NOTIFY_BYTES / 2);
        assert!(!should_notify(&metrics, LOW_DISK_BYTES));
    }

    #[test]
    fn notification_threshold_is_one_gibibyte() {
        let below = DailyMetrics {
            reclaimable_bytes: RECLAIMABLE_NOTIFY_BYTES - 1,
            ..DailyMetrics::default()
        };
        let at_threshold = DailyMetrics {
            reclaimable_bytes: RECLAIMABLE_NOTIFY_BYTES,
            ..DailyMetrics::default()
        };

        assert!(!should_notify(&below, LOW_DISK_BYTES));
        assert!(should_notify(&at_threshold, LOW_DISK_BYTES));
    }

    #[test]
    fn language_detection_only_treats_chinese_as_chinese() {
        assert_eq!(language_from_tag("zh-Hans-CN"), UiLanguage::Zh);
        assert_eq!(language_from_tag("en-CN"), UiLanguage::En);
        assert_eq!(language_from_tag("fr-FR"), UiLanguage::En);
    }

    #[test]
    fn saved_language_controls_report_and_notification_copy() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        fs::write(dir.path().join(LANGUAGE_FILE), "en").expect("写语言设置");
        let report = report_with_caches(0, RECLAIMABLE_NOTIFY_BYTES);
        let metrics = daily_metrics(&report);

        assert_eq!(preferred_language(dir.path()), UiLanguage::En);
        let rendered =
            render_daily_report(&report, &metrics, UiLanguage::En).expect("生成英文报告");
        let (title, body) = notification_content(&metrics, "80.0G", UiLanguage::En);
        assert!(rendered.starts_with("Scan complete:"));
        assert_eq!(title, "worktree-gc daily check");
        assert!(body.contains("1.0G reclaimable"));
    }

    #[test]
    fn history_keeps_only_the_latest_two_thousand_lines() {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let path = dir.path().join("history.log");
        let old = (0..2000)
            .map(|i| format!("old-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, format!("{old}\n")).expect("写初始历史");

        append_history(&path, "new\n").expect("追加历史");
        let lines = fs::read_to_string(path).expect("读历史");
        let lines: Vec<_> = lines.lines().collect();
        assert_eq!(lines.len(), 2000);
        assert_eq!(lines.first(), Some(&"old-1"));
        assert_eq!(lines.last(), Some(&"new"));
    }
}
