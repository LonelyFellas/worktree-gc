//! B5 —— 不处于 rebase / merge / cherry-pick / bisect 中间态。
//!
//! 这道门专治 D9：detached HEAD + rebase 中途时，工作树往往恰好是干净的
//! （冲突已解、还没 `--continue`），HEAD 又停在基线的祖先上，于是 B1 Dirty 与
//! B2 Landed 会**同时放行**。此刻删掉 worktree，销毁的是 sequencer 里那份还没走完的
//! 待办，以及该 worktree 独有的 HEAD reflog —— 后者是那些 detached 提交的唯一句柄，
//! 删完 git 也找不回来。
//!
//! 判据只看标记文件的存在性，不解析其内容：格式是 git 的内部实现，会随版本变；
//! 而「有没有这个标记」是 git 自己判断中间态的同一依据，稳定得多。

use crate::gates::{Gate, GateCtx};
use crate::model::{Cause, GateDetail, GateId, GateStatus};
use std::path::{Path, PathBuf};

pub struct InProgressGate;

/// `(标记名, 报告里显示的操作名)`。
///
/// 顺序即优先级：rebase 中途的 git 目录里还会躺着 `MERGE_MSG` 之类的残留，
/// 先报最外层那个操作，才对得上用户脑子里「我正在 rebase」的认知。
const MARKERS: [(&str, &str); 7] = [
    ("rebase-merge", "rebase"),
    ("rebase-apply", "rebase"),
    ("MERGE_HEAD", "merge"),
    ("CHERRY_PICK_HEAD", "cherry-pick"),
    ("REVERT_HEAD", "revert"),
    ("BISECT_LOG", "bisect"),
    ("sequencer", "sequencer"),
];

impl Gate for InProgressGate {
    fn id(&self) -> GateId {
        GateId::InProgress
    }

    fn evaluate(&self, ctx: &GateCtx<'_>) -> GateStatus {
        // linked worktree 的 `<wt>/.git` 是一个**文件**，中间态标记落在
        // `<main>/.git/worktrees/<name>/` 下。拼路径猜名字迟早猜错
        // （worktree 目录名与注册名可以不一致），只有问 git 才可靠。
        let git_dir = match ctx.git.run_ok(ctx.worktree, &["rev-parse", "--absolute-git-dir"]) {
            Ok(out) => {
                let s = out.stdout_utf8().trim().to_string();
                // 空输出若当成空路径用，后面的 join 会相对进程 cwd 解析，
                // 于是「没找到标记」就成了 Pass —— 典型的 fail-open，宁可判不准。
                if s.is_empty() {
                    return GateStatus::Unknown(Cause::CommandFailed {
                        cmd: "git rev-parse --absolute-git-dir".into(),
                        code: Some(0),
                        stderr: "命令成功但没有输出 git 目录路径".into(),
                    });
                }
                PathBuf::from(s)
            }
            Err(c) => return GateStatus::Unknown(c),
        };

        for (marker, kind) in MARKERS {
            let path = git_dir.join(marker);
            match marker_exists(&path) {
                Ok(true) => return GateStatus::Blocked(GateDetail::OperationInProgress { kind }),
                Ok(false) => {}
                Err(e) => {
                    return GateStatus::Unknown(Cause::Io { path, msg: e.to_string() });
                }
            }
        }

        GateStatus::Pass
    }
}

/// 只把「确实不存在」当作没有标记；权限不足、路径异常一律上报错误。
///
/// 用 `symlink_metadata` 而非 `metadata`：悬空软链在后者眼里等于不存在，
/// 而一个叫 `MERGE_HEAD` 的悬空软链正是「这里不对劲」的信号，该拦。
fn marker_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
