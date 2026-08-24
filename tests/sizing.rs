#![allow(clippy::expect_used)]
//! 体积统计的口径回归测试。
//!
//! 每条断言对着一种具体的「数字报大了」机制：稀疏文件、硬链接、软链跟随。
//! 这些不是覆盖率测试 —— 原型阶段正是靠 `du` 报出 168GB，实际执行完只多出几十 GB。

use std::path::{Path, PathBuf};
use wtgc::model::Cause;
use wtgc::sizing::{dir_size, dir_sizes};

const KIB: u64 = 1024;
#[cfg(unix)]
const MIB: u64 = 1024 * 1024;

/// 块对齐会让统计值略大于逻辑长度，留一格余量做上界。
const SLACK: u64 = 64 * KIB;

fn write_file(path: &Path, len: u64) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("建父目录");
    }
    std::fs::write(path, vec![b'x'; usize::try_from(len).expect("长度")]).expect("写文件");
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().expect("建临时目录")
}

#[test]
fn empty_dir_is_zero() {
    let d = tmp();
    assert_eq!(dir_size(d.path()), Ok(0), "空目录不该凭空产生可回收字节");
}

#[test]
fn counts_a_known_file() {
    let d = tmp();
    write_file(&d.path().join("blob.bin"), 100 * KIB);

    let n = dir_size(d.path()).expect("应能统计");
    assert!(n >= 100 * KIB, "少算了：{n} < {}", 100 * KIB);
    assert!(n < 100 * KIB + SLACK, "多算了：{n}");
}

#[test]
fn recurses_into_nested_dirs() {
    let d = tmp();
    write_file(&d.path().join("top.bin"), 100 * KIB);
    write_file(&d.path().join("a/b/c/deep.bin"), 300 * KIB);

    let n = dir_size(d.path()).expect("应能统计");
    assert!(n >= 400 * KIB, "嵌套子目录没被递归统计：{n}");
    assert!(n < 400 * KIB + SLACK, "多算了：{n}");
}

/// jwalk 默认跳过点开头的条目，而要统计的 `.next` / `.venv` / `.turbo` 全是点开头 ——
/// 用默认值等于把大头整个漏掉。
#[test]
fn dot_prefixed_caches_are_counted() {
    let d = tmp();
    write_file(&d.path().join(".next/chunks/app.js"), 128 * KIB);

    let n = dir_size(d.path()).expect("应能统计");
    assert!(n >= 128 * KIB, "点开头的缓存目录被跳过了：{n}");
}

/// 硬链接：同一份物理数据两个名字，块只有一份。
/// 不去重就会把 pnpm store / cargo 产物这类目录的体积成倍报大。
#[test]
fn hard_links_are_counted_once() {
    let d = tmp();
    let original = d.path().join("original.bin");
    write_file(&original, 256 * KIB);
    std::fs::hard_link(&original, d.path().join("linked.bin")).expect("建硬链接");

    let n = dir_size(d.path()).expect("应能统计");
    assert!(n >= 256 * KIB, "少算了：{n}");
    assert!(n < 256 * KIB + SLACK, "硬链接被重复计数：{n} 接近两份 256K");
}

/// 软链接不跟随：目标可能在别的卷上，删这棵树一个字节都不释放。
#[cfg(unix)]
#[test]
fn symlinks_are_not_followed() {
    let d = tmp();
    let outside = d.path().join("outside");
    let inside = d.path().join("inside");
    std::fs::create_dir_all(&inside).expect("建目录");
    write_file(&outside.join("big.bin"), 4 * MIB);

    // 两种形状都要拦住：软链到大文件、软链到装着大文件的目录（后者还会引出无谓的递归）。
    std::os::unix::fs::symlink(outside.join("big.bin"), inside.join("file-link"))
        .expect("建文件软链");
    std::os::unix::fs::symlink(&outside, inside.join("dir-link")).expect("建目录软链");

    let n = dir_size(&inside).expect("应能统计");
    assert!(n < SLACK, "跟随了软链，把 4MiB 的目标算进来了：{n}");
}

/// 稀疏文件的 `len()` 远大于实际占用。用 len 统计会报出根本不存在的可回收空间。
/// 前提是文件系统支持稀疏文件（APFS / ext4 都支持）。
#[cfg(unix)]
#[test]
fn sparse_file_counts_blocks_not_length() {
    let d = tmp();
    let sparse = d.path().join("sparse.bin");
    let f = std::fs::File::create(&sparse).expect("建文件");
    f.set_len(64 * MIB).expect("撑出稀疏长度");
    drop(f);

    let logical = std::fs::metadata(&sparse).expect("读元数据").len();
    assert_eq!(logical, 64 * MIB, "前提不成立：文件逻辑长度应为 64MiB");

    let n = dir_size(d.path()).expect("应能统计");
    assert!(n < 32 * MIB, "按 len() 统计了稀疏文件的空洞：{n}");
}

#[test]
fn missing_path_is_an_error_not_zero() {
    // 返回 0 会被上层当成「这里没什么可回收」而静默跳过 —— 必须是 Cause。
    match dir_size(Path::new("/definitely/not/here/xyzzy-wtgc")) {
        Err(Cause::Io { .. }) => {}
        other => panic!("不存在的路径必须落 Cause::Io，实际 {other:?}"),
    }
}

/// 批量统计必须严格保序，否则调用方按下标把体积对回 worktree 时会全线错位。
#[test]
fn dir_sizes_preserves_input_order() {
    let d = tmp();
    let small = d.path().join("small");
    let big = d.path().join("big");
    let empty = d.path().join("empty");
    write_file(&small.join("s.bin"), 8 * KIB);
    write_file(&big.join("b.bin"), 512 * KIB);
    std::fs::create_dir_all(&empty).expect("建目录");
    let missing = PathBuf::from("/definitely/not/here/xyzzy-wtgc");

    let paths = vec![small, big, empty, missing];
    let got = dir_sizes(&paths);

    assert_eq!(got.len(), paths.len(), "输出条数应与输入一致");
    match &got[0] {
        Ok(n) => assert!(
            *n >= 8 * KIB && *n < 8 * KIB + SLACK,
            "第 0 位应是 small：{n}"
        ),
        other => panic!("第 0 位应成功，实际 {other:?}"),
    }
    match &got[1] {
        Ok(n) => assert!(
            *n >= 512 * KIB && *n < 512 * KIB + SLACK,
            "第 1 位应是 big：{n}"
        ),
        other => panic!("第 1 位应成功，实际 {other:?}"),
    }
    assert_eq!(got[2], Ok(0), "第 2 位应是空目录");
    // 失败的那位留在原处，不能把成功的结果压紧顶上来。
    assert!(
        matches!(&got[3], Err(Cause::Io { .. })),
        "第 3 位应是错误，实际 {:?}",
        got[3]
    );
}

#[test]
fn dir_sizes_of_empty_input_is_empty() {
    assert!(dir_sizes(&[]).is_empty());
}
