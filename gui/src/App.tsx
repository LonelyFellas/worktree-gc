import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import type { RemoveTarget, ScanEnvelope, ScanReport } from "./types";
import { humanBytes } from "./types";
import { describeDetail, statusText } from "./describe";
import { adviceFor, primaryBlocker } from "./advice";
import { Collapsible } from "./Collapsible";
import { Sidebar } from "./Sidebar";
import { initialLanguage, LANGUAGE_KEY, messages, type Language } from "./i18n";
import "./App.css";

type PlanSummary = {
  id: string;
  items: { label: string; bytes: number }[];
  estimated_bytes: number;
  rejected: string[];
};
type ApplySummary = {
  done: number; stale: number; failed: number;
  measured_freed: number; lines: { zh: string; en: string }[];
};
type DailyCheckStatus = {
  supported: boolean;
  enabled: boolean;
  schedule: string;
};
type Page = "dashboard" | "settings";
type BusyState = "scanning" | "planning" | "reclaiming" | "removing";
type SidebarPosition = "left" | "right";
type LayoutDensity = "compact" | "standard" | "comfortable";
type RepoDropTarget = { path: string; edge: "before" | "after" };
type RepoDragPreview = {
  path: string;
  pointerX: number;
  pointerY: number;
  offsetX: number;
  offsetY: number;
  width: number;
};
type RepoContextMenu = { repo: string; left: number; top: number };
type CacheAdapterKind =
  | "cargo_sccache" | "gradle_build_cache" | "pnpm_store" | "pub_cache" | "uv_cache";
type CacheAdapterState = "shared" | "configured" | "available" | "missing_tool";
type RepoCacheSettings = {
  sccache_enabled: boolean;
  gradle_build_cache_enabled: boolean;
  sccache_dir: string | null;
  pnpm_store_dir: string | null;
  pub_cache_dir: string | null;
  uv_cache_dir: string | null;
};
type CachePathSetting =
  | "sccache_dir" | "pnpm_store_dir" | "pub_cache_dir" | "uv_cache_dir";
type CacheAdapterStatus = {
  kind: CacheAdapterKind;
  state: CacheAdapterState;
  path: string | null;
  tool: string | null;
};
type RepoCacheProfile = {
  repo: string;
  settings: RepoCacheSettings;
  adapters: CacheAdapterStatus[];
};
type UpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "current" }
  | { kind: "available"; version: string }
  | { kind: "downloading"; progress: number | null }
  | { kind: "installing" }
  | { kind: "error"; message: string };

const SIDEBAR_POSITION_KEY = "worktree-gc.sidebar-position";
const LAYOUT_DENSITY_KEY = "worktree-gc.layout-density";
const SIDEBAR_WIDTH_KEY = "worktree-gc.sidebar-width";
const FEEDBACK_URL = "https://github.com/LonelyFellas/worktree-gc/issues/new";
const DEFAULT_SIDEBAR_WIDTH = 268;
const MIN_SIDEBAR_WIDTH = 220;
const MAX_SIDEBAR_WIDTH = 420;
const COLLAPSE_SIDEBAR_WIDTH = 140;

function clampSidebarWidth(width: number) {
  return Math.min(MAX_SIDEBAR_WIDTH, Math.max(MIN_SIDEBAR_WIDTH, Math.round(width)));
}

function commandErrorText(error: unknown, language: Language) {
  const localized = (value: unknown) => {
    if (!value || typeof value !== "object") return null;
    const candidate = value as Partial<Record<Language, unknown>>;
    return typeof candidate[language] === "string" ? candidate[language] : null;
  };
  const direct = localized(error);
  if (direct) return direct;
  if (typeof error === "string") {
    try {
      const parsed = localized(JSON.parse(error));
      if (parsed) return parsed;
    } catch {
      // 兼容不是结构化错误的系统或插件异常。
    }
  }
  return String(error);
}

function savedSidebarPosition(): SidebarPosition {
  try {
    return localStorage.getItem(SIDEBAR_POSITION_KEY) === "left" ? "left" : "right";
  } catch {
    return "right";
  }
}

function savedLayoutDensity(): LayoutDensity {
  try {
    const density = localStorage.getItem(LAYOUT_DENSITY_KEY);
    return density === "compact" || density === "comfortable" ? density : "standard";
  } catch {
    return "standard";
  }
}

function savedSidebarWidth() {
  try {
    const width = Number(localStorage.getItem(SIDEBAR_WIDTH_KEY));
    return Number.isFinite(width) && width > 0
      ? clampSidebarWidth(width)
      : DEFAULT_SIDEBAR_WIDTH;
  } catch {
    return DEFAULT_SIDEBAR_WIDTH;
  }
}

function repoTargetAt(clientX: number, clientY: number, source: string): RepoDropTarget | null {
  const card = document
    .elementFromPoint(clientX, clientY)
    ?.closest<HTMLElement>(".repo-card[data-repo-path]");
  const path = card?.dataset.repoPath;
  if (!card || !path || path === source) return null;
  const rect = card.getBoundingClientRect();
  return { path, edge: clientY < rect.top + rect.height / 2 ? "before" : "after" };
}

function cachePathSetting(kind: CacheAdapterKind): CachePathSetting | null {
  switch (kind) {
    case "cargo_sccache": return "sccache_dir";
    case "pnpm_store": return "pnpm_store_dir";
    case "pub_cache": return "pub_cache_dir";
    case "uv_cache": return "uv_cache_dir";
    case "gradle_build_cache": return null;
  }
}

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
  removeTarget: RemoveTarget | null;
};

