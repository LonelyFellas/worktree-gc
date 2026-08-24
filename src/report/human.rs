//! 人类可读报告。
//!
//! 排版围绕一个判断展开：**用户扫一眼要拿到的是「现在能安全回收多少」，
//! 然后才是「剩下这些为什么不能动」。** 所以可回收量放汇总行，
//! 而拦下的理由逐条展开——那是他下一步要处理的东西。

use crate::model::*;
use crate::platform::disk::human_bytes;
use std::io::{Result, Write};

pub fn render(report: &ScanReport, w: &mut dyn Write) -> Result<()> {
    for repo in &report.repos {
        render_repo(repo, w)?;
    }
    render_summary(report, w)
}

fn render_repo(repo: &RepoReport, w: &mut dyn Write) -> Result<()> {
    writeln!(w)?;
    match &repo.baseline {
        Some(b) => writeln!(w, "═══ {}  (基线 {})", repo.root.display(), b.refname())?,
        None => {
            // 基线探测失败绝不能静默跳过：用户会把「没扫到」误读成「没东西可清」。
            writeln!(w, "═══ {}", repo.root.display())?;
            writeln!(w, "  ⛔ 未能识别主干分支，本仓未做判定")?;
            if let Some(c) = &repo.baseline_error {
                writeln!(w, "     └ {}", describe_cause(c))?;
            }
            writeln!(
                w,
                "     用 --repo 配合显式基线，或检查该仓是否有可用的远端 HEAD"
            )?;
            return Ok(());
        }
    }

    for wt in &repo.worktrees {
        render_worktree(wt, w)?;
    }
    if !repo.prunable.is_empty() {
        writeln!(
            w,
            "  🧹 有 {} 条陈旧注册记录（目录已消失）",
            repo.prunable.len()
        )?;
    }
    Ok(())
}

fn render_worktree(wt: &WorktreeReport, w: &mut dyn Write) -> Result<()> {
    let name = short_name(&wt.path);
    let branch = wt.branch.as_deref().unwrap_or("detached");
    let size = human_bytes(wt.bytes);

    let (icon, label) = match &wt.verdict {
        Verdict::Removable => ("✅", "可删除"),
        Verdict::CacheReclaimable => ("♻️ ", "可回收缓存"),
        Verdict::Blocked { .. } => ("⏸ ", "保留"),
        Verdict::NeedsAttention { .. } => ("❓", "判不准"),
        Verdict::Protected { .. } => ("🔒", "受保护"),
    };
    writeln!(w, "  {icon} {label}  {size:>7}  {name}  [{branch}]")?;

    // 拦下与判不准的理由要展开——这正是用户要做决策的地方
    for o in &wt.outcomes {
        match &o.status {
            GateStatus::Blocked(d) => writeln!(w, "       └ {}", describe_detail(d))?,
            GateStatus::Unknown(c) => writeln!(
                w,
                "       ❓ {} 判不准：{}",
                o.id.as_str(),
                describe_cause(c)
            )?,
            _ => {}
        }
    }

    // 缓存目录单独列：worktree 不能删，不代表它的构建缓存不能回收
    for c in &wt.caches {
        let blockers: Vec<&GateOutcome> =
            c.outcomes.iter().filter(|o| !o.status.is_pass()).collect();
        if blockers.is_empty() {
            writeln!(
                w,
                "       💡 可回收 {}/ ({})",
                c.kind.name,
                human_bytes(c.bytes)
            )?;
        } else if let Some(first) = blockers.first() {
            let why = match &first.status {
                GateStatus::Blocked(d) => describe_detail(d),
                GateStatus::Unknown(c) => format!("判不准：{}", describe_cause(c)),
                _ => "不适用".into(),
            };
            writeln!(
                w,
                "       ⛔ {}/ ({}) 暂不回收：{why}",
                c.kind.name,
                human_bytes(c.bytes)
            )?;
        }
    }
    Ok(())
}

