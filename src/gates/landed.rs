//! B2 —— 工作已进主干。
//!
//! B 组最难的一道。难点全在 squash-merge：**squash 之后分支的提交在主干里
//! 没有任何对应的 oid**，祖先判定必然失败；`git cherry` 也救不了——
//! 单提交 squash 它还能标出 `-`，多提交 squash 时两条会全标 `+`，等于没说。
//!
//! 所以判据三级降级：
//!
//! 1. **祖先判定**：分支尖端本身就在主干历史里。覆盖 merge commit 工作流，最快最可靠。
//! 2. **forge 查询**：squash / rebase 合入后，PR 状态是唯一权威答案。
//! 3. **路径受限 diff（离线）**：问「分支改过的那些文件，内容是否已经在主干里」——
//!    这是唯一不依赖 oid、也不依赖网络的判据。
//!
//! 第二级失败（断网、没装 gh、没鉴权）**不直接判不准**：网络不可达不该让整个判定瘫痪，
//! 先让第三级试一把。但此时第三级说「还有差异」只能落 `Unknown` 而不是 `Blocked`——
//! 已合入的 PR 完全可能因为主干后续又动了同一批文件而在离线视角里显示有差异，
//! 我们没资格把「查不到」说成「没合入」。

use crate::gates::{Gate, GateCtx};
use crate::git::porcelain::split_z;
use crate::model::{Cause, GateDetail, GateId, GateStatus};

pub struct LandedGate;

impl Gate for LandedGate {
    fn id(&self) -> GateId {
        GateId::Landed
    }

    fn evaluate(&self, ctx: &GateCtx<'_>) -> GateStatus {
        // 基线探测失败时整仓标红（D12）。这里最容易顺手写成「没基线就别拦着」，
        // 那正是把「根本没扫」伪装成「没东西可清」的形状。
        let Some(baseline) = ctx.baseline else {
            return GateStatus::Unknown(Cause::CommandFailed {
                cmd: "detect baseline".into(),
                code: None,
                stderr: "未知主干基线，无从判断工作是否已落地".into(),
            });
        };
        let refname = baseline.refname();

        // ① 祖先判定：0=是，1=否。这两级 refs 都读不动的话，后面的判据同样不可信 → Unknown
        match ctx.git.run_bool(
            ctx.worktree,
            &["merge-base", "--is-ancestor", "HEAD", &refname],
        ) {
            Ok(true) => return GateStatus::Pass,
            Ok(false) => {}
            Err(c) => return GateStatus::Unknown(c),
        }

        // ② forge 查询。查不通只记下成因，判定继续往下降级
        let mut forge_err: Option<Cause> = None;
        if let Some(branch) = ctx.branch {
            match ctx.forge.merged_pr(ctx.repo, branch, ctx.head_oid) {
                Ok(Some(_pr)) => return GateStatus::Pass,
                Ok(None) => {}
                Err(c) => forge_err = Some(c),
            }
        }

        let ahead = match count_ahead(ctx, &refname) {
            Ok(n) => n,
            Err(c) => return GateStatus::Unknown(c),
        };

        // ③ 路径受限 diff
        match landed_by_content(ctx, &refname, ahead) {
            Ok(true) => GateStatus::Pass,
            Ok(false) => match forge_err {
                // forge 没查通时「离线看还有差异」不足以下「没合入」的结论
                Some(c) => GateStatus::Unknown(c),
                None => GateStatus::Blocked(GateDetail::NotLanded {
                    ahead,
                    baseline: refname,
                }),
            },
            Err(c) => GateStatus::Unknown(c),
        }
    }
}

/// 分支相对基线独有的提交数。
fn count_ahead(ctx: &GateCtx<'_>, refname: &str) -> Result<usize, Cause> {
    let range = format!("{refname}..HEAD");
    let out = ctx
        .git
        .run_ok(ctx.worktree, &["rev-list", "--count", &range])?;
    let text = out.stdout_utf8();
    text.trim()
        .parse::<usize>()
        .map_err(|e| Cause::CommandFailed {
            cmd: format!("git rev-list --count {range}"),
            code: Some(0),
            stderr: format!("提交数无法解析（{e}）：{}", text.trim()),
        })
}

/// 第三级：分支改动过的那批文件，在 HEAD 与基线之间是否还有差异。
///
/// 只盯这批文件是关键：主干上的无关提交不该影响判定——「squash 合入后 base 又前进了」
/// 这一形态下，朴素的 `git diff <baseline> HEAD` 与三点 diff 都会失效。
fn landed_by_content(ctx: &GateCtx<'_>, refname: &str, ahead: usize) -> Result<bool, Cause> {
    // 退化陷阱：分支净变更为零（改了又改回去）时，下面的「无差异」会**平凡成立**，
    // 把一个什么都没落地的分支判成已落地。两个非空条件缺一不可。
    if ahead == 0 {
        return Ok(false);
    }

    let merge_base = {
        let out = ctx
            .git
            .run_ok(ctx.worktree, &["merge-base", refname, "HEAD"])?;
        let oid = out.stdout_utf8().trim().to_string();
        if oid.is_empty() {
            return Err(Cause::CommandFailed {
                cmd: format!("git merge-base {refname} HEAD"),
                code: Some(0),
                stderr: "merge-base 输出为空，无法确定分叉点".into(),
            });
        }
        oid
    };

    let out = ctx.git.run_ok(
        ctx.worktree,
        &["diff", "--name-only", "-z", &merge_base, "HEAD"],
    )?;
    let files = split_z(&out.stdout);
    if files.is_empty() {
        return Ok(false);
    }

    // 路径按字面量传：pathspec 默认按 glob 解释，而 `app/[slug]/page.tsx` 这种路径
    // 在前端仓里遍地都是，`[slug]` 会当字符类连带匹配到别的文件，把判据牵到不该看的文件上
    let specs: Vec<String> = files.iter().map(|f| format!(":(literal){f}")).collect();
    // --no-ext-diff：实测 git 2.53 的 --quiet 本就绕开外部 diff，这里显式钉一遍——
    // 判据的退出码不能托付给用户的 diff.external 配置
    let mut args: Vec<&str> = vec!["diff", "--quiet", "--no-ext-diff", refname, "HEAD", "--"];
    args.extend(specs.iter().map(String::as_str));

    // exit 0 = 这些文件已无差异 → 分支的改动（含 squash 形态）确实已存在于基线中
    ctx.git.run_bool(ctx.worktree, &args)
}
