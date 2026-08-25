//! 扫描阶段的一次性文件树事实采集。
//!
//! 旧实现会为 worktree 体积、每个缓存体积、Recent、Idle、Nested 分别遍历目录。
//! 这里一次走完整棵树，同时产出这些消费者所需的最小事实；apply 的实时复检不走这里。

use crate::config::ScanConfig;
use crate::model::Cause;
use crate::sizing;
use jwalk::{Parallelism, WalkDir};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const RECENT_DEPTH: usize = 3;
const NESTED_DEPTH: usize = 6;

pub(crate) type Activity = Result<Option<(PathBuf, SystemTime)>, Cause>;

#[derive(Debug)]
pub(crate) struct CacheFacts {
    pub bytes: u64,
    pub activity: Activity,
}

#[derive(Debug)]
pub(crate) struct WorktreeFacts {
    pub bytes: u64,
    pub activity: Activity,
    pub nested_git: Result<Vec<PathBuf>, Cause>,
    caches: HashMap<String, CacheFacts>,
}

#[derive(Default)]
struct CacheAccumulator {
    bytes: u64,
    seen: HashSet<(u64, u64)>,
    newest: Option<(PathBuf, SystemTime)>,
    activity_error: Option<Cause>,
    saw_root: bool,
}

impl WorktreeFacts {
    pub fn cache(&self, dir: &str) -> Option<&CacheFacts> {
        self.caches.get(dir)
    }
}

