// 每条拦下的理由，配一条「那我该干嘛」。
//
// 原来的界面只说结论不说下一步，用户看完「未进主干」还是不知道该做什么。
// 判定本身是机器的活，决定怎么办是人的活——界面至少要把选项摆出来。

import type { GateDetail, GateStatus } from "./types";

export function adviceFor(d: GateDetail): string | null {
  if ("ProcessesActive" in d)
    return "等它跑完，或先停掉那些进程再回来";
  if ("NotLanded" in d)
    return `把这个分支合进 ${d.NotLanded.baseline}，或确认放弃后手动删除`;
  if ("UncommittedChanges" in d)
    return "先提交或暂存这些改动；只想腾空间的话，回收它的构建缓存不受影响";
  if ("PreciousFiles" in d)
    return "这些文件主仓没有，删了不可恢复。确认不需要后再处理";
  if ("NestedWorktrees" in d)
    return "先处理里面那些工作区，外层才能删";
  if ("OperationInProgress" in d)
    return `先把 ${d.OperationInProgress.kind} 收尾（继续或 --abort）`;
  if ("WorktreeLocked" in d)
    return "确认可以动后 git worktree unlock";
  if ("RecentlyModified" in d)
    return "刚写过，稍后再试";
  if ("NotPureCache" in d) return null;
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
