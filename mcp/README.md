# worktree-gc MCP server

这是 `worktree-gc` 的本地、只读 MCP 入口。它通过现有 `wtgc` CLI 生成扫描报告，不复制安全判定逻辑。

当前只提供 `scan_worktrees`：

- 扫描指定仓库，或已知的 agent worktree 目录；
- 始终使用离线模式，不访问 GitHub；
- 只读，不回收缓存，也不删除 worktree；
- 仅监听 `127.0.0.1`。

工具同时注册了 `ui://worktree-gc/report-v1.html` MCP Apps 资源。支持 UI 的客户端会显示仓库分组、worktree 判定、缓存门禁和本地筛选；不支持 UI 的客户端仍可使用原来的文本与结构化结果。

`src/stdio.ts` 是 Codex 本地插件入口。`pnpm bundle:plugin` 会把 MCP Server 打包成无需安装 Node 依赖的 `plugins/worktree-gc/mcp/server.mjs`，并同步页面资源。

## 启动

```bash
cd /path/to/worktree-gc
cargo build --bin wtgc

cd mcp
pnpm install
pnpm build
pnpm start
```

默认 MCP 地址为 `http://127.0.0.1:8787/mcp`，健康检查为 `http://127.0.0.1:8787/health`。

可以通过环境变量覆盖端口或 `wtgc` 路径：

```bash
MCP_PORT=3000 WTGC_BIN=/absolute/path/to/wtgc pnpm start
```

## 检查

```bash
pnpm check
npx @modelcontextprotocol/inspector
```

在 MCP Inspector 中选择 `Streamable HTTP`，地址填写 `http://127.0.0.1:8787/mcp`。
