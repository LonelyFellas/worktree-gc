//! Worktree 之间的共享缓存配置。
//!
//! 这一模块只管理工具原生的外部缓存，不把 `target`、`node_modules`、`.dart_tool`
//! 等工作区状态搬出 worktree，也不把共享缓存交给现有回收计划。配置只有通过
//! `wtgc run -- <command>` 启动命令时才会注入，避免静默修改仓库或用户全局配置。

use crate::git::exe;
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const CONFIG_VERSION: u32 = 1;
const CONFIG_OVERRIDE_ENV: &str = "WTGC_SHARED_CACHE_CONFIG";

#[derive(Debug, thiserror::Error)]
pub enum SharedCacheError {
    #[error("无法确定共享缓存配置文件位置")]
    ConfigPathUnavailable,
    #[error("读取 {path} 失败：{source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("解析 {path} 失败：{source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("写入 {path} 失败：{source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("不支持共享缓存配置版本 {version}")]
    UnsupportedVersion { version: u32 },
    #[error("缓存路径 {path} 无效：{reason}")]
    InvalidCachePath { path: PathBuf, reason: String },
    #[error("wtgc run 后面没有命令")]
    EmptyCommand,
    #[error("已为仓库启用 sccache，但系统里找不到 sccache；请先安装或在设置里关闭")]
    MissingSccache,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedCacheConfig {
    #[serde(default = "config_version")]
    pub version: u32,
    #[serde(default)]
    pub repositories: Vec<RepositoryCacheConfig>,
}

impl Default for SharedCacheConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            repositories: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryCacheConfig {
    pub repo: PathBuf,
    #[serde(default)]
    pub settings: RepoCacheSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCacheSettings {
    #[serde(default)]
    pub sccache_enabled: bool,
    #[serde(default)]
    pub gradle_build_cache_enabled: bool,
    #[serde(default)]
    pub sccache_dir: Option<PathBuf>,
    #[serde(default)]
    pub pnpm_store_dir: Option<PathBuf>,
    #[serde(default)]
    pub pub_cache_dir: Option<PathBuf>,
    #[serde(default)]
    pub uv_cache_dir: Option<PathBuf>,
}

impl RepoCacheSettings {
    pub fn validate(&self, repo: &Path) -> Result<(), SharedCacheError> {
        for path in [
            self.sccache_dir.as_deref(),
            self.pnpm_store_dir.as_deref(),
            self.pub_cache_dir.as_deref(),
            self.uv_cache_dir.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_cache_path(path, repo)?;
        }
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.sccache_enabled
            || self.gradle_build_cache_enabled
            || self.sccache_dir.is_some()
            || self.pnpm_store_dir.is_some()
            || self.pub_cache_dir.is_some()
            || self.uv_cache_dir.is_some()
    }
}

impl SharedCacheConfig {
    pub fn settings_for(&self, repo: &Path) -> RepoCacheSettings {
        self.repositories
            .iter()
            .find(|entry| entry.repo == repo)
            .map(|entry| entry.settings.clone())
            .unwrap_or_default()
    }

