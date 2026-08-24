# Worktree GC Codex plugin

This plugin runs entirely on the user's machine. Its MCP server is read-only and
always calls `wtgc` with `--offline`; it never reclaims caches or removes a
worktree.

## Requirement

Install the `wtgc` CLI first:

```bash
cargo install --git https://github.com/LonelyFellas/worktree-gc --locked --bin wtgc
```

Set `WTGC_BIN` before starting Codex if the binary is not available as `wtgc` in
`PATH`.

## Install from GitHub

```bash
codex plugin marketplace add LonelyFellas/worktree-gc
codex plugin add worktree-gc@worktree-gc
```

Restart the Codex desktop app, start a new task, and ask:

> Scan my local worktrees and show what is using disk space.

The plugin returns model-readable text and a visual MCP Apps report.
