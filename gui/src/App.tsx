import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ScanReport, WorktreeReport, RepoReport } from "./types";
import { humanBytes } from "./types";
import { describeCause, statusText, verdictBadge } from "./describe";
import "./App.css";

/** 末两段路径。多个 agent worktree 常同名，只显示 basename 分不清。 */
function shortName(p: string): string {
  return p.split("/").slice(-2).join("/");
}

function reclaimableBytes(wt: WorktreeReport): number {
  return wt.caches
    .filter((c) => c.outcomes.every((o) => o.status === "Pass"))
    .reduce((n, c) => n + c.bytes, 0);
}

function Worktree({ wt }: { wt: WorktreeReport }) {
  const badge = verdictBadge(wt.verdict);
  const reasons = wt.outcomes.map((o) => statusText(o.status)).filter(Boolean);

  return (
    <div className={`wt ${badge.cls}`}>
      <div className="wt-head">
        <span className="badge">{badge.icon} {badge.label}</span>
        <span className="size">{humanBytes(wt.bytes)}</span>
        <span className="name">{shortName(wt.path)}</span>
        <span className="branch">{wt.branch ?? "detached"}</span>
      </div>

      {/* 拦下与判不准的理由要展开——这正是用户要做决策的地方，折叠起来这个界面就没价值了 */}
      {reasons.map((r, i) => (
        <div key={i} className={`reason ${r!.kind}`}>
          {r!.kind === "unknown" ? "❓" : "└"} {r!.text}
        </div>
      ))}

      {wt.caches.map((c) => {
        const blocker = c.outcomes.map((o) => statusText(o.status)).find(Boolean);
        return (
          <div key={c.path} className={`cache ${blocker ? "held" : "free"}`}>
            {blocker ? "⛔" : "💡"} {c.kind.name}/ ({humanBytes(c.bytes)})
            {blocker && <span className="why"> 暂不回收：{blocker.text}</span>}
          </div>
        );
      })}
    </div>
  );
}

function Repo({ repo }: { repo: RepoReport }) {
  if (!repo.baseline) {
    // 基线探测失败绝不能静默略过：用户会把「没扫到」误读成「没东西可清」
    return (
      <section className="repo">
        <h2>{repo.root}</h2>
        <div className="repo-error">
          ⛔ 未能识别主干分支，本仓未做判定
          {repo.baseline_error && <div className="reason">└ {describeCause(repo.baseline_error)}</div>}
        </div>
      </section>
    );
  }
  const base = repo.baseline.remote
    ? `${repo.baseline.remote}/${repo.baseline.branch}`
    : repo.baseline.branch;

  return (
    <section className="repo">
      <h2>
        {repo.root} <span className="baseline">基线 {base}</span>
      </h2>
      {repo.worktrees.map((wt) => <Worktree key={wt.path} wt={wt} />)}
      {repo.prunable.length > 0 && (
        <div className="prunable">🧹 有 {repo.prunable.length} 条陈旧注册记录（目录已消失）</div>
      )}
    </section>
  );
}

export default function App() {
  const [repos, setRepos] = useState<string[]>([]);
  const [report, setReport] = useState<ScanReport | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<string[]>("default_repos").then(setRepos).catch(() => setRepos([]));
  }, []);

  async function runScan() {
    setScanning(true);
    setError(null);
    try {
      setReport(await invoke<ScanReport>("scan_repos", { repos, offline: false }));
    } catch (e) {
      setError(String(e));
    } finally {
      setScanning(false);
    }
  }

  const totalReclaimable = report
    ? report.repos.flatMap((r) => r.worktrees).reduce((n, wt) => n + reclaimableBytes(wt), 0)
    : 0;
  const unclear = report
    ? report.repos.flatMap((r) => r.worktrees)
        .filter((wt) => typeof wt.verdict === "object" && "NeedsAttention" in wt.verdict).length
    : 0;

  return (
    <main>
      <header>
        <div>
          <h1>worktree-gc</h1>
          <p className="sub">回收 AI coding agent 留下的 worktree 所占磁盘</p>
        </div>
        <button onClick={runScan} disabled={scanning || repos.length === 0}>
          {scanning ? "扫描中…" : "扫描"}
        </button>
      </header>

      {repos.length === 0 && (
        <div className="empty">
          还没有配置要扫的仓库。在 <code>~/.claude/skills/worktree-gc/repos.txt</code> 里
          一行一个写上仓库路径。
        </div>
      )}

      {scanning && (
        <div className="empty">
          正在扫描 —— 要跑 git 子进程并遍历目录，几十秒是正常的。
        </div>
      )}

      {error && <div className="error">扫描失败：{error}</div>}

      {report && !scanning && (
        <>
          <div className="summary">
            {/* 标"约"不是谦虚：APFS 写时复制会让 du 口径系统性高估 */}
            <div><strong>{humanBytes(totalReclaimable)}</strong><span>约可回收缓存</span></div>
            <div><strong>{humanBytes(report.available_bytes)}</strong><span>当前可用</span></div>
            {unclear > 0 && <div className="warn"><strong>{unclear}</strong><span>判不准</span></div>}
          </div>
          {report.repos.map((r) => <Repo key={r.root} repo={r} />)}
          <footer>
            {report.tools.map((t) => (
              <span key={t.name}>
                {t.name}: {t.version ? `${t.version.split("\n")[0]}` : "未找到"}
              </span>
            ))}
          </footer>
        </>
      )}
    </main>
  );
}
