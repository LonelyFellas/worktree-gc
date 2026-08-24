//! A3 —— 目标确实是可重建的构建产物。
//!
//! 名字对不代表能删。原型只用 `[ -d "$wt/target" ]` 就 `rm -rf`，
//! 于是入库的 `dist/`、手改过的 Maven `target/` 都会被当成缓存删掉（D8）。
//!
//! 五条不变量必须**同时**成立，缺一不放行。

use crate::gates::{Gate, GateCtx};
use crate::git::porcelain::split_z;
use crate::model::{CacheUnsafeReason, Cause, GateDetail, GateId, GateStatus};
use std::path::{Path, PathBuf};

pub struct CacheSafeGate {
    /// 相对 worktree 根的缓存目录名。
    pub dir: String,
}

impl Gate for CacheSafeGate {
    fn id(&self) -> GateId {
        GateId::CacheSafe
    }

    fn evaluate(&self, ctx: &GateCtx<'_>) -> GateStatus {
        let target = ctx.worktree.join(&self.dir);

        // ① 必须匹配已知规则 —— 未知目录不当缓存处理
        if !ctx.cfg.cache_rules.iter().any(|r| r.dir == self.dir) {
            return blocked(CacheUnsafeReason::NoMatchingRule);
        }

        // ② 不能是符号链接：删它可能波及链接目标之外的东西
        match std::fs::symlink_metadata(&target) {
            Ok(m) if m.file_type().is_symlink() => {
                return blocked(CacheUnsafeReason::IsSymlink);
            }
            Ok(_) => {}
            Err(e) => {
                return GateStatus::Unknown(Cause::Io {
                    path: target.clone(),
                    msg: e.to_string(),
                });
            }
        }

        // ③ canonicalize 后必须仍在 worktree 根之下
        match (target.canonicalize(), ctx.worktree.canonicalize()) {
            (Ok(rt), Ok(rw)) if !rt.starts_with(&rw) => {
                return blocked(CacheUnsafeReason::EscapesWorktree { resolved: rt });
            }
            (Ok(_), Ok(_)) => {}
            (a, b) => {
                let p: PathBuf = a.unwrap_or_else(|_| target.clone());
                let msg = b.err().map(|e| e.to_string()).unwrap_or_else(|| "canonicalize 失败".into());
                return GateStatus::Unknown(Cause::Io { path: p, msg });
            }
        }

        // ④ 必须确实被 gitignore —— 没被忽略说明它可能是源码
        match ctx.git.run_bool(ctx.worktree, &["check-ignore", "-q", &self.dir]) {
            Ok(true) => {}
            Ok(false) => return blocked(CacheUnsafeReason::NotIgnored),
            Err(c) => return GateStatus::Unknown(c),
        }

        // ⑤ 目录内不得有任何被 git 跟踪的文件（入库的 dist/ 就栽在这条）
        match ctx.git.run_ok(ctx.worktree, &["ls-files", "-z", "--", &self.dir]) {
            Ok(out) => {
                let tracked = split_z(&out.stdout);
                if !tracked.is_empty() {
                    return blocked(CacheUnsafeReason::ContainsTrackedFiles {
                        sample: tracked.into_iter().take(5).collect(),
                    });
                }
            }
            Err(c) => return GateStatus::Unknown(c),
        }

        GateStatus::Pass
    }
}

fn blocked(reason: CacheUnsafeReason) -> GateStatus {
    GateStatus::Blocked(GateDetail::NotPureCache { reason })
}


/// 找出该 worktree 下所有匹配规则、且实际存在的缓存目录名。
pub fn candidates(worktree: &Path, cfg: &crate::config::ScanConfig) -> Vec<String> {
    cfg.cache_rules
        .iter()
        .filter(|r| worktree.join(&r.dir).is_dir())
        .map(|r| r.dir.clone())
        .collect()
}
