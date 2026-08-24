import { createMcpExpressApp } from "@modelcontextprotocol/sdk/server/express.js";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import { createWorktreeGcServer } from "./mcp.js";

const host = "127.0.0.1";
const port = parsePort(process.env.MCP_PORT);
const app = createMcpExpressApp({ host });

app.get("/health", (_req, res) => {
  res.json({ status: "ok", server: "worktree-gc", version: "0.1.2" });
});

app.post("/mcp", async (req, res) => {
  const server = createWorktreeGcServer();
  const transport = new StreamableHTTPServerTransport({
    sessionIdGenerator: undefined,
  });

  res.on("close", () => {
    void transport.close();
    void server.close();
  });

  try {
    await server.connect(transport);
    await transport.handleRequest(req, res, req.body);
  } catch (error) {
    console.error("MCP 请求处理失败", error);
    if (!res.headersSent) {
      res.status(500).json({
        jsonrpc: "2.0",
        error: { code: -32603, message: "Internal server error" },
        id: null,
      });
    }
  }
});

app.all("/mcp", (_req, res) => {
  res.status(405).json({
    jsonrpc: "2.0",
    error: { code: -32000, message: "Method not allowed" },
    id: null,
  });
});

const httpServer = app.listen(port, host, () => {
  console.log(`worktree-gc MCP server: http://${host}:${port}/mcp`);
});

process.on("SIGINT", () => httpServer.close(() => process.exit(0)));
process.on("SIGTERM", () => httpServer.close(() => process.exit(0)));

function parsePort(raw: string | undefined): number {
  const value = raw === undefined ? 8787 : Number(raw);
  if (!Number.isInteger(value) || value < 1 || value > 65_535) {
    throw new Error(`MCP_PORT 必须是 1 到 65535 之间的整数，收到: ${raw}`);
  }
  return value;
}
