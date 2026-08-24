// 每条拦下的理由，配一条「那我该干嘛」。
//
// 原来的界面只说结论不说下一步，用户看完「未进主干」还是不知道该做什么。
// 判定本身是机器的活，决定怎么办是人的活——界面至少要把选项摆出来。

import type { GateDetail, GateStatus } from "./types";
import type { Language } from "./i18n";

export function adviceFor(d: GateDetail, language: Language = "zh"): string | null {
  const zh = language === "zh";
  if ("ProcessesActive" in d)
    return zh
      ? "等待相关任务结束，或关闭正在使用此工作区的程序，然后重新扫描。"
      : "Wait for related tasks to finish, or close the programs using this worktree, then rescan.";
  if ("NotLanded" in d)
    return zh
      ? `先将此分支合并到 ${d.NotLanded.baseline}；如果确认不再需要，请手动删除此工作区。`
      : `Merge this branch into ${d.NotLanded.baseline} first. If it is no longer needed, remove the worktree manually.`;
  if ("UncommittedChanges" in d)
    return zh
      ? "先提交或暂存这些改动，再决定是否删除此工作区。"
      : "Commit or stash these changes before deciding whether to remove this worktree.";
  if ("PreciousFiles" in d)
    return zh
      ? "先备份这些文件，或确认不再需要后再处理此工作区。"
      : "Back up these files, or confirm they are no longer needed before handling this worktree.";
  if ("NestedWorktrees" in d)
    return zh
      ? "先移除或迁移内部工作区，再处理外层工作区。"
      : "Remove or relocate the nested worktrees before handling the outer worktree.";
  if ("OperationInProgress" in d)
    return zh
      ? `先完成或中止 ${d.OperationInProgress.kind} 操作，然后重新扫描。`
      : `Finish or abort the ${d.OperationInProgress.kind} operation, then rescan.`;
  if ("WorktreeLocked" in d)
    return zh
      ? "确认可以处理后，先运行 git worktree unlock 解锁。"
      : "Once it is safe to proceed, run git worktree unlock first.";
  if ("RecentlyModified" in d)
    return zh
      ? "等待一段时间，确认没有程序继续写入后再重新扫描。"
      : "Wait until no program is writing to the worktree, then rescan.";
  if ("NotPureCache" in d)
    return zh
      ? "先确认该目录只包含可以重新生成的文件；不确定时请保留。"
      : "Confirm the directory contains only reproducible files. Keep it if you are unsure.";
  return null;
}

/** 从一组门禁结果里挑出最该让用户先看的那条。 */
export function primaryBlocker(outcomes: { status: GateStatus }[]): GateDetail | null {
  // 优先级：有人在用 > 有未提交改动 > 未进主干 > 其它。
  // 这个顺序对应「离用户能动手有多近」——有进程占用等一等就好，
  // 未进主干却要走一遍 PR 流程。
  const order = ["ProcessesActive", "UncommittedChanges", "NestedWorktrees", "NotLanded"];
  const blocked = outcomes
    .map((o) => (typeof o.status === "object" && "Blocked" in o.status ? o.status.Blocked : null))
    .filter(Boolean) as GateDetail[];
  for (const key of order) {
    const hit = blocked.find((d) => key in d);
    if (hit) return hit;
  }
  return blocked[0] ?? null;
}
