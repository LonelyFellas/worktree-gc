import assert from "node:assert/strict";
import {
  chmod,
  copyFile,
  mkdir,
  mkdtemp,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { createWorktreeGcServer, WORKTREE_REPORT_URI } from "./mcp.js";
import { buildWtgcArgs, summarize, type ScanReport } from "./scanner.js";

const report: ScanReport = {
  repos: [
    {
      root: "/repo",
      worktrees: [
        {
          path: "/repo",
          is_main: true,
          bytes: 10,
          caches: [
            {
              path: "/repo/target",
              bytes: 5,
              outcomes: [{ status: "Pass" }],
            },
          ],
          verdict: { Protected: { why: "主工作区" } },
        },
        {
          path: "/repo-worktree",
          is_main: false,
          bytes: 20,
          caches: [],
          verdict: "Removable",
        },
        {
          path: "/repo-unknown",
          is_main: false,
          bytes: 30,
          caches: [],
          verdict: { NeedsAttention: { unknown: ["Busy"] } },
        },
      ],
    },
  ],
  available_bytes: 1_000,
  tools: [],
};

test("wtgc 参数始终保持只读离线，路径不经过 shell", () => {
  const suspiciousPath = "/tmp/repo $(touch should-not-run)";
  assert.deepEqual(buildWtgcArgs([suspiciousPath]), [
    "--json",
    "--offline",
    "--no-default-seeds",
    "--repo",
    suspiciousPath,
    "scan",
  ]);
  assert.deepEqual(buildWtgcArgs([]), ["--json", "--offline", "scan"]);
});

test("汇总可回收缓存、可删除 worktree 和待确认项", () => {
  assert.deepEqual(summarize(report), {
    repositories: 1,
    worktrees: 3,
    reclaimable_caches: 1,
    removable_worktrees: 1,
    needs_attention: 1,
  });
});

test("MCP 暴露只读 scan_worktrees 并返回结构化报告", async () => {
  let receivedRepos: string[] | undefined;
  const server = createWorktreeGcServer(async (repos) => {
    receivedRepos = repos;
    return report;
  });
  const client = new Client({ name: "worktree-gc-test", version: "1.0.0" });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();

  await server.connect(serverTransport);
  await client.connect(clientTransport);

  try {
    const listed = await client.listTools();
    assert.deepEqual(listed.tools.map((tool) => tool.name), ["scan_worktrees"]);
    assert.deepEqual(listed.tools[0]?.annotations, {
      readOnlyHint: true,
      destructiveHint: false,
      openWorldHint: false,
      idempotentHint: true,
    });
    assert.equal(
      listed.tools[0]?._meta?.ui &&
        (listed.tools[0]._meta.ui as { resourceUri?: string }).resourceUri,
      WORKTREE_REPORT_URI,
    );

    const result = await client.callTool({
      name: "scan_worktrees",
      arguments: { repos: ["/repo"] },
    });
    assert.deepEqual(receivedRepos, ["/repo"]);
    assert.equal(result.isError, undefined);
    assert.deepEqual(
      (result.structuredContent as { summary: unknown }).summary,
      summarize(report),
    );
  } finally {
    await client.close();
    await server.close();
  }
});

test("MCP 注册可渲染扫描报告的 UI 资源", async () => {
  const server = createWorktreeGcServer(async () => report);
  const client = new Client({ name: "worktree-gc-test", version: "1.0.0" });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();

  await server.connect(serverTransport);
  await client.connect(clientTransport);

  try {
    const listed = await client.listResources();
    assert.deepEqual(listed.resources.map((resource) => resource.uri), [
      WORKTREE_REPORT_URI,
    ]);

    const resource = await client.readResource({ uri: WORKTREE_REPORT_URI });
    const content = resource.contents[0];
    assert.equal(content?.mimeType, "text/html;profile=mcp-app");
    assert.equal(content && "text" in content, true);
    assert.match(content && "text" in content ? content.text : "", /Worktree GC/);
    assert.match(content && "text" in content ? content.text : "", /ui\/initialize/);
    assert.match(
      content && "text" in content ? content.text : "",
      /ui\/notifications\/initialized/,
    );
    assert.match(
      content && "text" in content ? content.text : "",
      /ui\/notifications\/tool-result/,
    );
  } finally {
    await client.close();
    await server.close();
  }
});

test("MCP 拒绝超过上限的仓库列表", async () => {
  const server = createWorktreeGcServer(async () => report);
  const client = new Client({ name: "worktree-gc-test", version: "1.0.0" });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();

  await server.connect(serverTransport);
  await client.connect(clientTransport);

  try {
    const result = await client.callTool({
      name: "scan_worktrees",
      arguments: { repos: Array.from({ length: 51 }, (_, index) => `/repo-${index}`) },
    });
    assert.equal(result.isError, true);
  } finally {
    await client.close();
    await server.close();
  }
});

test(
  "打包后的 Codex 插件可通过 stdio 调用本地 wtgc",
  { skip: process.platform === "win32" },
  async () => {
    const tempDir = await mkdtemp(join(tmpdir(), "worktree-gc-plugin-test-"));
    const sourceServer = fileURLToPath(
      new URL("../../plugins/worktree-gc/mcp/server.mjs", import.meta.url),
    );
    const sourceWidget = fileURLToPath(
      new URL(
        "../../plugins/worktree-gc/widget/worktree-report.html",
        import.meta.url,
      ),
    );
    const pluginServer = join(tempDir, "mcp", "server.mjs");
    const fakeWtgc = join(tempDir, "bin", "wtgc");
    await mkdir(join(tempDir, "mcp"), { recursive: true });
    await mkdir(join(tempDir, "bin"), { recursive: true });
    await mkdir(join(tempDir, "widget"), { recursive: true });
    await copyFile(sourceServer, pluginServer);
    await copyFile(sourceWidget, join(tempDir, "widget", "worktree-report.html"));
    await writeFile(
      fakeWtgc,
      `#!/usr/bin/env node\nprocess.stdout.write(${JSON.stringify(JSON.stringify(report))});\n`,
      "utf8",
    );
    await chmod(fakeWtgc, 0o755);

    const transport = new StdioClientTransport({
      command: process.execPath,
      args: [pluginServer],
      env: {
        PATH: process.env.PATH ?? "",
      },
    });
    const client = new Client({
      name: "worktree-gc-plugin-test",
      version: "1.0.0",
    });

    try {
      await client.connect(transport);
      const tools = await client.listTools();
      assert.deepEqual(tools.tools.map((tool) => tool.name), ["scan_worktrees"]);

      const result = await client.callTool({
        name: "scan_worktrees",
        arguments: { repos: ["/repo"] },
      });
      assert.equal(result.isError, undefined);
      assert.deepEqual(
        (result.structuredContent as { summary: unknown }).summary,
        summarize(report),
      );
    } finally {
      await client.close();
      await rm(tempDir, { recursive: true, force: true });
    }
  },
);
