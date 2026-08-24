import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { ScanReport } from "./types";
import { humanBytes } from "./types";
import { describeDetail, statusText } from "./describe";
import { adviceFor, primaryBlocker } from "./advice";
import { Collapsible } from "./Collapsible";
import "./App.css";

type PlanSummary = {
  id: string;
  items: { label: string; bytes: number }[];
  estimated_bytes: number;
  rejected: string[];
};
type ApplySummary = {
  done: number; stale: number; failed: number;
  measured_freed: number; lines: string[];
};

/** 一行待办：把 worktree 和它的缓存拍平成「一件可处置的事」。 */
type Item = {
  key: string;
  name: string;
  fullPath: string;
  branch: string;
  bytes: number;
  /** 现在就能安全回收的字节数 */
  free: number;
  blocked: ReturnType<typeof primaryBlocker>;
  unknown: string | null;
  isMain: boolean;
};

function toItems(report: ScanReport): Item[] {
  return report.repos.flatMap((repo) =>
    repo.worktrees.map((wt) => {
      const free = wt.caches
        .filter((c) => c.outcomes.every((o) => o.status === "Pass"))
        .reduce((n, c) => n + c.bytes, 0);
      const unknownOutcome = wt.outcomes.find(
        (o) => typeof o.status === "object" && "Unknown" in o.status,
      );
      return {
        key: wt.path,
        name: wt.path.split("/").slice(-1)[0],
        fullPath: wt.path,
        branch: wt.branch ?? "detached",
        bytes: wt.bytes,
        // 主工作区的缓存默认不碰——它是你天天在用的那个，重建代价最实在
        free: wt.is_main ? 0 : free,
        blocked: primaryBlocker(wt.outcomes),
        unknown: unknownOutcome ? statusText(unknownOutcome.status)?.text ?? null : null,
        isMain: wt.is_main,
      } satisfies Item;
    }),
  );
}

function Bar({ value, max }: { value: number; max: number }) {
  // 体积必须有视觉权重。原来 22.4G 和 4.4M 长得一模一样，
  // 而这个工具存在的意义就是先找到大头。
  const pct = max > 0 ? Math.max(2, (value / max) * 100) : 0;
  return <div className="bar"><div style={{ width: `${pct}%` }} /></div>;
}

function Row({ item, max }: { item: Item; max: number }) {
  const advice = item.blocked ? adviceFor(item.blocked) : null;
  return (
    <div className="row">
      <div className="row-main">
        <span className="row-name" title={item.fullPath}>{item.name}</span>
        <span className="row-branch">{item.branch}</span>
        <span className="row-size">{humanBytes(item.bytes)}</span>
      </div>
      <Bar value={item.bytes} max={max} />
      {item.blocked && <div className="row-why">{describeDetail(item.blocked)}</div>}
      {item.unknown && <div className="row-why unknown">{item.unknown}</div>}
      {advice && <div className="row-advice">→ {advice}</div>}
    </div>
  );
}

