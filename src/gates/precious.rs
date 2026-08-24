//! B3 —— 无独有的敏感文件。
//!
//! `git worktree remove` **会静默删掉被忽略的文件**：实测 `.env.local`、`secrets/prod.key`
//! 都是无声消失，git 连一句提示都没有。所以这道门只能本工具自己把，指望 git 兜底等于没有兜底。
//!
//! **黑名单语义**（D2）：原型用白名单只保护列出的模式，实测漏掉了 `.tfstate`、
//! Android 签名密钥、本地 sqlite、模型权重——别人仓里这个集合是无界的，列不完。
//! 这里翻转过来：**凡是被忽略、又不属于已知可弃的构建缓存，都要人看一眼。**
//!
//! 但「被忽略」不等于「宝贵」：各 worktree 的 `.env` 绝大多数只是主工作区的纯副本，
//! 全拦下来这道门就恒假、失去意义。所以再与主工作区（`ctx.repo`）同路径文件比对内容，
//! 只留主仓没有、或内容不同的那些。

use crate::gates::{Gate, GateCtx};
use crate::git::porcelain::split_z;
use crate::model::{Cause, GateDetail, GateId, GateStatus};
use std::io::BufRead;
use std::path::{Path, PathBuf};

/// 被忽略的目录最多往下走这么多层。
///
/// 不设上限等于把「一个被忽略的巨型数据目录」变成一次全盘遍历。超限的目录**不会被放过**，
/// 而是整个当成待人确认——见 `inspect` 里的 `depth >= MAX_DEPTH` 分支。
const MAX_DEPTH: usize = 4;

/// 最多列这么多条。判定在第一条就已经确定，继续扫只是让报告更长、让扫描更慢。
const MAX_REPORTED: usize = 50;

pub struct PreciousGate;

impl Gate for PreciousGate {
    fn id(&self) -> GateId {
        GateId::Precious
    }

    fn evaluate(&self, ctx: &GateCtx<'_>) -> GateStatus {
        // `--directory` 是这道门的性能命门：没有它，一个 30GB 的 target/ 会被逐文件列出来；
        // 有它则整个目录折叠成一行 `target/`，下面按名字一次跳过。
        let out = match ctx.git.run_ok(
            ctx.worktree,
            &[
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
                "-z",
            ],
        ) {
            Ok(o) => o,
            // 列不出来就是判不准。此处返回 Pass 等于宣布「没有敏感文件」，正是 D7 的形状。
            Err(c) => return GateStatus::Unknown(c),
        };

        let mut found: Vec<PathBuf> = Vec::new();
        for entry in split_z(&out.stdout) {
            // 折叠出来的目录条目带尾斜杠（`secrets/`）。原型在这里用 `[ -f ]` 一判了之，
            // 于是整个含 prod.key 的目录被当成「不是文件」跳过、随 worktree 一起没了（D1）。
            let rel = entry.trim_end_matches('/');
            if rel.is_empty() {
                continue;
            }
            if let Err(c) = inspect(ctx, Path::new(rel), 0, &mut found) {
                return GateStatus::Unknown(c);
            }
            if found.len() >= MAX_REPORTED {
                break;
            }
        }

        if found.is_empty() {
            GateStatus::Pass
        } else {
            // `read_dir` 的顺序由文件系统决定，同一份状态两次扫描能给出不同排列。
            // 报告与指纹都要按这份列表对账，顺序不稳会变成假的「状态变了」。
            found.sort();
            GateStatus::Blocked(GateDetail::PreciousFiles { paths: found })
        }
    }
}

/// 递归检查一个被忽略的条目，`rel` 相对 worktree 根。
///
/// 返回 `Err` 的都是「验不了」的情况，调用方一律落 `Unknown`——
/// 这里任何一个 `?` 换成忽略错误，都是一次静默的数据丢失。
fn inspect(
    ctx: &GateCtx<'_>,
    rel: &Path,
    depth: usize,
    found: &mut Vec<PathBuf>,
) -> Result<(), Cause> {
    let here = ctx.worktree.join(rel);
    let meta = std::fs::symlink_metadata(&here).map_err(|e| io_cause(&here, &e))?;

    // 软链不跟随：跟过去比对的是链接目标的内容，会把「指向别处的私钥」认成主仓副本。
    if meta.file_type().is_symlink() {
        found.push(here);
        return Ok(());
    }

    if meta.is_dir() {
        if is_disposable(ctx, rel) {
            return Ok(());
        }
        if depth >= MAX_DEPTH {
            // 验不动了。宁可让人看一眼整个目录，也不能默认深处什么都没有。
            found.push(here);
            return Ok(());
        }
        let entries = std::fs::read_dir(&here).map_err(|e| io_cause(&here, &e))?;
        for entry in entries {
            let entry = entry.map_err(|e| io_cause(&here, &e))?;
            inspect(ctx, &rel.join(entry.file_name()), depth + 1, found)?;
            if found.len() >= MAX_REPORTED {
                break;
            }
        }
        return Ok(());
    }

    // socket / fifo 之类没法比对内容，按拿不准处理。
    if !meta.is_file() {
        found.push(here);
        return Ok(());
    }

    // 额外保险：这些即便与主仓一模一样也拦——一份私钥拷贝两处仍然是私钥。
    if is_always_precious(ctx, rel) {
        found.push(here);
        return Ok(());
    }

    let mirror = ctx.repo.join(rel);
    match std::fs::symlink_metadata(&mirror) {
        // 主工作区没有同路径文件 → 这一份是 worktree 独有的
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            found.push(here);
            return Ok(());
        }
        Err(e) => return Err(io_cause(&mirror, &e)),
        // 同路径存在但不是普通文件，没法比对内容
        Ok(m) if !m.is_file() => {
            found.push(here);
            return Ok(());
        }
        Ok(_) => {}
    }

    if same_content(&here, &mirror).map_err(|e| io_cause(&here, &e))? {
        Ok(()) // 与主仓逐字节相同的纯副本，删了没有损失
    } else {
        found.push(here);
        Ok(())
    }
}

