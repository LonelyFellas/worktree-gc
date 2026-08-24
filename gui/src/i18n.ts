export type Language = "zh" | "en";

export const LANGUAGE_KEY = "worktree-gc.language";

export function languageFromTag(tag: string | undefined): Language {
  return tag?.toLowerCase().startsWith("zh") ? "zh" : "en";
}

export function initialLanguage(): Language {
  try {
    const saved = localStorage.getItem(LANGUAGE_KEY);
    if (saved === "zh" || saved === "en") return saved;
  } catch {
    // 本地存储不可用时仍可跟随当前系统语言。
  }
  const systemLanguage = navigator.languages?.[0] ?? navigator.language;
  return languageFromTag(systemLanguage);
}

const zh = {
  settings: "设置",
  done: "完成",
  rescan: "重新扫描",
  interfaceSection: "界面",
  sidebarPosition: "侧边栏位置",
  sidebarPositionDescription: "选择监控仓库侧边栏显示在窗口哪一侧。",
  left: "左侧",
  right: "右侧",
  density: "显示密度",
  densityDescription: "调整主界面和侧边栏的内容间距。",
  compact: "紧凑",
  standard: "标准",
  comfortable: "宽松",
  language: "语言",
  languageDescription: "默认跟随系统语言，也可以在这里手动切换。",
  chinese: "中文",
  english: "English",
  automation: "自动化",
  dailyCheck: "每日自动体检",
  dailyLoading: "正在读取状态…",
  dailyUnsupported: "目前仅支持 macOS",
  dailySchedule: (schedule: string) => `每天 ${schedule} · 只扫描和通知`,
  repoListLocation: "仓库清单保存在",
  released: (amount: string) => `已释放 ${amount}`,
  completed: (done: number, stale: number) =>
    `完成 ${done} 项${stale ? `，跳过 ${stale} 项` : ""}`,
  scanningTitle: "正在扫描工作区",
  scanningRepos: (count: number) => `正在检查 ${count} 个仓库的占用与安全门禁`,
  readingRepoList: "正在读取仓库列表",
  cleanTitle: "当前无需清理",
  cleanSummary: (repos: number, worktrees: number, available: string) =>
    `已检查 ${repos} 个仓库 · ${worktrees} 个工作区 · 磁盘剩余 ${available}`,
  safeNow: "现在就能安全回收",
  diskAvailable: (amount: string) => `磁盘剩余 ${amount}`,
  reclaim: "回收",
  safeReclaimable: "可安全回收",
  needsAttention: "需要关注",
  noAction: "无需操作",
  noRepos: "还没有监控任何仓库。",
  addRepo: "添加仓库",
  noWorktrees: "这些仓库里没有发现 worktree。",
  monitoredRepos: "监控的仓库",
  resizeSidebar: "调整侧边栏宽度",
  feedback: "反馈问题",
  dragToSort: (name: string) => `拖动排序 ${name}`,
  stopMonitoring: "不再监控",
  repoReclaimable: (amount: string) => `· ${amount} 可回收`,
  emptySidebar: "还没有仓库。点上方 + 添加。",
  collapse: "收起",
  reclaimTitle: (amount: string) => `回收 ${amount} 构建缓存？`,
  reclaimNote: "只删可重建的构建产物。源码与未提交改动不受影响，下次构建会重新生成。",
  skippedByGate: (count: number) => `跳过 ${count} 项（判定未放行）`,
  cancel: "取消",
  confirmReclaim: "确认回收",
  notOnBranch: "未在分支上",
  nextStep: "下一步",
  checkIncomplete: "检查未完成",
  pickerTitle: "选择要监控的仓库",
  operationFailed: "操作失败",
  feedbackFailed: "无法打开反馈页面",
  busy: {
    scanning: "扫描中",
    planning: "正在计算可回收的部分",
    reclaiming: "正在回收",
  },
  status: {
    processesActive: "正在使用",
    uncommitted: "有未提交改动",
    notLanded: "尚未合并",
    recentlyModified: "最近仍在写入",
    cacheUnknown: "缓存未确认",
    preciousFiles: "包含重要文件",
    nestedWorktrees: "包含嵌套工作区",
    operationInProgress: "Git 操作进行中",
    locked: "已锁定",
  },
};