    pub fn set(&mut self, repo: PathBuf, settings: RepoCacheSettings) {
        if settings == RepoCacheSettings::default() {
            self.repositories.retain(|entry| entry.repo != repo);
            return;
        }
        if let Some(entry) = self
            .repositories
            .iter_mut()
            .find(|entry| entry.repo == repo)
        {
            entry.settings = settings;
        } else {
            self.repositories
                .push(RepositoryCacheConfig { repo, settings });
            self.repositories.sort_by(|a, b| a.repo.cmp(&b.repo));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheAdapterKind {
    CargoSccache,
    GradleBuildCache,
    PnpmStore,
    PubCache,
    UvCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheAdapterState {
    Shared,
    Configured,
    Available,
    MissingTool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheAdapterStatus {
    pub kind: CacheAdapterKind,
    pub state: CacheAdapterState,
    pub path: Option<PathBuf>,
    pub tool: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoCacheProfile {
    pub repo: PathBuf,
    pub settings: RepoCacheSettings,
    pub adapters: Vec<CacheAdapterStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

pub fn config_file() -> Result<PathBuf, SharedCacheError> {
    if let Some(path) = non_empty_env(CONFIG_OVERRIDE_ENV) {
        return Ok(PathBuf::from(path));
    }

    #[cfg(windows)]
    if let Some(app_data) = non_empty_env("APPDATA") {
        return Ok(PathBuf::from(app_data)
            .join("worktree-gc")
            .join("shared-cache.json"));
    }

    #[cfg(not(windows))]
    if let Some(xdg) = non_empty_env("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg)
            .join("worktree-gc")
            .join("shared-cache.json"));
    }

    home_dir()
        .map(|home| {
            home.join(".config")
                .join("worktree-gc")
                .join("shared-cache.json")
        })
        .ok_or(SharedCacheError::ConfigPathUnavailable)
}

pub fn load() -> Result<SharedCacheConfig, SharedCacheError> {
    load_from(&config_file()?)
}

pub fn load_from(path: &Path) -> Result<SharedCacheConfig, SharedCacheError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SharedCacheConfig::default());
        }
        Err(source) => {
            return Err(SharedCacheError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let config: SharedCacheConfig =
        serde_json::from_str(&raw).map_err(|source| SharedCacheError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    if config.version != CONFIG_VERSION {
        return Err(SharedCacheError::UnsupportedVersion {
            version: config.version,
        });
    }
    Ok(config)
}

pub fn save(config: &SharedCacheConfig) -> Result<(), SharedCacheError> {
    save_to(&config_file()?, config)
}

pub fn save_to(path: &Path, config: &SharedCacheConfig) -> Result<(), SharedCacheError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| SharedCacheError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut raw =
        serde_json::to_string_pretty(config).map_err(|source| SharedCacheError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    raw.push('\n');
    fs::write(path, raw).map_err(|source| SharedCacheError::Write {
        path: path.to_path_buf(),
        source,
    })
}

pub fn inspect_repo(repo: &Path, settings: RepoCacheSettings) -> RepoCacheProfile {
    inspect_repo_with_tools(repo, settings, &DetectedTools::current())
}

pub fn prepare_run(
    command: &[OsString],
    settings: &RepoCacheSettings,
    sccache_exe: Option<&Path>,
    inherited_gradle_opts: Option<&OsStr>,
) -> Result<RunSpec, SharedCacheError> {
    let Some(program) = command.first() else {
        return Err(SharedCacheError::EmptyCommand);
    };
    let mut env = Vec::new();

    if settings.sccache_enabled {
        let Some(sccache) = sccache_exe else {
            return Err(SharedCacheError::MissingSccache);
        };
        env.push((OsString::from("RUSTC_WRAPPER"), sccache.as_os_str().into()));
        if let Some(path) = &settings.sccache_dir {
            env.push((OsString::from("SCCACHE_DIR"), path.as_os_str().into()));
        }
    }
    if let Some(path) = &settings.pnpm_store_dir {
        env.push((
            OsString::from("npm_config_store_dir"),
            path.as_os_str().into(),
        ));
    }
    if let Some(path) = &settings.pub_cache_dir {
        env.push((OsString::from("PUB_CACHE"), path.as_os_str().into()));
    }
    if let Some(path) = &settings.uv_cache_dir {
        env.push((OsString::from("UV_CACHE_DIR"), path.as_os_str().into()));
    }
    if settings.gradle_build_cache_enabled {
        let mut options = inherited_gradle_opts
            .map(|value| value.to_string_lossy().trim().to_string())
            .unwrap_or_default();
        if !options.is_empty() {
            options.push(' ');
        }
        // Gradle 把 -Dorg.gradle.caching 当作命令行级 Gradle property，优先级高于
        // 项目 gradle.properties；Flutter 间接启动 Gradle 时也会继承 GRADLE_OPTS。
        options.push_str("-Dorg.gradle.caching=true");
        env.push((OsString::from("GRADLE_OPTS"), OsString::from(options)));
    }

    Ok(RunSpec {
        program: program.clone(),
        args: command[1..].to_vec(),
        env,
    })
}

pub fn resolved_sccache() -> Option<PathBuf> {
    exe::resolve("sccache")
}

fn config_version() -> u32 {
    CONFIG_VERSION
}

fn non_empty_env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

fn home_dir() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    non_empty_env(key).map(PathBuf::from)
}

fn validate_cache_path(path: &Path, repo: &Path) -> Result<(), SharedCacheError> {
    let invalid = |reason: &str| SharedCacheError::InvalidCachePath {
        path: path.to_path_buf(),
        reason: reason.into(),
    };
    if !path.is_absolute() {
        return Err(invalid("必须使用绝对路径"));
    }
    if path.parent().is_none() {
        return Err(invalid("不能使用文件系统根目录"));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid("不能包含 . 或 .."));
    }
    if path.starts_with(repo) {
        return Err(invalid("共享缓存必须位于仓库之外"));
    }
    Ok(())
}

#[derive(Default)]
struct DetectedTools {
    sccache: Option<PathBuf>,
    pnpm: Option<PathBuf>,
    uv: Option<PathBuf>,
}

impl DetectedTools {
    fn current() -> Self {
        Self {
            sccache: exe::resolve("sccache"),
            pnpm: exe::resolve("pnpm"),
            uv: exe::resolve("uv"),
        }
    }
}

fn inspect_repo_with_tools(
    repo: &Path,
    settings: RepoCacheSettings,
    tools: &DetectedTools,
) -> RepoCacheProfile {
    let mut adapters = Vec::new();
    let ecosystems = Ecosystems::detect(repo);

    if ecosystems.cargo {
        let state = match (&tools.sccache, settings.sccache_enabled) {
            (None, _) => CacheAdapterState::MissingTool,
            (Some(_), true) => CacheAdapterState::Configured,
            (Some(_), false) => CacheAdapterState::Available,
        };
        adapters.push(CacheAdapterStatus {
            kind: CacheAdapterKind::CargoSccache,
            state,
            path: settings.sccache_dir.clone(),
            tool: tools.sccache.clone(),
        });
    }

    if ecosystems.gradle {
        adapters.push(CacheAdapterStatus {
            kind: CacheAdapterKind::GradleBuildCache,
            state: if settings.gradle_build_cache_enabled {
                CacheAdapterState::Configured
            } else {
                CacheAdapterState::Available
            },
            path: gradle_build_cache_dir(),
            tool: None,
        });
    }

    if let Some(cwd) = ecosystems.pnpm_dir {
        let configured = settings.pnpm_store_dir.clone();
        let detected = configured.clone().or_else(|| {
            tools
                .pnpm
                .as_ref()
                .and_then(|tool| command_path(tool, &["store", "path"], &cwd))
        });
        adapters.push(CacheAdapterStatus {
            kind: CacheAdapterKind::PnpmStore,
            state: if configured.is_some() {
                CacheAdapterState::Configured
            } else if tools.pnpm.is_some() && detected.is_some() {
                CacheAdapterState::Shared
            } else {
                CacheAdapterState::MissingTool
            },
            path: detected,
            tool: tools.pnpm.clone(),
        });
    }

    if ecosystems.pub_cache {
        let configured = settings.pub_cache_dir.clone();
        adapters.push(CacheAdapterStatus {
            kind: CacheAdapterKind::PubCache,
            state: if configured.is_some() {
                CacheAdapterState::Configured
            } else {
                CacheAdapterState::Shared
            },
            path: configured.or_else(pub_cache_dir),
            tool: exe::resolve("dart").or_else(|| exe::resolve("flutter")),
        });
    }

    if ecosystems.uv {
        let configured = settings.uv_cache_dir.clone();
        let detected = configured.clone().or_else(|| {
            tools
                .uv
                .as_ref()
                .and_then(|tool| command_path(tool, &["cache", "dir"], repo))
                .or_else(uv_cache_dir)
        });
        adapters.push(CacheAdapterStatus {
            kind: CacheAdapterKind::UvCache,
            state: if tools.uv.is_none() {
                CacheAdapterState::MissingTool
            } else if configured.is_some() {
                CacheAdapterState::Configured
            } else {
                CacheAdapterState::Shared
            },
            path: detected,
            tool: tools.uv.clone(),
        });
    }

    RepoCacheProfile {
        repo: repo.to_path_buf(),
        settings,
        adapters,
    }
}

fn command_path(tool: &Path, args: &[&str], cwd: &Path) -> Option<PathBuf> {
    let output = Command::new(tool)
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    (!value.is_empty()).then(|| PathBuf::from(value))
}

fn gradle_build_cache_dir() -> Option<PathBuf> {
    non_empty_env("GRADLE_USER_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".gradle")))
        .map(|home| home.join("caches").join("build-cache-1"))
}

fn pub_cache_dir() -> Option<PathBuf> {
    non_empty_env("PUB_CACHE")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".pub-cache")))
}

fn uv_cache_dir() -> Option<PathBuf> {
    non_empty_env("UV_CACHE_DIR")
        .or_else(|| {
            non_empty_env("XDG_CACHE_HOME").map(|root| PathBuf::from(root).join("uv").into())
        })
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".cache").join("uv")))
}

#[derive(Default)]
struct Ecosystems {
    cargo: bool,
    gradle: bool,
    pnpm_dir: Option<PathBuf>,
    pub_cache: bool,
    uv: bool,
}

impl Ecosystems {
    fn detect(repo: &Path) -> Self {
        let mut found = Self::default();
        for dir in bounded_dirs(repo, 2) {
            found.cargo |= dir.join("Cargo.toml").is_file();
            found.gradle |= dir.join("gradlew").is_file()
                || dir.join("settings.gradle").is_file()
                || dir.join("settings.gradle.kts").is_file();
            if found.pnpm_dir.is_none() && dir.join("pnpm-lock.yaml").is_file() {
                found.pnpm_dir = Some(dir.clone());
            }
            found.pub_cache |= dir.join("pubspec.yaml").is_file();
            found.uv |= dir.join("uv.lock").is_file();
        }
        found
    }
}

fn bounded_dirs(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = pending.pop() {
        out.push(dir.clone());
        if depth >= max_depth {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some(
                    ".git" | "target" | "node_modules" | ".dart_tool" | ".venv" | "build" | "dist"
                )
            ) {
                continue;
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn config_round_trips_and_removes_default_entries() {
        let temp = tempfile::tempdir().expect("临时目录");
        let path = temp.path().join("config/shared-cache.json");
        let repo = PathBuf::from("/repo/a");
        let settings = RepoCacheSettings {
            sccache_enabled: true,
            sccache_dir: Some(PathBuf::from("/cache/a/sccache")),
            ..RepoCacheSettings::default()
        };
        let mut config = SharedCacheConfig::default();
        config.set(repo.clone(), settings.clone());

        save_to(&path, &config).expect("保存配置");
        let loaded = load_from(&path).expect("读取配置");
        assert_eq!(loaded.settings_for(&repo), settings);

        config.set(repo.clone(), RepoCacheSettings::default());
        assert_eq!(config.settings_for(&repo), RepoCacheSettings::default());
        assert!(config.repositories.is_empty());
    }

    #[test]
    fn cache_paths_must_be_absolute_external_and_normalized() {
        let repo = Path::new("/repo/a");
        for path in [
            Path::new("cache"),
            Path::new("/"),
            Path::new("/repo/a/.cache"),
            Path::new("/cache/../other"),
        ] {
            let settings = RepoCacheSettings {
                uv_cache_dir: Some(path.to_path_buf()),
                ..RepoCacheSettings::default()
            };
            assert!(
                settings.validate(repo).is_err(),
                "应拒绝 {}",
                path.display()
            );
        }
        let valid = RepoCacheSettings {
            uv_cache_dir: Some(PathBuf::from("/cache/repo-a/uv")),
            ..RepoCacheSettings::default()
        };
        assert!(valid.validate(repo).is_ok());
    }

    #[test]
    fn detects_all_supported_ecosystems_without_walking_cache_trees() {
        let temp = tempfile::tempdir().expect("临时目录");
        for path in [
            "Cargo.toml",
            "web/pnpm-lock.yaml",
            "mobile/pubspec.yaml",
            "mobile/android/settings.gradle.kts",
            "tools/uv.lock",
            "target/hidden/pnpm-lock.yaml",
        ] {
            let path = temp.path().join(path);
            fs::create_dir_all(path.parent().expect("父目录")).expect("创建目录");
            fs::write(path, "").expect("写 marker");
        }

        let found = Ecosystems::detect(temp.path());
        assert!(found.cargo);
        assert!(found.gradle);
        assert!(found.pub_cache);
        assert!(found.uv);
        assert_eq!(found.pnpm_dir, Some(temp.path().join("web")));
    }

    #[test]
    fn run_spec_injects_only_managed_external_cache_configuration() {
        let settings = RepoCacheSettings {
            sccache_enabled: true,
            gradle_build_cache_enabled: true,
            sccache_dir: Some(PathBuf::from("/cache/sccache")),
            pnpm_store_dir: Some(PathBuf::from("/cache/pnpm")),
            pub_cache_dir: Some(PathBuf::from("/cache/pub")),
            uv_cache_dir: Some(PathBuf::from("/cache/uv")),
        };
        let command = vec![OsString::from("flutter"), OsString::from("build")];
        let spec = prepare_run(
            &command,
            &settings,
            Some(Path::new("/usr/local/bin/sccache")),
            Some(OsStr::new("-Dfile.encoding=UTF-8")),
        )
        .expect("准备命令");

        assert_eq!(spec.program, "flutter");
        assert_eq!(spec.args, [OsString::from("build")]);
        assert!(spec.env.contains(&(
            OsString::from("RUSTC_WRAPPER"),
            OsString::from("/usr/local/bin/sccache")
        )));
        assert!(spec.env.contains(&(
            OsString::from("npm_config_store_dir"),
            OsString::from("/cache/pnpm")
        )));
        assert!(spec.env.contains(&(
            OsString::from("GRADLE_OPTS"),
            OsString::from("-Dfile.encoding=UTF-8 -Dorg.gradle.caching=true")
        )));
        assert!(
            spec.env
                .iter()
                .all(|(key, _)| key != "CARGO_TARGET_DIR" && key != "GRADLE_USER_HOME")
        );
    }

    #[test]
    fn enabled_sccache_fails_closed_when_binary_is_missing() {
        let settings = RepoCacheSettings {
            sccache_enabled: true,
            ..RepoCacheSettings::default()
        };
        let error = prepare_run(
            &[OsString::from("cargo"), OsString::from("test")],
            &settings,
            None,
            None,
        )
        .expect_err("缺少 sccache 必须失败");
        assert!(matches!(error, SharedCacheError::MissingSccache));
    }

    #[test]
    fn disabled_sccache_does_not_inject_its_saved_directory() {
        let settings = RepoCacheSettings {
            sccache_enabled: false,
            sccache_dir: Some(PathBuf::from("/cache/sccache")),
            ..RepoCacheSettings::default()
        };
        let spec = prepare_run(&[OsString::from("cargo")], &settings, None, None)
            .expect("关闭 sccache 时路径只保存、不注入");

        assert!(
            spec.env
                .iter()
                .all(|(key, _)| key != "RUSTC_WRAPPER" && key != "SCCACHE_DIR")
        );
    }

    #[test]
    fn profile_reports_supported_adapters_for_mixed_repository() {
        let temp = tempfile::tempdir().expect("临时目录");
        for path in [
            "Cargo.toml",
            "pnpm-lock.yaml",
            "pubspec.yaml",
            "android/settings.gradle.kts",
            "scripts/uv.lock",
        ] {
            let path = temp.path().join(path);
            fs::create_dir_all(path.parent().expect("父目录")).expect("创建目录");
            fs::write(path, "").expect("写 marker");
        }
        let tools = DetectedTools {
            sccache: Some(PathBuf::from("/tools/sccache")),
            pnpm: None,
            uv: Some(PathBuf::from("/tools/uv")),
        };
        let profile = inspect_repo_with_tools(temp.path(), RepoCacheSettings::default(), &tools);
        let kinds: Vec<_> = profile.adapters.iter().map(|item| item.kind).collect();
        assert_eq!(
            kinds,
            [
                CacheAdapterKind::CargoSccache,
                CacheAdapterKind::GradleBuildCache,
                CacheAdapterKind::PnpmStore,
                CacheAdapterKind::PubCache,
                CacheAdapterKind::UvCache,
            ]
        );
    }
}
