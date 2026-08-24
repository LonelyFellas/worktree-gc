//! 仓库发现：把「显式声明的根」与「种子目录」两条来源汇成一张待扫描的仓库表。
//!
//! 两条来源缺一不可。显式根覆盖用户自己关心的仓库；种子目录覆盖 agent 自作主张
//! 建在别处的 worktree —— Codex 落在 `~/.codex/worktrees`，也有落在 `$TMPDIR` 的。
//! （`<repo>/.claude/worktrees/` 不必列进种子：那些是注册过的 worktree，
//! 从主仓一条 `worktree list` 自然就带出来了。）
//!
//! 一条贯穿本模块的取舍：**发现阶段的失败方向是「少发现一个仓」**。
//! 打不开的路径、读不动的目录一律跳过，而不是让整轮发现失败——代价只是漏掉
//! 几 GB 可回收空间。这与「判不准一律 `Unknown`」并不矛盾：本模块**不做任何放行判断**，
//! 能不能删全部由门禁决定，而一个压根没被发现的仓库连被删的机会都没有。
//! 也正因为如此，[`discover`] 返回 `Vec` 而不是 `Result`。

use crate::config::ScanConfig;
use crate::git::GitRunner;
use crate::git::porcelain::{self, WorktreeEntry};
use crate::model::Cause;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// 一个被发现的仓库及其全部 worktree。
#[derive(Debug, Clone)]
pub struct Discovered {
    pub root: PathBuf,
    pub worktrees: Vec<WorktreeEntry>,
    pub prunable: Vec<PathBuf>,
}

/// 种子扫描的最大深度（种子目录自身记为 0）。
///
/// 限深是为了不让「扫一眼常见落点」退化成全盘扫描。agent 的落点布局都很浅
/// （`~/.codex/worktrees/<name>`、`$TMPDIR/<name>`），埋得更深的仓库
/// 应当用 `cfg.repos` 显式声明，而不是靠加深度去碰运气。
const MAX_SEED_DEPTH: usize = 4;

/// 发现所有仓库。`cfg.repos` 是显式声明的根，`cfg.seeds` 是要扫描的种子目录。
pub fn discover(cfg: &ScanConfig, git: &dyn GitRunner) -> Vec<Discovered> {
    let mut acc = Acc::default();

    for repo in &cfg.repos {
        // 打不开就跳过：一个仓的路径写错了，不该连累其余的仓（见模块头）。
        if let Ok(d) = expand(git, repo) {
            acc.push(d);
        }
    }

    for seed in &cfg.seeds {
        walk_seed(cfg, git, seed, &mut acc);
    }

    acc.out
}

/// 已知的 agent worktree 落点，用作默认种子。
///
/// 只返回**当前确实存在**的目录：这份名单是内置默认值而非用户配置，
/// 把不存在的路径塞给扫描层只会让它白跑一趟、还得在报告里解释。
pub fn default_seeds() -> Vec<PathBuf> {
    let mut seeds = Vec::new();
    if let Some(home) = home_dir() {
        seeds.push(home.join(".codex").join("worktrees"));
    }
    // temp_dir() 在 unix 上就是 $TMPDIR（未设则 /tmp）
    seeds.push(std::env::temp_dir());
    seeds.retain(|p| p.is_dir());
    seeds
}

/// 在 `at` 处展开整个仓库家族。
///
/// `at` 可以是主仓根、任一 linked worktree、甚至仓内任意子目录——
/// `git worktree list` 走的是 common dir，从哪儿问都返回同一张全家表，
/// 且**主 worktree 恒为首条**（git 文档保证），所以首条的路径就是主仓根。
///
/// 失败一律上抛 `Cause`：调用方要么跳过这个仓，要么把原因摆给用户看，
/// 但绝不会拿到一个「看起来正常」的空结果。
pub fn expand(git: &dyn GitRunner, at: &Path) -> Result<Discovered, Cause> {
    let out = git.run_ok(at, &["worktree", "list", "--porcelain"])?;
    let mut entries = porcelain::parse_worktree_list(&out.stdout_utf8());

    // 命令成功却一条都没有：不是 git 仓，或者输出格式变了。两种都属于「说不清」，
    // 上抛而不是当成一个没有 worktree 的空仓库。
    let main = entries.first().ok_or_else(|| Cause::CommandFailed {
        cmd: "git worktree list --porcelain".into(),
        code: None,
        stderr: format!("{} 处的 worktree 列表为空", at.display()),
    })?;

    // canonicalize 后再做仓库身份：同一个仓可能既在 repos 里又被 seed 扫到，
    // 两边的写法还可能一个是 /var 一个是 /private/var（macOS 的 /var symlink），
    // 不归一化就会重复发现、重复扫描、在报告里出现两次。
    let root = main.path.canonicalize().map_err(|e| Cause::Io {
        path: main.path.clone(),
        msg: e.to_string(),
    })?;

    // Git for Windows 输出普通的 C:/...，canonicalize 则返回 \\?\C:\...；两种 PathBuf
    // 不相等，会让主工作区失去 is_main 保护。有效 worktree 必须统一到同一口径。
    for entry in &mut entries {
        if !entry.prunable {
            entry.path = entry.path.canonicalize().map_err(|e| Cause::Io {
                path: entry.path.clone(),
                msg: e.to_string(),
            })?;
        }
    }

    // prunable = 注册记录还在、目录已经没了。它没有磁盘可回收，只能 `git worktree prune`，
    // 混进 worktrees 会让后续每道门禁都对着一个不存在的路径判「判不准」。
    let (prunable, worktrees): (Vec<WorktreeEntry>, Vec<WorktreeEntry>) =
        entries.into_iter().partition(|e| e.prunable);

    Ok(Discovered {
        root,
        worktrees,
        prunable: prunable.into_iter().map(|e| e.path).collect(),
    })
}

