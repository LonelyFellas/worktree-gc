//! B4 —— 内部没有嵌套的其它 worktree / git 仓。
//!
//! Claude Code 默认把 worktree 建在 `<repo>/.claude/worktrees/` 下，而 `.claude/`
//! 常被 gitignore。于是外层的 B1 看不到任何未提交改动、判定为「干净」，
//! 删外层就把内层连同别人的未提交改动一起灭了（D5）。
//! 内层自己的六道门禁救不了它——根本轮不到内层被单独判定。
//!
//! 两条判据互补，任一命中即拦：
//! 1. **注册表**：不受深度与跳过表限制，但只认 `git worktree add` 注册过的。
//! 2. **文件系统**：认得出 `git init` 出来的独立仓、以及注册记录已损坏的残留，
//!    代价是必须限深并跳过构建产物目录。
//!
//! 单独任何一条都有确定的盲区，所以两条都跑，结果取并集。

use crate::gates::{Gate, GateCtx};
use crate::git::porcelain;
use crate::model::{Cause, GateDetail, GateId, GateStatus};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub struct NestedGate;

/// 文件系统扫描的最大深度（worktree 根记为 0）。
///
/// Claude Code 的 `.claude/worktrees/<name>/.git` 只有 3 层，Codex 之类的布局也在这个量级。
/// 再往深挖收益递减，代价却是把整棵源码树走一遍——而真正深埋的嵌套 worktree
/// 只要是注册过的，判据一不受深度限制照样能抓到。
const MAX_DEPTH: usize = 6;

impl Gate for NestedGate {
    fn id(&self) -> GateId {
        GateId::Nested
    }

    fn evaluate(&self, ctx: &GateCtx<'_>) -> GateStatus {
        // 比路径前先 canonicalize：注册表里存的是 `worktree add` 当时的写法，
        // 与 ctx.worktree 可能一个是 /var 一个是 /private/var（macOS 的 /var symlink），
        // 直接 starts_with 会把真嵌套判成不嵌套。
        let root = match ctx.worktree.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return GateStatus::Unknown(Cause::Io {
                    path: ctx.worktree.to_path_buf(),
                    msg: e.to_string(),
                });
            }
        };

        let entries = match registered_entries(ctx) {
            Ok(entries) => entries,
            Err(c) => return GateStatus::Unknown(c),
        };
        let filesystem = scan_for_git(
            &root,
            &ctx.cfg.cache_rules,
            &ctx.cfg.precious.disposable_dirs,
        )
        .map_err(|(path, error)| Cause::Io {
            path,
            msg: error.to_string(),
        });
        self.evaluate_entries_and_root(&entries, root, filesystem)
    }
}

impl NestedGate {
    /// 扫描阶段同时复用 discover 的注册表和文件树 facts。路径在这里映射到
    /// canonical worktree 根，保持与实时 NestedGate 的报告口径一致。
    pub(crate) fn evaluate_entries_with_filesystem(
        &self,
        ctx: &GateCtx<'_>,
        entries: &[porcelain::WorktreeEntry],
        filesystem: Result<Vec<PathBuf>, Cause>,
    ) -> GateStatus {
        let root = match ctx.worktree.canonicalize() {
            Ok(path) => path,
            Err(e) => {
                return GateStatus::Unknown(Cause::Io {
                    path: ctx.worktree.to_path_buf(),
                    msg: e.to_string(),
                });
            }
        };
        let filesystem = filesystem.map(|paths| {
            paths
                .into_iter()
                .map(|path| {
                    path.strip_prefix(ctx.worktree)
                        .map_or(path.clone(), |relative| root.join(relative))
                })
                .collect()
        });
        self.evaluate_entries_and_root(entries, root, filesystem)
    }

    fn evaluate_entries_and_root(
        &self,
        entries: &[porcelain::WorktreeEntry],
        root: PathBuf,
        filesystem: Result<Vec<PathBuf>, Cause>,
    ) -> GateStatus {
        let mut found = match registered_under_entries(entries, &root) {
            Ok(v) => v,
            Err(c) => return GateStatus::Unknown(c),
        };

        match filesystem {
            Ok(v) => found.extend(v),
            Err(cause) => return GateStatus::Unknown(cause),
        }

        // 两条判据会重复命中同一个内层工作区（注册过的必然也有 .git）
        found.sort();
        found.dedup();

        if found.is_empty() {
            GateStatus::Pass
        } else {
            GateStatus::Blocked(GateDetail::NestedWorktrees { paths: found })
        }
    }
}

