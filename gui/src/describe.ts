// 把结构化的判定结果翻成人话。
//
// 之所以不在 Rust 侧拼好字符串再传过来：同一份 GateDetail 要在 CLI、JSON、
// 未来的 HTML 报告里各自呈现，提前拼死就没法各用各的了。

import type { Cause, GateDetail, GateStatus, Verdict, CacheUnsafeReason } from "./types";

export function describeCause(c: Cause): string {
  if ("CommandFailed" in c) {
    const first = c.CommandFailed.stderr.split("\n")[0]?.trim();
    return `\`${c.CommandFailed.cmd}\` 退出码 ${c.CommandFailed.code ?? "?"}${first ? `：${first}` : ""}`;
  }
  if ("Timeout" in c) return `\`${c.Timeout.cmd}\` 超过 ${c.Timeout.secs} 秒未返回`;
  if ("Unsupported" in c) return `本平台（${c.Unsupported.platform}）无法获取${c.Unsupported.what}`;
  if ("ToolMissing" in c) return `未安装 ${c.ToolMissing.tool}`;
  if ("ForgeUnavailable" in c) return `无法查询合入状态：${c.ForgeUnavailable.detail}`;
  return `读取 ${c.Io.path} 失败：${c.Io.msg}`;
}

function describeCacheReason(r: CacheUnsafeReason): string {
  if (r === "NotIgnored") return "该目录未被 gitignore，可能不是构建产物";
  if (r === "IsSymlink") return "是符号链接，删它会波及链接目标";
  if (r === "NoMatchingRule") return "不匹配任何已知的构建缓存规则";
  if ("ContainsTrackedFiles" in r)
    return `目录内有被 git 跟踪的文件（如 ${r.ContainsTrackedFiles.sample.join("、")}），删了不可恢复`;
  return `解析后不在 worktree 内（指向 ${r.EscapesWorktree.resolved}）`;
}

export function describeDetail(d: GateDetail): string {
  if ("ProcessesActive" in d)
    return `有进程正在使用（${d.ProcessesActive.pids.length} 个：${d.ProcessesActive.sample.join("、")}）`;
  if ("RecentlyModified" in d)
    return `最近 ${Math.round(d.RecentlyModified.age_secs / 60)} 分钟内仍有写入`;
  if ("NotPureCache" in d) return describeCacheReason(d.NotPureCache.reason);
  if ("UncommittedChanges" in d)
    return `有 ${d.UncommittedChanges.count} 处未提交改动（如 ${d.UncommittedChanges.sample.join("、")}）`;
  if ("NotLanded" in d)
    return `工作未进主干（领先 ${d.NotLanded.baseline} ${d.NotLanded.ahead} 个提交）`;
  if ("PreciousFiles" in d)
    return `含主仓没有或内容不同的敏感文件：${d.PreciousFiles.paths.slice(0, 3).join("、")}`;
  if ("NestedWorktrees" in d)
    return `内部嵌套了 ${d.NestedWorktrees.paths.length} 个其它工作区，删外层会连它们一起灭`;
  if ("OperationInProgress" in d)
    return `正处于 ${d.OperationInProgress.kind} 中间态，删除会销毁进行中的状态`;
  return d.WorktreeLocked.reason
    ? `已被 git worktree lock 锁定：${d.WorktreeLocked.reason}`
    : "已被 git worktree lock 锁定";
}

export function statusText(s: GateStatus): { kind: "blocked" | "unknown"; text: string } | null {
  if (s === "Pass" || s === "Skipped") return null;
  if ("Blocked" in s) return { kind: "blocked", text: describeDetail(s.Blocked) };
  return { kind: "unknown", text: `判不准：${describeCause(s.Unknown)}` };
}

export function verdictBadge(v: Verdict): { icon: string; label: string; cls: string } {
  if (v === "Removable") return { icon: "✅", label: "可删除", cls: "ok" };
  if (v === "CacheReclaimable") return { icon: "♻️", label: "可回收缓存", cls: "ok" };
  if ("Blocked" in v) return { icon: "⏸", label: "保留", cls: "blocked" };
  // 判不准必须与被拦下视觉区分：前者是"我们不知道那里有什么"，比后者更需要人介入
  if ("NeedsAttention" in v) return { icon: "❓", label: "判不准", cls: "unknown" };
  return { icon: "🔒", label: "受保护", cls: "protected" };
}