/// 发现结果累加器：按主仓根去重，同时保持发现顺序（repos 在前，seeds 在后）。
#[derive(Default)]
struct Acc {
    out: Vec<Discovered>,
    seen: HashSet<PathBuf>,
}

impl Acc {
    fn push(&mut self, d: Discovered) {
        if self.seen.insert(d.root.clone()) {
            self.out.push(d);
        }
    }
}

/// 扫描一个种子目录，找出其中的工作区。
fn walk_seed(cfg: &ScanConfig, git: &dyn GitRunner, seed: &Path, acc: &mut Acc) {
    let mut stack = vec![(seed.to_path_buf(), 0usize)];

    while let Some((dir, depth)) = stack.pop() {
        if has_git(&dir) {
            // 命中即剪枝：这个仓的全家已经由 `worktree list` 一次性展开，
            // 再往下探只会（a）把每个嵌套 worktree 重复发现一遍，
            // （b）一头扎进 30GB 的 target/ 里空转。
            // 代价是恰好嵌套在某个 worktree 里的**无关**仓库不会单独成表——
            // 它的去留本来也是跟着外层走的（B4 会因为嵌套而拦下外层）。
            //
            // 展开失败（仓库损坏、gitdir 指向已消失的位置）同样剪枝：一个 git 都读不懂的
            // 目录，继续下探也只会把它内部的东西当成独立仓库。
            if let Ok(d) = expand(git, &dir) {
                acc.push(d);
            }
            continue;
        }

        if depth < MAX_SEED_DEPTH {
            for child in subdirs(&dir, &cfg.precious.disposable_dirs) {
                stack.push((child, depth + 1));
            }
        }
    }
}

/// 该目录是不是一个 git 工作区。
///
/// **文件和目录都算**：linked worktree 的 `.git` 是一个写着 `gitdir: ...` 的文件，
/// 只判目录会把 agent 建的 worktree 整个漏掉——而那正是本工具要找的东西。
/// 用 `symlink_metadata` 而非 `exists()`：断掉的 `.git` 符号链接同样说明这里曾是工作区，
/// 该交给 git 去判，而不是当作普通目录继续下探。
fn has_git(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).is_ok()
}

/// 列出可继续下探的子目录。读不动就返回空——见模块头：这里的失败只会少发现。
fn subdirs(dir: &Path, skip: &[String]) -> Vec<PathBuf> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for entry in rd {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name();
        // 跳过表：不为了找仓库去遍历一个 30GB 的 target/。构建产物目录里不会有
        // agent 的工作区，就算有，也该由外层 worktree 的 B4 门禁去管。
        if skip
            .iter()
            .any(|d| name.as_os_str() == OsStr::new(d.as_str()))
        {
            continue;
        }
        // file_type 不跟随符号链接：跟过去既可能走出种子之外，也可能兜圈子。
        // 类型都取不到的条目直接不下探——同样是「少发现」这一侧。
        match entry.file_type() {
            Ok(t) if t.is_dir() => out.push(entry.path()),
            Ok(_) | Err(_) => {}
        }
    }
    out
}

/// 用户主目录。
///
/// 不用 `std::env::home_dir`（在本项目的最低支持版本 1.85 上仍是 deprecated），
/// 也不为这一处引第三方依赖。
fn home_dir() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}
