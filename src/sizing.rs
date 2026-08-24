//! 体积统计。
//!
//! 这块的难点全在**口径**上，三条各对应一次「报告数字对不上」的实测教训：
//!
//! 1. 用 `st_blocks * 512` 而不是 `len()`。稀疏文件的逻辑长度可以远大于实际占用，
//!    而我们要回答的问题是「删了能腾出多少」，不是「文件自称多大」。
//! 2. 按 `(st_dev, st_ino)` 去重。硬链接（pnpm store、cargo 的部分产物）会让同一份
//!    物理数据被反复计数 —— 这是「可回收 168GB」这类系统性高估的主因之一。
//! 3. 不跟随符号链接。跟过去会把链接目标算进来，而目标可能在别的卷上、
//!    也可能根本不归这个 worktree 管，删掉 worktree 一个字节都不会释放。
//!
//! 即便这样，结果仍然只是**上界**：某个文件剩下的硬链接若在树外
//! （pnpm 的 node_modules 恰好就是这个形状），删掉这棵树并不释放它的块。
//! 所以 [`crate::platform::disk`] 才要在执行前后各读一次可用空间报实测差值，
//! 而把这里的数字标成「约」。

use crate::model::Cause;
use jwalk::{Parallelism, WalkDir};
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

/// 比 jwalk 默认的 1 秒宽松。这个超时一旦触发，整个目录就测不出体积了，
/// 代价远大于多等几秒。
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// 目录占用的字节数。
pub fn dir_size(path: &std::path::Path) -> Result<u64, crate::model::Cause> {
    walk_size(path, Parallelism::RayonDefaultPool { busy_timeout: BUSY_TIMEOUT })
}

/// 批量统计，内部并行。顺序与输入一致。
pub fn dir_sizes(paths: &[std::path::PathBuf]) -> Vec<Result<u64, crate::model::Cause>> {
    // 内层刻意用 Serial：外层这些闭包已经跑在全局 rayon 池的 worker 上，
    // 内层再往同一个池 spawn，活会排在队尾没人取（worker 都阻塞在等待里），
    // 撞上 jwalk 的忙检测后遍历直接中止。并行度由路径条数提供，够用。
    //
    // par_iter().collect() 对索引型迭代器保序，调用方可以按下标对回输入。
    paths.par_iter().map(|p| walk_size(p, Parallelism::Serial)).collect()
}

fn walk_size(path: &Path, parallelism: Parallelism) -> Result<u64, Cause> {
    // 根路径本身读不到必须是错误：返回 0 会被上层当成「这里没什么可回收」而静默跳过，
    // 正是要避免的 fail-open 形状。
    std::fs::symlink_metadata(path)
        .map_err(|e| Cause::Io { path: path.to_path_buf(), msg: e.to_string() })?;

    // 去重集合由本次调用独占 —— dir_sizes 的各个分支之间不共享任何状态，也就不需要锁。
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut total: u64 = 0;

    let walk = WalkDir::new(path)
        // jwalk 默认跳过点开头的条目。`.next` / `.venv` / `.turbo` 全是点开头，
        // 用默认值等于把要统计的大头整个漏掉。
        .skip_hidden(false)
        .follow_links(false)
        .sort(false)
        .parallelism(parallelism);

    for item in walk {
        let entry = match item {
            Ok(e) => e,
            // 线程池占满时 jwalk 会中止遍历，剩下的子树根本没走过。
            // 当成「单个条目失败」跳过就会报出一个荒谬的小数字 —— 必须是整体失败。
            Err(e) if e.is_busy() => {
                return Err(Cause::Io { path: path.to_path_buf(), msg: e.to_string() });
            }
            // 单个条目失败（权限不足、遍历途中被别的进程删掉）只损失它自己那点体积。
            Err(_) => continue,
        };

        // 目录自身的块数是文件系统的记账方式，随 FS 而异（APFS 恒为 0，ext4 是 4K 起）。
        // 算进去会让同一棵树在不同机器上给出不同数字，连「空目录」都不是 0。
        // 少报是安全方向，跳过。
        if entry.file_type().is_dir() {
            continue;
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        let (bytes, id) = probe(&meta);
        // 同一份物理数据的第二个硬链接：块只有一份，不能再算一次。
        if let Some(id) = id
            && !seen.insert(id)
        {
            continue;
        }
        total = total.saturating_add(bytes);
    }

    Ok(total)
}

/// 一个条目的实际占用字节数，以及（仅当它可能被重复遇到时）它的物理身份。
#[cfg(unix)]
fn probe(meta: &std::fs::Metadata) -> (u64, Option<(u64, u64)>) {
    use std::os::unix::fs::MetadataExt;

    let bytes = meta.blocks().saturating_mul(512);
    // 只有多链接的文件才可能在同一棵树里被遇到两次。单链接的不进集合，
    // 省掉 node_modules 那种几十万条目规模下的哈希表内存。
    let id = if meta.nlink() > 1 { Some((meta.dev(), meta.ino())) } else { None };
    (bytes, id)
}

/// 非 unix 平台拿不到块数与 inode：退回逻辑长度，且无法去重。
#[cfg(not(unix))]
fn probe(meta: &std::fs::Metadata) -> (u64, Option<(u64, u64)>) {
    (meta.len(), None)
}