/// 名字和生态 marker 必须同时匹配，且**只对目录生效**。
/// 否则一个被忽略、恰好叫 `target/` 的资料目录会绕过敏感文件检查。
fn is_disposable(ctx: &GateCtx<'_>, rel: &Path) -> bool {
    match rel.file_name().and_then(|n| n.to_str()) {
        Some(name) if ctx.cfg.precious.disposable_dirs.iter().any(|d| d == name) => ctx
            .cfg
            .cache_rules
            .iter()
            .find(|rule| rule.dir == name)
            .is_some_and(|rule| rule.has_marker_for(ctx.worktree, rel)),
        None => false,
        Some(_) => false,
    }
}

fn is_always_precious(ctx: &GateCtx<'_>, rel: &Path) -> bool {
    let full = rel.to_string_lossy();
    // git 的 `-z` 输出一律用 `/` 分隔，不必考虑平台分隔符
    let name = match full.rsplit('/').next() {
        Some(n) => n,
        None => full.as_ref(),
    };
    ctx.cfg
        .precious
        .always_precious
        .iter()
        .any(|p| glob_match(p, full.as_ref()) || glob_match(p, name))
}

/// 极简 glob，只认 `*`。配置里的模式都是 `*.tfstate` / `id_rsa` 这种形状，
/// 为它引一个 glob crate 不划算。
fn glob_match(pat: &str, s: &str) -> bool {
    let mut parts = pat.split('*');
    let head = match parts.next() {
        Some(h) => h,
        None => return false,
    };
    let mut segs: Vec<&str> = parts.collect();
    let tail = match segs.pop() {
        Some(t) => t,
        None => return pat == s, // 不含 `*`，就是字面比较
    };
    let mut rest = match s.strip_prefix(head) {
        Some(r) => r,
        None => return false,
    };
    for seg in segs {
        match rest.find(seg) {
            Some(i) => rest = &rest[i + seg.len()..],
            None => return false,
        }
    }
    // 长度这一比不能省：否则 `*.tfstate` 会匹配上只有 7 个字符的 `tfstate`
    tail.len() <= rest.len() && rest.ends_with(tail)
}

/// 逐块比对而非整读进内存：被忽略的东西里正有 sqlite、模型权重这类大文件。
fn same_content(a: &Path, b: &Path) -> std::io::Result<bool> {
    let (ma, mb) = (std::fs::metadata(a)?, std::fs::metadata(b)?);
    if ma.len() != mb.len() {
        return Ok(false);
    }
    let mut fa = std::io::BufReader::new(std::fs::File::open(a)?);
    let mut fb = std::io::BufReader::new(std::fs::File::open(b)?);
    loop {
        let ba = fa.fill_buf()?;
        let bb = fb.fill_buf()?;
        if ba.is_empty() || bb.is_empty() {
            return Ok(ba.is_empty() && bb.is_empty());
        }
        let n = ba.len().min(bb.len());
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
        fa.consume(n);
        fb.consume(n);
    }
}

fn io_cause(path: &Path, e: &std::io::Error) -> Cause {
    Cause::Io {
        path: path.to_path_buf(),
        msg: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn star_matches_suffix_but_not_the_bare_suffix() {
        assert!(glob_match("*.tfstate", "foo.tfstate"));
        assert!(glob_match("*.tfstate", "infra/prod.tfstate"));
        assert!(!glob_match("*.tfstate", "tfstate"));
        assert!(!glob_match("*.tfstate", "foo.tfstate.bak"));
    }

    #[test]
    fn pattern_without_star_is_literal() {
        assert!(glob_match("id_rsa", "id_rsa"));
        assert!(!glob_match("id_rsa", "id_rsa.pub"));
        assert!(!glob_match("id_rsa", "my_id_rsa"));
    }
}