/// 完整遍历一次 worktree，顺手采集体积、浅层活动时间与嵌套 `.git`。
pub(crate) fn collect(root: &Path, cache_dirs: &[String], cfg: &ScanConfig) -> WorktreeFacts {
    let mut caches: HashMap<String, CacheAccumulator> = cache_dirs
        .iter()
        .map(|dir| (dir.clone(), CacheAccumulator::default()))
        .collect();
    let mut total = 0u64;
    let mut total_seen = HashSet::new();
    let mut newest = None;
    let mut activity_error = None;
    let mut nested_hits = Vec::new();
    let mut nested_error = None;

    let walk = WalkDir::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .sort(false)
        // worktree 之间由 scan 的有界池并行；树内再占用同一个池会造成嵌套争抢。
        .parallelism(Parallelism::Serial);
    let walk = match walk.try_into_iter() {
        Ok(walk) => walk,
        Err(error) => return failed(root, cache_dirs, error.to_string()),
    };

    for item in walk {
        let entry = match item {
            Ok(entry) => entry,
            Err(error) if error.is_busy() => {
                return failed(root, cache_dirs, error.to_string());
            }
            Err(error) => {
                let path = error.path().unwrap_or(root).to_path_buf();
                let cause = Cause::Io {
                    path: path.clone(),
                    msg: error.to_string(),
                };
                mark_activity_error(
                    root,
                    &path,
                    error.depth(),
                    &cause,
                    &mut activity_error,
                    &mut caches,
                );
                if error.depth() <= NESTED_DEPTH + 1
                    && !is_disposable_descendant(root, &path, cfg)
                    && error
                        .io_error()
                        .is_none_or(|io| io.kind() != std::io::ErrorKind::NotFound)
                {
                    nested_error.get_or_insert(cause);
                }
                continue;
            }
        };

        let path = entry.path();
        let depth = entry.depth();
        let cache_match = cache_dirs.iter().find_map(|dir| {
            let cache_root = root.join(dir);
            path.strip_prefix(&cache_root)
                .ok()
                .map(|relative| (dir.as_str(), relative.components().count()))
        });

        if entry.file_name() == OsStr::new(".git")
            && depth > 1
            && depth <= NESTED_DEPTH + 1
            && let Some(parent) = path.parent()
            && !is_disposable_descendant(root, parent, cfg)
        {
            nested_hits.push(parent.to_path_buf());
        }

        let needs_activity_meta = depth <= RECENT_DEPTH
            || cache_match.is_some_and(|(_, relative_depth)| relative_depth <= RECENT_DEPTH);
        let needs_size_meta = !entry.file_type().is_dir();
        if !needs_activity_meta && !needs_size_meta {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                let cause = Cause::Io {
                    path: path.clone(),
                    msg: error.to_string(),
                };
                if needs_activity_meta {
                    mark_activity_error(
                        root,
                        &path,
                        depth,
                        &cause,
                        &mut activity_error,
                        &mut caches,
                    );
                }
                // 体积统计沿用原口径：单个条目的 metadata 失败只漏掉该条目。
                continue;
            }
        };

        if needs_activity_meta {
            match metadata.modified() {
                Ok(mtime) => {
                    if depth <= RECENT_DEPTH {
                        update_newest(&mut newest, &path, mtime);
                    }
                    if let Some((dir, relative_depth)) = cache_match
                        && relative_depth <= RECENT_DEPTH
                        && let Some(cache) = caches.get_mut(dir)
                    {
                        if relative_depth == 0 {
                            cache.saw_root = true;
                        }
                        update_newest(&mut cache.newest, &path, mtime);
                    }
                }
                Err(error) => {
                    let cause = Cause::Io {
                        path: path.clone(),
                        msg: error.to_string(),
                    };
                    mark_activity_error(
                        root,
                        &path,
                        depth,
                        &cause,
                        &mut activity_error,
                        &mut caches,
                    );
                }
            }
        }

        if needs_size_meta {
            let (bytes, id) = sizing::probe(&metadata);
            if id.is_none_or(|id| total_seen.insert(id)) {
                total = total.saturating_add(bytes);
            }
            if let Some((dir, _)) = cache_match
                && let Some(cache) = caches.get_mut(dir)
                && id.is_none_or(|id| cache.seen.insert(id))
            {
                cache.bytes = cache.bytes.saturating_add(bytes);
            }
        }
    }

    // 组合遍历不能像旧 NestedGate 那样命中即停止，最后去掉命中仓库内部的更深命中。
    nested_hits.sort();
    nested_hits.dedup();
    let mut outer_hits: Vec<PathBuf> = Vec::new();
    for hit in nested_hits {
        if !outer_hits.iter().any(|outer| hit.starts_with(outer)) {
            outer_hits.push(hit);
        }
    }

    // 保持旧 IdleGate 的合并顺序：先 worktree 根三层，再按配置顺序合入各缓存三层。
    // mtime 相同保留较早来源，避免组合遍历的访问顺序改变 newest_path。
    for dir in cache_dirs {
        if let Some(cache) = caches.get_mut(dir) {
            if !cache.saw_root && cache.activity_error.is_none() {
                cache.activity_error = Some(Cause::Io {
                    path: root.join(dir),
                    msg: "扫描期间缓存目录消失或无法读取".into(),
                });
            }
            if let Some(cause) = &cache.activity_error {
                activity_error.get_or_insert_with(|| cause.clone());
            } else {
                newest = newer(newest, cache.newest.clone());
            }
        }
    }

    let caches = caches
        .into_iter()
        .map(|(dir, cache)| {
            let activity = if let Some(cause) = cache.activity_error {
                Err(cause)
            } else {
                Ok(cache.newest)
            };
            (
                dir,
                CacheFacts {
                    bytes: cache.bytes,
                    activity,
                },
            )
        })
        .collect();

    WorktreeFacts {
        bytes: total,
        activity: activity_error.map_or(Ok(newest), Err),
        nested_git: nested_error.map_or(Ok(outer_hits), Err),
        caches,
    }
}

fn failed(root: &Path, cache_dirs: &[String], msg: String) -> WorktreeFacts {
    let root_cause = Cause::Io {
        path: root.to_path_buf(),
        msg,
    };
    WorktreeFacts {
        bytes: 0,
        activity: Err(root_cause.clone()),
        nested_git: Err(root_cause.clone()),
        caches: cache_dirs
            .iter()
            .map(|dir| {
                (
                    dir.clone(),
                    CacheFacts {
                        bytes: 0,
                        activity: Err(root_cause.clone()),
                    },
                )
            })
            .collect(),
    }
}

fn update_newest(best: &mut Option<(PathBuf, SystemTime)>, path: &Path, mtime: SystemTime) {
    if best.as_ref().is_none_or(|(_, current)| mtime > *current) {
        *best = Some((path.to_path_buf(), mtime));
    }
}