function toItems(
  report: ScanReport,
  language: Language,
  removeTargets: RemoveTarget[],
): Item[] {
  const ui = messages[language];
  const targetByPath = new Map(removeTargets.map((target) => [target.path, target]));
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
        branch: wt.branch ?? ui.notOnBranch,
        bytes: wt.bytes,
        // 主工作区的缓存默认不碰——它是你天天在用的那个，重建代价最实在
        free: wt.is_main ? 0 : free,
        blocked: primaryBlocker(wt.outcomes),
        unknown: unknownOutcome ? statusText(unknownOutcome.status, language)?.text ?? null : null,
        isMain: wt.is_main,
        removeTarget: targetByPath.get(wt.path) ?? null,
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

function blockerBadge(detail: Item["blocked"], language: Language) {
  if (!detail) return null;
  const status = messages[language].status;
  if ("ProcessesActive" in detail) return { label: status.processesActive, tone: "info" };
  if ("UncommittedChanges" in detail) return { label: status.uncommitted, tone: "warn" };
  if ("NotLanded" in detail) return { label: status.notLanded, tone: "note" };
  if ("RecentlyModified" in detail) return { label: status.recentlyModified, tone: "info" };
  if ("NotPureCache" in detail) return { label: status.cacheUnknown, tone: "warn" };
  if ("PreciousFiles" in detail) return { label: status.preciousFiles, tone: "warn" };
  if ("NestedWorktrees" in detail) return { label: status.nestedWorktrees, tone: "note" };
  if ("OperationInProgress" in detail) return { label: status.operationInProgress, tone: "info" };
  return { label: status.locked, tone: "neutral" };
}

function Row({
  item,
  language,
  disabled,
  onRemove,
}: {
  item: Item;
  language: Language;
  disabled: boolean;
  onRemove: (item: Item) => void;
}) {
  const ui = messages[language];
  const advice = item.blocked ? adviceFor(item.blocked, language) : null;
  const badge = blockerBadge(item.blocked, language)
    ?? (item.unknown ? { label: ui.checkIncomplete, tone: "warn" } : null);
  const detail = item.blocked ? describeDetail(item.blocked, language) : item.unknown;
  return (
    <div className="row">
      <div className="row-main">
        {badge && <span className={`row-status ${badge.tone}`}>{badge.label}</span>}
        <span className="row-name" title={item.fullPath}>{item.name}</span>
        <span className="row-branch" title={item.branch}>{item.branch}</span>
        <span className="row-size">{humanBytes(item.bytes)}</span>
        {item.removeTarget && (
          <button
            type="button"
            className="row-remove"
            disabled={disabled}
            onClick={() => onRemove(item)}
          >
            {ui.removeWorktree}
          </button>
        )}
      </div>
      {detail && (
        <div className={`row-why${item.unknown ? " unknown" : ""}`} title={detail}>
          {detail}
        </div>
      )}
      {advice && (
        <div className="row-advice">
          <strong>{ui.nextStep}</strong>
          <span>{advice}</span>
        </div>
      )}
    </div>
  );
}

export default function App() {
  const [repos, setRepos] = useState<string[]>([]);
  const [report, setReport] = useState<ScanReport | null>(null);
  const [scanId, setScanId] = useState<string | null>(null);
  const [removeTargets, setRemoveTargets] = useState<RemoveTarget[]>([]);
  const [busy, setBusy] = useState<BusyState | null>(null);
  const [pending, setPending] = useState<PlanSummary | null>(null);
  const [result, setResult] = useState<ApplySummary | null>(null);
  const [removePrompt, setRemovePrompt] = useState<{ item: Item; input: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [hasScanned, setHasScanned] = useState(false);
  const [showIdle, setShowIdle] = useState(false);
  const [showRepos, setShowRepos] = useState(true);
  const [dailyCheck, setDailyCheck] = useState<DailyCheckStatus | null>(null);
  const [dailyCheckBusy, setDailyCheckBusy] = useState(false);
  const [page, setPage] = useState<Page>("dashboard");
  const [language, setLanguage] = useState<Language>(initialLanguage);
  const [sidebarPosition, setSidebarPosition] = useState<SidebarPosition>(savedSidebarPosition);
  const [layoutDensity, setLayoutDensity] = useState<LayoutDensity>(savedLayoutDensity);
  const [sidebarWidth, setSidebarWidth] = useState(savedSidebarWidth);
  const [sidebarResizing, setSidebarResizing] = useState(false);
  const sidebarResizeStart = useRef<{ pointerX: number; width: number } | null>(null);
  const [draggedRepo, setDraggedRepo] = useState<string | null>(null);
  const [repoDropTarget, setRepoDropTarget] = useState<RepoDropTarget | null>(null);
  const [repoOrderBusy, setRepoOrderBusy] = useState(false);
  const [repoDragPreview, setRepoDragPreview] = useState<RepoDragPreview | null>(null);
  const [repoContextMenu, setRepoContextMenu] = useState<RepoContextMenu | null>(null);
  const [cacheDialogRepo, setCacheDialogRepo] = useState<string | null>(null);
  const [cacheProfiles, setCacheProfiles] = useState<RepoCacheProfile[]>([]);
  const [cacheProfilesLoading, setCacheProfilesLoading] = useState(false);
  const [cacheSaveBusy, setCacheSaveBusy] = useState<string | null>(null);
  const [appVersion, setAppVersion] = useState("…");
  const [updateState, setUpdateState] = useState<UpdateState>({ kind: "idle" });
  const updateResource = useRef<Update | null>(null);
  const startupUpdateCheck = useRef(false);
  const ui = messages[language];
  // 路径以 ~ 缩写显示——绝对路径又长又挤，看的人只需要认出是哪个仓。
  // 从已有清单反推 home，省一个后端往返。
  const home = repos[0]?.match(/^(\/Users\/[^/]+)/)?.[1] ?? "";

  useEffect(() => {
    void refresh();
    void refreshDailyCheck();
    if (!startupUpdateCheck.current) {
      startupUpdateCheck.current = true;
      void getVersion().then(setAppVersion);
      void checkForUpdate();
    }
    return () => {
      void updateResource.current?.close();
      updateResource.current = null;
    };
  }, []);

  useEffect(() => {
    document.documentElement.lang = language === "zh" ? "zh-CN" : "en";
    void invoke("set_ui_language", { language }).catch((error) => {
      setError(`${messages[language].operationFailed}: ${commandErrorText(error, language)}`);
    });
  }, [language]);

  useEffect(() => {
    if (!repoContextMenu && !cacheDialogRepo) return;
    const close = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape") return;
      setRepoContextMenu(null);
      setCacheDialogRepo(null);
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [repoContextMenu, cacheDialogRepo]);

  useEffect(() => {
    if (!cacheDialogRepo) return;
    void refreshSharedCaches();
  }, [cacheDialogRepo, repos]);

  async function refreshDailyCheck() {
    try {
      setDailyCheck(await invoke<DailyCheckStatus>("daily_check_status"));
    } catch (e) {
      setError(`${ui.operationFailed}: ${commandErrorText(e, language)}`);
    }
  }

  async function refreshSharedCaches() {
    setCacheProfilesLoading(true);
    setError(null);
    try {
      setCacheProfiles(await invoke<RepoCacheProfile[]>("shared_cache_profiles", { repos }));
    } catch (e) {
      setError(`${ui.operationFailed}: ${commandErrorText(e, language)}`);
    } finally {
      setCacheProfilesLoading(false);
    }
  }

  function updateCacheSetting<K extends keyof RepoCacheSettings>(
    repo: string,
    key: K,
    value: RepoCacheSettings[K],
  ) {
    setCacheProfiles((current) => current.map((profile) => profile.repo === repo
      ? { ...profile, settings: { ...profile.settings, [key]: value } }
      : profile));
  }

  async function saveCacheSettings(profile: RepoCacheProfile) {
    setCacheSaveBusy(profile.repo);
    setError(null);
    try {
      const saved = await invoke<RepoCacheProfile>("save_shared_cache_settings", {
        repo: profile.repo,
        settings: profile.settings,
      });
      setCacheProfiles((current) => current.map((item) =>
        item.repo === profile.repo ? saved : item));
    } catch (e) {
      setError(`${ui.operationFailed}: ${commandErrorText(e, language)}`);
    } finally {
      setCacheSaveBusy(null);
    }
  }

  function openRepoMenu(repo: string, event: ReactMouseEvent<HTMLElement>) {
    event.preventDefault();
    const width = 180;
    const height = 84;
    const margin = 8;
    setRepoContextMenu({
      repo,
      left: Math.max(margin, Math.min(event.clientX, window.innerWidth - width - margin)),
      top: Math.max(margin, Math.min(event.clientY, window.innerHeight - height - margin)),
    });
  }

  function openRepoCache(repo: string) {
    setRepoContextMenu(null);
    setCacheDialogRepo(repo);
  }

  async function toggleDailyCheck() {
    if (!dailyCheck?.supported) return;
    setDailyCheckBusy(true);
    setError(null);
    try {
      setDailyCheck(await invoke<DailyCheckStatus>("set_daily_check_enabled", {
        enabled: !dailyCheck.enabled,
      }));
    } catch (e) {
      setError(`${ui.operationFailed}: ${commandErrorText(e, language)}`);
    } finally {
      setDailyCheckBusy(false);
    }
  }

  async function checkForUpdate() {
    if (updateState.kind === "checking" || updateState.kind === "downloading"
      || updateState.kind === "installing") return;
    setUpdateState({ kind: "checking" });
    try {
      const available = await check({ timeout: 15_000 });
      if (updateResource.current && updateResource.current !== available) {
        await updateResource.current.close();
      }
      updateResource.current = available;
      setUpdateState(available
        ? { kind: "available", version: available.version }
        : { kind: "current" });
    } catch (e) {
      setUpdateState({ kind: "error", message: commandErrorText(e, language) });
    }
  }

  async function installUpdate() {
    const update = updateResource.current;
    if (!update || updateState.kind !== "available") return;
    let downloaded = 0;
    let contentLength: number | undefined;
    setUpdateState({ kind: "downloading", progress: null });
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          contentLength = event.data.contentLength;
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
        }
        const progress = contentLength && contentLength > 0
          ? Math.min(100, Math.round((downloaded / contentLength) * 100))
          : null;
        setUpdateState({ kind: "downloading", progress });
      }, { timeout: 5 * 60_000 });
      setUpdateState({ kind: "installing" });
      await relaunch();
    } catch (e) {
      setUpdateState({ kind: "error", message: commandErrorText(e, language) });
    }
  }

  function updateStatusText() {
    switch (updateState.kind) {
      case "idle": return ui.currentVersion(appVersion);
      case "checking": return ui.checkingForUpdates;
      case "current": return `${ui.currentVersion(appVersion)} · ${ui.updateCurrent}`;
      case "available": return `${ui.currentVersion(appVersion)} · ${ui.updateAvailable(updateState.version)}`;
      case "downloading": return ui.updateDownloading(updateState.progress);
      case "installing": return ui.updateInstalling;
      case "error": return `${ui.currentVersion(appVersion)} · ${ui.updateCheckFailed}`;
    }
  }

  function changeSidebarPosition(position: SidebarPosition) {
    setSidebarPosition(position);
    try {
      localStorage.setItem(SIDEBAR_POSITION_KEY, position);
    } catch {
      // WebView 禁用本地存储时仍允许本次会话切换位置。
    }
  }

  function changeLayoutDensity(density: LayoutDensity) {
    setLayoutDensity(density);
    try {
      localStorage.setItem(LAYOUT_DENSITY_KEY, density);
    } catch {
      // WebView 禁用本地存储时仍允许本次会话调整间距。
    }
  }

  function changeLanguage(next: Language) {
    setLanguage(next);
    try {
      localStorage.setItem(LANGUAGE_KEY, next);
    } catch {
      // WebView 禁用本地存储时仍允许本次会话切换语言。
    }
  }

  async function openFeedback() {
    setError(null);
    try {
      await openUrl(FEEDBACK_URL);
    } catch (e) {
      setError(`${ui.feedbackFailed}: ${commandErrorText(e, language)}`);
    }
  }

  function widthAtPointer(clientX: number) {
    const start = sidebarResizeStart.current;
    if (!start) return sidebarWidth;
    const delta = sidebarPosition === "right"
      ? start.pointerX - clientX
      : clientX - start.pointerX;
    return Math.min(MAX_SIDEBAR_WIDTH, Math.round(start.width + delta));
  }

  function saveSidebarWidth(width: number) {
    const next = clampSidebarWidth(width);
    setSidebarWidth(next);
    try {
      localStorage.setItem(SIDEBAR_WIDTH_KEY, String(next));
    } catch {
      // WebView 禁用本地存储时仍允许本次会话调整宽度。
    }
  }

  function beginSidebarResize(event: ReactPointerEvent<HTMLDivElement>) {
    if (!showRepos || event.button !== 0) return;
    sidebarResizeStart.current = { pointerX: event.clientX, width: sidebarWidth };
    event.currentTarget.setPointerCapture(event.pointerId);
    setSidebarResizing(true);
    event.preventDefault();
  }

  function moveSidebarResize(event: ReactPointerEvent<HTMLDivElement>) {
    const start = sidebarResizeStart.current;
    if (!start) return;
    const next = widthAtPointer(event.clientX);
    if (next <= COLLAPSE_SIDEBAR_WIDTH) {
      sidebarResizeStart.current = null;
      if (event.currentTarget.hasPointerCapture(event.pointerId)) {
        event.currentTarget.releasePointerCapture(event.pointerId);
      }
      saveSidebarWidth(start.width);
      setSidebarResizing(false);
      setShowRepos(false);
      return;
    }

    // 最小宽度以下仍跟随鼠标，松手时再回弹；这样拖动收起不会卡在 220px。
    setSidebarWidth(next);
    if (next >= MIN_SIDEBAR_WIDTH) {
      try {
        localStorage.setItem(SIDEBAR_WIDTH_KEY, String(next));
      } catch {
        // WebView 禁用本地存储时仍允许本次会话调整宽度。
      }
    }
  }

  function finishSidebarResize(event: ReactPointerEvent<HTMLDivElement>) {
    if (!sidebarResizeStart.current) return;
    const next = widthAtPointer(event.clientX);
    sidebarResizeStart.current = null;
    setSidebarResizing(false);
    saveSidebarWidth(next);
  }

  function cancelSidebarResize() {
    const start = sidebarResizeStart.current;
    if (!start) return;
    sidebarResizeStart.current = null;
    saveSidebarWidth(start.width);
    setSidebarResizing(false);
  }

  function resizeSidebarWithKeyboard(event: ReactKeyboardEvent<HTMLDivElement>) {
    const step = event.shiftKey ? 24 : 8;
    let delta = 0;
    if (event.key === "ArrowLeft") delta = sidebarPosition === "right" ? step : -step;
    if (event.key === "ArrowRight") delta = sidebarPosition === "right" ? -step : step;
    if (delta === 0) return;
    event.preventDefault();
    saveSidebarWidth(sidebarWidth + delta);
  }

  async function refresh() {
    setBusy("scanning");
    setError(null);
    setResult(null);
    setScanId(null);
    setRemoveTargets([]);
    try {
      const r = await invoke<string[]>("default_repos");
      setRepos(r);
      const scan = await invoke<ScanEnvelope>("scan_repos", { repos: r, offline: false });
      setReport(scan.report);
      setScanId(scan.scan_id);
      setRemoveTargets(scan.remove_targets);
    } catch (e) { setError(`${ui.operationFailed}: ${commandErrorText(e, language)}`); }
    finally {
      setHasScanned(true);
      setBusy(null);
    }
  }

  async function addRepo() {
    const picked = await open({ directory: true, multiple: false, title: ui.pickerTitle });
    if (typeof picked !== "string") return;
    setError(null);
    try {
      // 后端会把选中的子目录归位到仓库根，并拒绝非 git 目录——
      // 静默收下一个非仓库路径，只会让之后每次扫描都多一条无解的噪音
      setRepos(await invoke<string[]>("add_repo", { path: picked }));
      await refresh();
    } catch (e) { setError(`${ui.operationFailed}: ${commandErrorText(e, language)}`); }
  }

  async function dropRepo(path: string) {
    setRepoContextMenu(null);
    setCacheDialogRepo(null);
    setError(null);
    try {
      setRepos(await invoke<string[]>("remove_repo", { path }));
      await refresh();
    } catch (e) { setError(`${ui.operationFailed}: ${commandErrorText(e, language)}`); }
  }

  async function reorderRepo(source: string, target: string, edge: RepoDropTarget["edge"]) {
    if (source === target || repoOrderBusy) return;
    const previous = [...repos];
    const next = repos.filter((repo) => repo !== source);
    let targetIndex = next.indexOf(target);
    if (targetIndex < 0) return;
    if (edge === "after") targetIndex += 1;
    next.splice(targetIndex, 0, source);
    if (next.every((repo, index) => repo === previous[index])) return;

    setRepoOrderBusy(true);
    setError(null);
    setRepos(next);
    try {
      setRepos(await invoke<string[]>("reorder_repos", { repos: next }));
    } catch (e) {
      setRepos(previous);
      setError(`${ui.operationFailed}: ${commandErrorText(e, language)}`);
    } finally {
      setRepoOrderBusy(false);
    }
  }

  async function proposeReclaim() {
    if (!scanId) return;
    setBusy("planning");
    setError(null);
    try {
      setPending(await invoke<PlanSummary>("create_plan", {
        scanId, kind: "reclaim", includeMain: false,
      }));
    } catch (e) { setError(`${ui.operationFailed}: ${commandErrorText(e, language)}`); }
    finally { setBusy(null); }
  }

  async function confirmApply() {
    if (!pending) return;
    setBusy("reclaiming");
    try {
      const summary = await invoke<ApplySummary>("apply_plan", { id: pending.id });
      setPending(null);
      await refresh();
      setResult(summary);
    } catch (e) {
      setError(`${ui.operationFailed}: ${commandErrorText(e, language)}`);
      setPending(null);
    }
    finally { setBusy(null); }
  }

  async function confirmForceRemove() {
    if (!scanId || !removePrompt?.item.removeTarget) return;
    setBusy("removing");
    setError(null);
    try {
      const target = removePrompt.item.removeTarget;
      const p = await invoke<PlanSummary>("create_plan", {
        scanId,
        kind: "remove",
        includeMain: false,
        targetId: target.id,
        confirmation: removePrompt.input,
      });
      const summary = await invoke<ApplySummary>("apply_plan", { id: p.id });
      setRemovePrompt(null);
      await refresh();
      setResult(summary);
    } catch (e) {
      setRemovePrompt(null);
      setError(`${ui.operationFailed}: ${commandErrorText(e, language)}`);
    } finally {
      setBusy(null);
    }
  }

  const items = useMemo(
    () => (report ? toItems(report, language, removeTargets) : []),
    [report, language, removeTargets],
  );
  const freeNow = items.reduce((n, i) => n + i.free, 0);
  const needsYou = items.filter((i) => i.free === 0 && (i.blocked || i.unknown) && !i.isMain)
    .sort((a, b) => b.bytes - a.bytes);
  const idle = items.filter((i) => !needsYou.includes(i) && i.free === 0)
    .sort((a, b) => b.bytes - a.bytes);
  const ready = items.filter((i) => i.free > 0).sort((a, b) => b.free - a.free);
  const readyMax = Math.max(1, ...ready.map((i) => i.free));
  const scanning = busy === "scanning" || !hasScanned;
  const clean = !!report && items.length > 0 && freeNow === 0 && needsYou.length === 0;
  const cacheAdapterNames: Record<CacheAdapterKind, string> = {
    cargo_sccache: ui.cacheCargoSccache,
    gradle_build_cache: ui.cacheGradleBuild,
    pnpm_store: ui.cachePnpmStore,
    pub_cache: ui.cachePub,
    uv_cache: ui.cacheUv,
  };
  const cacheStateLabels: Record<CacheAdapterState, string> = {
    shared: ui.cacheStateShared,
    configured: ui.cacheStateConfigured,
    available: ui.cacheStateAvailable,
    missing_tool: ui.cacheStateMissingTool,
  };
  const cacheDialogProfile = cacheDialogRepo
    ? cacheProfiles.find((profile) => profile.repo === cacheDialogRepo) ?? null
    : null;

  return (
    <div
      className={`shell sidebar-${sidebarPosition} density-${layoutDensity}${sidebarResizing ? " sidebar-resizing" : ""}`}
      style={{ "--sidebar-width": `${sidebarWidth}px` } as CSSProperties}
    >
    <main>
      <header>
        <h1>{page === "settings" ? ui.settings : "worktree-gc"}</h1>
        <div className="header-actions">
          {page === "settings" ? (
            <button className="ghost" onClick={() => setPage("dashboard")}>{ui.done}</button>
          ) : (
            <button className="ghost" onClick={refresh} disabled={!!busy}>
              {busy ? `${ui.busy[busy]}…` : ui.rescan}
            </button>
          )}
        </div>
      </header>

      {error && <div className="error">{error}</div>}

      {page === "settings" ? (
        <section className="settings-page">
          <h2>{ui.interfaceSection}</h2>
          <div className="setting-card">
            <div className="setting-copy">
              <strong>{ui.sidebarPosition}</strong>
              <span>{ui.sidebarPositionDescription}</span>
            </div>
            <div className="segmented" role="radiogroup" aria-label={ui.sidebarPosition}>
              <button
                type="button"
                role="radio"
                aria-checked={sidebarPosition === "left"}
                className={sidebarPosition === "left" ? "active" : ""}
                onClick={() => changeSidebarPosition("left")}
              >
                {ui.left}
              </button>
              <button
                type="button"
                role="radio"
                aria-checked={sidebarPosition === "right"}
                className={sidebarPosition === "right" ? "active" : ""}
                onClick={() => changeSidebarPosition("right")}
              >
                {ui.right}
              </button>
            </div>
          </div>
          <div className="setting-card">
            <div className="setting-copy">
              <strong>{ui.density}</strong>
              <span>{ui.densityDescription}</span>
            </div>
            <div className="segmented" role="radiogroup" aria-label={ui.density}>
              <button
                type="button"
                role="radio"
                aria-checked={layoutDensity === "compact"}
                className={layoutDensity === "compact" ? "active" : ""}
                onClick={() => changeLayoutDensity("compact")}
              >
                {ui.compact}
              </button>
              <button
                type="button"
                role="radio"
                aria-checked={layoutDensity === "standard"}
                className={layoutDensity === "standard" ? "active" : ""}
                onClick={() => changeLayoutDensity("standard")}
              >
                {ui.standard}
              </button>
              <button
                type="button"
                role="radio"
                aria-checked={layoutDensity === "comfortable"}
                className={layoutDensity === "comfortable" ? "active" : ""}
                onClick={() => changeLayoutDensity("comfortable")}
              >
                {ui.comfortable}
              </button>
            </div>
          </div>
          <div className="setting-card">
            <div className="setting-copy">
              <strong>{ui.language}</strong>
              <span>{ui.languageDescription}</span>
            </div>
            <div className="segmented" role="radiogroup" aria-label={ui.language}>
              <button
                type="button"
                role="radio"
                aria-checked={language === "zh"}
                className={language === "zh" ? "active" : ""}
                onClick={() => changeLanguage("zh")}
              >
                {ui.chinese}
              </button>
              <button
                type="button"
                role="radio"
                aria-checked={language === "en"}
                className={language === "en" ? "active" : ""}
                onClick={() => changeLanguage("en")}
              >
                {ui.english}
              </button>
            </div>
          </div>
          <h2>{ui.automation}</h2>
          <div className="setting-card">
            <div className="setting-copy">
              <strong>{ui.dailyCheck}</strong>
              <span>
                {!dailyCheck
                  ? ui.dailyLoading
                  : dailyCheck.supported
                    ? ui.dailySchedule(dailyCheck.schedule)
                    : ui.dailyUnsupported}
              </span>
              <span>{ui.repoListLocation} <code>repos.txt</code></span>
            </div>
            <button
              type="button"
              className={`switch${dailyCheck?.enabled ? " on" : ""}`}
              role="switch"
              aria-checked={dailyCheck?.enabled ?? false}
              aria-label={ui.dailyCheck}
              disabled={!dailyCheck?.supported || dailyCheckBusy}
              onClick={toggleDailyCheck}
            >
              <span />
            </button>
          </div>
          <h2>{ui.updates}</h2>
          <div className="setting-card">
            <div className="setting-copy">
              <strong>{ui.applicationVersion}</strong>
              <span title={updateState.kind === "error" ? updateState.message : undefined}>
                {updateStatusText()}
              </span>
            </div>
            <button
              type="button"
              className={updateState.kind === "available" ? "primary setting-action" : "ghost setting-action"}
              disabled={updateState.kind === "checking" || updateState.kind === "downloading"
                || updateState.kind === "installing"}
              onClick={updateState.kind === "available" ? installUpdate : checkForUpdate}
            >
              {updateState.kind === "available" ? ui.installUpdate : ui.checkForUpdates}
            </button>
          </div>
        </section>
      ) : (
      <>
      {result && (
        <div className="done-card">
          <strong>{ui.released(humanBytes(Math.max(0, result.measured_freed)))}</strong>
          <span>{ui.completed(result.done, result.stale)}</span>
          {result.lines.map((line, i) => (
            <div key={i} className="done-line">{line[language]}</div>
          ))}
        </div>
      )}

      {/* 扫描中不展示旧结果，避免把上一次的 0B 或容量误认为本次结论。 */}
      {scanning ? (
        <section className="hero hero-scanning" aria-live="polite">
          <span className="hero-spinner" aria-hidden="true" />
          <div className="hero-label">
            <strong className="hero-title">{ui.scanningTitle}</strong>
            <span className="hero-sub">
              {repos.length > 0 ? ui.scanningRepos(repos.length) : ui.readingRepoList}
            </span>
          </div>
        </section>
      ) : clean ? (
        <section className="hero hero-clean">
          <span className="hero-check" aria-hidden="true">✓</span>
          <div className="hero-label">
            <strong className="hero-title">{ui.cleanTitle}</strong>
            <span className="hero-sub">
              {ui.cleanSummary(report.repos.length, items.length, humanBytes(report.available_bytes))}
            </span>
          </div>
        </section>
      ) : report && freeNow > 0 ? (
        <section className="hero">
          <div className="hero-num">{humanBytes(freeNow)}</div>
          <div className="hero-label">
            {ui.safeNow}
            <span className="hero-sub">{ui.diskAvailable(humanBytes(report.available_bytes))}</span>
          </div>
          <button className="primary" onClick={proposeReclaim} disabled={!!busy || !scanId}>
            {ui.reclaim}
          </button>
        </section>
      ) : null}

      {!scanning && ready.length > 0 && (
        <section>
          <h2>{ui.safeReclaimable} <em>{ready.length}</em></h2>
          {ready.map((i) => (
            <div key={i.key} className="row ok">
              <div className="row-main">
                <span className="row-name" title={i.fullPath}>{i.name}</span>
                <span className="row-branch">{i.branch}</span>
                <span className="row-size">{humanBytes(i.free)}</span>
                {i.removeTarget && (
                  <button
                    type="button"
                    className="row-remove"
                    disabled={!!busy}
                    onClick={() => setRemovePrompt({ item: i, input: "" })}
                  >
                    {ui.removeWorktree}
                  </button>
                )}
              </div>
              <Bar value={i.free} max={readyMax} />
            </div>
          ))}
        </section>
      )}

      {!scanning && needsYou.length > 0 && (
        <section>
          <h2>{ui.needsAttention} <em>{needsYou.length}</em></h2>
          {needsYou.map((i) => (
            <Row
              key={i.key}
              item={i}
              language={language}
              disabled={!!busy}
              onRemove={(item) => setRemovePrompt({ item, input: "" })}
            />
          ))}
        </section>
      )}

      {!scanning && idle.length > 0 && (
        <Collapsible
          open={showIdle}
          title={ui.noAction}
          count={idle.length}
          onToggle={() => setShowIdle(!showIdle)}
        >
          {idle.map((i) => (
            <Row
              key={i.key}
              item={i}
              language={language}
              disabled={!!busy}
              onRemove={(item) => setRemovePrompt({ item, input: "" })}
            />
          ))}
        </Collapsible>
      )}

      {repos.length === 0 && items.length === 0 && hasScanned && !busy && (
        <div className="onboard">
          <p>{ui.noRepos}</p>
          <button className="primary" onClick={addRepo}>{ui.addRepo}</button>
        </div>
      )}

      {!scanning && report && repos.length > 0 && items.length === 0 && (
        <div className="empty">{ui.noWorktrees}</div>
      )}

      </>
      )}

    </main>

      <Sidebar
        open={showRepos}
        onToggle={() => setShowRepos(!showRepos)}
        collapseLabel={ui.collapse}
        title={ui.monitoredRepos}
        action={<button className="add" onClick={addRepo} title={ui.addRepo}>+</button>}
        resizer={(
          <div
            className="sidebar-resizer"
            role="separator"
            aria-label={ui.resizeSidebar}
            aria-orientation="vertical"
            aria-valuemin={COLLAPSE_SIDEBAR_WIDTH}
            aria-valuemax={MAX_SIDEBAR_WIDTH}
            aria-valuenow={sidebarWidth}
            tabIndex={showRepos ? 0 : -1}
            onPointerDown={beginSidebarResize}
            onPointerMove={moveSidebarResize}
            onPointerUp={finishSidebarResize}
            onPointerCancel={cancelSidebarResize}
            onKeyDown={resizeSidebarWithKeyboard}
          />
        )}
        footer={(
          <>
            <button
              type="button"
              className="sidebar-footer-icon"
              onClick={() => void openFeedback()}
              aria-label={ui.feedback}
              title={ui.feedback}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M6 5h12a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H9l-4 3v-3.3A2 2 0 0 1 4 15V7a2 2 0 0 1 2-2Z" />
                <path d="M8 9h8M8 13h5" />
              </svg>
            </button>
            <button
              type="button"
              className={`sidebar-footer-icon${page === "settings" ? " active" : ""}`}
              onClick={() => setPage("settings")}
              aria-label={ui.settings}
              title={ui.settings}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M12 8.6a3.4 3.4 0 1 0 0 6.8 3.4 3.4 0 0 0 0-6.8Z" />
                <path d="m19.2 13.5 1.2 1-.1 1-1.4 2.4-.8.5-1.5-.5a7.6 7.6 0 0 1-1.7 1l-.3 1.6-.8.5h-2.9l-.8-.5-.3-1.6a7.6 7.6 0 0 1-1.7-1l-1.5.5-.8-.5-1.4-2.4-.1-1 1.2-1a7.7 7.7 0 0 1 0-2l-1.2-1 .1-1 1.4-2.4.8-.5 1.5.5a7.6 7.6 0 0 1 1.7-1l.3-1.6.8-.5h2.9l.8.5.3 1.6a7.6 7.6 0 0 1 1.7 1l1.5-.5.8.5 1.4 2.4.1 1-1.2 1a7.7 7.7 0 0 1 0 2Z" />
              </svg>
            </button>
          </>
        )}
      >
        {repos.length === 0 && (
          <p className="side-note">{ui.emptySidebar}</p>
        )}
        {repos.map((r) => {
          const stat = report?.repos.find((x) => x.root === r);
          const wts = stat?.worktrees.length ?? 0;
          const parentPath = r.replace(home, "~").split("/").slice(0, -1).join("/");
          const free = stat
            ? stat.worktrees.reduce(
                (n, wt) =>
                  n + (wt.is_main ? 0 : wt.caches
                    .filter((c) => c.outcomes.every((o) => o.status === "Pass"))
                    .reduce((m, c) => m + c.bytes, 0)),
                0,
              )
            : 0;
          return (
            <div
              key={r}
              className={[
                "repo-card",
                draggedRepo === r ? "dragging" : "",
                repoDropTarget?.path === r ? `drop-${repoDropTarget.edge}` : "",
              ].filter(Boolean).join(" ")}
              data-repo-path={r}
              aria-label={ui.dragToSort(r.split("/").filter(Boolean).slice(-1)[0])}
              onContextMenu={(event) => openRepoMenu(r, event)}
              onPointerDown={(event) => {
                if (repoOrderBusy || event.button !== 0
                  || (event.target as Element).closest("button")) return;
                setRepoContextMenu(null);
                const rect = event.currentTarget.getBoundingClientRect();
                event.currentTarget.setPointerCapture(event.pointerId);
                setDraggedRepo(r);
                setRepoDragPreview({
                  path: r,
                  pointerX: event.clientX,
                  pointerY: event.clientY,
                  offsetX: event.clientX - rect.left,
                  offsetY: event.clientY - rect.top,
                  width: rect.width,
                });
                event.preventDefault();
              }}
              onPointerMove={(event) => {
                if (draggedRepo !== r) return;
                setRepoDragPreview((preview) => preview
                  ? { ...preview, pointerX: event.clientX, pointerY: event.clientY }
                  : null);
                setRepoDropTarget(repoTargetAt(event.clientX, event.clientY, r));
              }}
              onPointerUp={(event) => {
                if (draggedRepo === r) {
                  const target = repoTargetAt(event.clientX, event.clientY, r);
                  if (target) void reorderRepo(r, target.path, target.edge);
                }
                setDraggedRepo(null);
                setRepoDropTarget(null);
                setRepoDragPreview(null);
              }}
              onPointerCancel={() => {
                setDraggedRepo(null);
                setRepoDropTarget(null);
                setRepoDragPreview(null);
              }}
            >
              <div className="repo-top">
                <span className="repo-icon" aria-hidden="true" />
                {/* 名字单独一行且永不截断——路径从中间截断恰好把仓库名切掉，
                    而那正是用户唯一需要认出来的东西 */}
                <span className="repo-name">{r.split("/").filter(Boolean).slice(-1)[0]}</span>
              </div>
              <div className="repo-meta" title={r}>
                <span>{parentPath}</span>
                {report && <span>· {wts}</span>}
                {free > 0 && <span className="repo-free">{ui.repoReclaimable(humanBytes(free))}</span>}
              </div>
            </div>
          );
        })}
      </Sidebar>

      {repoContextMenu && (
        <div
          className="repo-context-layer"
          onPointerDown={() => setRepoContextMenu(null)}
          onContextMenu={(event) => event.preventDefault()}
        >
          <div
            className="repo-context-menu"
            role="menu"
            aria-label={ui.repoActions}
            style={{ left: repoContextMenu.left, top: repoContextMenu.top }}
            onPointerDown={(event) => event.stopPropagation()}
          >
            <button
              type="button"
              role="menuitem"
              autoFocus
              onClick={() => openRepoCache(repoContextMenu.repo)}
            >
              {ui.sharedCaches}
            </button>
            <div className="repo-context-separator" role="separator" />
            <button
              type="button"
              role="menuitem"
              className="repo-context-danger"
              onClick={() => void dropRepo(repoContextMenu.repo)}
            >
              {ui.stopMonitoring}
            </button>
          </div>
        </div>
      )}

      {repoDragPreview && (
        <div
          className="repo-drag-preview"
          style={{
            left: repoDragPreview.pointerX - repoDragPreview.offsetX,
            top: repoDragPreview.pointerY - repoDragPreview.offsetY,
            width: repoDragPreview.width,
          }}
        >
          <span className="repo-icon" aria-hidden="true" />
          <span>{repoDragPreview.path.split("/").filter(Boolean).slice(-1)[0]}</span>
        </div>
      )}

      {cacheDialogRepo && (
        <div className="sheet" onClick={() => setCacheDialogRepo(null)}>
          <div
            className="sheet-body cache-sheet-body"
            role="dialog"
            aria-modal="true"
            aria-labelledby="shared-cache-title"
            onClick={(event) => event.stopPropagation()}
          >
            <div className="cache-dialog-head">
              <div>
                <h3 id="shared-cache-title">{ui.sharedCaches}</h3>
                <p>{ui.sharedCachesDescription}</p>
              </div>
              <button
                type="button"
                className="cache-dialog-close"
                aria-label={ui.closeSharedCaches}
                onClick={() => setCacheDialogRepo(null)}
              >
                ×
              </button>
            </div>
            {cacheProfilesLoading ? (
              <p className="cache-empty">{ui.sharedCachesLoading}</p>
            ) : cacheDialogProfile ? (
              <div className="cache-repo-card">
                <div className="cache-repo-head">
                  <div className="setting-copy">
                    <strong>{cacheDialogProfile.repo.split("/").filter(Boolean).slice(-1)[0]}</strong>
                    <span title={cacheDialogProfile.repo}>
                      {cacheDialogProfile.repo.replace(home, "~")}
                    </span>
                  </div>
                  <button
                    type="button"
                    className="primary setting-action"
                    disabled={cacheSaveBusy !== null}
                    onClick={() => saveCacheSettings(cacheDialogProfile)}
                  >
                    {cacheSaveBusy === cacheDialogProfile.repo
                      ? ui.savingCacheSettings
                      : ui.saveCacheSettings}
                  </button>
                </div>
                {cacheDialogProfile.adapters.length === 0 ? (
                  <p className="cache-empty">{ui.noSupportedCaches}</p>
                ) : cacheDialogProfile.adapters.map((adapter) => {
                  const pathSetting = cachePathSetting(adapter.kind);
                  const isSccache = adapter.kind === "cargo_sccache";
                  const isGradle = adapter.kind === "gradle_build_cache";
                  const enabled = isSccache
                    ? cacheDialogProfile.settings.sccache_enabled
                    : isGradle ? cacheDialogProfile.settings.gradle_build_cache_enabled : null;
                  return (
                    <div className="cache-adapter" key={adapter.kind}>
                      <div className="cache-adapter-head">
                        <div className="setting-copy">
                          <strong>{cacheAdapterNames[adapter.kind]}</strong>
                          <span>{adapter.path ?? ui.cachePathUnavailable}</span>
                        </div>
                        <span className={`cache-state ${adapter.state}`}>
                          {cacheStateLabels[adapter.state]}
                        </span>
                        {enabled !== null && (
                          <button
                            type="button"
                            className={`switch${enabled ? " on" : ""}`}
                            role="switch"
                            aria-checked={enabled}
                            aria-label={cacheAdapterNames[adapter.kind]}
                            onClick={() => updateCacheSetting(
                              cacheDialogProfile.repo,
                              isSccache ? "sccache_enabled" : "gradle_build_cache_enabled",
                              !enabled,
                            )}
                          >
                            <span />
                          </button>
                        )}
                      </div>
                      {pathSetting && (
                        <label className="cache-path-field">
                          <span>{ui.customCachePath}</span>
                          <input
                            value={cacheDialogProfile.settings[pathSetting] ?? ""}
                            placeholder={adapter.path ?? ui.useToolDefault}
                            autoComplete="off"
                            spellCheck={false}
                            onChange={(event) => updateCacheSetting(
                              cacheDialogProfile.repo,
                              pathSetting,
                              event.target.value.trim() || null,
                            )}
                          />
                        </label>
                      )}
                      {adapter.state === "missing_tool" && (
                        <p className="cache-warning">
                          {isSccache ? ui.installSccache : ui.cacheToolMissing}
                        </p>
                      )}
                    </div>
                  );
                })}
              </div>
            ) : (
              <p className="cache-empty">{ui.sharedCachesEmpty}</p>
            )}
            <p className="setting-section-note cache-run-note">
              {ui.sharedCacheRunNote} <code>wtgc run -- &lt;command&gt;</code>
            </p>
          </div>
        </div>
      )}

      {pending && (
        <div className="sheet" onClick={() => setPending(null)}>
          <div className="sheet-body" onClick={(e) => e.stopPropagation()}>
            <h3>{ui.reclaimTitle(humanBytes(pending.estimated_bytes))}</h3>
            <p className="sheet-note">{ui.reclaimNote}</p>
            <ul>
              {pending.items.map((it, i) => (
                <li key={i}><span>{it.label}</span><em>{humanBytes(it.bytes)}</em></li>
              ))}
            </ul>
            {pending.rejected.length > 0 && (
              <p className="sheet-note">{ui.skippedByGate(pending.rejected.length)}</p>
            )}
            <div className="sheet-actions">
              <button className="ghost" onClick={() => setPending(null)}>{ui.cancel}</button>
              <button className="primary" onClick={confirmApply} disabled={!!busy}>
                {busy ? ui.busy[busy] : ui.confirmReclaim}
              </button>
            </div>
          </div>
        </div>
      )}

      {removePrompt?.item.removeTarget && (
        <div className="sheet" onClick={() => !busy && setRemovePrompt(null)}>
          <div
            className="sheet-body"
            role="dialog"
            aria-modal="true"
            aria-labelledby="remove-worktree-title"
            onClick={(event) => event.stopPropagation()}
          >
            <h3 id="remove-worktree-title">
              {ui.removeTitle(removePrompt.item.branch)}
            </h3>
            <p className="sheet-note">{ui.removeRisk}</p>
            <div className="remove-target">
              <strong>{removePrompt.item.branch}</strong>
              <span title={removePrompt.item.fullPath}>{removePrompt.item.fullPath}</span>
            </div>
            <label className="confirm-field">
              <span>{ui.typeToConfirm(removePrompt.item.removeTarget.confirmation)}</span>
              <input
                autoFocus
                value={removePrompt.input}
                disabled={busy === "removing"}
                onChange={(event) => setRemovePrompt({
                  item: removePrompt.item,
                  input: event.target.value,
                })}
                onKeyDown={(event) => {
                  if (event.key === "Enter"
                    && removePrompt.input === removePrompt.item.removeTarget?.confirmation
                    && !busy) {
                    void confirmForceRemove();
                  }
                }}
              />
            </label>
            <p className="sheet-note branch-preserved">{ui.branchPreserved}</p>
            <div className="sheet-actions">
              <button className="ghost" onClick={() => setRemovePrompt(null)} disabled={!!busy}>
                {ui.cancel}
              </button>
              <button
                className="danger"
                onClick={confirmForceRemove}
                disabled={!!busy
                  || removePrompt.input !== removePrompt.item.removeTarget.confirmation}
              >
                {busy === "removing" ? ui.busy.removing : ui.confirmRemove}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
