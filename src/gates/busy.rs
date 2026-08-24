//! A1 —— 目录当前无进程占用。
//!
//! **A 组唯一真正承重的门禁。** 回收一个正在被 `cargo build` 写入的 target，
//! 结果是构建中断加一堆半成品产物，比不回收糟得多。
//!
//! 进程检测的实现见 `platform::procs` —— 必须同时看 cwd 与命令行。

use crate::gates::{Gate, GateCtx};
use crate::model::{GateDetail, GateId, GateStatus};

pub struct BusyGate;

impl Gate for BusyGate {
    fn id(&self) -> GateId {
        GateId::Busy
    }

    fn evaluate(&self, ctx: &GateCtx<'_>) -> GateStatus {
        match ctx.procs.processes_under(ctx.worktree) {
            Ok(ps) if ps.is_empty() => GateStatus::Pass,
            Ok(ps) => GateStatus::Blocked(GateDetail::ProcessesActive {
                pids: ps.iter().map(|p| p.pid).collect(),
                sample: ps.iter().take(5).map(|p| p.name.clone()).collect(),
            }),
            // 判不准就说判不准。返回空集当作「没占用」正是 D11 那个假阴性的形状。
            Err(c) => GateStatus::Unknown(c),
        }
    }
}
