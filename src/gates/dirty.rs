//! B1 —— 无未提交改动。
//!
//! 看着是最平凡的一道门，实际上是 B 组唯一「git 自己会跟着一起失守」的门：
//! `git worktree remove` 拒绝删除的依据同样是 status，**status 看不见的东西它也不拦**。
//! 所以这里不能把 remove 当兜底，两条已知的击穿路径必须自己堵死：
//!
//! - **D3** 仓库配置 `status.showUntrackedFiles=no`（大仓提速的常见设置）：
//!   新建的未跟踪文件在 porcelain 里完全消失，两层保护同时破。
//! - **D4** `skip-worktree` / `assume-unchanged`：覆盖本地配置的标准手法。
//!   实测 git 2.53：打了标记的文件改到面目全非，`status --porcelain` 仍为空，
//!   连 `git diff --quiet HEAD -- <file>` 都返回 0 —— 用 diff 判这批文件就是 fail-open。

use crate::gates::{Gate, GateCtx};
use crate::git::porcelain;
use crate::model::{Cause, GateDetail, GateId, GateStatus};
use std::collections::HashMap;

pub struct DirtyGate;

/// 报告里展示几条样例。与 A 组保持一致。
const SAMPLE: usize = 5;

impl Gate for DirtyGate {
    fn id(&self) -> GateId {
        GateId::Dirty
    }

    fn evaluate(&self, ctx: &GateCtx<'_>) -> GateStatus {
        // `-c` 是 git 的**全局**选项，必须排在子命令之前才生效；写成
        // `git status -c ...` 会被当成 status 的参数直接报错。
        // `-uall` 是第二道保险：即便覆盖生效，默认的 `normal` 模式也只报折叠的目录名。
        let changed = match ctx.git.run_ok(
            ctx.worktree,
            &["-c", "status.showUntrackedFiles=all", "status", "--porcelain=v1", "-z", "-uall"],
        ) {
            Ok(out) => porcelain::parse_status_z(&out.stdout),
            Err(c) => return GateStatus::Unknown(c),
        };
        if !changed.is_empty() {
            // 已经确定要拦，就不必再为 D4 逐个文件起子进程——判定结果不会因此改变
            return blocked(changed);
        }

        // status 干净不等于真干净：带标记的条目被它整个跳过，只能自己比对内容。
        match marked_and_modified(ctx) {
            Ok(v) if v.is_empty() => GateStatus::Pass,
            Ok(v) => blocked(v),
            Err(c) => GateStatus::Unknown(c),
        }
    }
}

fn blocked(paths: Vec<String>) -> GateStatus {
    GateStatus::Blocked(GateDetail::UncommittedChanges {
        count: paths.len(),
        sample: paths.into_iter().take(SAMPLE).collect(),
    })
}

/// 找出被标记隐藏、且工作区内容与索引不一致的文件（D4）。
fn marked_and_modified(ctx: &GateCtx<'_>) -> Result<Vec<String>, Cause> {
    // core.quotePath=false：否则非 ASCII 文件名会被转义成 `"\344\275\240"`，
    // 与下面 `-z` 读出来的原始路径对不上，整道门白白退化成 Unknown。
    let listing = ctx
        .git
        .run_ok(ctx.worktree, &["-c", "core.quotePath=false", "ls-files", "-v"])?
        .stdout_utf8();
    let hidden = porcelain::parse_ls_files_marked(&listing);
    if hidden.is_empty() {
        return Ok(Vec::new());
    }

    let index = index_oids(ctx)?;
    let mut dirty = Vec::new();
    for path in hidden {
        if differs_from_index(ctx, &index, &path)? {
            dirty.push(path);
        }
    }
    Ok(dirty)
}

/// 一次读出整个索引的 oid 表。`git ls-files -s -z` 每条形如
/// `<mode> <oid> <stage>\t<path>`，冲突态下同一路径有 stage 1/2/3 三条，全收。
///
/// 整表一次读完而不是逐文件查：省掉每个文件一次子进程，也避开了路径当 pathspec
/// 传回去时 `*` `[` `?` 被当通配符解释的风险（匹到别的文件 = 拿错 oid = 误判干净）。
fn index_oids(ctx: &GateCtx<'_>) -> Result<HashMap<String, Vec<String>>, Cause> {
    let out = ctx.git.run_ok(ctx.worktree, &["ls-files", "-s", "-z"])?;
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for rec in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let s = String::from_utf8_lossy(rec);
        let Some((meta, path)) = s.split_once('\t') else {
            continue;
        };
        if let Some(oid) = meta.split(' ').nth(1) {
            map.entry(path.to_string()).or_default().push(oid.to_string());
        }
    }
    Ok(map)
}

/// 工作区里的这个文件，内容是否已经和索引记录的不一致。
fn differs_from_index(
    ctx: &GateCtx<'_>,
    index: &HashMap<String, Vec<String>>,
    path: &str,
) -> Result<bool, Cause> {
    let abs = ctx.worktree.join(path);

    // 文件被删掉同样是未提交改动。用 symlink_metadata 而非 exists()：
    // 断链的符号链接也是「文件还在」，不该当成删除。
    match std::fs::symlink_metadata(&abs) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(Cause::Io { path: abs, msg: e.to_string() }),
    }

    // quotePath 只管非 ASCII；文件名含换行或引号时 `ls-files -v` 仍会转义，
    // 于是在 `-z` 读出的原始路径表里查不到。判不准就说判不准，绝不当成「没改」（D7）。
    let Some(want) = index.get(path) else {
        return Err(Cause::CommandFailed {
            cmd: "git ls-files -s -z".into(),
            code: Some(0),
            stderr: format!("索引里找不到 `ls-files -v` 列出的路径：{path}"),
        });
    };

    // 用 hash-object 而不是直接比字节：它会按 .gitattributes 的 clean 过滤器与
    // 换行规则算 oid，与索引里存的形态可比。裸比字节会被 autocrlf 之类整批误判成脏。
    let raw = ctx.git.run_ok(ctx.worktree, &["hash-object", "--", path])?.stdout_utf8();
    let got = raw.trim();
    if got.is_empty() {
        return Err(Cause::CommandFailed {
            cmd: format!("git hash-object -- {path}"),
            code: Some(0),
            stderr: "退出码为 0 但没有输出 oid".into(),
        });
    }
    Ok(!want.iter().any(|o| o == got))
}
