//! 合入状态查询。
//!
//! 只在祖先判定失败时才用得上——也就是 squash-merge 与 rebase-merge 的情形。
//! 网络不可达、未鉴权、没装 CLI 都是常态，所以**这一层的失败不能让整个判定瘫痪**，
//! 由 landed 门禁降级到离线的路径受限 diff。

pub mod github;

use crate::gates::MergeStatusProvider;
use crate::model::Cause;
use std::path::Path;

/// 什么都不查。用于 `--offline`，以及没有可用 provider 时。
pub struct Offline;

impl MergeStatusProvider for Offline {
    fn merged_pr(&self, _repo: &Path, _branch: &str, _oid: &str) -> Result<Option<u64>, Cause> {
        Ok(None)
    }
}
