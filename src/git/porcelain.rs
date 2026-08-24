//! 机器格式解析。**只解析 `--porcelain` / `-z`，不碰人类可读输出。**

use std::path::PathBuf;

/// `git worktree list --porcelain` 的一条记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub locked: Option<String>,
    pub prunable: bool,
}

/// 解析 `git worktree list --porcelain`。
///
/// 记录之间以空行分隔；`branch` 值形如 `refs/heads/foo`，这里剥成 `foo`。
pub fn parse_worktree_list(s: &str) -> Vec<WorktreeEntry> {
    let mut out = Vec::new();
    let mut cur: Option<WorktreeEntry> = None;

    for line in s.lines() {
        if line.is_empty() {
            if let Some(e) = cur.take() {
                out.push(e);
            }
            continue;
        }
        let (key, val) = match line.split_once(' ') {
            Some((k, v)) => (k, v),
            None => (line, ""),
        };
        match key {
            "worktree" => {
                if let Some(e) = cur.take() {
                    out.push(e);
                }
                cur = Some(WorktreeEntry {
                    path: PathBuf::from(val),
                    head: None,
                    branch: None,
                    detached: false,
                    locked: None,
                    prunable: false,
                });
            }
            "HEAD" => {
                if let Some(e) = cur.as_mut() {
                    e.head = Some(val.to_string());
                }
            }
            "branch" => {
                if let Some(e) = cur.as_mut() {
                    e.branch = Some(val.strip_prefix("refs/heads/").unwrap_or(val).to_string());
                }
            }
            "detached" => {
                if let Some(e) = cur.as_mut() {
                    e.detached = true;
                }
            }
            "locked" => {
                if let Some(e) = cur.as_mut() {
                    e.locked = Some(val.to_string());
                }
            }
            "prunable" => {
                if let Some(e) = cur.as_mut() {
                    e.prunable = true;
                }
            }
            _ => {}
        }
    }
    if let Some(e) = cur {
        out.push(e);
    }
    out
}

/// 解析 `git status --porcelain=v1 -z` 的 NUL 分隔输出，返回改动的路径。
///
/// 用 `-z` 而非换行分隔：文件名可能含换行，按行切会算错数量。
pub fn parse_status_z(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = bytes.split(|b| *b == 0).peekable();
    while let Some(rec) = it.next() {
        if rec.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(rec);
        // 形如 "XY path"；重命名/复制（R/C）会紧跟一条独立的源路径记录，跳过它
        let xy: Vec<char> = s.chars().take(2).collect();
        if xy.first() == Some(&'R') || xy.first() == Some(&'C') {
            let _ = it.next();
        }
        if s.len() > 3 {
            out.push(s[3..].to_string());
        }
    }
    out
}

/// 切分 git 的 `-z` NUL 分隔输出。
///
/// 凡是路径可能含空格、换行、引号的地方都必须用 `-z` 而不是按行切——
/// 文件名里带换行在真实仓库里并不罕见，按行切会算错数量甚至切断路径。
pub fn split_z(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .map(|r| String::from_utf8_lossy(r).into_owned())
        .collect()
}

/// 解析 `git ls-files -v`，取出会让本地修改在 `status` 里**完全隐身**的文件（D4）。
///
/// 标签的规则不是「一个字母对应一种标记」，而是两个正交的维度：
/// **字母表示索引状态**（`H` 已缓存、`S` skip-worktree、`M` 未合并 …），
/// **小写表示该条目被打了 assume-unchanged**。
///
/// 所以组合起来有四种形态，实测 git 2.53：
///
/// | 标记 | 标签 |
/// |---|---|
/// | 无 | `H` |
/// | 仅 assume-unchanged | `h` |
/// | 仅 skip-worktree | `S` |
/// | 两者都有 | `s` |
///
/// 最初这里只认 `S` 和 `h`，`s` 从正中间漏过去——而它同样对 status 完全隐身。
/// 判据因此改为：**任意小写标签，或 `S`**。
pub fn parse_ls_files_marked(s: &str) -> Vec<String> {
    s.lines()
        .filter_map(|l| {
            let tag = l.chars().next()?;
            let hidden = tag.is_lowercase() || tag == 'S';
            if hidden { l.get(2..).map(|p| p.to_string()) } else { None }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_worktree_list_with_all_fields() {
        let s = "worktree /a/main\nHEAD abc123\nbranch refs/heads/main\n\n\
                 worktree /a/wt1\nHEAD def456\ndetached\n\n\
                 worktree /a/gone\nHEAD 000\nprunable gitdir file points to non-existent location\n\n\
                 worktree /a/held\nHEAD 111\nbranch refs/heads/x\nlocked busy\n";
        let got = parse_worktree_list(s);
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].branch.as_deref(), Some("main"));
        assert!(got[1].detached);
        assert!(got[2].prunable);
        assert_eq!(got[3].locked.as_deref(), Some("busy"));
    }

    #[test]
    fn status_z_counts_renames_once() {
        // "R  new" 后面紧跟一条源路径记录，不能算成两处改动
        let b = b"R  new.txt\0old.txt\0 M other.txt\0";
        let got = parse_status_z(b);
        assert_eq!(got, vec!["new.txt", "other.txt"]);
    }

    #[test]
    fn status_z_handles_filename_with_newline() {
        let b = b" M we\nird.txt\0";
        assert_eq!(parse_status_z(b), vec!["we\nird.txt"]);
    }

    #[test]
    fn picks_every_tag_that_hides_changes_from_status() {
        // 实测 git 2.53 的四种形态。`s` 是两个标记同时置位时的标签，
        // 早期只认 S/h 的写法会把它漏掉——而它改动后 status 一样是空的。
        let s = "H normal.txt\nS skip.txt\nh assume.txt\ns both.txt\nH other.txt\n";
        assert_eq!(parse_ls_files_marked(s), vec!["skip.txt", "assume.txt", "both.txt"]);
    }

    #[test]
    fn plain_cached_entries_are_not_marked() {
        assert!(parse_ls_files_marked("H a.txt\nH b.txt\n").is_empty());
    }
}
