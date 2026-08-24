import { copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const mcpRoot = fileURLToPath(new URL("../", import.meta.url));
const pluginRoot = fileURLToPath(
  new URL("../../plugins/worktree-gc/", import.meta.url),
);

await mkdir(`${pluginRoot}/mcp`, { recursive: true });
await mkdir(`${pluginRoot}/widget`, { recursive: true });

await build({
  entryPoints: [`${mcpRoot}src/stdio.ts`],
  outfile: `${pluginRoot}/mcp/server.mjs`,
  bundle: true,
  format: "esm",
  platform: "node",
  target: "node20",
  sourcemap: false,
  minify: true,
  legalComments: "eof",
});

const bundlePath = `${pluginRoot}/mcp/server.mjs`;
const bundle = await readFile(bundlePath, "utf8");
await writeFile(bundlePath, bundle.replace(/[\t ]+$/gm, ""), "utf8");

await copyFile(
  `${mcpRoot}widget/worktree-report.html`,
  `${pluginRoot}/widget/worktree-report.html`,
);
