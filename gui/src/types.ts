// 与 Rust 侧 serde 的外部标签形式一一对应。
// 单元变体序列化成字符串，带字段的变体序列化成 { 变体名: { 字段 } }。

export type GateId =
  | "Busy" | "Recent" | "CacheSafe"
  | "Idle" | "Dirty" | "Landed" | "Precious" | "Nested" | "InProgress" | "Locked";

export type Cause =
  | { CommandFailed: { cmd: string; code: number | null; stderr: string } }
  | { Timeout: { cmd: string; secs: number } }
  | { Unsupported: { what: string; platform: string } }
  | { ToolMissing: { tool: string } }
  | { ForgeUnavailable: { detail: string } }
  | { Io: { path: string; msg: string } };

export type CacheUnsafeReason =
  | "NotIgnored" | "IsSymlink" | "NoMatchingRule"
  | { MissingMarker: { expected: string[] } }
  | { ContainsTrackedFiles: { sample: string[] } }
  | { EscapesWorktree: { resolved: string } };

export type GateDetail =
  | { ProcessesActive: { pids: number[]; sample: string[] } }
  | { RecentlyModified: { newest_path: string; age_secs: number } }
  | { NotPureCache: { reason: CacheUnsafeReason } }
  | { UncommittedChanges: { count: number; sample: string[] } }
  | { NotLanded: { ahead: number; baseline: string } }
  | { PreciousFiles: { paths: string[] } }
  | { NestedWorktrees: { paths: string[] } }
  | { OperationInProgress: { kind: string } }
  | { WorktreeLocked: { reason: string | null } };

export type GateStatus =
  | "Pass" | "Skipped"
  | { Blocked: GateDetail }
  | { Unknown: Cause };

export type GateOutcome = { id: GateId; status: GateStatus };

export type Verdict =
  | "CacheReclaimable" | "Removable"
  | { Blocked: { by: GateId[] } }
  | { NeedsAttention: { unknown: GateId[] } }
  | { Protected: { why: string } };

export type CacheDir = {
  path: string;
  kind: { name: string; ecosystem: string };
  bytes: number;
  outcomes: GateOutcome[];
};

export type WorktreeReport = {
  path: string;
  branch: string | null;
  head_oid: string;
  is_main: boolean;
  bytes: number;
  caches: CacheDir[];
  outcomes: GateOutcome[];
  verdict: Verdict;
};

export type RepoReport = {
  root: string;
  baseline: { remote: string | null; branch: string; source: string } | null;
  baseline_error: Cause | null;
  worktrees: WorktreeReport[];
  prunable: string[];
};

export type ScanReport = {
  repos: RepoReport[];
  available_bytes: number;
  tools: { name: string; path: string | null; version: string | null }[];
};

export type ScanEnvelope = {
  scan_id: string;
  report: ScanReport;
};

/// 与 Rust 侧 platform::disk::human_bytes 同口径（1024 进制、一位小数），
/// 这样界面上的数字和终端里 du -h 的输出能相互印证。
export function humanBytes(n: number): string {
  const units = ["B", "K", "M", "G", "T", "P"];
  let v = n, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${n}B` : `${v.toFixed(1)}${units[i]}`;
}
