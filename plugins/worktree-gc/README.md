# Worktree GC Codex plugin

This plugin runs entirely on the user's machine. Its MCP server is read-only and
always calls `wtgc` with `--offline`; it never reclaims caches or removes a
worktree.

## Requirement

Install Git and Node.js 20 or newer. Official platform release bundles include
the scanner under `bin/`. When installing directly from the Git marketplace,
also install Rust 1.95 or newer and then install the `wtgc` CLI:

```bash
cargo install --git https://github.com/LonelyFellas/worktree-gc --locked --bin wtgc wtgc
```

The plugin also checks `CARGO_HOME/bin`, `~/.cargo/bin`, and the equivalent
Windows user-profile path. Set `WTGC_BIN` before starting Codex for a custom
installation location.

## Install from GitHub

```bash
codex plugin marketplace add LonelyFellas/worktree-gc
codex plugin add worktree-gc@worktree-gc
```

Restart the Codex desktop app, start a new task, and ask:

> Scan my local worktrees and show what is using disk space.

The plugin returns model-readable text and a visual MCP Apps report.