export default function App() {
  const [repos, setRepos] = useState<string[]>([]);
  const [report, setReport] = useState<ScanReport | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [pending, setPending] = useState<PlanSummary | null>(null);
  const [result, setResult] = useState<ApplySummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showIdle, setShowIdle] = useState(false);
  const [showRepos, setShowRepos] = useState(false);
  // 路径以 ~ 缩写显示——绝对路径又长又挤，看的人只需要认出是哪个仓。
  // 从已有清单反推 home，省一个后端往返。
  const home = repos[0]?.match(/^(\/Users\/[^/]+)/)?.[1] ?? "";

  useEffect(() => { void refresh(); }, []);

  async function refresh() {
    setBusy("扫描中");
    setError(null);
    setResult(null);
    try {
      const r = await invoke<string[]>("default_repos");
      setRepos(r);
      if (r.length) setReport(await invoke<ScanReport>("scan_repos", { repos: r, offline: false }));
    } catch (e) { setError(String(e)); }
    finally { setBusy(null); }
  }

  async function addRepo() {
    const picked = await open({ directory: true, multiple: false, title: "选择要监控的仓库" });
    if (typeof picked !== "string") return;
    setError(null);
    try {
      // 后端会把选中的子目录归位到仓库根，并拒绝非 git 目录——
      // 静默收下一个非仓库路径，只会让之后每次扫描都多一条无解的噪音
      setRepos(await invoke<string[]>("add_repo", { path: picked }));
      await refresh();
    } catch (e) { setError(String(e)); }
  }

  async function dropRepo(path: string) {
    setError(null);
    try {
      setRepos(await invoke<string[]>("remove_repo", { path }));
      await refresh();
    } catch (e) { setError(String(e)); }
  }

  async function proposeReclaim() {
    setBusy("正在计算可回收的部分");
    setError(null);
    try {
      setPending(await invoke<PlanSummary>("create_plan", {
        repos, kind: "reclaim", includeMain: false,
      }));
    } catch (e) { setError(String(e)); }
    finally { setBusy(null); }
  }

  async function confirmApply() {
    if (!pending) return;
    setBusy("正在回收");
    try {
      setResult(await invoke<ApplySummary>("apply_plan", { id: pending.id }));
      setPending(null);
      await refresh();
    } catch (e) { setError(String(e)); setPending(null); }
    finally { setBusy(null); }
  }

  const items = useMemo(() => (report ? toItems(report) : []), [report]);
  const max = Math.max(1, ...items.map((i) => i.bytes));
  const freeNow = items.reduce((n, i) => n + i.free, 0);
  const needsYou = items.filter((i) => i.free === 0 && (i.blocked || i.unknown) && !i.isMain)
    .sort((a, b) => b.bytes - a.bytes);
  const idle = items.filter((i) => !needsYou.includes(i) && i.free === 0)
    .sort((a, b) => b.bytes - a.bytes);
  const ready = items.filter((i) => i.free > 0).sort((a, b) => b.free - a.free);

  return (
    <main>
      <header>
        <h1>worktree-gc</h1>
        <button className="ghost" onClick={refresh} disabled={!!busy}>
          {busy ? busy + "…" : "重新扫描"}
        </button>
      </header>

      {error && <div className="error">{error}</div>}

      {result && (
        <div className="done-card">
          <strong>已释放 {humanBytes(Math.max(0, result.measured_freed))}</strong>
          <span>完成 {result.done} 项{result.stale ? `，跳过 ${result.stale} 项` : ""}</span>
          {result.lines.map((l, i) => <div key={i} className="done-line">{l}</div>)}
        </div>
      )}

      {/* 头条只放「现在就能动手」的量，而不是所有可回收的量。
          原来把默认受保护的主仓缓存算进头条，最醒目的数字反而不可操作。 */}
      <section className="hero">
        <div className="hero-num">{humanBytes(freeNow)}</div>
        <div className="hero-label">
          现在就能安全回收
          {report && <span className="hero-sub">磁盘剩余 {humanBytes(report.available_bytes)}</span>}
        </div>
        <button className="primary" onClick={proposeReclaim} disabled={!!busy || freeNow === 0}>
          回收
        </button>
      </section>

      {ready.length > 0 && (
        <section>
          <h2>可以立即回收 <em>{ready.length}</em></h2>
          {ready.map((i) => (
            <div key={i.key} className="row ok">
              <div className="row-main">
                <span className="row-name" title={i.fullPath}>{i.name}</span>
                <span className="row-branch">{i.branch}</span>
                <span className="row-size">{humanBytes(i.free)}</span>
              </div>
              <Bar value={i.free} max={max} />
            </div>
          ))}
        </section>
      )}

      {needsYou.length > 0 && (
        <section>
          <h2>需要你决定 <em>{needsYou.length}</em></h2>
          {needsYou.map((i) => <Row key={i.key} item={i} max={max} />)}
        </section>
      )}

      {idle.length > 0 && (
        <Collapsible
          open={showIdle}
          title="不用管"
          count={idle.length}
          onToggle={() => setShowIdle(!showIdle)}
        >
          {idle.map((i) => <Row key={i.key} item={i} max={max} />)}
        </Collapsible>
      )}

      {repos.length === 0 && !busy && (
        <div className="onboard">
          <p>还没有监控任何仓库。</p>
          <button className="primary" onClick={addRepo}>添加仓库</button>
        </div>
      )}

      {report && repos.length > 0 && items.length === 0 && (
        <div className="empty">这些仓库里没有发现 worktree。</div>
      )}

      {repos.length > 0 && (
        <Collapsible
          open={showRepos}
          title="监控的仓库"
          count={repos.length}
          onToggle={() => setShowRepos(!showRepos)}
          action={
            <button className="add" onClick={addRepo} title="添加仓库">+</button>
          }
        >
          {repos.map((r) => (
            <div key={r} className="repo-line">
              <span title={r}>{r.replace(home, "~")}</span>
              <button className="tiny" onClick={() => dropRepo(r)}>移除</button>
            </div>
          ))}
          <p className="repo-note">
            清单存在 <code>~/.claude/skills/worktree-gc/repos.txt</code>，
            每日定时体检读的是同一份。
          </p>
        </Collapsible>
      )}

      {pending && (
        <div className="sheet" onClick={() => setPending(null)}>
          <div className="sheet-body" onClick={(e) => e.stopPropagation()}>
            <h3>回收 {humanBytes(pending.estimated_bytes)} 构建缓存？</h3>
            <p className="sheet-note">
              只删可重建的构建产物。源码与未提交改动不受影响，下次构建会重新生成。
            </p>
            <ul>
              {pending.items.map((it, i) => (
                <li key={i}><span>{it.label}</span><em>{humanBytes(it.bytes)}</em></li>
              ))}
            </ul>
            {pending.rejected.length > 0 && (
              <p className="sheet-note">跳过 {pending.rejected.length} 项（判定未放行）</p>
            )}
            <div className="sheet-actions">
              <button className="ghost" onClick={() => setPending(null)}>取消</button>
              <button className="primary" onClick={confirmApply} disabled={!!busy}>
                {busy ?? "确认回收"}
              </button>
            </div>
          </div>
        </div>
      )}
    </main>
  );
}
