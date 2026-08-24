import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { createWorktreeGcServer } from "./mcp.js";

const server = createWorktreeGcServer();
const transport = new StdioServerTransport();

await server.connect(transport);
