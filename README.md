# worktree-gc

**English** · [简体中文](README.zh-CN.md)

[![CI](https://github.com/LonelyFellas/worktree-gc/actions/workflows/ci.yml/badge.svg)](https://github.com/LonelyFellas/worktree-gc/actions/workflows/ci.yml)
[![Release](https://github.com/LonelyFellas/worktree-gc/actions/workflows/release.yml/badge.svg)](https://github.com/LonelyFellas/worktree-gc/releases/latest)
[![Latest release](https://img.shields.io/github/v/release/LonelyFellas/worktree-gc)](https://github.com/LonelyFellas/worktree-gc/releases/latest)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Safely reclaim disk space from git worktrees left behind by AI coding agents.

> ⚠️ **Status: early development.** The desktop app reclaims approved build caches;
> removing whole worktrees is currently CLI-only and still requires an explicit `--apply`.
> The Codex plugin is read-only.

## Choose an interface

| Interface | Best for | Can change files? |
|---|---|---|
| **Desktop app** | Interactive scanning and cache reclamation | Only after confirmation; build caches only |
| **Codex plugin** | Asking Codex to inspect and explain worktrees | No — always read-only |
| **CLI (`wtgc`)** | Scripts, automation, and advanced cleanup | Dry-run by default; requires `--apply` |

## See the result at a glance

![worktree-gc desktop app separating reclaimable caches from worktrees that need attention](.github/assets/worktree-gc-desktop.png)

The desktop app keeps every decision visible:

- **Safe to reclaim** lists only rebuildable caches that passed every safety gate.
- **Needs attention** explains why a worktree was blocked and what to do next.
- **No action needed** keeps active or protected worktrees visible without offering cleanup.

## Install the desktop app

Download the installer for your platform from the
[latest GitHub Release](https://github.com/LonelyFellas/worktree-gc/releases/latest):

| Platform | Asset |
|---|---|
| macOS · Apple Silicon | `worktree-gc_<version>_aarch64.dmg` |
| macOS · Intel | `worktree-gc_<version>_x64.dmg` |
| Windows · x64 | `worktree-gc_<version>_x64-setup.exe` (recommended) or `.msi` |
| Linux · x64 | `worktree-gc_<version>_amd64.AppImage` (recommended) or `.deb` |

Open the app, add the repositories you want to monitor, then scan. Reclamation is
confirmation-gated and only removes caches that pass all safety checks; source files and
uncommitted changes are left untouched.

The app checks for Tauri-signed updates at startup and from **Settings → App updates**. The
update feed supports the macOS app, Windows setup executable, and Linux AppImage; use the
Release page to update MSI or DEB installations manually. Automatic updates are available
starting with `v0.1.8`, so older installations need one manual upgrade first.

Release installers are not yet Apple-notarized or Microsoft code-signed and may trigger an
operating-system warning. Download them only from this repository and verify `SHA256SUMS` if
needed. The updater signature is separate and is checked by the app before installation.

### macOS says the app is damaged

If you downloaded the DMG from this repository's official Release page and verified its
checksum, this warning is normally caused by Gatekeeper quarantine rather than a damaged app.
Move `worktree-gc.app` to `/Applications`, quit it if it is running, then run:

```bash
sudo xattr -r -d com.apple.quarantine "/Applications/worktree-gc.app"
open "/Applications/worktree-gc.app"
```

Enter your macOS login password when `sudo` prompts; the terminal does not display password
characters while you type. This removes only the quarantine attribute from this app — it does
not disable Gatekeeper globally. If the app is installed elsewhere, replace the path with its
actual location.

On macOS, the optional daily health check only scans and sends a notification — it never
cleans automatically.

## Install the Codex plugin

Requirements:

- Git
- Node.js 20 or newer

### Prebuilt bundle (recommended)

Download the `worktree-gc-codex-<platform>.tar.gz` asset from
[GitHub Releases](https://github.com/LonelyFellas/worktree-gc/releases), unpack it,
then install that directory as a local marketplace:

```bash
codex plugin marketplace add /absolute/path/to/worktree-gc-codex-<platform>
codex plugin add worktree-gc@worktree-gc
```

The platform bundle contains the matching `wtgc` binary, so Rust is not required.

### Install from source

This route additionally requires Rust 1.95 or newer. Install the local scanner
(the final `wtgc` selects the CLI package from this multi-package repository):

```bash
cargo install --git https://github.com/LonelyFellas/worktree-gc --locked --bin wtgc wtgc
```

Then add the GitHub marketplace and install the plugin:

```bash
codex plugin marketplace add LonelyFellas/worktree-gc
codex plugin add worktree-gc@worktree-gc
```

Restart the Codex desktop app, open a new task, and ask it to scan your local
worktrees. The MCP integration is read-only and always runs offline.

## Install the CLI

### Prebuilt binary (recommended)

Download `worktree-gc-<platform>.tar.gz` from
[GitHub Releases](https://github.com/LonelyFellas/worktree-gc/releases), unpack it, and put
`wtgc` somewhere on your `PATH`.

### From source

Requires Rust 1.95 or newer:

```bash
cargo install --git https://github.com/LonelyFellas/worktree-gc --locked --bin wtgc wtgc
```

### Basic usage

```bash
# Scan and explain; this is also the default command.
wtgc --repo /path/to/repository scan

# Preview cache reclamation, then explicitly apply it.
wtgc --repo /path/to/repository reclaim
wtgc --repo /path/to/repository reclaim --apply

# Whole-worktree removal is also a dry-run until --apply is added.
wtgc --repo /path/to/repository remove
```

Without `--repo`, `wtgc` scans known AI-agent worktree locations. Use one or more `--repo`
arguments to strictly limit the scan scope, `--json` for scripts, or `--offline` to disable
forge lookups. Custom seed directories can be added with `--seed`; pair it with
`--no-default-seeds` to exclude the built-in locations.

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
| CacheSafe | Ignored by git · contains no tracked files · inside the worktree root · not a symlink · matches a known cache rule and ecosystem marker |

**B — remove the whole worktree** (Busy plus seven worktree-level gates)

| Gate | Requires |
|---|---|
| Idle | The worktree itself has been quiet for N hours |
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
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
corepack pnpm@10.33.0 --dir mcp check
corepack pnpm@10.33.0 --dir gui build
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