/// 判据一：`git worktree list --porcelain` 里位于本 worktree 之下的**其它** worktree。
fn registered_entries(ctx: &GateCtx<'_>) -> Result<Vec<porcelain::WorktreeEntry>, Cause> {
    let out = ctx
        .git
        .run_ok(ctx.repo, &["worktree", "list", "--porcelain"])?;
    Ok(porcelain::parse_worktree_list(&out.stdout_utf8()))
}

fn registered_under_entries(
    entries: &[porcelain::WorktreeEntry],
    root: &Path,
) -> Result<Vec<PathBuf>, Cause> {
    let mut hits = Vec::new();
    for entry in entries {
        let resolved = match entry.path.canonicalize() {
            Ok(p) => p,
            // 目录已经不在了（陈旧注册记录，见 D13）：磁盘上没有可丢的东西，
            // 拿它拦住外层只会让用户永远清不掉。其它 IO 错误则是真的判不准。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(Cause::Io {
                    path: entry.path.clone(),
                    msg: e.to_string(),
                });
            }
        };
        // 自身要排除；starts_with 是按路径分量比的，`wt-2` 不会被算进 `wt` 之下
        if resolved != *root && resolved.starts_with(root) {
            hits.push(resolved);
        }
    }
    Ok(hits)
}

/// 判据二：子树里出现 `.git`。
///
/// **文件和目录都算**：linked worktree 的 `.git` 是一个指向 gitdir 的文件，
/// 只判目录会把 `git worktree add` 出来的内层整个漏掉——而那正是 D5 的主角。
///
/// 返回 `.git` 所在的目录（即内层工作区的根）而非 `.git` 本身，
/// 这样与判据一同形，去重和展示都直接可用。
///
/// 错误一律上抛成 `Unknown`：读不动的子目录里完全可能正藏着一个内层 worktree。
fn scan_for_git(
    root: &Path,
    cache_rules: &[crate::config::CacheRule],
    disposable_dirs: &[String],
) -> Result<Vec<PathBuf>, (PathBuf, std::io::Error)> {
    let dot_git = OsStr::new(".git");
    let mut hits = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];

    while let Some((dir, depth)) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            // 扫描途中目录消失：它已经不在了，也就没有可丢的东西
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err((dir, e)),
        };

        let mut has_git = false;
        let mut children = Vec::new();
        for entry in rd {
            let entry = match entry {
                Ok(x) => x,
                Err(e) => return Err((dir.clone(), e)),
            };
            let name = entry.file_name();
            if name.as_os_str() == dot_git {
                has_git = true;
                continue;
            }
            // 跳过表：不为了找 .git 去遍历一个 30GB 的 target/。
            // 代价是藏在构建产物目录里的**未注册**内层仓会漏掉——注册过的仍由判据一兜住。
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let disposable = name.to_str().is_some_and(|name| {
                disposable_dirs.iter().any(|dir| dir == name)
                    && cache_rules
                        .iter()
                        .find(|rule| rule.dir == name)
                        .is_some_and(|rule| rule.has_marker_for(root, rel))
            });
            if disposable {
                continue;
            }
            // file_type 不跟随符号链接：跟过去既可能走出 worktree 之外，也可能兜圈子
            match entry.file_type() {
                Ok(t) if t.is_dir() => children.push(path),
                Ok(_) => {}
                Err(e) => return Err((entry.path(), e)),
            }
        }

        // depth == 0 的那个 .git 是本 worktree 自己的，不算嵌套
        if has_git && depth > 0 {
            // 命中即止：内层工作区里面还有什么，与「外层能不能删」无关
            hits.push(dir);
            continue;
        }

        if depth < MAX_DEPTH {
            stack.extend(children.into_iter().map(|c| (c, depth + 1)));
        }
    }

    Ok(hits)
}
