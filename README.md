# worktree-gc

**English** · [简体中文](README.zh-CN.md)

Reclaim disk space from git worktrees left behind by AI coding agents — **safely**.

> ⚠️ **Status: early development.** The CLI and read-only Codex plugin can scan and
> explain local worktrees. Destructive CLI actions still require an explicit `--apply`.

## Install the Codex plugin

Install the local scanner:

```bash
cargo install --git https://github.com/LonelyFellas/worktree-gc --locked --bin wtgc
```

Then add the GitHub marketplace and install the plugin:

```bash
codex plugin marketplace add LonelyFellas/worktree-gc
codex plugin add worktree-gc@worktree-gc
```

Restart the Codex desktop app, open a new task, and ask it to scan your local
worktrees. The MCP integration is read-only and always runs offline.

## The problem

AI coding agents (Claude Code, Codex, Cursor …) create a separate git worktree per task,
and each one runs its own build. The artifacts are never shared and never cleaned up.

A real incident that started this project: six agent worktrees accumulated **164 GB over five
days** and took a 494 GB Mac down to 1.9 GB free. What that space actually was:

| | Size | Share |
|---|---|---|
| Build artifacts (`target/`, `node_modules/`) | ~168 GB | 99.96% |
| Source code and uncommitted work | ~72 MB | 0.04% |

**So the main action is reclaiming build caches, not deleting worktrees.** Deleting is secondary.

## Why not just `rm -rf */target`

Because some of those worktrees have an agent actively building in them, some have
uncommitted work, and some have a `secrets/` directory that `git worktree remove` will
delete without a word. The hard part is not deleting — it is knowing what is safe to delete.

Existing tools solve half of it:

- **worktrunk / treehouse / git-parsec** create and switch worktrees; their prune only
  handles worktrees they created themselves. `worktree-gc` only reclaims, never creates —
  it works on whatever created them.
- **kondo / npkill / cargo-sweep** delete build output by directory name and mtime, with no
  git awareness. They cannot tell a `target/` whose agent is mid-build from an abandoned one.

## How it decides

Two gate groups for two very different blast radii:

**A — reclaim build cache** (source and uncommitted work untouched)

| Gate | Requires |
|---|---|
| Busy | No process has its cwd or executable inside the worktree |
| Recent | The cache directory itself has been quiet for N minutes |
| CacheSafe | Ignored by git · contains no tracked files · inside the worktree root · not a symlink · matches a known cache rule |

**B — remove the whole worktree** (A plus six more)

| Gate | Requires |
|---|---|
| Dirty | No uncommitted changes, including under `showUntrackedFiles=no` and `skip-worktree` |
| Landed | Work is in the trunk — ancestry, forge API, or path-restricted diff for squash merges |
| Precious | No ignored file that is not a known build cache (blacklist, not whitelist) |
| Nested | No other worktree or git repo nested inside |
| InProgress | No rebase / merge / cherry-pick / bisect in flight |
| Locked | Not `git worktree lock`ed |

### Three states, never two

Every gate returns `Pass`, `Blocked`, or **`Unknown`** — and `Unknown` never means pass.
A failed subcommand, a timeout, a missing capability on this platform: all of it lands in
`Unknown`, which lands the worktree in `NeedsAttention` for a human to look at.

This is the single most important design decision in the project. The prototype this grew out
of used `cmd 2>/dev/null` and treated empty output as "clean" — and a corrupted `.git`
returning exit 128 with empty stdout read as "no uncommitted changes".

The failure modes are not symmetric: skipping something costs a few gigabytes, deleting
something wrong can cost work that exists nowhere else. Every trade-off leans toward skipping.

## Development

```bash
cargo test
cargo clippy --all-targets
```

Tests build **real git repositories** rather than mocking git. Every genuine bug this project
has hit was git behaving unexpectedly — `worktree remove` silently eating `.env.local`,
ancestry checks failing after a squash merge, `ls-files --directory` collapsing a directory
into one entry. Mocking git would only test a story we made up.

The test suite is driven by a catalogue of sixteen concrete data-loss scenarios, each one
reproduced before it was fixed. They live as regression tests under `tests/` — start there
if you want to understand why a gate is written the way it is.

## License

MIT OR Apache-2.0
