//! B6 —— 未被 `git worktree lock` 锁定。
//!
//! 这道门和别的门不一样：其余门禁都在**推断**「删了会不会丢东西」，
//! 这一道是用户已经**明说过**「别动这个」。典型场景是 worktree 放在外置盘上，
//! 用户专门敲了一条 lock 命令免得 git 自己把它 prune 掉。
//! 越过一个显式的锁，等于把用户最后一道自保手段拆掉。
//!
//! 锁状态取自 `git worktree list --porcelain` 的 `locked` 字段，
//! 而不是去读 `.git/worktrees/<id>/locked` —— 后者是 git 的内部布局，
//! 会随版本变，且 worktree 的 gitdir 位置本身就要另行解析才知道。

use crate::gates::{Gate, GateCtx};
use crate::git::porcelain;
use crate::model::{Cause, GateDetail, GateId, GateStatus};

pub struct LockedGate;

impl Gate for LockedGate {
    fn id(&self) -> GateId {
        GateId::Locked
    }

    fn evaluate(&self, ctx: &GateCtx<'_>) -> GateStatus {
        let out = match ctx
            .git
            .run_ok(ctx.repo, &["worktree", "list", "--porcelain"])
        {
            Ok(o) => o,
            // 拿不到注册表就等于不知道锁没锁。这里返回 Pass 就是 D7 的 fail-open。
            Err(c) => return GateStatus::Unknown(c),
        };

        // 两边都 canonicalize：git 报的是解析过 symlink 的真实路径
        // （macOS 上 /var/... 一律变成 /private/var/...），字面比较必然对不上。
        let want = match ctx.worktree.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return GateStatus::Unknown(Cause::Io {
                    path: ctx.worktree.to_path_buf(),
                    msg: e.to_string(),
                });
            }
        };

        // 条目路径 canonicalize 失败（目录已消失的 prunable 记录）就跳过：
        // 那条记录一定不是我们要找的这个存在着的 worktree。
        let entry = porcelain::parse_worktree_list(&out.stdout_utf8())
            .into_iter()
            .find(|e| e.path.canonicalize().is_ok_and(|p| p == want));

        match entry {
            Some(e) => status_from_entry(&e),
            // 这个 worktree 不在注册表里 —— 我们和 git 对现状的认知已经不一致，
            // 此时任何猜测都没有依据（它可能刚被 move 走、也可能压根不是 worktree）。
            None => GateStatus::Unknown(Cause::Io {
                path: want,
                msg: "git worktree list --porcelain 中没有这条注册记录".into(),
            }),
        }
    }
}

impl LockedGate {
    /// 扫描阶段复用 discover 已取得的 porcelain 条目；apply 仍走 [`Gate::evaluate`]
    /// 重新读取注册表，不能拿扫描时快照代替执行前复检。
    pub(crate) fn evaluate_entry(&self, entry: &porcelain::WorktreeEntry) -> GateStatus {
        status_from_entry(entry)
    }
}

fn status_from_entry(entry: &porcelain::WorktreeEntry) -> GateStatus {
    match &entry.locked {
        // `git worktree lock` 不带 --reason 时 porcelain 只有裸的 `locked`，
        // 解析出来是空串。照样拦，只是没有理由可展示。
        Some(reason) if reason.is_empty() => blocked(None),
        Some(reason) => blocked(Some(unquote_c_style(reason))),
        None => GateStatus::Pass,
    }
}

fn blocked(reason: Option<String>) -> GateStatus {
    GateStatus::Blocked(GateDetail::WorktreeLocked { reason })
}

/// 还原被 C 风格转义的锁定理由。
///
/// 非 `-z` 的 porcelain 里，git 只在理由含非 ASCII / 双引号 / 控制字符时才加引号
/// （`write_name_quoted` 的行为），所以不能无条件解码：
/// `locked external drive` 是原样，`locked "\345\244\226..."` 才是转义过的。
/// 之所以不改用 `-z` 绕开这件事：`-z` 用 NUL 分隔记录，
/// 现成的 `parse_worktree_list` 是按行解析的，换格式等于重写解析器。
///
/// 解不出来就原样返回 —— 理由只用于展示给人看，拦下的结论不受它影响。
fn unquote_c_style(s: &str) -> String {
    let inner = match s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(i) => i,
        None => return s.to_string(),
    };

    let mut bytes: Vec<u8> = Vec::with_capacity(inner.len());
    let mut it = inner.bytes().peekable();
    while let Some(b) = it.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match it.next() {
            Some(b'a') => bytes.push(0x07),
            Some(b'b') => bytes.push(0x08),
            Some(b'f') => bytes.push(0x0c),
            Some(b'n') => bytes.push(b'\n'),
            Some(b'r') => bytes.push(b'\r'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'v') => bytes.push(0x0b),
            // 八进制转义：非 ASCII 字节被逐字节拆成 \nnn，最多三位
            Some(d @ b'0'..=b'7') => {
                let mut v = d - b'0';
                let mut digits = 1;
                while digits < 3 {
                    match it.peek() {
                        Some(&n) if (b'0'..=b'7').contains(&n) => {
                            // 只有畸形输入（如 \777）才会溢出，wrapping 是为了不 panic
                            v = v.wrapping_mul(8).wrapping_add(n - b'0');
                            it.next();
                            digits += 1;
                        }
                        _ => break,
                    }
                }
                bytes.push(v);
            }
            // 覆盖 \" 与 \\，以及任何我们没预料到的转义
            Some(other) => bytes.push(other),
            // 结尾悬空的反斜杠说明这串不是我们以为的格式，别硬解
            None => return s.to_string(),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
