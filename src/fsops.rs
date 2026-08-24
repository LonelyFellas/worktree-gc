//! 文件系统写操作的抽象。
//!
//! 抽出来不是为了跨平台，是为了**能断言"没有发生"**。
//! 这个工具最重要的一类测试是否定性的：dry-run 路径下一次删除都不能发出、
//! `git worktree remove` 失败时不能退化成 `rm -rf`。
//! 只有把写操作收敛到一个 trait 后面，spy 才能证明这些。

use crate::model::Cause;
use std::path::Path;

pub trait FsOps: Send + Sync {
    fn remove_dir_all(&self, path: &Path) -> Result<(), Cause>;
    fn create_dir_all(&self, path: &Path) -> Result<(), Cause>;
    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), Cause>;
    fn exists(&self, path: &Path) -> bool;
}

pub struct RealFs;

impl FsOps for RealFs {
    fn remove_dir_all(&self, path: &Path) -> Result<(), Cause> {
        std::fs::remove_dir_all(path)
            .map_err(|e| Cause::Io { path: path.to_path_buf(), msg: e.to_string() })
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), Cause> {
        std::fs::create_dir_all(path)
            .map_err(|e| Cause::Io { path: path.to_path_buf(), msg: e.to_string() })
    }

    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), Cause> {
        std::fs::copy(from, to)
            .map(|_| ())
            .map_err(|e| Cause::Io { path: from.to_path_buf(), msg: e.to_string() })
    }

    fn exists(&self, path: &Path) -> bool {
        // 用 symlink_metadata 而非 exists：断链的符号链接也是"存在"的东西，
        // 把它当不存在会让 prune 把一条本该保留的注册记录删掉。
        std::fs::symlink_metadata(path).is_ok()
    }
}
