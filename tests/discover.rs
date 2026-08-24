#![allow(clippy::expect_used)]
//! 仓库发现的回归测试。
//!
//! 全部用真 git 造仓：这里要验的恰好都是 git 行为的细节——
//! linked worktree 的 `.git` 是文件而非目录、主 worktree 恒为列表首条、
//! 目录被删后注册记录变 prunable。mock 掉 git 等于测自己编的故事。

use std::path::{Path, PathBuf};
use wtgc::config::ScanConfig;
use wtgc::discover::{default_seeds, discover};
use wtgc::testkit::{TempRepo, test_git};

/// 造一个只有一次提交的仓库。
fn repo_with_commit() -> TempRepo {
    let r = TempRepo::new();
    r.write("a.txt", "x");
    r.commit("init");
    r
}

/// 在任意绝对路径处挂一个该仓的 linked worktree（`TempRepo::worktree` 只能挂在仓内）。
fn worktree_at(r: &TempRepo, at: &Path) -> PathBuf {
    let head = r.head();
    r.worktree_at(at, &head)
}

fn cfg(repos: Vec<PathBuf>, seeds: Vec<PathBuf>) -> ScanConfig {
    ScanConfig {
        repos,
        seeds,
        ..ScanConfig::default()
    }
}

fn roots(found: &[wtgc::discover::Discovered]) -> Vec<PathBuf> {
    found.iter().map(|d| d.root.clone()).collect()
}

/// 显式声明一个根，应当展开出主 worktree + 全部 linked worktree。
#[test]
fn explicit_repo_expands_whole_family() {
    let r = repo_with_commit();
    let wt1 = r.worktree("wt1", &r.head());
    let wt2 = r.worktree("wt2", &r.head());

    let found = discover(&cfg(vec![r.root.clone()], vec![]), &test_git());

    assert_eq!(
        roots(&found),
        vec![r.root.clone()],
        "应当只发现这一个仓，且 root 是主仓根"
    );
    let paths: Vec<PathBuf> = found[0].worktrees.iter().map(|w| w.path.clone()).collect();
    assert_eq!(paths.len(), 3, "主 worktree + 两个 linked，实际 {paths:?}");
    assert_eq!(paths[0], r.root, "主 worktree 必须是首条");
    assert!(
        paths.contains(&wt1) && paths.contains(&wt2),
        "两个 linked 都要在，实际 {paths:?}"
    );
    assert!(found[0].prunable.is_empty(), "没有陈旧记录");
}

/// 从 linked worktree 那一侧问，也应当解析出同一个主仓根。
#[test]
fn expanding_from_a_linked_worktree_yields_the_main_root() {
    let r = repo_with_commit();
    let wt = r.worktree("wt", &r.head());

    let found = discover(&cfg(vec![wt], vec![]), &test_git());

    assert_eq!(
        roots(&found),
        vec![r.root.clone()],
        "root 应是主仓根而非被指定的那个 worktree"
    );
}

/// 目录被删掉、注册记录还在的 worktree 归入 prunable，不混进 worktrees——
/// 它没有磁盘可回收，让每道门禁对着一个不存在的路径判「判不准」纯属噪音。
#[test]
fn prunable_entries_are_separated() {
    let r = repo_with_commit();
    let alive = r.worktree("alive", &r.head());
    let gone = r.worktree("gone", &r.head());
    std::fs::remove_dir_all(&gone).expect("删掉 worktree 目录");

    let found = discover(&cfg(vec![r.root.clone()], vec![]), &test_git());

    assert_eq!(found.len(), 1);
    let paths: Vec<PathBuf> = found[0].worktrees.iter().map(|w| w.path.clone()).collect();
    assert_eq!(
        paths,
        vec![r.root.clone(), alive],
        "已消失的那个不该出现在 worktrees 里"
    );
    assert_eq!(
        found[0].prunable.len(),
        1,
        "陈旧记录应当被单独收走，实际 {:?}",
        found[0].prunable
    );
    assert!(
        found[0].prunable[0].ends_with("gone"),
        "prunable 记的应是消失的那个路径，实际 {:?}",
        found[0].prunable[0]
    );
}

/// 种子扫描：linked worktree 的 `.git` 是**文件**，只判目录会整个漏掉。
#[test]
fn seed_scan_finds_worktree_whose_dot_git_is_a_file() {
    let r = repo_with_commit();
    let seed = tempfile::tempdir().expect("建种子目录");
    let seed_root = seed.path().canonicalize().expect("canonicalize");
    let wt = worktree_at(&r, &seed_root.join("agent-a"));

    assert!(
        wt.join(".git").is_file(),
        "前提：linked worktree 的 .git 是文件"
    );

    let found = discover(&cfg(vec![], vec![seed_root]), &test_git());

    assert_eq!(
        roots(&found),
        vec![r.root.clone()],
        "种子里的 worktree 应解析回主仓根"
    );
}

/// 同一个仓既被显式声明、又被种子扫到，只出现一次。
#[test]
fn repo_listed_and_seeded_appears_once() {
    let r = repo_with_commit();
    let seed = tempfile::tempdir().expect("建种子目录");
    let seed_root = seed.path().canonicalize().expect("canonicalize");
    worktree_at(&r, &seed_root.join("agent-a"));
    worktree_at(&r, &seed_root.join("agent-b"));

    let found = discover(&cfg(vec![r.root.clone()], vec![seed_root]), &test_git());

    assert_eq!(
        roots(&found),
        vec![r.root.clone()],
        "去重后只该有一条，实际 {:?}",
        roots(&found)
    );
    assert_eq!(
        found[0].worktrees.len(),
        3,
        "全家仍应完整：主 + 两个 agent worktree"
    );
}

