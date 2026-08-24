// 把结构化的判定结果翻成人话。
//
// 之所以不在 Rust 侧拼好字符串再传过来：同一份 GateDetail 要在 CLI、JSON、
// 未来的 HTML 报告里各自呈现，提前拼死就没法各用各的了。

import type { Cause, GateDetail, GateStatus, Verdict, CacheUnsafeReason } from "./types";
import type { Language } from "./i18n";

export function describeCause(c: Cause, language: Language = "zh"): string {
  const zh = language === "zh";
  if ("CommandFailed" in c) {
    const first = c.CommandFailed.stderr.split("\n")[0]?.trim();
    return zh
      ? `检查命令 \`${c.CommandFailed.cmd}\` 执行失败（退出码 ${c.CommandFailed.code ?? "未知"}）${first ? `：${first}` : ""}`
      : `Command \`${c.CommandFailed.cmd}\` failed (exit code ${c.CommandFailed.code ?? "unknown"})${first ? `: ${first}` : ""}`;
  }
  if ("Timeout" in c) return zh
    ? `检查命令 \`${c.Timeout.cmd}\` 在 ${c.Timeout.secs} 秒内没有完成`
    : `Command \`${c.Timeout.cmd}\` did not finish within ${c.Timeout.secs} seconds`;
  if ("Unsupported" in c) return zh
    ? `当前系统（${c.Unsupported.platform}）无法检查${c.Unsupported.what}`
    : `This system (${c.Unsupported.platform}) cannot check ${c.Unsupported.what}`;
  if ("ToolMissing" in c) return zh
    ? `未找到 ${c.ToolMissing.tool}，无法完成检查`
    : `${c.ToolMissing.tool} was not found, so the check could not be completed`;
  if ("ForgeUnavailable" in c) return zh
    ? `无法确认分支是否已合并：${c.ForgeUnavailable.detail}`
    : `Could not determine whether the branch is merged: ${c.ForgeUnavailable.detail}`;
  return zh
    ? `无法读取 ${c.Io.path}：${c.Io.msg}`
    : `Could not read ${c.Io.path}: ${c.Io.msg}`;
}

function describeCacheReason(r: CacheUnsafeReason, language: Language): string {
  const zh = language === "zh";
  if (typeof r === "object" && "MissingMarker" in r)
    return zh
      ? `无法确认这是可重建缓存：未找到 ${r.MissingMarker.expected.join("、")} 中的任一文件`
      : `Could not verify this as reproducible cache: none of ${r.MissingMarker.expected.join(", ")} were found`;
  if (r === "NotIgnored") return zh
    ? "该目录未被 Git 忽略，可能包含需要保留的文件"
    : "This directory is not ignored by Git and may contain files that should be kept";
  if (r === "IsSymlink") return zh
    ? "该目录是链接，清理可能影响它指向的位置"
    : "This directory is a symlink; cleaning it may affect its target";
  if (r === "NoMatchingRule") return zh
    ? "无法确认该目录属于已知的构建缓存"
    : "This directory does not match a known build-cache rule";
  if ("ContainsTrackedFiles" in r)
    return zh
      ? `目录中包含 Git 已跟踪的文件（例如 ${r.ContainsTrackedFiles.sample.join("、")}），不能安全清理`
      : `The directory contains Git-tracked files (for example ${r.ContainsTrackedFiles.sample.join(", ")}) and cannot be cleaned safely`;
  return zh
    ? `该目录实际位于工作区外部（${r.EscapesWorktree.resolved}），已阻止清理`
    : `The directory resolves outside the worktree (${r.EscapesWorktree.resolved}), so cleaning was blocked`;
}

