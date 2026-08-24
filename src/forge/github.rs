//! GitHub provider，经 `gh` CLI 查询。
//!
//! 这里有两个实测踩出来的坑，都会导致**把未合入的分支误判为已合入**：
//!
//! 1. **`gh` 按当前工作目录解析仓库。** 从 A 仓的目录去查 B 仓的分支，
//!    会拿 B 的分支名到 A 仓里找，永远查不到 —— 表现为 squash-merge 的分支
//!    被一直判成"未进主干"。实测因此误扣留了 35GB。所以必须显式把 cwd 设成目标仓。
//!
//! 2. **`--head <branch>` 会匹配任意 fork 的同名分支。** 实测
//!    `gh pr list -R cli/cli --head patch-1 --state merged` 返回 8 个分属不同 owner 的 PR。
//!    取 `.[0]` 而不校验 SHA，意味着"别人合过的 fix/typo"会把你自己未合入的同名分支判成已落地。
//!    所以必须比对 `headRefOid` 与本地 HEAD，对不上一律不算。

use crate::gates::MergeStatusProvider;
use crate::model::Cause;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct GhCli {
    exe: PathBuf,
}

impl GhCli {
    /// 找不到 `gh` 时返回 None，调用方退回 [`crate::forge::Offline`]。
    ///
    /// 注意不能依赖继承的 PATH：实测 `launchctl getenv PATH` 为空，
    /// 而 `gh` 只装在 `/opt/homebrew/bin`，从 launchd 启动时根本看不到它。
    pub fn detect() -> Option<Self> {
        crate::git::exe::resolve("gh").map(|exe| Self { exe })
    }
}

impl MergeStatusProvider for GhCli {
    fn merged_pr(&self, repo: &Path, branch: &str, head_oid: &str) -> Result<Option<u64>, Cause> {
        let out = Command::new(&self.exe)
            .current_dir(repo) // ← 坑 1：gh 按 cwd 解析仓库
            .args([
                "pr",
                "list",
                "--head",
                branch,
                "--state",
                "merged",
                "--limit",
                "20",
                "--json",
                "number,headRefOid",
            ])
            .output()
            .map_err(|e| Cause::Io {
                path: repo.to_path_buf(),
                msg: e.to_string(),
            })?;

        if !out.status.success() {
            return Err(Cause::ForgeUnavailable {
                detail: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }

        let parsed: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(|e| Cause::ForgeUnavailable {
                detail: format!("gh 输出不是合法 JSON: {e}"),
            })?;

        let Some(items) = parsed.as_array() else {
            return Err(Cause::ForgeUnavailable {
                detail: "gh 输出不是数组".into(),
            });
        };

        // ← 坑 2：必须逐条比对 headRefOid，不能取 .[0]
        for it in items {
            let oid = it
                .get("headRefOid")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if oid == head_oid {
                let n = it.get("number").and_then(|v| v.as_u64());
                return Ok(n);
            }
        }
        Ok(None)
    }
}