const en: typeof zh = {
  settings: "Settings",
  done: "Done",
  rescan: "Rescan",
  interfaceSection: "Interface",
  sidebarPosition: "Sidebar position",
  sidebarPositionDescription: "Choose which side of the window shows monitored repositories.",
  left: "Left",
  right: "Right",
  density: "Display density",
  densityDescription: "Adjust spacing in the main view and sidebar.",
  compact: "Compact",
  standard: "Standard",
  comfortable: "Comfortable",
  language: "Language",
  languageDescription: "Defaults to your system language; you can override it here.",
  chinese: "中文",
  english: "English",
  automation: "Automation",
  dailyCheck: "Daily health check",
  dailyLoading: "Loading status…",
  dailyUnsupported: "Currently available on macOS only",
  dailySchedule: (schedule: string) => `Daily at ${schedule} · Scan and notify only`,
  repoListLocation: "Repository list is stored in",
  released: (amount: string) => `Freed ${amount}`,
  completed: (done: number, stale: number) =>
    `${done} item${done === 1 ? "" : "s"} completed${stale ? `, ${stale} skipped` : ""}`,
  scanningTitle: "Scanning worktrees",
  scanningRepos: (count: number) =>
    `Checking disk usage and safety gates for ${count} repositor${count === 1 ? "y" : "ies"}`,
  readingRepoList: "Reading repository list",
  cleanTitle: "Nothing to clean up",
  cleanSummary: (repos: number, worktrees: number, available: string) =>
    `Checked ${repos} repositor${repos === 1 ? "y" : "ies"} · ${worktrees} worktree${worktrees === 1 ? "" : "s"} · ${available} available`,
  safeNow: "Safe to reclaim now",
  diskAvailable: (amount: string) => `${amount} disk space available`,
  reclaim: "Reclaim",
  safeReclaimable: "Safe to reclaim",
  needsAttention: "Needs attention",
  noAction: "No action needed",
  noRepos: "No repositories are being monitored yet.",
  addRepo: "Add repository",
  noWorktrees: "No worktrees were found in these repositories.",
  monitoredRepos: "Monitored repositories",
  resizeSidebar: "Resize sidebar",
  feedback: "Send feedback",
  dragToSort: (name: string) => `Drag to reorder ${name}`,
  stopMonitoring: "Stop monitoring",
  repoReclaimable: (amount: string) => `· ${amount} reclaimable`,
  emptySidebar: "No repositories yet. Use + above to add one.",
  collapse: "Collapse",
  reclaimTitle: (amount: string) => `Reclaim ${amount} of build cache?`,
  reclaimNote: "Only reproducible build artifacts will be removed. Source files and uncommitted changes are unaffected; the next build will recreate the cache.",
  skippedByGate: (count: number) => `${count} item${count === 1 ? "" : "s"} skipped by safety checks`,
  cancel: "Cancel",
  confirmReclaim: "Confirm reclaim",
  notOnBranch: "Detached HEAD",
  nextStep: "Next",
  checkIncomplete: "Check incomplete",
  pickerTitle: "Choose a repository to monitor",
  operationFailed: "Operation failed",
  feedbackFailed: "Could not open the feedback page",
  busy: {
    scanning: "Scanning",
    planning: "Calculating reclaimable cache",
    reclaiming: "Reclaiming",
  },
  status: {
    processesActive: "In use",
    uncommitted: "Uncommitted changes",
    notLanded: "Not merged",
    recentlyModified: "Recently modified",
    cacheUnknown: "Cache unverified",
    preciousFiles: "Important files",
    nestedWorktrees: "Nested worktrees",
    operationInProgress: "Git operation active",
    locked: "Locked",
  },
};

export const messages: Record<Language, typeof zh> = { zh, en };