export function describeDetail(d: GateDetail, language: Language = "zh"): string {
  const zh = language === "zh";
  if ("ProcessesActive" in d) {
    const programs = [...new Set(d.ProcessesActive.sample)].slice(0, 3);
    const count = d.ProcessesActive.pids.length;
    return zh
      ? `${count} 个程序正在使用此工作区${programs.length ? `（例如 ${programs.join("、")}）` : ""}`
      : `${count} process${count === 1 ? " is" : "es are"} using this worktree${programs.length ? ` (for example ${programs.join(", ")})` : ""}`;
  }
  if ("RecentlyModified" in d)
    return zh
      ? `此工作区在最近 ${Math.round(d.RecentlyModified.age_secs / 60)} 分钟内仍有文件写入`
      : `Files were still being written to this worktree within the last ${Math.round(d.RecentlyModified.age_secs / 60)} minutes`;
  if ("NotPureCache" in d) return describeCacheReason(d.NotPureCache.reason, language);
  if ("UncommittedChanges" in d) {
    const count = d.UncommittedChanges.count;
    return zh
      ? `此工作区有 ${count} 处改动尚未提交（例如 ${d.UncommittedChanges.sample.join("、")}）`
      : `This worktree has ${count} uncommitted change${count === 1 ? "" : "s"} (for example ${d.UncommittedChanges.sample.join(", ")})`;
  }
  if ("NotLanded" in d) {
    const count = d.NotLanded.ahead;
    return zh
      ? `此分支还有 ${count} 个提交尚未合并到 ${d.NotLanded.baseline}`
      : `This branch has ${count} commit${count === 1 ? "" : "s"} not merged into ${d.NotLanded.baseline}`;
  }
  if ("PreciousFiles" in d)
    return zh
      ? `此工作区包含主仓库中没有相同副本的重要文件：${d.PreciousFiles.paths.slice(0, 3).join("、")}`
      : `This worktree contains important files without matching copies in the main repository: ${d.PreciousFiles.paths.slice(0, 3).join(", ")}`;
  if ("NestedWorktrees" in d)
    return zh
      ? `此工作区内还包含 ${d.NestedWorktrees.paths.length} 个嵌套工作区`
      : `This worktree contains ${d.NestedWorktrees.paths.length} nested worktree${d.NestedWorktrees.paths.length === 1 ? "" : "s"}`;
  if ("OperationInProgress" in d)
    return zh
      ? `此工作区正在进行 ${d.OperationInProgress.kind} 操作`
      : `A ${d.OperationInProgress.kind} operation is in progress in this worktree`;
  if (zh) return d.WorktreeLocked.reason
    ? `此工作区已被 Git 锁定：${d.WorktreeLocked.reason}`
    : "此工作区已被 Git 锁定";
  return d.WorktreeLocked.reason
    ? `This worktree is locked by Git: ${d.WorktreeLocked.reason}`
    : "This worktree is locked by Git";
}

export function statusText(s: GateStatus, language: Language = "zh"): { kind: "blocked" | "unknown"; text: string } | null {
  if (s === "Pass" || s === "Skipped") return null;
  if ("Blocked" in s) return { kind: "blocked", text: describeDetail(s.Blocked, language) };
  return {
    kind: "unknown",
    text: language === "zh"
      ? `检查未完成：${describeCause(s.Unknown, language)}`
      : `Check incomplete: ${describeCause(s.Unknown, language)}`,
  };
}

export function verdictBadge(v: Verdict, language: Language = "zh"): { icon: string; label: string; cls: string } {
  const zh = language === "zh";
  if (v === "Removable") return { icon: "✅", label: zh ? "可删除" : "Removable", cls: "ok" };
  if (v === "CacheReclaimable") return { icon: "♻️", label: zh ? "可回收缓存" : "Cache reclaimable", cls: "ok" };
  if ("Blocked" in v) return { icon: "⏸", label: zh ? "保留" : "Keep", cls: "blocked" };
  // 判不准必须与被拦下视觉区分：前者是"我们不知道那里有什么"，比后者更需要人介入
  if ("NeedsAttention" in v) return { icon: "❓", label: zh ? "判不准" : "Uncertain", cls: "unknown" };
  return { icon: "🔒", label: zh ? "受保护" : "Protected", cls: "protected" };
}