fn newer(
    left: Option<(PathBuf, SystemTime)>,
    right: Option<(PathBuf, SystemTime)>,
) -> Option<(PathBuf, SystemTime)> {
    match (left, right) {
        (Some(a), Some(b)) if b.1 > a.1 => Some(b),
        (Some(a), Some(_)) => Some(a),
        (None, value) | (value, None) => value,
    }
}

fn mark_activity_error(
    root: &Path,
    path: &Path,
    depth: usize,
    cause: &Cause,
    worktree_error: &mut Option<Cause>,
    caches: &mut HashMap<String, CacheAccumulator>,
) {
    if depth <= RECENT_DEPTH {
        worktree_error.get_or_insert_with(|| cause.clone());
    }
    for (dir, cache) in caches {
        let cache_root = root.join(dir);
        if let Ok(relative) = path.strip_prefix(cache_root)
            && relative.components().count() <= RECENT_DEPTH
        {
            cache.activity_error.get_or_insert_with(|| cause.clone());
            worktree_error.get_or_insert_with(|| cause.clone());
        }
    }
}

/// 判断路径是否位于一条有效的 disposable 目录之下；这复刻 NestedGate 的跳过表，
/// 但不真的剪枝，因为同一遍历还必须统计这些构建产物的体积。
fn is_disposable_descendant(root: &Path, path: &Path, cfg: &ScanConfig) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut current = PathBuf::new();
    for component in relative.components() {
        current.push(component.as_os_str());
        let Some(name) = component.as_os_str().to_str() else {
            continue;
        };
        if cfg.precious.disposable_dirs.iter().any(|dir| dir == name)
            && cfg
                .cache_rules
                .iter()
                .find(|rule| rule.dir == name)
                .is_some_and(|rule| rule.has_marker_for(root, &current))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn one_walk_keeps_total_and_cache_hardlink_scopes_separate() {
        let temp = tempfile::tempdir().expect("建临时目录");
        let root = temp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'",
        )
        .expect("写 marker");
        std::fs::create_dir(root.join("target")).expect("建缓存");
        let source = root.join("source.bin");
        std::fs::write(&source, vec![b'x'; 16 * 1024]).expect("写文件");
        std::fs::hard_link(&source, root.join("target/link.bin")).expect("建硬链接");

        let cfg = ScanConfig::default();
        let facts = collect(root, &["target".into()], &cfg);
        let cache = facts.cache("target").expect("缓存事实");

        assert!(facts.bytes > 0);
        assert!(cache.bytes > 0, "缓存自己的去重集合不能被整树集合污染");
        assert!(facts.bytes >= cache.bytes);
    }

    #[test]
    fn nested_git_under_disposable_cache_is_ignored_but_still_sized() {
        let temp = tempfile::tempdir().expect("建临时目录");
        let root = temp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'",
        )
        .expect("写 marker");
        std::fs::create_dir_all(root.join("target/generated/.git")).expect("建嵌套仓");
        std::fs::write(root.join("target/generated/blob"), vec![b'x'; 16 * 1024]).expect("写缓存");

        let cfg = ScanConfig::default();
        let facts = collect(root, &["target".into()], &cfg);

        assert_eq!(facts.nested_git, Ok(Vec::new()));
        assert!(facts.bytes > 0);
        assert!(facts.cache("target").expect("缓存事实").bytes > 0);
    }

    #[test]
    fn nested_git_reports_only_the_outermost_repository() {
        let temp = tempfile::tempdir().expect("建临时目录");
        let root = temp.path();
        std::fs::create_dir_all(root.join("outer/.git")).expect("建外层仓");
        std::fs::create_dir_all(root.join("outer/inner/.git")).expect("建内层仓");

        let facts = collect(root, &[], &ScanConfig::default());

        assert_eq!(facts.nested_git, Ok(vec![root.join("outer")]));
    }

    #[test]
    fn a_cache_that_disappears_makes_cache_and_idle_activity_unknown() {
        let temp = tempfile::tempdir().expect("建临时目录");
        let facts = collect(temp.path(), &["target".into()], &ScanConfig::default());

        assert!(facts.activity.is_err());
        assert!(facts.cache("target").expect("缓存事实").activity.is_err());
    }
}