/// 种子目录里的普通目录不该被当成仓库。
#[test]
fn plain_directories_are_not_reported_as_repos() {
    let seed = tempfile::tempdir().expect("建种子目录");
    let base = seed.path();
    std::fs::create_dir_all(base.join("notes/2026")).expect("建目录");
    std::fs::write(base.join("notes/todo.md"), "not a repo").expect("写文件");
    std::fs::create_dir_all(base.join("downloads")).expect("建目录");

    let found = discover(&cfg(vec![], vec![base.to_path_buf()]), &test_git());

    assert!(
        found.is_empty(),
        "普通目录不该产生仓库，实际 {:?}",
        roots(&found)
    );
}

/// 剪枝：命中 `.git` 后不再下探。
///
/// 造一个「worktree 里嵌着另一个仓的 worktree」的现场（Claude Code 把 worktree
/// 建在 `<repo>/.claude/worktrees/` 下就是这个形状）。内层属于**另一个**仓，
/// 所以按主仓根去重救不了——只有真的剪枝了，结果才是一条。
#[test]
fn pruning_stops_at_the_first_workspace() {
    let outer = repo_with_commit();
    let inner = repo_with_commit();
    let seed = tempfile::tempdir().expect("建种子目录");
    let seed_root = seed.path().canonicalize().expect("canonicalize");

    let wt = worktree_at(&outer, &seed_root.join("agent-a"));
    let nested = worktree_at(&inner, &wt.join(".claude").join("worktrees").join("x"));
    assert!(nested.join(".git").is_file(), "前提：内层也是个真 worktree");

    let found = discover(&cfg(vec![], vec![seed_root]), &test_git());

    assert_eq!(
        roots(&found),
        vec![outer.root.clone()],
        "命中 agent-a 就该止步，内层不该被当成独立仓库，实际 {:?}",
        roots(&found)
    );
}

/// 限深：埋得太深的仓库不发现——种子扫描不能退化成全盘扫描。
#[test]
fn seed_scan_is_depth_limited() {
    let r = repo_with_commit();
    let seed = tempfile::tempdir().expect("建种子目录");
    let seed_root = seed.path().canonicalize().expect("canonicalize");
    let deep = seed_root.join("a").join("b").join("c").join("d").join("e");
    std::fs::create_dir_all(&deep).expect("建深目录");
    worktree_at(&r, &deep.join("wt"));

    let found = discover(&cfg(vec![], vec![seed_root]), &test_git());

    assert!(
        found.is_empty(),
        "超出深度限制的仓库不该被发现，实际 {:?}",
        roots(&found)
    );
}

/// 跳过表生效：不为了找仓库去遍历构建产物目录。
#[test]
fn build_output_directories_are_skipped() {
    let r = repo_with_commit();
    let seed = tempfile::tempdir().expect("建种子目录");
    let seed_root = seed.path().canonicalize().expect("canonicalize");
    let buried = seed_root.join("target").join("tmp");
    std::fs::create_dir_all(&buried).expect("建目录");
    worktree_at(&r, &buried.join("wt"));

    let found = discover(&cfg(vec![], vec![seed_root]), &test_git());

    assert!(
        found.is_empty(),
        "target/ 下面不该去遍历，实际 {:?}",
        roots(&found)
    );
}

/// 一个仓出错不能让整轮发现失败。
#[test]
fn a_bad_path_does_not_sink_the_rest() {
    let r = repo_with_commit();
    let not_a_repo = tempfile::tempdir().expect("建临时目录");

    let repos = vec![
        PathBuf::from("/definitely/not/here/xyzzy"), // 路径不存在 → git 起不来
        not_a_repo.path().to_path_buf(),             // 存在但不是 git 仓
        r.root.clone(),
    ];
    let found = discover(&cfg(repos, vec![]), &test_git());

    assert_eq!(
        roots(&found),
        vec![r.root.clone()],
        "好的那个仓必须照样发现"
    );
}

/// 种子目录本身就是工作区的情形（`~/.codex/worktrees` 被直接指到某个 worktree 上）。
#[test]
fn the_seed_directory_itself_can_be_a_workspace() {
    let r = repo_with_commit();

    let found = discover(&cfg(vec![], vec![r.root.clone()]), &test_git());

    assert_eq!(roots(&found), vec![r.root.clone()]);
}

/// 默认种子只返回真实存在的目录，且必然包含 `$TMPDIR`。
#[test]
fn default_seeds_are_existing_directories() {
    let seeds = default_seeds();

    assert!(
        seeds.iter().all(|p| p.is_dir()),
        "不该返回不存在的路径，实际 {seeds:?}"
    );
    let tmp = std::env::temp_dir().canonicalize().expect("canonicalize");
    assert!(
        seeds
            .iter()
            .any(|p| p.canonicalize().map(|c| c == tmp).unwrap_or(false)),
        "$TMPDIR 是已知的 agent 落点，实际 {seeds:?}"
    );
}
