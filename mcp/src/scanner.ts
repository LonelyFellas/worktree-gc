import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { z } from "zod";

const gateOutcomeSchema = z
  .object({
    status: z.unknown(),
  })
  .passthrough();

const cacheSchema = z
  .object({
    path: z.string(),
    bytes: z.number().nonnegative(),
    outcomes: z.array(gateOutcomeSchema),
  })
  .passthrough();

const worktreeSchema = z
  .object({
    path: z.string(),
    is_main: z.boolean(),
    bytes: z.number().nonnegative(),
    caches: z.array(cacheSchema),
    verdict: z.unknown(),
  })
  .passthrough();

const repoSchema = z
  .object({
    root: z.string(),
    worktrees: z.array(worktreeSchema),
  })
  .passthrough();

export const scanReportSchema = z
  .object({
    repos: z.array(repoSchema),
    available_bytes: z.number().nonnegative(),
    tools: z.array(z.unknown()),
  })
  .passthrough();

export const scanSummarySchema = z.object({
  repositories: z.number().int().nonnegative(),
  worktrees: z.number().int().nonnegative(),
  reclaimable_caches: z.number().int().nonnegative(),
  removable_worktrees: z.number().int().nonnegative(),
  needs_attention: z.number().int().nonnegative(),
});

export type ScanReport = z.infer<typeof scanReportSchema>;
export type ScanSummary = z.infer<typeof scanSummarySchema>;

const packageRoot = fileURLToPath(new URL("../", import.meta.url));
const developmentRoot = fileURLToPath(new URL("../../", import.meta.url));
const workingRoot = existsSync(join(developmentRoot, "Cargo.toml"))
  ? developmentRoot
  : packageRoot;

export function buildWtgcArgs(repos: string[]): string[] {
  return [
    "--json",
    "--offline",
    ...(repos.length > 0 ? ["--no-default-seeds"] : []),
    ...repos.flatMap((repo) => ["--repo", repo]),
    "scan",
  ];
}

export function summarize(report: ScanReport): ScanSummary {
  const worktrees = report.repos.flatMap((repo) => repo.worktrees);

  return {
    repositories: report.repos.length,
    worktrees: worktrees.length,
    reclaimable_caches: worktrees
      .flatMap((worktree) => worktree.caches)
      .filter((cache) =>
        cache.outcomes.every((outcome) => outcome.status === "Pass"),
      ).length,
    removable_worktrees: worktrees.filter(
      (worktree) => worktree.verdict === "Removable",
    ).length,
    needs_attention: worktrees.filter(
      (worktree) =>
        typeof worktree.verdict === "object" &&
        worktree.verdict !== null &&
        "NeedsAttention" in worktree.verdict,
    ).length,
  };
}

export async function scanWorktrees(repos: string[]): Promise<ScanReport> {
  const developmentBinary = join(
    developmentRoot,
    "target",
    "debug",
    process.platform === "win32" ? "wtgc.exe" : "wtgc",
  );
  const binary = resolveWtgcBinary(developmentBinary);
  const stdout = await run(binary, buildWtgcArgs(repos));

  let value: unknown;
  try {
    value = JSON.parse(stdout);
  } catch {
    throw new Error("wtgc 返回的不是有效 JSON");
  }

  const parsed = scanReportSchema.safeParse(value);
  if (!parsed.success) {
    throw new Error(`wtgc 报告结构无效: ${z.prettifyError(parsed.error)}`);
  }
  return parsed.data;
}

function resolveWtgcBinary(developmentBinary: string): string {
  const configured = process.env.WTGC_BIN?.trim();
  if (configured) return configured;

  const name = process.platform === "win32" ? "wtgc.exe" : "wtgc";
  const candidates = [
    join(packageRoot, "bin", name),
    developmentBinary,
    process.env.CARGO_HOME && join(process.env.CARGO_HOME, "bin", name),
    process.env.HOME && join(process.env.HOME, ".cargo", "bin", name),
    process.env.USERPROFILE &&
      join(process.env.USERPROFILE, ".cargo", "bin", name),
  ].filter((candidate): candidate is string => Boolean(candidate));

  return candidates.find((candidate) => existsSync(candidate)) ?? name;
}

function run(binary: string, args: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(
      binary,
      args,
      {
        cwd: workingRoot,
        timeout: 5 * 60_000,
        maxBuffer: 32 * 1024 * 1024,
      },
      (error, stdout, stderr) => {
        if (!error) {
          resolve(stdout);
          return;
        }

        if ((error as NodeJS.ErrnoException).code === "ENOENT") {
          reject(
            new Error(
              `找不到 wtgc 可执行文件 ${binary}。请先运行 cargo install --git https://github.com/LonelyFellas/worktree-gc --locked --bin wtgc wtgc，或设置 WTGC_BIN。`,
            ),
          );
          return;
        }

        const detail = stderr.trim().slice(0, 4_000);
        reject(new Error(detail || error.message));
      },
    );
  });
}
