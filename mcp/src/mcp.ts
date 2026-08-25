import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { z } from "zod";
import {
  scanReportSchema,
  scanSummarySchema,
  scanWorktrees,
  summarize,
  type ScanReport,
} from "./scanner.js";

type Scanner = (repos: string[]) => Promise<ScanReport>;

export const WORKTREE_REPORT_URI = "ui://worktree-gc/report-v1.html";
const widgetHtml = readFileSync(
  fileURLToPath(new URL("../widget/worktree-report.html", import.meta.url)),
  "utf8",
);

export function createWorktreeGcServer(scanner: Scanner = scanWorktrees): McpServer {
  const server = new McpServer(
    {
      name: "worktree-gc",
      version: "0.1.6",
    },
    {
      instructions:
        "Use scan_worktrees to inspect local Git worktrees and rebuildable caches. This server is read-only and always scans offline. It cannot reclaim caches or remove worktrees.",
    },
  );

  server.registerResource(
    "worktree-report",
    WORKTREE_REPORT_URI,
    {},
    async () => ({
      contents: [
        {
          uri: WORKTREE_REPORT_URI,
          mimeType: "text/html;profile=mcp-app",
          text: widgetHtml,
          _meta: {
            ui: {
              prefersBorder: true,
              csp: {
                connectDomains: [],
                resourceDomains: [],
              },
            },
            "openai/widgetDescription":
              "按仓库展示本地 worktree、构建缓存、安全门禁和可处理状态。",
          },
        },
      ],
    }),
  );

  server.registerTool(
    "scan_worktrees",
    {
      title: "Scan local worktrees",
      description:
        "Inspect local Git worktrees and rebuildable caches when the user wants a disk-usage or safety report. This is an offline, read-only scan; it cannot reclaim caches or remove worktrees.",
      inputSchema: {
        repos: z
          .array(z.string().trim().min(1).max(4_096))
          .max(50)
          .default([])
          .describe(
            "Optional local repository paths. When omitted, wtgc scans its known agent worktree locations.",
          ),
      },
      outputSchema: {
        summary: scanSummarySchema,
        report: scanReportSchema,
      },
      annotations: {
        readOnlyHint: true,
        destructiveHint: false,
        openWorldHint: false,
        idempotentHint: true,
      },
      _meta: {
        ui: { resourceUri: WORKTREE_REPORT_URI },
        "openai/outputTemplate": WORKTREE_REPORT_URI,
        "openai/toolInvocation/invoking": "正在扫描本地 worktree…",
        "openai/toolInvocation/invoked": "worktree 扫描完成",
      },
    },
    async ({ repos }) => {
      try {
        const report = await scanner(repos);
        const summary = summarize(report);
        return {
          structuredContent: { summary, report },
          content: [
            {
              type: "text",
              text:
                `离线扫描完成：${summary.repositories} 个仓库、${summary.worktrees} 个 worktree；` +
                `${summary.reclaimable_caches} 个构建缓存通过回收门禁，` +
                `${summary.removable_worktrees} 个 worktree 通过删除门禁，` +
                `${summary.needs_attention} 个需要人工确认。`,
            },
          ],
        };
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        return {
          isError: true,
          content: [{ type: "text", text: `扫描失败：${message}` }],
        };
      }
    },
  );

  return server;
}
