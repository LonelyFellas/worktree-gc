# worktree-gc

[English](README.md) · **简体中文**

**安全地**回收 AI coding agent 留下的 git worktree 所占磁盘。

> ⚠️ **状态：早期开发中。** 安全门禁引擎与其回归测试正在落地，CLI 尚未接线，
> 目前还不能当工具用。

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
| CacheSafe | 被 git 忽略 · 不含任何 tracked 文件 · 位于 worktree 根内 · 非符号链接 · 匹配已知缓存规则 |

**B 组 —— 删除整个 worktree**（A 组之上再加六道）

| 门禁 | 要求 |
|---|---|
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
cargo clippy --all-targets
```

测试**构造真实的 git 仓库**，而不是 mock git。这个项目遇到的每一个真实 bug
都是 git 的行为出乎意料 —— `worktree remove` 静默吃掉 `.env.local`、
squash merge 之后祖先判定失效、`ls-files --directory` 把整个目录折叠成一行。
mock 掉 git 只会测出我们自己编的故事。

测试套件由一份 16 条数据丢失场景的清单驱动，每一条都是先复现、再修复。
它们以回归测试的形式落在 `tests/` 下 —— 想知道某道门禁为什么写成那样，从那里看起。

## 许可

MIT OR Apache-2.0
