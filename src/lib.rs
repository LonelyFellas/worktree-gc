//! wtgc —— 安全回收 AI coding agent 留下的 git worktree 所占磁盘。
//!
//! 三段式：`scan`（只读）→ `plan`（选定并冻结指纹）→ `apply`（执行前复检）。
//!
//! 设计要点见 `docs/design.md`。一条贯穿全局的规矩：
//! **失败方向不对称** —— 漏删只是少省几 GB，误删可能永久丢数据，
//! 所以任何判不准的情况一律落 [`model::GateStatus::Unknown`]，绝不放行。

pub mod apply;
pub mod config;
pub mod discover;
pub mod forge;
pub mod fsops;
pub mod gates;
pub mod git;
pub mod model;
pub mod plan;
pub mod platform;
pub mod report;
pub mod scan;
pub mod sizing;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
