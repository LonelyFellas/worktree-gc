# worktree-gc

[English](README.md) · **简体中文**

[![CI](https://github.com/LonelyFellas/worktree-gc/actions/workflows/ci.yml/badge.svg)](https://github.com/LonelyFellas/worktree-gc/actions/workflows/ci.yml)
[![Release](https://github.com/LonelyFellas/worktree-gc/actions/workflows/release.yml/badge.svg)](https://github.com/LonelyFellas/worktree-gc/releases/latest)
[![最新版本](https://img.shields.io/github/v/release/LonelyFellas/worktree-gc)](https://github.com/LonelyFellas/worktree-gc/releases/latest)
[![许可证](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#许可)

安全回收 AI coding agent 留下的 git worktree 所占磁盘。

> ⚠️ **状态：早期开发中。** 桌面端可以回收判定放行的构建缓存；删除整个
> worktree 目前仅 CLI 支持，并且仍须显式传入 `--apply`。Codex 插件始终只读。

## 选择使用方式

| 方式 | 适合场景 | 是否会修改文件 |
|---|---|---|
| **桌面端** | 交互式扫描与回收构建缓存 | 确认后仅删除构建缓存 |
| **Codex 插件** | 让 Codex 检查并解释 worktree | 不会，始终只读 |
| **CLI（`wtgc`）** | 脚本、自动化与高级清理 | 默认演练，必须显式传入 `--apply` |

## 安装桌面端

从 [最新 GitHub Release](https://github.com/LonelyFellas/worktree-gc/releases/latest)
下载对应平台的安装包：

| 平台 | 安装包 |
|---|---|
| macOS · Apple Silicon | `worktree-gc_<version>_aarch64.dmg` |
| macOS · Intel | `worktree-gc_<version>_x64.dmg` |
| Windows · x64 | `worktree-gc_<version>_x64-setup.exe`（推荐）或 `.msi` |
| Linux · x64 | `worktree-gc_<version>_amd64.AppImage`（推荐）或 `.deb` |

打开应用，添加需要监控的仓库，然后开始扫描。回收操作必须经过确认，并且只会删除
通过全部安全门禁的构建缓存；源码与未提交改动不会受到影响。

应用会在启动时检查 Tauri 签名更新，也可以从 **设置 → 应用更新** 手动检查。更新源支持
macOS 应用、Windows 安装程序和 Linux AppImage；通过 MSI 或 DEB 安装时，请从 Release
页面手动升级。应用内自动更新从 `v0.1.8` 开始提供，因此更早的版本需要先手动升级一次。

Release 安装包目前尚未经过 Apple 公证或 Microsoft 代码签名，操作系统可能显示警告。
请只从本仓库下载，必要时使用 `SHA256SUMS` 校验。自动更新包使用独立签名，安装前会由
应用验证。

### macOS 提示“应用已损坏”

如果 DMG 来自本仓库的官方 Release 页面，并且校验和一致，这个提示通常是 Gatekeeper
隔离属性导致的，并不代表应用文件真的损坏。先把 `worktree-gc.app` 移到“应用程序”目录，
退出正在运行的应用，然后打开终端执行：

```bash
sudo xattr -r -d com.apple.quarantine "/Applications/worktree-gc.app"
open "/Applications/worktree-gc.app"
```

`sudo` 提示时输入当前 macOS 登录密码；输入过程中终端不会显示字符。该命令只移除这个
应用的隔离属性，不会全局关闭 Gatekeeper。如果应用安装在其他位置，请把命令中的路径
替换为实际路径。

macOS 还可以开启每日自动体检，但它只扫描和通知，绝不会自动清理。

## 安装 Codex 插件

环境要求：

- Git
- Node.js 20 或更高版本

### 预编译包（推荐）

从 [GitHub Releases](https://github.com/LonelyFellas/worktree-gc/releases) 下载
`worktree-gc-codex-<platform>.tar.gz`，解压后把该目录作为本地 marketplace 安装：

```bash
codex plugin marketplace add /绝对路径/worktree-gc-codex-<platform>
codex plugin add worktree-gc@worktree-gc
```

平台包已包含匹配的 `wtgc` 二进制，因此不需要安装 Rust。

### 从源码安装

这种方式还需要 Rust 1.95 或更高版本。先安装本地扫描器（末尾的 `wtgc`
用于从这个多包仓库中明确选择 CLI 包）：

```bash
cargo install --git https://github.com/LonelyFellas/worktree-gc --locked --bin wtgc wtgc
```

再添加 GitHub marketplace 并安装插件：

```bash
codex plugin marketplace add LonelyFellas/worktree-gc
codex plugin add worktree-gc@worktree-gc
```

重启 Codex 桌面端并新建任务，然后让它扫描本地 worktree。MCP 集成只读，
并且始终使用离线模式。

## 安装 CLI

### 预编译二进制（推荐）

从 [GitHub Releases](https://github.com/LonelyFellas/worktree-gc/releases) 下载
`worktree-gc-<platform>.tar.gz`，解压后把 `wtgc` 放进 `PATH`。

### 从源码安装

需要 Rust 1.95 或更高版本：

```bash
cargo install --git https://github.com/LonelyFellas/worktree-gc --locked --bin wtgc wtgc
```

### 基本用法

```bash
# 扫描并解释；这也是默认行为。
wtgc --repo /仓库路径 scan

# 先演练缓存回收，再显式执行。
wtgc --repo /仓库路径 reclaim
wtgc --repo /仓库路径 reclaim --apply

# 删除整个 worktree 同样默认只演练，加入 --apply 才执行。
wtgc --repo /仓库路径 remove
```

不传 `--repo` 时，`wtgc` 会扫描已知的 AI agent worktree 落点。传入一个或多个
`--repo` 可以严格限制扫描范围；脚本可使用 `--json`，`--offline` 可以禁止查询 forge。
`--seed` 用于添加自定义种子目录，与 `--no-default-seeds` 一起使用可以排除内置落点。

## 要解决的问题

AI coding agent（Claude Code、Codex、Cursor 等）会为每个任务开一个独立的 git worktree，
每个都各自跑构建。产物既不共享，也不会自己消失。

催生这个项目的真实事故：6 个 agent worktree 在 **5 天里累积 164 GB**，
把一台 494 GB 的 Mac 压到只剩 1.9 GB。这些空间的实际构成：

| | 体积 | 占比 |
|---|---|---|
| 构建产物（`target/`、`node_modules/`） | ~168 GB | 99.96% |
| 源码与未提交改动 | ~72 MB | 0.04% |

**所以主行动是回收构建缓存，不是删除 worktree。** 删除是次要动作。

## 为什么不能直接 `rm -rf */target`

因为其中一些 worktree 正有 agent 在里面构建，一些有未提交的改动，
还有一些放着 `secrets/` 目录 —— 而 `git worktree remove` 会一声不吭地把它删掉。
难的从来不是删除，是判断什么删得。

现有工具各解决了一半：

- **worktrunk / treehouse / git-parsec** 负责创建与切换 worktree，它们的 prune
  只管自己创建的那些。`worktree-gc` **只回收、不创建** —— 谁创建的都能处理。
- **kondo / npkill / cargo-sweep** 按目录名和 mtime 删产物，完全不懂 git 语义。
  它们分不清「agent 正在构建的 target」和「已经废弃的 target」。

## 它怎么判断

两组门禁对应两种完全不同的破坏半径：

**A 组 —— 回收构建缓存**（源码与未提交改动分毫不动）

| 门禁 | 要求 |
|---|---|
| Busy | 没有进程的工作目录或可执行文件位于该 worktree 内 |
| Recent | 缓存目录本身已安静 N 分钟 |
| CacheSafe | 被 git 忽略 · 不含任何 tracked 文件 · 位于 worktree 根内 · 非符号链接 · 匹配已知缓存规则及生态 marker |

**B 组 —— 删除整个 worktree**（Busy 加七道 worktree 级门禁）

| 门禁 | 要求 |
|---|---|
| Idle | worktree 本身已安静 N 小时 |
| Dirty | 无未提交改动，`showUntrackedFiles=no` 与 `skip-worktree` 两种击穿路径也要堵住 |
| Landed | 工作已进主干 —— 祖先判定、forge API、或针对 squash-merge 的路径受限 diff |
| Precious | 没有「不属于已知构建缓存」的忽略文件（黑名单语义，不是白名单） |
| Nested | 内部没有嵌套的其它 worktree 或 git 仓 |
| InProgress | 不处于 rebase / merge / cherry-pick / bisect 中间态 |
| Locked | 未被 `git worktree lock` 锁定 |

### 三态，而非两态

每道门禁返回 `Pass`、`Blocked` 或 **`Unknown`** —— 而 `Unknown` 永远不等于放行。
子命令失败、超时、当前平台缺少某项能力，统统落进 `Unknown`，
进而让该 worktree 落到 `NeedsAttention`，交给人看一眼。

**这是整个项目最重要的一个设计决定。** 它的前身原型全篇是 `cmd 2>/dev/null`
然后把空输出当成「干净」—— 于是一个损坏的 `.git` 返回退出码 128、stdout 为空，
被读成了「没有未提交改动」。

失败方向不对称：漏删只是少省几 GB，误删可能丢掉别处不存在的工作。
所有取舍一律偏向漏删。

## 开发

```bash
cargo test
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
corepack pnpm@10.33.0 --dir mcp check
corepack pnpm@10.33.0 --dir gui build
```

测试**构造真实的 git 仓库**，而不是 mock git。这个项目遇到的每一个真实 bug
都是 git 的行为出乎意料 —— `worktree remove` 静默吃掉 `.env.local`、
squash merge 之后祖先判定失效、`ls-files --directory` 把整个目录折叠成一行。
mock 掉 git 只会测出我们自己编的故事。

测试套件由一份 16 条数据丢失场景的清单驱动，每一条都是先复现、再修复。
它们以回归测试的形式落在 `tests/` 下 —— 想知道某道门禁为什么写成那样，从那里看起。

## 许可

MIT OR Apache-2.0