fn render_summary(report: &ScanReport, w: &mut dyn Write) -> Result<()> {
    let reclaimable: u64 = report
        .repos
        .iter()
        .flat_map(|r| &r.worktrees)
        .map(|wt| wt.reclaimable_bytes())
        .sum();
    let removable: u64 = report
        .repos
        .iter()
        .flat_map(|r| &r.worktrees)
        .filter(|wt| matches!(wt.verdict, Verdict::Removable))
        .map(|wt| wt.bytes)
        .sum();
    let unclear = report
        .repos
        .iter()
        .flat_map(|r| &r.worktrees)
        .filter(|wt| matches!(wt.verdict, Verdict::NeedsAttention { .. }))
        .count();

    writeln!(w)?;
    // 标「约」不是谦虚：APFS 的写时复制会让 du 口径系统性高估，
    // 真实回收量以执行前后的可用空间差值为准。
    writeln!(
        w,
        "约可回收构建缓存 {}，可删除 worktree {}",
        human_bytes(reclaimable),
        human_bytes(removable)
    )?;
    if unclear > 0 {
        writeln!(w, "有 {unclear} 个判不准，已列出缺失的能力——补齐后再跑一次")?;
    }
    writeln!(w, "当前可用空间 {}", human_bytes(report.available_bytes))?;

    for t in &report.tools {
        match (&t.path, &t.version) {
            (Some(p), Some(v)) => writeln!(w, "  {}: {} ({})", t.name, v, p.display())?,
            _ => writeln!(w, "  {}: 未找到", t.name)?,
        }
    }
    Ok(())
}

/// 末两段路径。多个 agent worktree 常同名（都叫 tsz-rust），只显示 basename 分不清。
fn short_name(p: &std::path::Path) -> String {
    let parts: Vec<_> = p.components().rev().take(2).collect();
    parts
        .into_iter()
        .rev()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn describe_detail(d: &GateDetail) -> String {
    match d {
        GateDetail::ProcessesActive { pids, sample } => {
            format!("有进程正在使用（{} 个：{}）", pids.len(), sample.join(", "))
        }
        GateDetail::RecentlyModified { age_secs, .. } => {
            format!("最近 {} 分钟内仍有写入", age_secs / 60)
        }
        GateDetail::NotPureCache { reason } => match reason {
            CacheUnsafeReason::MissingMarker { expected } => {
                format!("缺少项目佐证文件（需要其一：{}）", expected.join("、"))
            }
            CacheUnsafeReason::NotIgnored => "该目录未被 gitignore，可能不是构建产物".into(),
            CacheUnsafeReason::ContainsTrackedFiles { sample } => {
                format!(
                    "目录内有被 git 跟踪的文件（如 {}），删了不可恢复",
                    sample.join(", ")
                )
            }
            CacheUnsafeReason::IsSymlink => "是符号链接，删它会波及链接目标".into(),
            CacheUnsafeReason::EscapesWorktree { resolved } => {
                format!("解析后不在 worktree 内（指向 {}）", resolved.display())
            }
            CacheUnsafeReason::NoMatchingRule => "不匹配任何已知的构建缓存规则".into(),
        },
        GateDetail::UncommittedChanges { count, sample } => {
            format!("有 {count} 处未提交改动（如 {}）", sample.join(", "))
        }
        GateDetail::NotLanded { ahead, baseline } => {
            format!("工作未进主干（领先 {baseline} {ahead} 个提交）")
        }
        GateDetail::PreciousFiles { paths } => {
            let names: Vec<_> = paths
                .iter()
                .take(3)
                .map(|p| p.display().to_string())
                .collect();
            format!("含主仓没有或内容不同的敏感文件：{}", names.join(", "))
        }
        GateDetail::NestedWorktrees { paths } => {
            format!(
                "内部嵌套了 {} 个其它工作区，删外层会连它们一起灭",
                paths.len()
            )
        }
        GateDetail::OperationInProgress { kind } => {
            format!("正处于 {kind} 中间态，删除会销毁进行中的状态")
        }
        GateDetail::WorktreeLocked { reason } => match reason {
            Some(r) if !r.is_empty() => format!("已被 git worktree lock 锁定：{r}"),
            _ => "已被 git worktree lock 锁定".into(),
        },
    }
}

fn describe_cause(c: &Cause) -> String {
    match c {
        Cause::CommandFailed { cmd, code, stderr } => {
            let tail = stderr.lines().next().unwrap_or("").trim();
            format!(
                "`{cmd}` 退出码 {:?}{}",
                code,
                if tail.is_empty() {
                    String::new()
                } else {
                    format!("：{tail}")
                }
            )
        }
        Cause::Timeout { cmd, secs } => format!("`{cmd}` 超过 {secs} 秒未返回"),
        Cause::Unsupported { what, platform } => {
            format!("本平台（{platform}）无法获取{what}")
        }
        Cause::ToolMissing { tool } => format!("未安装 {tool}"),
        Cause::ForgeUnavailable { detail } => format!("无法查询合入状态：{detail}"),
        Cause::Io { path, msg } => format!("读取 {} 失败：{msg}", path.display()),
    }
}
