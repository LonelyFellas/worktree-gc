//! 扫描配置。所有「以前写死在 bash 里」的东西都在这里成为可配置项。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// 一条构建缓存识别规则。
///
/// 光靠目录名不足以判定可删——入库的 `dist/` 也叫 dist。名字只是**候选**，
/// 真正的放行由 A3 门禁（被忽略 + 无 tracked 文件 + 在根内 + 非 symlink）决定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheRule {
    /// 目录名，相对 worktree 根。
    pub dir: String,
    pub ecosystem: String,
    /// 佐证文件：存在则更确信这是该生态的产物目录。为空表示不要求。
    pub markers: Vec<String>,
}

impl CacheRule {
    fn new(dir: &str, ecosystem: &str, markers: &[&str]) -> Self {
        Self {
            dir: dir.to_string(),
            ecosystem: ecosystem.to_string(),
            markers: markers.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// 内置规则表。做成数据而非 if-else，用户可在配置里追加。
    pub fn defaults() -> Vec<CacheRule> {
        vec![
            CacheRule::new("target", "rust", &["Cargo.toml"]),
            CacheRule::new("node_modules", "node", &["package.json"]),
            CacheRule::new("dist", "node", &["package.json"]),
            CacheRule::new(".next", "node", &["package.json"]),
            CacheRule::new(".turbo", "node", &["package.json"]),
            CacheRule::new(".venv", "python", &["pyproject.toml"]),
            CacheRule::new("__pycache__", "python", &[]),
            CacheRule::new(".gradle", "jvm", &["build.gradle", "build.gradle.kts"]),
        ]
    }

    /// 候选目录还必须有生态佐证文件。`target/`、`dist/` 之类的名字太常见，
    /// 只凭名字和 gitignore 无法证明里面一定是可重建产物。
    pub fn has_marker_for(&self, worktree: &std::path::Path, cache_rel: &std::path::Path) -> bool {
        let owner = cache_rel
            .parent()
            .map_or_else(|| worktree.to_path_buf(), |parent| worktree.join(parent));
        self.markers.is_empty()
            || self
                .markers
                .iter()
                .any(|marker| owner.join(marker).is_file())
    }
}

/// 敏感文件策略。
///
/// **黑名单语义**：不在「已知构建缓存」名单里的忽略文件，一律当作可能宝贵。
/// 原型用的是白名单（只保护列出的模式），实测漏掉了 tfstate、签名密钥、本地数据库（D2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreciousPolicy {
    /// 确定可弃的目录名（构建产物）。除此之外的忽略内容都要人看一眼。
    pub disposable_dirs: Vec<String>,
    /// 即便与主仓内容相同也要拦下的模式（额外保险）。
    pub always_precious: Vec<String>,
}

impl Default for PreciousPolicy {
    fn default() -> Self {
        Self {
            disposable_dirs: CacheRule::defaults().into_iter().map(|r| r.dir).collect(),
            always_precious: vec![
                "*.tfstate".into(),
                "*.jks".into(),
                "*.keystore".into(),
                "*.p12".into(),
                "id_rsa".into(),
                "id_ed25519".into(),
            ],
        }
    }
}

/// 主干基线的确定方式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BaseBranchPolicy {
    /// 自动探测：remote HEAD → 常见名 → 失败则整仓标红（**不静默跳过**，见 D12）。
    Auto,
    Explicit {
        remote: Option<String>,
        branch: String,
    },
}

/// 是否刷新远端引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FetchPolicy {
    Never,
    IfStale { secs: u64 },
    Always,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanConfig {
    /// 显式声明的仓库根。
    pub repos: Vec<PathBuf>,
    /// 种子目录：在这些位置下找 agent 留下的 worktree。
    pub seeds: Vec<PathBuf>,
    /// 删除整个 worktree 前要求的空闲阈值。它只作 Busy 的时间侧补充，
    /// 不影响可重建缓存的回收，也不能绕过 Dirty、Landed 等其它删除门禁。
    pub idle: Duration,
    pub cache_rules: Vec<CacheRule>,
    pub precious: PreciousPolicy,
    pub baseline: BaseBranchPolicy,
    pub fetch: FetchPolicy,
    /// 缓存目录「安静多久」才认为构建已结束。A2 门用，比 idle 短得多。
    pub cache_quiet: Duration,
    /// 单条 git 子命令的超时。
    pub git_timeout: Duration,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            repos: Vec::new(),
            seeds: Vec::new(),
            idle: Duration::from_secs(24 * 3600),
            cache_rules: CacheRule::defaults(),
            precious: PreciousPolicy::default(),
            baseline: BaseBranchPolicy::Auto,
            fetch: FetchPolicy::IfStale { secs: 3600 },
            cache_quiet: Duration::from_secs(600),
            git_timeout: Duration::from_secs(30),
        }
    }
}
