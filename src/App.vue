<script setup lang="ts">
import { ref, watch, computed, onMounted, onUnmounted, nextTick } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { revealItemInDir, openUrl } from "@tauri-apps/plugin-opener";
import { save, open } from "@tauri-apps/plugin-dialog";

interface RepoStatus {
  path: string;
  branch: string;
  remote_url: string;
  dirty: boolean;
  changed?: number;
  ahead: number;
  behind: number;
  last_commit?: number | null;
  error?: string | null;
  parent?: string | null;
}
interface Row {
  r: RepoStatus;
  depth: number;
  hasChildren: boolean;
  expanded: boolean;
  seq: string;
  /** 未命中/未收藏、仅随父仓库展开显示的次要行 */
  follow?: boolean;
  /** 因命中子仓库而带出的父容器行（默认展开） */
  container?: boolean;
}
interface OpResult {
  path: string;
  ok: boolean;
  message: string;
}
interface ChangeFile {
  path: string;
  status: string;
}
interface CommitModal {
  repo: RepoStatus;
  changes: ChangeFile[];
  selected: Set<number>;
  message: string;
  busy: boolean;
}
interface CommitInfo {
  hash: string;
  author: string;
  time: number;
  subject: string;
}
interface LogModal {
  path: string;
  name: string;
  list: CommitInfo[];
  loading: boolean;
}

const roots = ref<string[]>([]);
const rootInput = ref("");
const repos = ref<RepoStatus[]>([]);
const scanning = ref(false);
const log = ref<string[]>([]);
try {
  const savedLog = localStorage.getItem("repopilot-log");
  if (savedLog) log.value = JSON.parse(savedLog);
} catch {
  /* 忽略 */
}
const oldUrl = ref("");
const newUrl = ref("");
const customCmd = ref("");
const switchBranch = ref("");
const branchOptions = ref<string[]>([]);
const branchSrc = ref("");
const autoRefresh = ref(false);
const countdown = ref(0);
const lastRefresh = ref("");
const busy = ref(false);
const pendingConfirm = ref<"" | "push" | "replace" | "stashpop">("");
const selected = ref<Set<string>>(new Set());
const expanded = ref<Set<string>>(new Set());
const dark = ref(false);
try {
  dark.value = localStorage.getItem("repopilot-dark") === "1";
} catch {
  /* 忽略 */
}
const showAbout = ref(false);
const appVersion = ref("0.1.0");
const commitModal = ref<CommitModal | null>(null);
const cloneModal = ref<{ url: string; base: string; busy: boolean } | null>(null);
const logModal = ref<LogModal | null>(null);
const changesTip = ref<{ path: string; files: ChangeFile[]; x: number; y: number; err?: string } | null>(null);
const changesCache = new Map<string, ChangeFile[]>();
const progress = ref<{ done: number; total: number; ok: number; path: string } | null>(null);
const batchSummary = ref<{ ok: number; fail: number } | null>(null);
const updateInfo = ref<{ update: Update; version: string; installing: boolean } | null>(null);
let progressUnlisten: (() => void) | undefined;
watch(
  dark,
  (v) => {
    document.documentElement.classList.toggle("dark", v);
    try {
      localStorage.setItem("repopilot-dark", v ? "1" : "0");
    } catch {
      /* 忽略 */
    }
  },
  { immediate: true }
);

// ===== 国际化 =====
type Lang = "zh" | "en";
const messages: Record<Lang, Record<string, string>> = {
  zh: {
    appTitle: "RepoPilot · 多仓管家",
    envWarn:
      "⚠ 浏览器预览模式：这里只是界面预览，扫描/刷新/分组等操作请在 RepoPilot 应用窗口中运行",
    addRootPlaceholder: "输入根目录（可多个），回车添加，如 /Users/you/code",
    addRoot: "添加",
    cloneTitle: "克隆仓库",
    cloneUrlPlaceholder: "Git 仓库 URL（https 或 ssh）",
    cloneBase: "目标根目录",
    cloneRun: "克隆",
    cloneDone: "已克隆",
    cloneFail: "克隆失败",
    scanPlaceholder: "输入要扫描的根目录，如 /Users/you/code",
    scan: "扫描仓库",
    scanAll: "扫描全部",
    scanning: "扫描中…",
    refresh: "刷新状态",
    removeRoot: "移除该根目录",
    logRootAdded: "已添加根目录：{p}",
    logRootRemoved: "已移除根目录：{p}",
    emptyNoRoots: "还没有根目录——输入目录并添加后点「扫描全部」",
    dark: "🌙 深色",
    light: "☀️ 浅色",
    deselect: "取消选择",
    selectVisible: "全选当前({n})",
    selected: "已选 {a} / {b}",
    showOfTotal: "匹配 {a} / {b}",
    followHint: "随父显示（未收藏 / 未命中）",
    containerHint: "父仓库（含匹配的子仓库）",
    exportCfg: "导出",
    exportCfgTip: "导出配置（根目录/分组/收藏）",
    importCfg: "导入",
    importCfgTip: "导入配置（覆盖当前）",
    importConfirmShort: "确认导入？",
    importConfirm: "再次点击「导入」确认，将覆盖当前根目录/分组/收藏",
    logExportOk: "配置已导出：{p}",
    logExportFail: "导出失败：{e}",
    logImportOk: "配置已导入",
    logImportFail: "导入失败：{e}",
    logImportSkipDirs: "已跳过不存在的目录（{n} 个）：{d}",
    logImportNoDirs: "导入的根目录均不存在，请手动添加目录",
    copyUrl: "复制 URL",
    logCopied: "已复制：{u}",
    logCopyFail: "复制失败，请手动复制",
    autoRefresh: "自动刷新",
    lastRefresh: "上次刷新 {t}",
    searchPlaceholder: "搜索仓库/路径/分支/地址…",
    busyTip: "⏳ 处理中，界面已可操作…",
    batchDone: "完成：成功 {ok} / 失败 {fail}",
    updateCheck: "检查更新",
    updateAvailable: "发现新版本 v{v}，点击安装",
    updateInstalling: "下载安装中…",
    updateNone: "已是最新版本",
    updateFail: "更新失败: {e}",
    pull: "批量 Pull",
    push: "批量 Push",
    confirmPush: "确认 Push？",
    stash: "暂存",
    stashPop: "恢复",
    confirmStashPop: "确认恢复？",
    titleRefresh: "刷新状态（⌘R）",
    titlePull: "批量 Pull（⌘P）",
    titlePush: "批量 Push（⌘U）",
    titleStash: "批量暂存改动（git stash，可随时恢复）",
    titleStashPop: "批量恢复改动（git stash pop）",
    logConfirmStashPop: "再点一次确认恢复所有选中仓库的 stash",
    groups: "分组",
    all: "全部",
    ungrouped: "未分组",
    newGroup: "+ 新建分组",
    deleteGroup: "删除该组",
    renameGroup: "重命名",
    groupRenamePlaceholder: "输入新分组名",
    logGroupRenamed: "已重命名分组：{a} → {b}",
    logGroupRenameConflict: "分组名冲突：{g} 已存在",
    confirmDelete: "确认删除？",
    groupPlaceholder: "分组名称（支持 父级/子级，如 工作/前端）",
    ok: "确定",
    cancel: "取消",
    stAll: "全部",
    stDirty: "有改动",
    stBehind: "落后",
    stAhead: "领先",
    stError: "错误",
    fAll: "时间：全部",
    f30d: "30 天内活跃",
    f90d: "90 天内活跃",
    fStale30: "超过 30 天没动",
    fStale90: "超过 90 天没动",
    emptyNoCommit: "没有符合时间范围的仓库",
    colIdx: "#",
    colName: "仓库",
    colGroup: "分组",
    colBranch: "分支",
    colStatus: "状态",
    colSync: "领先/落后",
    colCommit: "最后提交",
    colRemote: "Remote 地址",
    colPath: "路径",
    colOps: "操作",
    expand: "展开子仓库",
    collapse: "折叠子仓库",
    bErr: "错误",
    bDirty: "有改动",
    bClean: "干净",
    sync: "同步",
    noRemote: "无远程",
    cmdTitle: "批量执行自定义命令（对选中的每个仓库运行同一命令）",
    cmdPlaceholder: "如：git fetch --prune / npm update / git switch dev",
    run: "批量执行",
    cmdHint: "命令会在每个选中仓库的目录下执行，等同你在该目录的终端里手动运行。",
    swTitle: "批量切换分支（对选中的仓库执行 git switch）",
    swPlaceholder: "分支名，如 dev / feature/login",
    swRun: "批量切换",
    swHint: "可点击输入框从已勾选仓库的下拉选择分支（多选显示所有仓库的共同分支）；仅切换到已存在的分支，不存在会报错。",
    swFrom: "共同分支",
    rpTitle: "批量替换 Remote 地址（服务器迁移 / 域名变更）",
    rpOld: "旧地址串，如 gitlab.old.com",
    rpNew: "新地址串，如 gitlab.new.com",
    rpRun: "执行替换",
    rpConfirm: "确认替换？",
    rpHint:
      "替换前会读取当前地址，仅对“包含旧地址串”的仓库生效；若仓库有 submodule，会同步更新 .gitmodules（改动需自行 commit）。",
    logTitle: "操作日志",
    noLog: "暂无日志",
    ctxPull: "⇩ Pull",
    ctxPush: "⇧ Push",
    ctxOnly: "◉ 仅选中此仓库",
    ctxFinder: "📂 Finder 显示",
    ctxTerm: ">_ 打开终端",
    ctxWeb: "🌐 打开远程页",
    ctxCommit: "📝 提交…",
    ctxLog: "🕘 提交历史",
    logHistTitle: "提交历史 · {name}",
    logHistEmpty: "该仓库没有提交记录",
    logHistLoading: "加载中…",
    commitTitle: "提交 · {name}",
    commitPlaceholder: "提交信息，如 fix: 修复登录 bug",
    commitRun: "提交",
    commitNoChanges: "该仓库没有改动文件",
    commitSelectHint: "勾选要提交的文件，未勾选的保留在工作区",
    commitSelectAll: "全选",
    commitDeselectAll: "全不选",
    logCommitDone: "提交成功：{name}（{n} 个文件）",
    logCommitFail: "提交失败: {e}",
    logListFail: "读取改动失败: {e}",
    stMod: "修改",
    stAdd: "新增",
    stDel: "删除",
    stUntracked: "未跟踪",
    stRenamed: "重命名",
    stStaged: "已暂存",
    aboutVer: "版本 {v}",
    aboutDesc: "本地多仓库批量管理工具 · Tauri 2 + Vue 3",
    aboutF1: "批量 pull / push / 自定义命令 / 切换分支",
    aboutF2: "批量替换 remote 地址（自动同步 .gitmodules）",
    aboutF3: "分组树 · 状态汇总 · 搜索 · 自动刷新",
    aboutF4: "单仓库操作 · 右键菜单 · 深色模式",
    aboutF5: "快捷键：⌘R 刷新 · ⌘⇧A 全选 · ⌘⇧D 取消 · ⌘P Pull · ⌘U Push",
    close: "关闭",
    emptyNoRepo: "还没有仓库——先输入根目录点“扫描仓库”",
    emptyNoStatus: "没有“{s}”状态的仓库",
    emptyNoMatch: "没有匹配的仓库",
    emptyNoUngrouped: "没有未分组的仓库",
    favs: "收藏",
    favAdd: "收藏",
    favRemove: "取消收藏",
    emptyNoFav: "还没有收藏的仓库，点击仓库名旁的 ☆ 收藏",
    emptyGroupHint:
      "「{g}」分组暂无仓库——点上方『全部』，在表格中对仓库选择该分组即可归类",
    relNow: "刚刚",
    relMin: "{n} 分钟前",
    relHour: "{n} 小时前",
    relDay: "{n} 天前",
    relMonth: "{n} 个月前",
    relYear: "{n} 年前",
    logNoSel: "未选择任何仓库",
    logLabelCmd: "批量命令",
    logNeedCmd: "请输入要执行的命令",
    logConfirmPush: "再次点击「批量 Push」确认执行（将推送本地提交到远程）",
    logStart: "开始{label}：并行处理 {n} 个仓库…",
    logDone: "{label} 完成：成功 {ok}/{total}",
    logFail: "执行失败: {e}",
    logScanDone: "扫描完成：找到 {n} 个仓库",
    logScanFail: "扫描失败: {e}",
    logRestoreDir: "已恢复上次的根目录：{p}",
    logRefreshed: "状态已刷新",
    logAutoOn: "已开启自动刷新（每 30 秒）",
    logNeedBranch: "请输入分支名",
    logSwitchStart: "开始切换分支 {b}：并行处理 {n} 个仓库…",
    logSwitchDone: "切换分支完成：成功 {ok}/{total}",
    logNeedUrls: "请填写旧地址串和新地址串",
    logConfirmReplace: "再次点击「执行替换」确认执行（将修改所选仓库的 remote 地址）",
    logReplaceStart: "开始替换 remote：并行处理 {n} 个仓库…",
    logReplaceDone: "改地址完成：成功 {ok}/{total}",
    logGroupCreated: "已创建分组：{g}",
    logGroupDeleted: "已删除分组「{g}」及其子分组",
    logGroupDelConfirm: "再次点击「删除该组」确认删除「{g}」",
    logGroupSaveFail: "保存分组失败: {e}",
    logFavFail: "保存收藏失败: {e}",
    logGroupAssigned: "「{p}」→ 分组「{g}」",
    logGroupCleared: "「{p}」已移出分组",
    logFinderFail: "打开目录失败: {e}",
    logTermOk: "已在终端打开：{p}",
    logTermFail: "打开终端失败: {e}",
    logNoWeb: "该仓库无可用网页地址：{p}",
    logWebFail: "打开网页失败: {e}",
  },
  en: {
    appTitle: "RepoPilot · Multi-Repo Manager",
    envWarn:
      "⚠ Browser preview: UI only — open the RepoPilot app window for scan/refresh/groups",
    addRootPlaceholder: "Enter root directory (multiple ok), Enter to add, e.g. /Users/you/code",
    addRoot: "Add",
    cloneTitle: "Clone repository",
    cloneUrlPlaceholder: "Git repo URL (https or ssh)",
    cloneBase: "Target root",
    cloneRun: "Clone",
    cloneDone: "Cloned",
    cloneFail: "Clone failed",
    scanPlaceholder: "Enter root directory to scan, e.g. /Users/you/code",
    scan: "Scan",
    scanAll: "Scan All",
    scanning: "Scanning…",
    refresh: "Refresh",
    removeRoot: "Remove this root",
    logRootAdded: "Added root: {p}",
    logRootRemoved: "Removed root: {p}",
    emptyNoRoots: "No roots yet — add a directory then click Scan All",
    dark: "🌙 Dark",
    light: "☀️ Light",
    deselect: "Deselect",
    selectVisible: "Select visible ({n})",
    selected: "{a} / {b} selected",
    showOfTotal: "Matched {a} of {b}",
    followHint: "Shown with parent (not favorited / not matched)",
    containerHint: "Parent repo (contains matched children)",
    exportCfg: "Export",
    exportCfgTip: "Export config (roots/groups/favorites)",
    importCfg: "Import",
    importCfgTip: "Import config (overwrites current)",
    importConfirmShort: "Confirm import?",
    importConfirm: "Click Import again to confirm; it overwrites current roots/groups/favorites",
    logExportOk: "Config exported: {p}",
    logExportFail: "Export failed: {e}",
    logImportOk: "Config imported",
    logImportFail: "Import failed: {e}",
    logImportSkipDirs: "Skipped missing directories ({n}): {d}",
    logImportNoDirs: "None of the imported roots exist; please add directories manually",
    copyUrl: "Copy URL",
    logCopied: "Copied: {u}",
    logCopyFail: "Copy failed, please copy manually",
    autoRefresh: "Auto refresh",
    lastRefresh: "Last refresh {t}",
    searchPlaceholder: "Search name/path/branch/url…",
    busyTip: "⏳ Working…",
    batchDone: "Done: {ok} ok / {fail} failed",
    updateCheck: "Check for updates",
    updateAvailable: "New version v{v} available, click to install",
    updateInstalling: "Downloading & installing…",
    updateNone: "You're up to date",
    updateFail: "Update failed: {e}",
    pull: "Pull All",
    push: "Push All",
    confirmPush: "Confirm Push?",
    stash: "Stash",
    stashPop: "Pop",
    confirmStashPop: "Confirm pop?",
    titleRefresh: "Refresh (⌘R)",
    titlePull: "Pull All (⌘P)",
    titlePush: "Push All (⌘U)",
    titleStash: "Stash changes in selected repos (recoverable)",
    titleStashPop: "Pop stash in selected repos",
    logConfirmStashPop: "Click again to confirm popping stash for all selected repos",
    groups: "Groups",
    all: "All",
    ungrouped: "Ungrouped",
    newGroup: "+ New group",
    deleteGroup: "Delete group",
    renameGroup: "Rename",
    groupRenamePlaceholder: "Enter new group name",
    logGroupRenamed: "Renamed group: {a} → {b}",
    logGroupRenameConflict: "Group name conflict: {g} already exists",
    confirmDelete: "Confirm?",
    groupPlaceholder: "Group name (supports parent/child, e.g. work/frontend)",
    ok: "OK",
    cancel: "Cancel",
    stAll: "All",
    stDirty: "Dirty",
    stBehind: "Behind",
    stAhead: "Ahead",
    stError: "Error",
    fAll: "Time: All",
    f30d: "Active ≤30d",
    f90d: "Active ≤90d",
    fStale30: "Stale >30d",
    fStale90: "Stale >90d",
    emptyNoCommit: "No repos in this time range",
    colIdx: "#",
    colName: "Repo",
    colGroup: "Group",
    colBranch: "Branch",
    colStatus: "Status",
    colSync: "Ahead/Behind",
    colCommit: "Last commit",
    colRemote: "Remote URL",
    colPath: "Path",
    colOps: "Actions",
    expand: "Expand",
    collapse: "Collapse",
    bErr: "Error",
    bDirty: "Dirty",
    bClean: "Clean",
    sync: "Synced",
    noRemote: "No remote",
    cmdTitle: "Run custom command on selected repos (same command each)",
    cmdPlaceholder: "e.g. git fetch --prune / npm update / git switch dev",
    run: "Run",
    cmdHint:
      "The command runs in each selected repo's directory, as if you typed it in that terminal.",
    swTitle: "Switch branch on selected repos (git switch)",
    swPlaceholder: "Branch name, e.g. dev / feature/login",
    swRun: "Switch",
    swHint: "Click the input to pick a branch (multiple repos show only common branches); only switches to existing branches.",
    swFrom: "Common branches",
    rpTitle: "Bulk replace Remote URLs (server migration / domain change)",
    rpOld: "Old string, e.g. gitlab.old.com",
    rpNew: "New string, e.g. gitlab.new.com",
    rpRun: "Replace",
    rpConfirm: "Confirm?",
    rpHint:
      "Reads current URL first; only repos containing the old string are changed. Submodules are updated in .gitmodules (commit manually).",
    logTitle: "Log",
    noLog: "No logs",
    ctxPull: "⇩ Pull",
    ctxPush: "⇧ Push",
    ctxOnly: "◉ Select only this",
    ctxFinder: "📂 Reveal in Finder",
    ctxTerm: ">_ Open terminal",
    ctxWeb: "🌐 Open remote page",
    ctxCommit: "📝 Commit…",
    ctxLog: "🕘 History",
    logHistTitle: "History · {name}",
    logHistEmpty: "No commits in this repo",
    logHistLoading: "Loading…",
    commitTitle: "Commit · {name}",
    commitPlaceholder: "Commit message, e.g. fix: fix login bug",
    commitRun: "Commit",
    commitNoChanges: "No changes in this repo",
    commitSelectHint: "Check files to commit; unchecked stay in working tree",
    commitSelectAll: "Select all",
    commitDeselectAll: "Deselect all",
    logCommitDone: "Committed: {name} ({n} files)",
    logCommitFail: "Commit failed: {e}",
    logListFail: "Failed to read changes: {e}",
    stMod: "Modified",
    stAdd: "Added",
    stDel: "Deleted",
    stUntracked: "Untracked",
    stRenamed: "Renamed",
    stStaged: "Staged",
    aboutVer: "Version {v}",
    aboutDesc: "Local multi-repo batch manager · Tauri 2 + Vue 3",
    aboutF1: "Bulk pull / push / custom command / switch branch",
    aboutF2: "Bulk replace remote URLs (with .gitmodules sync)",
    aboutF3: "Group tree · status summary · search · auto refresh",
    aboutF4: "Per-repo actions · context menu · dark mode",
    aboutF5: "Shortcuts: ⌘R refresh · ⌘⇧A select all · ⌘⇧D deselect · ⌘P pull · ⌘U push",
    close: "Close",
    emptyNoRepo: "No repos yet — enter a root directory and click Scan",
    emptyNoStatus: "No repos with status “{s}”",
    emptyNoMatch: "No matching repos",
    emptyNoUngrouped: "No ungrouped repos",
    favs: "Favorites",
    favAdd: "Favorite",
    favRemove: "Unfavorite",
    emptyNoFav: "No favorites yet — click ☆ next to a repo name to favorite it",
    emptyGroupHint:
      "Group “{g}” has no repos — go to All and assign repos to this group in the table",
    relNow: "just now",
    relMin: "{n} min ago",
    relHour: "{n} h ago",
    relDay: "{n} d ago",
    relMonth: "{n} mo ago",
    relYear: "{n} yr ago",
    logNoSel: "No repos selected",
    logLabelCmd: "bulk command",
    logNeedCmd: "Please enter a command",
    logConfirmPush: "Click Push again to confirm (pushes local commits to remote)",
    logStart: "Starting {label}: {n} repos in parallel…",
    logDone: "{label} done: {ok}/{total} ok",
    logFail: "Failed: {e}",
    logScanDone: "Scan done: found {n} repos",
    logScanFail: "Scan failed: {e}",
    logRestoreDir: "Restored last roots: {p}",
    logRefreshed: "Status refreshed",
    logAutoOn: "Auto refresh enabled (every 30s)",
    logNeedBranch: "Please enter a branch name",
    logSwitchStart: "Switching to {b}: {n} repos in parallel…",
    logSwitchDone: "Branch switch done: {ok}/{total} ok",
    logNeedUrls: "Please fill in both old and new strings",
    logConfirmReplace: "Click Replace again to confirm (modifies remote URLs)",
    logReplaceStart: "Replacing remotes: {n} repos in parallel…",
    logReplaceDone: "Replace done: {ok}/{total} ok",
    logGroupCreated: "Group created: {g}",
    logGroupDeleted: "Deleted group “{g}” and its children",
    logFavFail: "Failed to save favorites: {e}",
    logGroupDelConfirm: "Click Delete again to confirm deleting “{g}”",
    logGroupSaveFail: "Failed to save groups: {e}",
    logGroupAssigned: "“{p}” → group “{g}”",
    logGroupCleared: "“{p}” removed from group",
    logFinderFail: "Failed to open directory: {e}",
    logTermOk: "Opened terminal at: {p}",
    logTermFail: "Failed to open terminal: {e}",
    logNoWeb: "No web URL for repo: {p}",
    logWebFail: "Failed to open page: {e}",
  },
};
const lang = ref<Lang>("zh");
try {
  const savedLang = localStorage.getItem("repopilot-lang");
  if (savedLang === "en" || savedLang === "zh") lang.value = savedLang;
} catch {
  /* 忽略 */
}
watch(lang, (v) => {
  try {
    localStorage.setItem("repopilot-lang", v);
  } catch {
    /* 忽略 */
  }
});
function t(key: string): string {
  return messages[lang.value][key] ?? key;
}
function tr(key: string, params?: Record<string, string | number>): string {
  let s = messages[lang.value][key] ?? key;
  if (params) for (const k in params) s = s.split(`{${k}}`).join(String(params[k]));
  return s;
}
// 是否运行在 Tauri 环境（浏览器预览时无后端，操作不可用）
const isTauri = computed(
  () => typeof (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !== "undefined"
);

let timer: number | undefined;
// 防止自动刷新在上一轮未结束时又触发一轮（并发刷新导致旧结果覆盖新结果）
let refreshing = false;

type SortKey =
  | "name"
  | "branch"
  | "status"
  | "sync"
  | "commit"
  | "remote"
  | "path";
const sortKey = ref<SortKey>("name");
const sortDir = ref<1 | -1>(1);

function statusRank(r: RepoStatus) {
  if (r.error) return 2;
  if (r.dirty) return 1;
  return 0;
}
function syncVal(r: RepoStatus) {
  return r.behind * 10 + r.ahead; // 落后的优先排前面（更危险）
}
function relTime(sec: number | null | undefined): string {
  if (!sec) return "—";
  const diff = Date.now() / 1000 - sec;
  if (diff < 60) return tr("relNow");
  if (diff < 3600) return tr("relMin", { n: Math.floor(diff / 60) });
  if (diff < 86400) return tr("relHour", { n: Math.floor(diff / 3600) });
  if (diff < 86400 * 30) return tr("relDay", { n: Math.floor(diff / 86400) });
  if (diff < 86400 * 365)
    return tr("relMonth", { n: Math.floor(diff / (86400 * 30)) });
  return tr("relYear", { n: Math.floor(diff / (86400 * 365)) });
}

const sortedRepos = computed(() => {
  const list = [...repos.value];
  const dir = sortDir.value;
  const name = (p: string) => p.split("/").pop() ?? "";
  list.sort((a, b) => {
    let r = 0;
    switch (sortKey.value) {
      case "name":
        r = name(a.path).localeCompare(name(b.path), "zh");
        break;
      case "branch":
        r = (a.branch || "").localeCompare(b.branch || "", "zh");
        break;
      case "status":
        r = statusRank(a) - statusRank(b);
        break;
      case "sync":
        r = syncVal(a) - syncVal(b);
        break;
      case "commit":
        r = (a.last_commit ?? -1) - (b.last_commit ?? -1);
        break;
      case "remote":
        r = (a.remote_url || "").localeCompare(b.remote_url || "");
        break;
      case "path":
        r = a.path.localeCompare(b.path, "zh");
        break;
    }
    return r * dir;
  });
  return list;
});

function setSort(k: SortKey) {
  if (sortKey.value === k) sortDir.value = sortDir.value === 1 ? -1 : 1;
  else {
    sortKey.value = k;
    sortDir.value = 1;
  }
}
function sortArrow(k: SortKey) {
  return sortKey.value === k ? (sortDir.value === 1 ? "↑" : "↓") : "";
}

const search = ref("");
const groups = ref<Record<string, string>>({});
const groupNames = ref<string[]>([]);
const activeGroup = ref("");
const favs = ref<Set<string>>(new Set());
function isUnder(assign: string | undefined, target: string) {
  return !!assign && (assign === target || assign.startsWith(target + "/"));
}
function groupCount(name: string) {
  // 聚合计数：含该组及其子组的仓库数
  return Object.values(groups.value).filter((g) => isUnder(g, name)).length;
}
// 分组树节点：由分组名推导出所有父节点与叶子节点
const groupNodes = computed(() => {
  const nodes = new Set<string>();
  for (const n of groupNames.value) {
    const parts = n.split("/");
    let cur = "";
    for (const p of parts) {
      if (!p) continue;
      cur = cur ? `${cur}/${p}` : p;
      nodes.add(cur);
    }
  }
  return [...nodes]
    .sort()
    .map((path) => ({ path, depth: path.split("/").length - 1 }));
});
const collapsed = ref<Set<string>>(new Set());
function hasChildren(path: string) {
  return groupNodes.value.some((n) => n.path.startsWith(path + "/"));
}
function toggleCollapse(path: string) {
  const s = new Set(collapsed.value);
  if (s.has(path)) s.delete(path);
  else s.add(path);
  collapsed.value = s;
}
// 只显示未折叠、且非空（自身或其子分组有仓库）的节点
const visibleTree = computed(() =>
  groupNodes.value.filter((n) => {
    // 空分组（含所有后代）隐藏，只显示到有仓库的那层
    if (groupCount(n.path) === 0) return false;
    const parts = n.path.split("/");
    for (let i = 1; i < parts.length; i++) {
      if (collapsed.value.has(parts.slice(0, i).join("/"))) return false;
    }
    return true;
  })
);
const ungroupedCount = computed(
  () => repos.value.filter((r) => !groups.value[r.path]).length
);
function persistGroups() {
  invoke("save_groups", {
    state: { names: groupNames.value, assign: groups.value },
  }).catch((e) => addLog(tr("logGroupSaveFail", { e: String(e) })));
}
function setRepoGroup(path: string, name: string) {
  const g = { ...groups.value };
  if (name) g[path] = name;
  else delete g[path];
  groups.value = g;
  persistGroups();
}
function toggleFav(path: string) {
  const s = new Set(favs.value);
  if (s.has(path)) s.delete(path);
  else s.add(path);
  favs.value = s;
  invoke("save_favs", { paths: [...s] }).catch((e) =>
    addLog(tr("logFavFail", { e: String(e) }))
  );
}
const showNewGroup = ref(false);
const newGroupName = ref("");
const newGrpInput = ref<HTMLInputElement | null>(null);
const pendingDelete = ref("");
function openNewGroup() {
  showNewGroup.value = true;
  newGroupName.value = "";
  nextTick(() => newGrpInput.value?.focus());
}
function confirmNewGroup() {
  const n = newGroupName.value.trim();
  if (!n) return;
  if (!groupNames.value.includes(n)) {
    groupNames.value = [...groupNames.value, n];
    addLog(tr("logGroupCreated", { g: n }));
  }
  activeGroup.value = n;
  showNewGroup.value = false;
  newGroupName.value = "";
  persistGroups();
}
function removeGroup(name: string) {
  if (pendingDelete.value !== name) {
    pendingDelete.value = name;
    addLog(tr("logGroupDelConfirm", { g: name }));
    return;
  }
  pendingDelete.value = "";
  // 删除该分组及其所有子分组
  const g: Record<string, string> = {};
  for (const k in groups.value)
    if (groups.value[k] !== name && !groups.value[k].startsWith(name + "/"))
      g[k] = groups.value[k];
  groups.value = g;
  groupNames.value = groupNames.value.filter(
    (n) => n !== name && !n.startsWith(name + "/")
  );
  if (
    activeGroup.value === name ||
    activeGroup.value.startsWith(name + "/")
  )
    activeGroup.value = "";
  persistGroups();
  addLog(tr("logGroupDeleted", { g: name }));
}
const showRename = ref(false);
const renameGroupName = ref("");
const renameGrpInput = ref<HTMLInputElement | null>(null);
function openRename() {
  renameGroupName.value = activeGroup.value;
  showRename.value = true;
  nextTick(() => renameGrpInput.value?.focus());
}
function confirmRename() {
  const newName = renameGroupName.value.trim();
  const oldName = activeGroup.value;
  showRename.value = false;
  if (!newName || newName === oldName) return;
  // 不能与其它分组（或其子分组）重名
  if (
    groupNames.value.some(
      (n) => n !== oldName && (n === newName || n.startsWith(newName + "/"))
    )
  ) {
    addLog(tr("logGroupRenameConflict", { g: newName }));
    return;
  }
  // 更新分组名列表：自身 + 子分组前缀
  groupNames.value = groupNames.value.map((n) =>
    n === oldName ? newName : n.startsWith(oldName + "/") ? newName + n.slice(oldName.length) : n
  );
  // 更新仓库的分组赋值：同样替换前缀
  const g: Record<string, string> = {};
  for (const k in groups.value) {
    const v = groups.value[k];
    g[k] =
      v === oldName ? newName : v.startsWith(oldName + "/") ? newName + v.slice(oldName.length) : v;
  }
  groups.value = g;
  activeGroup.value = newName;
  persistGroups();
  addLog(tr("logGroupRenamed", { a: oldName, b: newName }));
}
const statusFilter = ref("");
const commitFilter = ref("all");
const baseFiltered = computed(() => {
  let list = sortedRepos.value;
  const q = search.value.trim().toLowerCase();
  if (q)
    list = list.filter((r) =>
      [r.path, r.path.split("/").pop() ?? "", r.branch, r.remote_url]
        .join(" ")
        .toLowerCase()
        .includes(q)
    );
  if (activeGroup.value === "__fav")
    list = list.filter((r) => favs.value.has(r.path));
  else if (activeGroup.value === "__none")
    list = list.filter((r) => !groups.value[r.path]);
  else if (activeGroup.value)
    list = list.filter((r) => isUnder(groups.value[r.path], activeGroup.value));
  // 最后提交时间筛选
  if (commitFilter.value !== "all") {
    const now = Date.now() / 1000;
    const d30 = 86400 * 30;
    const d90 = 86400 * 90;
    if (commitFilter.value === "30d")
      list = list.filter((r) => !!r.last_commit && now - r.last_commit <= d30);
    else if (commitFilter.value === "90d")
      list = list.filter((r) => !!r.last_commit && now - r.last_commit <= d90);
    else if (commitFilter.value === "stale30")
      list = list.filter((r) => !r.last_commit || now - r.last_commit > d30);
    else if (commitFilter.value === "stale90")
      list = list.filter((r) => !r.last_commit || now - r.last_commit > d90);
  }
  return list;
});
const statCards = computed(() => {
  const list = baseFiltered.value;
  return [
    { key: "", label: t("stAll"), count: list.length, cls: "all" },
    {
      key: "dirty",
      label: t("stDirty"),
      count: list.filter((r) => r.dirty && !r.error).length,
      cls: "dirty",
    },
    { key: "behind", label: t("stBehind"), count: list.filter((r) => r.behind).length, cls: "behind" },
    { key: "ahead", label: t("stAhead"), count: list.filter((r) => r.ahead).length, cls: "ahead" },
    { key: "error", label: t("stError"), count: list.filter((r) => r.error).length, cls: "error" },
  ];
});
function setStatusFilter(key: string) {
  statusFilter.value = statusFilter.value === key ? "" : key;
}
const filteredRepos = computed(() => {
  let list = baseFiltered.value;
  if (statusFilter.value === "dirty") list = list.filter((r) => r.dirty && !r.error);
  else if (statusFilter.value === "behind") list = list.filter((r) => r.behind);
  else if (statusFilter.value === "ahead") list = list.filter((r) => r.ahead);
  else if (statusFilter.value === "error") list = list.filter((r) => r.error);
  return list;
});
const emptyHint = computed(() => {
  if (!repos.value.length) return t("emptyNoRepo");
  if (statusFilter.value) {
    const label = statCards.value.find((c) => c.key === statusFilter.value)?.label;
    return tr("emptyNoStatus", { s: label ?? "" });
  }
  if (commitFilter.value !== "all") return t("emptyNoCommit");
  if (search.value.trim()) return t("emptyNoMatch");
  if (activeGroup.value === "__fav") return t("emptyNoFav");
  if (activeGroup.value === "__none") return t("emptyNoUngrouped");
  if (activeGroup.value) return tr("emptyGroupHint", { g: activeGroup.value });
  return "";
});

// ===== 嵌套仓库树 =====
// 把过滤后的仓库按父子关系展开为树形行序列：父仓库可展开/折叠，子仓库缩进显示。
// 过滤/收藏/搜索视图下：
//  - 父仓库被命中/收藏 → 带上其全部子仓库（未命中/未收藏的标记为 follow 次要行）
//  - 子仓库被命中/收藏 → 带出其父仓库作为容器行（默认展开，便于看到命中的子）
const visibleRows = computed<Row[]>(() => {
  const list = filteredRepos.value;
  const inList = new Set(list.map((x) => x.path));
  // 全量子节点映射（基于全量 repos，保证过滤/收藏/搜索视图也能带出子仓库）
  const allChildren = new Map<string, RepoStatus[]>();
  for (const r of repos.value) {
    if (r.parent) {
      const arr = allChildren.get(r.parent);
      if (arr) arr.push(r);
      else allChildren.set(r.parent, [r]);
    }
  }
  // 容器父仓库：子仓库被命中/收藏、父仓库不在结果中 → 父作为容器行带出（默认展开）
  const containerSet = new Set<string>();
  for (const r of list) {
    if (r.parent && !inList.has(r.parent)) containerSet.add(r.parent);
  }
  const rows: Row[] = [];
  const emitted = new Set<string>();

  const emitChildren = (parentPath: string, depth: number, prefix: string) => {
    const kids = allChildren.get(parentPath) ?? [];
    let idx = 0;
    for (const k of kids) {
      if (emitted.has(k.path)) continue;
      emitted.add(k.path);
      const isContainer = containerSet.has(k.path);
      const hasCh = !!allChildren.get(k.path)?.length;
      // 容器行默认展开，但可用 expanded 集合记录"已折叠"（语义与普通行相反）
      const isExp = isContainer
        ? !expanded.value.has(k.path)
        : expanded.value.has(k.path);
      idx += 1;
      const seq = `${prefix}.${idx}`;
      rows.push({
        r: k,
        depth,
        hasChildren: hasCh,
        expanded: isExp,
        seq,
        follow: !inList.has(k.path) && !isContainer,
        container: isContainer,
      });
      if (hasCh && isExp) emitChildren(k.path, depth + 1, seq);
    }
  };

  let top = 0;
  for (const r of list) {
    if (emitted.has(r.path)) continue;
    // 子仓库由父/容器展开统一输出，不单独作为顶层行
    if (r.parent && (inList.has(r.parent) || containerSet.has(r.parent))) continue;
    emitted.add(r.path);
    const hasCh = !!allChildren.get(r.path)?.length;
    const isExp = expanded.value.has(r.path);
    top += 1;
    const seq = String(top);
    rows.push({ r, depth: 0, hasChildren: hasCh, expanded: isExp, seq });
    if (hasCh && isExp) emitChildren(r.path, 1, seq);
  }
  // 容器行：父仓库不在结果中、但有子仓库被命中/收藏 → 顶层容器，默认展开带出命中子
  for (const c of containerSet) {
    if (emitted.has(c)) continue;
    const cr = repos.value.find((x) => x.path === c);
    if (!cr) continue;
    emitted.add(c);
    const hasCh = !!allChildren.get(c)?.length;
    top += 1;
    const seq = String(top);
    rows.push({
      r: cr,
      depth: 0,
      hasChildren: hasCh,
      expanded: !expanded.value.has(c), // 容器行默认展开，可折叠
      seq,
      container: true,
    });
    if (hasCh) emitChildren(c, 1, seq);
  }
  return rows;
});
function toggleExpand(path: string) {
  const s = new Set(expanded.value);
  if (s.has(path)) s.delete(path);
  else s.add(path);
  expanded.value = s;
}
function childCount(path: string): number {
  return repos.value.filter((r) => r.parent === path).length;
}
// 直接子仓库中有改动/错误的数量（用于父仓库脏状态角标）
function childDirtyCount(path: string): number {
  return repos.value.filter((r) => r.parent === path && (r.dirty || !!r.error)).length;
}
// 当前是否有搜索/筛选/分组过滤在生效
const filterHint = computed(() => {
  const n = filteredRepos.value.length;
  const total = repos.value.length;
  const hasFilter =
    !!search.value.trim() ||
    !!statusFilter.value ||
    commitFilter.value !== "all" ||
    !!activeGroup.value;
  if (!hasFilter || n === total || total === 0) return "";
  return tr("showOfTotal", { a: n, b: total });
});
async function copyUrl(url: string) {
  try {
    await navigator.clipboard.writeText(url);
    addLog(tr("logCopied", { u: url }));
  } catch {
    addLog(t("logCopyFail"));
  }
}

// ===== 配置导出 / 导入 =====
const pendingImport = ref(false);
async function exportConfig() {
  try {
    const data = {
      app: "repo-pilot",
      version: 1,
      exportedAt: new Date().toISOString(),
      roots: roots.value,
      groups: groups.value,
      groupNames: groupNames.value,
      favs: [...favs.value],
    };
    const path = await save({
      defaultPath: `repopilot-config-${new Date().toISOString().slice(0, 10)}.json`,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!path) return;
    await invoke("export_config", { path, data: JSON.stringify(data, null, 2) });
    addLog(tr("logExportOk", { p: path }));
  } catch (e) {
    addLog(tr("logExportFail", { e: String(e) }));
  }
}
async function importConfig() {
  if (!pendingImport.value) {
    pendingImport.value = true;
    addLog(t("importConfirm"));
    setTimeout(() => {
      pendingImport.value = false;
    }, 3000);
    return;
  }
  pendingImport.value = false;
  try {
    const path = await open({ multiple: false, filters: [{ name: "JSON", extensions: ["json"] }] });
    if (!path) return;
    const raw = await invoke<string>("import_config", { path });
    const cfg = JSON.parse(raw) as {
      roots?: string[];
      groups?: Record<string, string>;
      groupNames?: string[];
      favs?: string[];
    };
    if (Array.isArray(cfg.roots) && cfg.roots.length) {
      const exists = await invoke<boolean[]>("check_dirs", { paths: cfg.roots });
      const valid = cfg.roots.filter((_, i) => exists[i]);
      const invalid = cfg.roots.filter((_, i) => !exists[i]);
      if (invalid.length)
        addLog(tr("logImportSkipDirs", { n: invalid.length, d: invalid.join(", ") }));
      if (!valid.length) addLog(t("logImportNoDirs"));
      roots.value = valid;
      await invoke("save_roots", { roots: valid }).catch(() => {});
    }
    if (Array.isArray(cfg.groupNames)) groupNames.value = cfg.groupNames;
    if (cfg.groups && typeof cfg.groups === "object") groups.value = cfg.groups;
    persistGroups();
    if (Array.isArray(cfg.favs)) {
      favs.value = new Set(cfg.favs);
      await invoke("save_favs", { paths: cfg.favs }).catch(() => {});
    }
    addLog(t("logImportOk"));
    if (roots.value.length) scan();
  } catch (e) {
    addLog(tr("logImportFail", { e: String(e) }));
  }
}

// 把 git remote 地址转成网页可打开的 URL（https 直用，ssh/git@ 转 https）
function remoteToWeb(url: string): string | null {
  if (!url) return null;
  if (url.startsWith("http://") || url.startsWith("https://"))
    return url.replace(/\.git$/, "");
  const scp = url.match(/^git@([^:]+):(.+)$/);
  if (scp) return `https://${scp[1]}/${scp[2].replace(/\.git$/, "")}`;
  const ssh = url.match(/^ssh:\/\/git@([^:/\s]+)(?::\d+)?\/(.+)$/);
  if (ssh) return `https://${ssh[1]}/${ssh[2].replace(/\.git$/, "")}`;
  return null;
}

function openInFinder(path: string) {
  revealItemInDir(path).catch((e) => addLog(tr("logFinderFail", { e: String(e) })));
}
function openTerm(path: string) {
  invoke("open_terminal", { path })
    .then(() => addLog(tr("logTermOk", { p: path })))
    .catch((e) => addLog(tr("logTermFail", { e: String(e) })));
}
function openRemotePage(r: RepoStatus) {
  const web = remoteToWeb(r.remote_url);
  if (!web) return addLog(tr("logNoWeb", { p: r.path }));
  openUrl(web).catch((e) => addLog(tr("logWebFail", { e: String(e) })));
}

// ===== 部分提交 =====
function statusLabel(s: string) {
  const [x, y] = [s[0], s[1] ?? ""];
  if (s === "??") return t("stUntracked");
  if (x === "D" || y === "D") return t("stDel");
  if (x === "R") return t("stRenamed");
  if (x === "A") return t("stAdd");
  if (x === "M") return t("stMod");
  if (y === "M") return t("stMod");
  return s;
}
async function openCommit(r: RepoStatus) {
  try {
    const changes = await invoke<ChangeFile[]>("list_changes", { path: r.path });
    commitModal.value = {
      repo: r,
      changes,
      selected: new Set(changes.map((_, i) => i)),
      message: "",
      busy: false,
    };
  } catch (e) {
    addLog(tr("logListFail", { e: String(e) }));
  }
}
function toggleCommitFile(i: number) {
  if (!commitModal.value) return;
  const s = new Set(commitModal.value.selected);
  if (s.has(i)) s.delete(i);
  else s.add(i);
  commitModal.value.selected = s;
}
function allCommitSelected() {
  const m = commitModal.value;
  if (!m) return false;
  return m.changes.length > 0 && m.changes.every((_, i) => m.selected.has(i));
}
function toggleAllCommitFiles() {
  if (!commitModal.value) return;
  const m = commitModal.value;
  m.selected = allCommitSelected()
    ? new Set()
    : new Set(m.changes.map((_, i) => i));
}
async function confirmCommit() {
  if (!commitModal.value) return;
  const m = commitModal.value;
  const files = m.changes.filter((_, i) => m.selected.has(i)).map((c) => c.path);
  if (!files.length || !m.message.trim()) return;
  m.busy = true;
  try {
    const res = await invoke<OpResult>("commit_files", {
      path: m.repo.path,
      files,
      message: m.message.trim(),
    });
    addLog(`${res.ok ? "✅" : "❌"} ${m.repo.path.split("/").pop()}: ${res.message}`);
    if (res.ok) {
      addLog(
        tr("logCommitDone", {
          name: m.repo.path.split("/").pop() ?? "",
          n: files.length,
        })
      );
      commitModal.value = null;
      await refreshStatus(true);
    }
  } catch (e) {
    addLog(tr("logCommitFail", { e: String(e) }));
  } finally {
    if (commitModal.value) commitModal.value.busy = false;
  }
}

// 克隆新仓库
function openClone() {
  const base = roots.value[0] ?? "";
  cloneModal.value = { url: "", base, busy: false };
}
// 提交历史
async function openLog(r: RepoStatus) {
  logModal.value = { path: r.path, name: r.path.split("/").pop() ?? r.path, list: [], loading: true };
  try {
    const list = await invoke<CommitInfo[]>("get_log", { path: r.path, count: 20 });
    if (logModal.value && logModal.value.path === r.path) {
      logModal.value.list = list;
      logModal.value.loading = false;
    }
  } catch (e) {
    if (logModal.value && logModal.value.path === r.path) {
      logModal.value.loading = false;
      addLog(tr("logFail", { e: String(e) }));
    }
  }
}

// 检查更新（启动时静默，手动按钮带提示）
async function checkUpdate(manual = false) {
  try {
    const update = await check();
    if (update) {
      updateInfo.value = { update, version: update.version, installing: false };
      addLog(tr("updateAvailable", { v: update.version }));
    } else if (manual) {
      addLog(t("updateNone"));
    }
  } catch {
    /* 无更新服务或浏览器预览，静默 */
  }
}
async function installUpdate() {
  if (!updateInfo.value || updateInfo.value.installing) return;
  updateInfo.value.installing = true;
  try {
    await updateInfo.value.update.downloadAndInstall();
    await relaunch();
  } catch (e) {
    addLog(tr("updateFail", { e: String(e) }));
    if (updateInfo.value) updateInfo.value.installing = false;
  }
}
async function confirmClone() {
  const m = cloneModal.value;
  if (!m || !m.url.trim() || m.busy) return;
  m.busy = true;
  try {
    const res = await invoke<OpResult>("clone_repo", {
      url: m.url.trim(),
      baseDir: m.base,
    });
    addLog(`${res.ok ? "✅" : "❌"} ${t("cloneRun")}: ${res.message}`);
    if (res.ok) {
      cloneModal.value = null;
      await refreshStatus(true);
    }
  } catch (e) {
    addLog(`${t("cloneFail")}: ${String(e)}`);
  } finally {
    if (cloneModal.value) cloneModal.value.busy = false;
  }
}

// 悬停查看改动文件
let hideTipTimer: number | undefined;
async function showChanges(e: MouseEvent, r: RepoStatus) {
  if (hideTipTimer) {
    clearTimeout(hideTipTimer);
    hideTipTimer = undefined;
  }
  let files = changesCache.get(r.path);
  if (!files) {
    try {
      files = await invoke<ChangeFile[]>("list_changes", { path: r.path });
      changesCache.set(r.path, files);
    } catch {
      files = [];
    }
  }
  const pad = 14;
  const x = Math.min(e.clientX + pad, window.innerWidth - 440);
  const y = Math.min(e.clientY + pad, window.innerHeight - 260);
  changesTip.value = { path: r.path, files, x, y };
}
function hideChangesDelayed() {
  if (hideTipTimer) clearTimeout(hideTipTimer);
  hideTipTimer = window.setTimeout(() => {
    changesTip.value = null;
  }, 250);
}
// 悬停查看错误状态的具体原因
function showErrTip(e: MouseEvent, r: RepoStatus) {
  if (hideTipTimer) {
    clearTimeout(hideTipTimer);
    hideTipTimer = undefined;
  }
  const pad = 14;
  const x = Math.min(e.clientX + pad, window.innerWidth - 440);
  const y = Math.min(e.clientY + pad, window.innerHeight - 260);
  changesTip.value = { path: r.path, files: [], err: r.error ?? "", x, y };
}
function keepChanges() {
  if (hideTipTimer) {
    clearTimeout(hideTipTimer);
    hideTipTimer = undefined;
  }
}

// 右键菜单
const ctxMenu = ref<{
  x: number;
  y: number;
  repo: RepoStatus;
  confirm: boolean;
} | null>(null);
function openCtx(e: MouseEvent, r: RepoStatus) {
  e.preventDefault();
  const x = Math.min(e.clientX, window.innerWidth - 210);
  const y = Math.min(e.clientY, window.innerHeight - 280);
  ctxMenu.value = { x, y, repo: r, confirm: false };
}
function closeCtx() {
  ctxMenu.value = null;
}
function ctxAction(
  kind: "pull" | "push" | "finder" | "term" | "web" | "only" | "commit" | "log"
) {
  if (!ctxMenu.value) return;
  // push 走菜单内二次确认
  if (kind === "push" && !ctxMenu.value.confirm) {
    ctxMenu.value = { ...ctxMenu.value, confirm: true };
    return;
  }
  const r = ctxMenu.value.repo;
  const paths = [r.path];
  ctxMenu.value = null;
  if (kind === "only") {
    selected.value = new Set(paths);
    return;
  }
  selected.value = new Set(paths);
  if (kind === "pull") void runOnRepos("pull");
  else if (kind === "push") {
    pendingConfirm.value = "push"; // 让 runOnRepos 的 push 校验直接通过（已在此确认）
    void runOnRepos("push");
  } else if (kind === "finder") openInFinder(r.path);
  else if (kind === "term") openTerm(r.path);
  else if (kind === "web") openRemotePage(r);
  else if (kind === "commit") void openCommit(r);
  else if (kind === "log") void openLog(r);
}

function addLog(s: string) {
  log.value.unshift(`[${new Date().toLocaleTimeString()}] ${s}`);
  if (log.value.length > 200) log.value = log.value.slice(0, 200);
  try {
    localStorage.setItem("repopilot-log", JSON.stringify(log.value));
  } catch {
    /* 忽略 */
  }
}

async function saveRoots() {
  try {
    await invoke("save_roots", { roots: roots.value });
  } catch {
    /* 忽略保存失败 */
  }
}
function addRoot() {
  const p = rootInput.value.trim();
  if (!p) return;
  if (!roots.value.includes(p)) {
    roots.value = [...roots.value, p];
    addLog(tr("logRootAdded", { p }));
  }
  rootInput.value = "";
  void saveRoots();
}
function removeRoot(p: string) {
  roots.value = roots.value.filter((r) => r !== p);
  addLog(tr("logRootRemoved", { p }));
  void saveRoots();
}

async function scan() {
  if (!roots.value.length) return;
  scanning.value = true;
  try {
    const all = new Map<string, RepoStatus>();
    for (const r of roots.value) {
      const entries: { path: string; parent?: string | null }[] = await invoke("scan_repos", { root: r });
      const parentMap = new Map<string, string | null>();
      for (const e of entries) parentMap.set(e.path, e.parent ?? null);
      const paths = entries.map((e) => e.path);
      if (!paths.length) continue;
      const statuses: RepoStatus[] = await invoke("get_statuses", { paths });
      for (const s of statuses) {
        s.parent = parentMap.get(s.path) ?? null;
        all.set(s.path, s);
      }
    }
    repos.value = [...all.values()];
    // 仓库集合已变化，旧的改动文件缓存作废
    changesCache.clear();
    // 默认不勾选任何仓库，需要操作时用「全选当前」或手动勾选（安全优先）
    selected.value = new Set();
    // 记住本次目录，下次打开自动恢复
    await saveRoots();
    void updateBadge();
    addLog(tr("logScanDone", { n: repos.value.length }));
  } catch (e) {
    addLog(tr("logScanFail", { e: String(e) }));
  } finally {
    scanning.value = false;
  }
}

function onKeydown(e: KeyboardEvent) {
  const el = e.target as HTMLElement | null;
  const tag = el?.tagName ?? "";
  const inInput =
    tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || !!el?.isContentEditable;
  if (!(e.metaKey || e.ctrlKey)) return;
  const k = e.key.toLowerCase();
  // ⌘R 刷新状态（输入框内也拦截，防止触发浏览器刷新/覆盖输入）
  if (k === "r") {
    e.preventDefault();
    void refreshStatus(false);
    return;
  }
  if (inInput) return; // 其余快捷键不在输入框内触发，避免干扰输入
  if (k === "p") {
    e.preventDefault();
    void runOnRepos("pull");
  } else if (k === "u") {
    e.preventDefault();
    void runOnRepos("push");
  } else if (k === "a" && e.shiftKey) {
    e.preventDefault();
    toggleAll();
  } else if (k === "d" && e.shiftKey) {
    e.preventDefault();
    if (selected.value.size) selected.value = new Set();
  }
}

onMounted(async () => {
  window.addEventListener("keydown", onKeydown);
  // 监听批量操作进度（真实 Tauri 环境）
  try {
    progressUnlisten = await listen<{ done: number; total: number; ok: number; path: string }>(
      "repopilot-progress",
      (e) => {
        progress.value = e.payload;
        if (e.payload.done >= e.payload.total) {
          batchSummary.value = {
            ok: e.payload.ok,
            fail: e.payload.total - e.payload.ok,
          };
          setTimeout(() => {
            progress.value = null;
          }, 1000);
        }
      }
    );
  } catch {
    /* 浏览器预览无事件通道，忽略 */
  }
  void checkUpdate();
  try {
    appVersion.value = await getVersion();
  } catch {
    /* 使用默认版本号 */
  }
  try {
    const st = await invoke<{ names: string[]; assign: Record<string, string> }>(
      "load_groups"
    );
    groupNames.value = st.names ?? [];
    groups.value = st.assign ?? {};
  } catch {
    /* 无分组配置 */
  }
  try {
    const savedFavs: string[] = await invoke("load_favs");
    favs.value = new Set(savedFavs);
  } catch {
    /* 无收藏配置 */
  }
  try {
    const savedRoots: string[] = await invoke("load_roots");
    if (savedRoots.length) {
      roots.value = savedRoots;
      addLog(tr("logRestoreDir", { p: savedRoots.join("、") }));
      await scan();
    }
  } catch {
    /* 无历史记录则等待手动输入 */
  }
});

async function updateBadge() {
  try {
    const n = repos.value.filter((r) => r.dirty && !r.error).length;
    await getCurrentWindow().setBadgeCount(n > 0 ? n : undefined);
  } catch {
    /* 浏览器预览或无权限时忽略 */
  }
}
async function refreshStatus(silent = false) {
  const paths = repos.value.map((r) => r.path);
  if (!paths.length) return;
  // 保留旧的父子关系（get_statuses 不返回 parent，刷新时不能丢）
  const oldParent = new Map<string, string | null>();
  for (const r of repos.value) oldParent.set(r.path, r.parent ?? null);
  const statuses: RepoStatus[] = await invoke("get_statuses", { paths });
  for (const s of statuses) s.parent = oldParent.get(s.path) ?? null;
  repos.value = statuses;
  // 状态已变化，改动文件缓存作废（下次悬停重新拉取）
  changesCache.clear();
  lastRefresh.value = new Date().toLocaleTimeString();
  void updateBadge();
  if (!silent) addLog(t("logRefreshed"));
}

watch(autoRefresh, (on) => {
  if (timer) {
    clearInterval(timer);
    timer = undefined;
  }
  countdown.value = 0;
  if (on) {
    countdown.value = 30;
    timer = window.setInterval(() => {
      countdown.value--;
      if (countdown.value <= 0) {
        countdown.value = 30;
        if (repos.value.length && !refreshing) {
          refreshing = true;
          refreshStatus(true)
            .catch(() => {})
            .finally(() => {
              refreshing = false;
            });
        }
      }
    }, 1000);
    addLog(t("logAutoOn"));
  }
});
onUnmounted(() => {
  if (timer) clearInterval(timer);
  window.removeEventListener("keydown", onKeydown);
  if (progressUnlisten) progressUnlisten();
});

function toggle(p: string) {
  const s = new Set(selected.value);
  if (s.has(p)) s.delete(p);
  else s.add(p);
  selected.value = s;
}

const viewAllSelected = computed(
  () =>
    filteredRepos.value.length > 0 &&
    filteredRepos.value.every((r) => selected.value.has(r.path))
);
function toggleAll() {
  const view = filteredRepos.value.map((r) => r.path);
  const s = new Set(selected.value);
  if (viewAllSelected.value) for (const p of view) s.delete(p);
  else for (const p of view) s.add(p);
  selected.value = s;
}

async function runOnRepos(action: "pull" | "push" | "cmd" | "stash" | "stashpop") {
  const paths = [...selected.value];
  if (!paths.length) return addLog(t("logNoSel"));
  if (action === "cmd" && !customCmd.value.trim())
    return addLog(t("logNeedCmd"));
  const label =
    action === "pull"
      ? "pull"
      : action === "push"
      ? "push"
      : action === "stash"
      ? "stash"
      : action === "stashpop"
      ? "stash pop"
      : t("logLabelCmd");
  if ((action === "push" || action === "stashpop") && pendingConfirm.value !== action) {
    pendingConfirm.value = action;
    addLog(action === "push" ? t("logConfirmPush") : t("logConfirmStashPop"));
    return;
  }
  // 已确认（或非确认类操作），清除确认态，按钮恢复默认文案
  pendingConfirm.value = "";
  busy.value = true;
  progress.value = null;
  batchSummary.value = null;
  addLog(tr("logStart", { label, n: paths.length }));
  try {
    let results: OpResult[];
    if (action === "pull") results = await invoke("pull_repos", { paths });
    else if (action === "push") results = await invoke("push_repos", { paths });
    else if (action === "stash") {
      const label = `RepoPilot ${new Date().toLocaleString("zh-CN", { hour12: false })}`;
      results = await invoke("stash_repos", { paths, includeUntracked: false, label });
    }
    else if (action === "stashpop") results = await invoke("stash_pop_repos", { paths });
    else
      results = await invoke("run_command", {
        paths,
        command: customCmd.value.trim(),
      });
    let ok = 0;
    for (const r of results) {
      if (r.ok) ok++;
      addLog(`${r.ok ? "✅" : "❌"} ${r.path}: ${r.message}`);
    }
    addLog(tr("logDone", { label, ok, total: results.length }));
    if (action !== "cmd") await refreshStatus(true);
  } catch (e) {
    addLog(tr("logFail", { e: String(e) }));
  } finally {
    busy.value = false;
  }
}

async function runSwitchBranch() {
  const paths = [...selected.value];
  if (!paths.length) return addLog(t("logNoSel"));
  const branch = switchBranch.value.trim();
  if (!branch) return addLog(t("logNeedBranch"));
  busy.value = true;
  progress.value = null;
  batchSummary.value = null;
  addLog(tr("logSwitchStart", { b: branch, n: paths.length }));
  try {
    const results: OpResult[] = await invoke("switch_branches", { paths, branch });
    let ok = 0;
    for (const r of results) {
      if (r.ok) ok++;
      addLog(`${r.ok ? "✅" : "❌"} ${r.path}: ${r.message}`);
    }
    addLog(tr("logSwitchDone", { ok, total: results.length }));
    await refreshStatus(true);
  } catch (e) {
    addLog(tr("logFail", { e: String(e) }));
  } finally {
    busy.value = false;
  }
}

// 勾选变化时，从选中仓库加载可用分支（交集：所有选中仓库都有的分支，保证批量切换成功）
watch(
  selected,
  async () => {
    const paths = [...selected.value].slice(0, 20);
    if (!paths.length) {
      branchOptions.value = [];
      branchSrc.value = "";
      return;
    }
    try {
      const lists = await Promise.all(
        paths.map((p) => invoke<string[]>("list_branches", { path: p }))
      );
      const sets = lists.map((l) => new Set(l));
      const inter = [...sets[0]].filter((b) => sets.every((s) => s.has(b))).sort((a, b) => a.localeCompare(b, "zh"));
      branchOptions.value = inter;
      branchSrc.value =
        paths.length === 1
          ? (paths[0].split("/").pop() ?? "")
          : `${inter.length} / ${paths.length}`;
    } catch {
      branchOptions.value = [];
      branchSrc.value = "";
    }
  },
  { immediate: true }
);

async function replaceRemote() {
  const paths = [...selected.value];
  if (!paths.length) return addLog(t("logNoSel"));
  if (!oldUrl.value.trim() || !newUrl.value.trim())
    return addLog(t("logNeedUrls"));
  if (pendingConfirm.value !== "replace") {
    pendingConfirm.value = "replace";
    addLog(t("logConfirmReplace"));
    return;
  }
  pendingConfirm.value = "";
  busy.value = true;
  progress.value = null;
  batchSummary.value = null;
  addLog(tr("logReplaceStart", { n: paths.length }));
  try {
    const results: OpResult[] = await invoke("replace_remotes", {
      paths,
      old: oldUrl.value.trim(),
      new: newUrl.value.trim(),
    });
    let ok = 0;
    for (const r of results) {
      if (r.ok) ok++;
      addLog(`${r.ok ? "✅" : "❌"} ${r.path}: ${r.message}`);
    }
    addLog(tr("logReplaceDone", { ok, total: results.length }));
    await refreshStatus(true);
  } catch (e) {
    addLog(tr("logFail", { e: String(e) }));
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="app" @click="closeCtx">
    <header class="topbar">
      <div v-if="!isTauri" class="env-warn">{{ t("envWarn") }}</div>
      <h1>{{ t("appTitle") }}</h1>
      <div class="topbar-main">
        <div class="scan-row">
          <div class="tool-group input-grp">
            <input
              v-model="rootInput"
              :placeholder="t('addRootPlaceholder')"
              @keyup.enter="addRoot"
            />
            <button class="ghost" @click="addRoot" :disabled="!rootInput.trim()">
              {{ t("addRoot") }}
            </button>
          </div>
          <div class="tool-group">
            <button :disabled="scanning || !roots.length" @click="scan">
              {{ scanning ? t("scanning") : t("scanAll") }}
            </button>
            <button class="ghost" @click="openClone" :disabled="!roots.length" :title="t('cloneTitle')">
              {{ t("cloneRun") }}
            </button>
            <button class="ghost" @click="refreshStatus(false)" :disabled="!repos.length" :title="t('titleRefresh')">
              {{ t("refresh") }}
            </button>
          </div>
          <div class="tool-group">
            <button class="ghost" @click="lang = lang === 'zh' ? 'en' : 'zh'" :title="lang === 'zh' ? 'Switch to English' : '切换中文'">
              {{ lang === "zh" ? "EN" : "中文" }}
            </button>
            <button class="ghost theme-btn" @click="dark = !dark">
              {{ dark ? t("light") : t("dark") }}
            </button>
            <button class="ghost" @click="showAbout = true" title="About">ⓘ</button>
            <button class="ghost" @click="checkUpdate(true)" :title="t('updateCheck')">🔄</button>
            <button class="ghost" @click="exportConfig" :title="t('exportCfgTip')">{{ t("exportCfg") }}</button>
            <button class="ghost" @click="importConfig" :title="t('importCfgTip')">{{ pendingImport ? t("importConfirmShort") : t("importCfg") }}</button>
          </div>
        </div>
        <div v-if="roots.length" class="roots-row">
          <span v-for="r in roots" :key="r" class="root-chip">
            {{ r }}
            <button class="chip-x" :title="t('removeRoot')" @click="removeRoot(r)">×</button>
          </span>
        </div>
      </div>
    </header>

    <div class="main">
      <div class="sidebar">
        <div class="tree-title">{{ t("groups") }}</div>
        <div class="tree">
          <div
            class="tree-node"
            :class="{ on: activeGroup === '' }"
            @click="activeGroup = ''"
          >
            <span class="tw"></span>
            <span class="tn">{{ t("all") }}</span>
            <span class="cnt">{{ repos.length }}</span>
          </div>
          <div
            class="tree-node"
            :class="{ on: activeGroup === '__none' }"
            @click="activeGroup = '__none'"
          >
            <span class="tw"></span>
            <span class="tn">{{ t("ungrouped") }}</span>
            <span class="cnt">{{ ungroupedCount }}</span>
          </div>
          <div
            class="tree-node"
            :class="{ on: activeGroup === '__fav' }"
            @click="activeGroup = '__fav'"
          >
            <span class="tw">⭐</span>
            <span class="tn">{{ t("favs") }}</span>
            <span class="cnt">{{ favs.size }}</span>
          </div>
          <div
            v-for="node in visibleTree"
            :key="node.path"
            class="tree-node"
            :class="{ on: activeGroup === node.path }"
            :style="{ paddingLeft: node.depth * 16 + 8 + 'px' }"
            @click="activeGroup = node.path"
          >
            <span
              v-if="hasChildren(node.path)"
              class="tw"
              @click.stop="toggleCollapse(node.path)"
            >
              {{ collapsed.has(node.path) ? "▸" : "▾" }}
            </span>
            <span v-else class="tw"></span>
            <span class="tn">{{ node.path.split("/").pop() }}</span>
            <span class="cnt">{{ groupCount(node.path) }}</span>
          </div>
        </div>
        <div class="tree-actions">
          <button class="new-grp" @click="openNewGroup">{{ t("newGroup") }}</button>
          <button
            v-if="activeGroup && groupNodes.some((n) => n.path === activeGroup)"
            class="ghost del"
            @click="removeGroup(activeGroup)"
          >
            {{ pendingDelete === activeGroup ? t("confirmDelete") : t("deleteGroup") }}
          </button>
          <button
            v-if="activeGroup && groupNodes.some((n) => n.path === activeGroup)"
            class="ghost"
            @click="openRename"
          >
            {{ t("renameGroup") }}
          </button>
          <div class="new-grp-row" v-if="showRename">
            <input
              v-model="renameGroupName"
              ref="renameGrpInput"
              :placeholder="t('groupRenamePlaceholder')"
              @keyup.enter="confirmRename"
              @keyup.esc="showRename = false"
            />
            <button @click="confirmRename">{{ t("ok") }}</button>
            <button class="ghost" @click="showRename = false">{{ t("cancel") }}</button>
          </div>
          <div class="new-grp-row" v-if="showNewGroup">
            <input
              v-model="newGroupName"
              ref="newGrpInput"
              :placeholder="t('groupPlaceholder')"
              @keyup.enter="confirmNewGroup"
              @keyup.esc="showNewGroup = false"
            />
            <button @click="confirmNewGroup">{{ t("ok") }}</button>
            <button class="ghost" @click="showNewGroup = false">{{ t("cancel") }}</button>
          </div>
        </div>
      </div>

      <div class="content">
      <div class="toolbar">
        <button @click="toggleAll">
          {{ viewAllSelected ? t("deselect") : tr("selectVisible", { n: filteredRepos.length }) }}
        </button>
        <span class="count">{{ tr("selected", { a: selected.size, b: repos.length }) }}</span>
        <label class="auto">
          <input type="checkbox" v-model="autoRefresh" />
          {{ t("autoRefresh") }}{{ autoRefresh ? `（${countdown}s）` : "" }}
        </label>
        <span v-if="lastRefresh" class="count">{{ tr("lastRefresh", { t: lastRefresh }) }}</span>
        <input v-model="search" class="search" :placeholder="t('searchPlaceholder')" />
        <select v-model="commitFilter" class="time-filter">
          <option value="all">{{ t("fAll") }}</option>
          <option value="30d">{{ t("f30d") }}</option>
          <option value="90d">{{ t("f90d") }}</option>
          <option value="stale30">{{ t("fStale30") }}</option>
          <option value="stale90">{{ t("fStale90") }}</option>
        </select>
        <span class="spacer"></span>
        <span v-if="progress && busy" class="busy-tip" :title="progress.path">⏳ {{ progress.done }}/{{ progress.total }} · {{ t("ok") }} {{ progress.ok }}</span>
        <span v-else-if="busy" class="busy-tip">{{ t("busyTip") }}</span>
        <span v-else-if="batchSummary" class="busy-tip" :class="{ bad: batchSummary.fail > 0 }">
          {{ tr("batchDone", { ok: batchSummary.ok, fail: batchSummary.fail }) }}
        </span>
        <span v-if="updateInfo" class="busy-tip upd" @click="installUpdate">
          {{ updateInfo.installing ? t("updateInstalling") : tr("updateAvailable", { v: updateInfo.version }) }}
        </span>
        <button class="primary" @click="runOnRepos('pull')" :disabled="!selected.size || busy" :title="t('titlePull')">
          {{ t("pull") }}
        </button>
        <button class="primary" @click="runOnRepos('push')" :disabled="!selected.size || busy" :title="t('titlePush')">
          {{ pendingConfirm === "push" ? t("confirmPush") : t("push") }}
        </button>
        <button class="ghost" @click="runOnRepos('stash')" :disabled="!selected.size || busy" :title="t('titleStash')">
          {{ t("stash") }}
        </button>
        <button class="ghost" @click="runOnRepos('stashpop')" :disabled="!selected.size || busy" :title="t('titleStashPop')">
          {{ pendingConfirm === "stashpop" ? t("confirmStashPop") : t("stashPop") }}
        </button>
      </div>

      <div class="stat-bar">
        <button
          v-for="c in statCards"
          :key="c.key"
          class="stat"
          :class="[c.cls, { on: statusFilter === c.key }]"
          @click="setStatusFilter(c.key)"
        >
          <span class="st">{{ c.label }}</span>
          <span class="sc">{{ c.count }}</span>
        </button>
      </div>
      <div v-if="filterHint" class="filter-hint">{{ filterHint }}</div>

      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th class="idx">{{ t("colIdx") }}</th>
              <th class="chk"></th>
              <th class="sortable" @click="setSort('name')">{{ t("colName") }} {{ sortArrow('name') }}</th>
              <th class="grp">{{ t("colGroup") }}</th>
              <th class="sortable" @click="setSort('branch')">{{ t("colBranch") }} {{ sortArrow('branch') }}</th>
              <th class="sortable" @click="setSort('status')">{{ t("colStatus") }} {{ sortArrow('status') }}</th>
              <th class="sortable" @click="setSort('sync')">{{ t("colSync") }} {{ sortArrow('sync') }}</th>
              <th class="sortable" @click="setSort('commit')">{{ t("colCommit") }} {{ sortArrow('commit') }}</th>
              <th class="sortable" @click="setSort('remote')">{{ t("colRemote") }} {{ sortArrow('remote') }}</th>
              <th class="sortable" @click="setSort('path')">{{ t("colPath") }} {{ sortArrow('path') }}</th>
              <th class="ops">{{ t("colOps") }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="row in visibleRows"
              :key="row.r.path"
              :class="{ off: !selected.has(row.r.path), follow: !!row.follow }"
              @contextmenu="openCtx($event, row.r)"
            >
              <td class="idx">{{ row.seq }}</td>
              <td class="chk">
                <input
                  type="checkbox"
                  :checked="selected.has(row.r.path)"
                  @change="toggle(row.r.path)"
                />
              </td>
              <td class="name">
                <span class="tree-cell" :style="{ 'padding-left': row.depth * 20 + 'px' }">
                  <button
                    v-if="row.hasChildren"
                    class="twist"
                    :title="row.expanded ? t('collapse') : t('expand')"
                    @click.stop="toggleExpand(row.r.path)"
                  >{{ row.expanded ? "▾" : "▸" }}</button>
                  <span v-else class="twist ph"></span>
                  <span class="tree-name" :title="row.follow ? t('followHint') : row.container ? t('containerHint') : ''">{{ row.r.path.split("/").pop() }}</span>
                  <button
                    class="fav"
                    :title="favs.has(row.r.path) ? t('favRemove') : t('favAdd')"
                    :class="{ on: favs.has(row.r.path) }"
                    @click.stop="toggleFav(row.r.path)"
                  >{{ favs.has(row.r.path) ? "★" : "☆" }}</button>
                  <span v-if="row.hasChildren" class="child-badge">{{ childCount(row.r.path) }}</span>
                  <span v-if="row.hasChildren && childDirtyCount(row.r.path)" class="child-badge warn">⚠{{ childDirtyCount(row.r.path) }}</span>
                </span>
              </td>
              <td class="grp">
                <select
                  :value="groups[row.r.path] || ''"
                  @change="setRepoGroup(row.r.path, ($event.target as HTMLSelectElement).value)"
                >
                  <option value="">{{ t("ungrouped") }}</option>
                  <option v-for="node in groupNodes" :key="node.path" :value="node.path">
                    {{ "　".repeat(node.depth) }}{{ node.path }}
                  </option>
                </select>
              </td>
              <td>{{ row.r.branch || "—" }}</td>
              <td>
                <span
                  v-if="row.r.error"
                  class="badge err tipable"
                  @mouseenter="showErrTip($event, row.r)"
                  @mouseleave="hideChangesDelayed"
                >{{ t("bErr") }}</span>
                <span
                  v-else-if="row.r.dirty"
                  class="badge dirty tipable"
                  @mouseenter="showChanges($event, row.r)"
                  @mouseleave="hideChangesDelayed"
                >{{ t("bDirty") }}{{ row.r.changed ? ` ×${row.r.changed}` : "" }}</span>
                <span v-else class="badge clean">{{ t("bClean") }}</span>
              </td>
              <td>
                <span v-if="row.r.ahead" class="ahead">↑{{ row.r.ahead }}</span>
                <span v-if="row.r.behind" class="behind">↓{{ row.r.behind }}</span>
                <span v-if="!row.r.ahead && !row.r.behind" class="muted">{{ t("sync") }}</span>
              </td>
              <td class="commit">{{ relTime(row.r.last_commit) }}</td>
              <td class="url" :title="row.r.remote_url">
                <span class="url-cell">
                  <span class="url-text">{{ row.r.remote_url || t("noRemote") }}</span>
                  <button
                    v-if="row.r.remote_url"
                    class="mini copy"
                    :title="t('copyUrl')"
                    @click.stop="copyUrl(row.r.remote_url)"
                  >📋</button>
                </span>
              </td>
              <td class="path" :title="row.r.path">{{ row.r.path }}</td>
              <td class="ops">
                <button class="mini" :title="t('ctxFinder')" @click="openInFinder(row.r.path)">📂</button>
                <button class="mini" :title="t('ctxTerm')" @click="openTerm(row.r.path)">>_</button>
                <button class="mini" :title="t('ctxCommit')" @click="openCommit(row.r)">📝</button>
                <button class="mini" :title="t('ctxLog')" @click="openLog(row.r)">🕘</button>
                <button class="mini" :title="t('ctxWeb')" @click="openRemotePage(row.r)">🌐</button>
              </td>
            </tr>
            <tr v-if="!filteredRepos.length">
              <td colspan="11" class="empty">{{ emptyHint }}</td>
            </tr>
          </tbody>
        </table>
      </div>

      <div class="panels-grid">
      <div class="panel cmd">
        <h3>{{ t("cmdTitle") }}</h3>
        <div class="cmd-row">
          <input
            v-model="customCmd"
            :placeholder="t('cmdPlaceholder')"
            @keyup.enter="runOnRepos('cmd')"
          />
          <button @click="runOnRepos('cmd')" :disabled="!selected.size || busy">
            {{ t("run") }}
          </button>
        </div>
        <p class="hint">{{ t("cmdHint") }}</p>
      </div>

      <div class="panel switch">
        <h3>{{ t("swTitle") }}</h3>
        <div class="cmd-row">
          <input
            v-model="switchBranch"
            list="branch-options"
            :placeholder="t('swPlaceholder')"
            @keyup.enter="runSwitchBranch"
          />
          <button @click="runSwitchBranch" :disabled="!selected.size || busy">
            {{ t("swRun") }}
          </button>
          <datalist id="branch-options">
            <option v-for="b in branchOptions" :key="b" :value="b" />
          </datalist>
        </div>
        <p class="hint">
          {{ t("swHint") }}
          <span v-if="branchOptions.length" class="hint-src">· {{ t("swFrom") }} {{ branchSrc }}</span>
        </p>
      </div>

      <div class="panel replace">
        <h3>{{ t("rpTitle") }}</h3>
        <div class="replace-row">
          <input v-model="oldUrl" :placeholder="t('rpOld')" />
          <span class="arrow">→</span>
          <input v-model="newUrl" :placeholder="t('rpNew')" />
          <button @click="replaceRemote" :disabled="!selected.size || busy">
            {{ pendingConfirm === "replace" ? t("rpConfirm") : t("rpRun") }}
          </button>
        </div>
        <p class="hint">{{ t("rpHint") }}</p>
      </div>

      <div class="panel log">
        <h3>{{ t("logTitle") }}</h3>
        <ul v-if="log.length">
          <li v-for="(l, i) in log" :key="i">{{ l }}</li>
        </ul>
        <p v-else class="hint">{{ t("noLog") }}</p>
      </div>
      </div>
      </div>
    </div>

    <div
      v-if="ctxMenu"
      class="ctx-menu"
      :style="{ left: ctxMenu.x + 'px', top: ctxMenu.y + 'px' }"
      @click.stop
    >
      <div class="ctx-title">{{ ctxMenu.repo.path.split("/").pop() }}</div>
      <button @click="ctxAction('pull')">{{ t("ctxPull") }}</button>
      <button @click="ctxAction('push')">
        {{ ctxMenu.confirm ? t("confirmPush") : t("ctxPush") }}
      </button>
      <button @click="ctxAction('only')">{{ t("ctxOnly") }}</button>
      <div class="ctx-sep"></div>
      <button @click="ctxAction('commit')">{{ t("ctxCommit") }}</button>
      <button @click="ctxAction('log')">{{ t("ctxLog") }}</button>
      <div class="ctx-sep"></div>
      <button @click="ctxAction('finder')">{{ t("ctxFinder") }}</button>
      <button @click="ctxAction('term')">{{ t("ctxTerm") }}</button>
      <button @click="ctxAction('web')">{{ t("ctxWeb") }}</button>
    </div>

    <div v-if="cloneModal" class="modal-mask" @click.self="cloneModal = null">
      <div class="modal">
        <h2>{{ t("cloneTitle") }}</h2>
        <input
          v-model="cloneModal.url"
          :placeholder="t('cloneUrlPlaceholder')"
          autofocus
          @keydown.enter="confirmClone"
        />
        <p class="hint">{{ t("cloneBase") }}</p>
        <select v-model="cloneModal.base" class="clone-base">
          <option v-for="r in roots" :key="r" :value="r">{{ r }}</option>
        </select>
        <div class="modal-actions">
          <button class="ghost" @click="cloneModal = null">{{ t("cancel") }}</button>
          <button
            class="primary"
            :disabled="cloneModal.busy || !cloneModal.url.trim() || !cloneModal.base"
            @click="confirmClone"
          >
            {{ cloneModal.busy ? t("scanning") : t("cloneRun") }}
          </button>
        </div>
      </div>
    </div>

    <div v-if="logModal" class="modal-mask" @click.self="logModal = null">
      <div class="modal log-modal">
        <h2>{{ tr("logHistTitle", { name: logModal.name }) }}</h2>
        <div v-if="logModal.loading" class="hint">{{ t("logHistLoading") }}</div>
        <div v-else-if="!logModal.list.length" class="hint">{{ t("logHistEmpty") }}</div>
        <ul v-else class="log-list">
          <li v-for="(c, i) in logModal.list" :key="i">
            <span class="lh">{{ c.hash }}</span>
            <span class="ls" :title="c.subject">{{ c.subject }}</span>
            <span class="la">{{ c.author }} · {{ relTime(c.time) }}</span>
          </li>
        </ul>
        <div class="modal-actions">
          <button class="ghost" @click="logModal = null">{{ t("close") }}</button>
        </div>
      </div>
    </div>

    <div v-if="changesTip" class="changes-tip" :style="{ left: changesTip.x + 'px', top: changesTip.y + 'px' }" @mouseenter="keepChanges" @mouseleave="hideChangesDelayed">
      <div class="ct-title">{{ changesTip.path.split("/").pop() }}<template v-if="changesTip.err !== undefined"> · {{ t("bErr") }}</template><template v-else> · {{ changesTip.files.length }}</template></div>
      <div v-if="changesTip.err !== undefined" class="ct-list">
        <div class="ct-err">{{ changesTip.err }}</div>
      </div>
      <div v-else class="ct-list">
        <div v-if="!changesTip.files.length" class="hint">{{ t("commitNoChanges") }}</div>
        <div v-for="(f, i) in changesTip.files" :key="i" class="ct-file">
          <span class="cst" :class="{ unt: f.status.trim() === '??' }">{{ statusLabel(f.status) }}</span>
          <span class="cp" :title="f.path">{{ f.path }}</span>
        </div>
      </div>
    </div>

    <div v-if="commitModal" class="modal-mask" @click.self="commitModal = null">
      <div class="modal commit-modal">
        <h2>{{ tr("commitTitle", { name: commitModal.repo.path.split("/").pop() ?? "" }) }}</h2>
        <div class="commit-top">
          <button class="ghost" @click="toggleAllCommitFiles">
            {{ allCommitSelected() ? t("commitDeselectAll") : t("commitSelectAll") }}
          </button>
          <span class="cnt">{{ commitModal.selected.size }} / {{ commitModal.changes.length }}</span>
        </div>
        <div class="commit-list">
          <label v-for="(c, i) in commitModal.changes" :key="i" class="commit-file">
            <input
              type="checkbox"
              :checked="commitModal.selected.has(i)"
              @change="toggleCommitFile(i)"
            />
            <span class="cst" :class="{ unt: c.status.trim() === '??' }">{{ statusLabel(c.status) }}</span>
            <span class="cp" :title="c.path">{{ c.path }}</span>
          </label>
          <p v-if="!commitModal.changes.length" class="hint">{{ t("commitNoChanges") }}</p>
        </div>
        <p class="hint">{{ t("commitSelectHint") }}</p>
        <textarea
          v-model="commitModal.message"
          :placeholder="t('commitPlaceholder')"
          rows="2"
          @keydown.meta.enter="confirmCommit"
          @keydown.ctrl.enter="confirmCommit"
        ></textarea>
        <div class="modal-actions">
          <button class="ghost" @click="commitModal = null">{{ t("cancel") }}</button>
          <button
            class="primary"
            :disabled="commitModal.busy || !commitModal.message.trim() || !commitModal.selected.size || !commitModal.changes.length"
            @click="confirmCommit"
          >
            {{ commitModal.busy ? t("scanning") : t("commitRun") }}
          </button>
        </div>
      </div>
    </div>

    <div v-if="showAbout" class="modal-mask" @click.self="showAbout = false">
      <div class="modal">
        <h2>{{ t("appTitle") }}</h2>
        <p class="ver">{{ tr("aboutVer", { v: appVersion }) }}</p>
        <p class="desc">{{ t("aboutDesc") }}</p>
        <ul class="features">
          <li>{{ t("aboutF1") }}</li>
          <li>{{ t("aboutF2") }}</li>
          <li>{{ t("aboutF3") }}</li>
          <li>{{ t("aboutF4") }}</li>
          <li>{{ t("aboutF5") }}</li>
        </ul>
        <button @click="showAbout = false">{{ t("close") }}</button>
      </div>
    </div>
  </div>
</template>

<style>
:root {
  font-family: -apple-system, "PingFang SC", "Microsoft YaHei", Helvetica, Arial, sans-serif;
  font-size: 14px;
  color: #1f2328;
  background: #f6f8fa;
}
* { box-sizing: border-box; }
body { margin: 0; }
#app { height: 100vh; }
.app { display: flex; flex-direction: column; height: 100vh; }
.topbar {
  padding: 14px 18px;
  background: #24292f;
  color: #fff;
  display: flex;
  align-items: center;
  gap: 18px;
  flex-wrap: wrap;
}
.topbar h1 { font-size: 16px; margin: 0; white-space: nowrap; }
.env-warn {
  position: absolute;
  top: 6px;
  right: 12px;
  font-size: 11px;
  color: #ffd33d;
  background: #7a4f01;
  padding: 2px 10px;
  border-radius: 10px;
}
.topbar { position: relative; }
.topbar-main { display: flex; flex-direction: column; gap: 8px; flex: 1; min-width: 320px; }
.scan-row { display: flex; gap: 10px; flex-wrap: wrap; align-items: center; }
.tool-group { display: flex; gap: 6px; align-items: center; }
.tool-group.input-grp { flex: 1 1 320px; }
.tool-group.input-grp input { flex: 1; min-width: 200px; }
.tool-group + .tool-group { padding-left: 10px; border-left: 1px solid #ffffff2e; }
.roots-row { display: flex; flex-wrap: wrap; gap: 6px; }
.root-chip {
  background: #ffffff22;
  color: #fff;
  padding: 2px 10px;
  border-radius: 12px;
  font-size: 12px;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.chip-x {
  background: none;
  border: none;
  color: #ffffffaa;
  cursor: pointer;
  font-size: 14px;
  line-height: 1;
  padding: 0 2px;
}
.chip-x:hover { color: #fff; }
input {
  padding: 7px 10px;
  border: 1px solid #d0d7de;
  border-radius: 6px;
  font-size: 13px;
  flex: 1;
  outline: none;
}
input:focus { border-color: #0969da; }
button {
  padding: 7px 14px;
  border: 1px solid #d0d7de;
  border-radius: 6px;
  background: #f6f8fa;
  cursor: pointer;
  font-size: 13px;
  white-space: nowrap;
}
button:hover { border-color: #0969da; color: #0969da; }
button:disabled { opacity: 0.5; cursor: not-allowed; }
button.primary { background: #0969da; color: #fff; border-color: #0969da; }
button.primary:hover { background: #0a5cc0; color: #fff; }
button.ghost { background: transparent; border-color: #d0d7de; color: #24292f; }
button.ghost:hover { border-color: #0969da; color: #0969da; }
/* 顶栏（深色背景）上的幽灵按钮保持浅色 */
.topbar button.ghost { border-color: #ffffff55; color: #ddd; }
.topbar button.ghost:hover { border-color: #fff; color: #fff; }
/* 深色模式内容区 */
html.dark button.ghost { border-color: #444c56; color: #adbac7; }
html.dark button.ghost:hover { border-color: #58a6ff; color: #58a6ff; }
/* 顶栏在深色模式下同样保持浅色（更高特异性） */
html.dark .topbar button.ghost { border-color: #ffffff55; color: #ddd; }
html.dark .topbar button.ghost:hover { border-color: #fff; color: #fff; }
.main { flex: 1; display: flex; gap: 12px; padding: 14px 18px; overflow: hidden; }
.sidebar { width: 230px; flex-shrink: 0; background: #fff; border: 1px solid #d0d7de; border-radius: 8px; padding: 10px; display: flex; flex-direction: column; gap: 8px; overflow: auto; }
.content { flex: 1; display: flex; flex-direction: column; gap: 12px; overflow: auto; min-width: 0; }
.toolbar { display: flex; align-items: center; gap: 10px; }
.stat-bar { display: flex; gap: 8px; flex-wrap: wrap; }
.stat { display: flex; align-items: center; gap: 6px; padding: 5px 12px; border-radius: 8px; font-size: 13px; cursor: pointer; background: #fff; border: 1px solid #d0d7de; }
.stat:hover { border-color: #0969da; }
.stat.on { background: #0969da; border-color: #0969da; color: #fff; }
.stat .st { font-weight: 600; }
.stat .sc { background: #eaeef2; border-radius: 10px; padding: 0 8px; font-size: 12px; color: #57606a; }
.stat.dirty .sc { color: #9a6700; }
.stat.behind .sc { color: #cf222e; }
.stat.ahead .sc { color: #1a7f37; }
.stat.error .sc { color: #cf222e; }
.stat.on .sc { color: #fff; background: #ffffff33; }
.tree-title { font-size: 12px; color: #57606a; font-weight: 600; padding: 2px 6px; }
.tree { display: flex; flex-direction: column; gap: 1px; }
.tree-node { display: flex; align-items: center; gap: 4px; padding: 4px 8px; border-radius: 6px; cursor: pointer; font-size: 13px; white-space: nowrap; }
.tree-node:hover { background: #f6f8fa; }
.tree-node.on { background: #0969da; color: #fff; }
.tree-node.on .tw, .tree-node.on .cnt { color: #ddf4ff; }
.tree-node .tw { width: 20px; flex-shrink: 0; text-align: center; color: #57606a; font-size: 20px; line-height: 1; }
.tree-node .tn { flex: 1; overflow: hidden; text-overflow: ellipsis; }
.cnt { margin-left: 4px; opacity: 0.7; font-size: 11px; }
.tree-actions { margin-top: auto; display: flex; flex-direction: column; gap: 6px; padding-top: 8px; border-top: 1px solid #eaeef2; }
button.new-grp { background: transparent; border-color: #0969da; color: #0969da; font-weight: 600; }
button.new-grp:hover { background: #ddf4ff; }
button.del { color: #cf222e; border-color: #ff8182; }
.grp select { padding: 2px 4px; font-size: 12px; max-width: 130px; }
.new-grp-row { display: flex; gap: 8px; align-items: center; }
.new-grp-row input { flex: 0 0 260px; padding: 5px 10px; }
.sidebar .new-grp-row { flex-direction: column; align-items: stretch; }
.sidebar .new-grp-row input { width: 100%; flex: none; }
.cnt { margin-left: 4px; opacity: 0.7; font-size: 11px; }
.grp select { padding: 2px 4px; font-size: 12px; max-width: 130px; }
.count { color: #57606a; font-size: 12px; }
.auto { font-size: 12px; color: #57606a; display: flex; align-items: center; gap: 4px; }
.auto input { width: auto; }
.search { flex: 0 0 250px; padding: 5px 10px; }
.time-filter { flex: 0 0 auto; padding: 5px 8px; }
.busy-tip { font-size: 12px; color: #9a6700; background: #fff8c5; padding: 3px 10px; border-radius: 10px; }
.busy-tip.bad { color: #cf222e; background: #ffebe9; }
.busy-tip.upd { color: #fff; background: #0969da; cursor: pointer; font-weight: 600; }
.busy-tip.upd:hover { background: #0a5cc0; }
html.dark .busy-tip.bad { background: #f8514926; color: #f85149; }
html.dark .busy-tip.upd { background: #316dca; color: #fff; }
.spacer { flex: 1; }
.table-wrap { flex: 1; overflow: auto; background: #fff; border: 1px solid #d0d7de; border-radius: 8px; }
table { width: 100%; min-width: 1080px; border-collapse: collapse; font-size: 13px; }
th, td { padding: 7px 10px; text-align: left; border-bottom: 1px solid #eaeef2; white-space: nowrap; }
th { background: #f6f8fa; position: sticky; top: 0; z-index: 2; color: #57606a; font-weight: 600; }
/* 横向滚动时序号列与勾选列吸附左侧 */
th.idx, td.idx, th.chk, td.chk { position: sticky; }
th.idx { left: 0; z-index: 4; }
th.chk { left: 36px; z-index: 4; }
td.idx { left: 0; z-index: 3; background: #fff; }
td.chk { left: 36px; z-index: 3; background: #fff; }
.chk { width: 40px; text-align: center; }
html.dark td.idx, html.dark td.chk { background: #1c2128; }
.filter-hint { font-size: 12px; color: #8c959f; padding: 4px 2px 0; }
.url-cell { display: inline-flex; align-items: center; gap: 6px; max-width: 100%; }
.url-text { overflow: hidden; text-overflow: ellipsis; }
td.url .copy { visibility: hidden; padding: 0 5px; }
td.url:hover .copy { visibility: visible; }
.child-badge.warn { background: #fff8c5; color: #9a6700; }
html.dark .child-badge.warn { background: #9e6a0333; color: #d29922; }
th.sortable { cursor: pointer; user-select: none; }
th.sortable:hover { color: #0969da; }
.idx { width: 36px; text-align: center; color: #8c959f; }
tr.off td { opacity: 0.85; }
tr.follow td { opacity: 0.55; }
td.name { font-weight: 600; }
.tree-cell { display: inline-flex; align-items: center; gap: 2px; }
.twist { width: 24px; height: 32px; padding: 0; border: none; background: none; cursor: pointer; color: #57606a; font-size: 20px; line-height: 1; display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0; }
.twist:hover { color: #0969da; }
.twist.ph { visibility: hidden; }
.tree-name { white-space: nowrap; }
.fav { padding: 0 2px; border: none; background: none; cursor: pointer; font-size: 14px; line-height: 1; color: #8c959f; flex-shrink: 0; }
.fav:hover { color: #eac54f; }
.fav.on { color: #eac54f; }
html.dark .fav { color: #8b949e; }
html.dark .fav:hover, html.dark .fav.on { color: #f0c648; }
.child-badge { background: #ddf4ff; color: #0969da; border-radius: 9px; font-size: 11px; padding: 0 6px; line-height: 16px; font-weight: 600; }
html.dark .twist { color: #9198a1; }
html.dark .twist:hover { color: #58a6ff; }
html.dark .child-badge { background: #316dca33; color: #79c0ff; }
td.path, td.url { max-width: 260px; overflow: hidden; text-overflow: ellipsis; color: #57606a; }
.ops { width: 120px; text-align: center; }
.mini { padding: 1px 7px; font-size: 12px; line-height: 1.7; border-color: #d0d7de; }
.badge { padding: 1px 8px; border-radius: 10px; font-size: 12px; }
.badge.clean { background: #dafbe1; color: #1a7f37; }
.badge.dirty { background: #fff8c5; color: #9a6700; }
.badge.err { background: #ffebe9; color: #cf222e; }
.ahead { color: #1a7f37; }
.behind { color: #cf222e; margin-left: 4px; }
.muted { color: #8c959f; }
.empty { text-align: center; color: #8c959f; padding: 30px 0; }
.panel { background: #fff; border: 1px solid #d0d7de; border-radius: 8px; padding: 10px 12px; }
.panel h3 { margin: 0 0 6px; font-size: 13px; }
.panels-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; align-items: start; }
.panels-grid .replace, .panels-grid .log { grid-column: 1 / -1; }
.cmd-row, .replace-row { display: flex; gap: 8px; align-items: center; }
.arrow { color: #57606a; }
.hint { color: #8c959f; font-size: 12px; margin: 6px 0 0; }
.hint .hint-src { color: #57606a; }
html.dark .hint .hint-src { color: #8b949e; }
.log ul { margin: 0; padding: 0; list-style: none; max-height: 160px; overflow: auto; }
.log li {
  font-size: 12px;
  padding: 3px 0;
  border-bottom: 1px dashed #eaeef2;
  color: #57606a;
  word-break: break-all;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

/* ===== 深色模式 ===== */
html.dark body { background: #0d1117; color: #e6edf3; }
html.dark .topbar { background: #161b22; }
html.dark .sidebar,
html.dark .table-wrap,
html.dark .panel,
html.dark .stat { background: #1c2128; border-color: #30363d; }
html.dark th { background: #22272e; color: #adbac7; }
html.dark th, html.dark td { border-color: #30363d; }
html.dark input, html.dark select { background: #22272e; color: #e6edf3; border-color: #444c56; }
html.dark input:focus { border-color: #539bf5; }
html.dark button { background: #22272e; color: #adbac7; border-color: #444c56; }
html.dark button:hover { border-color: #539bf5; color: #539bf5; }
html.dark button.primary { background: #316dca; border-color: #316dca; color: #fff; }
html.dark button.primary:hover { background: #2f6fc4; color: #fff; }
html.dark .tree-node:hover { background: #22272e; }
html.dark .tree-node.on { background: #316dca; color: #fff; }
html.dark .stat.on { background: #316dca; border-color: #316dca; color: #fff; }
html.dark .stat .sc { background: #30363d; color: #adbac7; }
html.dark .stat.on .sc { background: #ffffff33; color: #fff; }
html.dark .count, html.dark .hint, html.dark .tree-title, html.dark .tree-node .tw,
html.dark .idx, html.dark .muted, html.dark .empty, html.dark .arrow { color: #8b949e; }
html.dark td.path, html.dark td.url, html.dark .log li { color: #8b949e; }
html.dark .tree-actions { border-color: #30363d; }
html.dark .badge.clean { background: #1f883d33; color: #3fb950; }
html.dark .badge.dirty { background: #9e6a0333; color: #d29922; }
html.dark .badge.err { background: #f8514926; color: #f85149; }
html.dark .ahead { color: #3fb950; }
html.dark .behind { color: #f85149; }
html.dark .busy-tip { background: #9e6a0333; color: #d29922; }
html.dark .log li { border-color: #30363d; }
html.dark .btn:hover { border-color: #539bf5; }
.ctx-menu { position: fixed; z-index: 100; background: #fff; border: 1px solid #d0d7de; border-radius: 8px; box-shadow: 0 4px 16px #0000002e; padding: 6px; min-width: 180px; }
.ctx-menu .ctx-title { font-size: 12px; color: #8c959f; padding: 4px 8px 6px; border-bottom: 1px solid #eaeef2; margin-bottom: 4px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.ctx-menu button { display: block; width: 100%; text-align: left; background: transparent; border: none; padding: 6px 8px; border-radius: 6px; font-size: 13px; }
.ctx-menu button:hover { background: #f6f8fa; color: #0969da; }
.ctx-menu .ctx-sep { height: 1px; background: #eaeef2; margin: 4px 0; }
html.dark .ctx-menu { background: #1c2128; border-color: #30363d; box-shadow: 0 4px 16px #000a; }
html.dark .ctx-menu .ctx-title { color: #8b949e; border-color: #30363d; }
html.dark .ctx-menu button:hover { background: #22272e; color: #539bf5; }
html.dark .ctx-menu .ctx-sep { background: #30363d; }
.modal-mask { position: fixed; inset: 0; background: #000a; display: flex; align-items: center; justify-content: center; z-index: 200; }
.commit-modal { width: 720px; max-width: 94vw; }
.commit-top { display: flex; align-items: center; justify-content: space-between; margin-bottom: 6px; }
.commit-top .cnt { font-size: 12px; color: #57606a; }
html.dark .commit-top .cnt { color: #8b949e; }
.commit-list {
  max-height: 55vh;
  overflow: auto;
  border: 1px solid #d0d7de;
  border-radius: 6px;
  padding: 4px 6px;
}
.commit-file { display: flex; align-items: center; gap: 8px; padding: 4px 6px; font-size: 13px; border-radius: 4px; }
.commit-file:hover { background: #f6f8fa; }
html.dark .commit-file:hover { background: #21262d; }
.cst { flex: 0 0 auto; font-size: 11px; padding: 1px 7px; border-radius: 8px; background: #ddf4ff; color: #0969da; }
.cst.unt { background: #fff8c5; color: #9a6700; }
.cp { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.commit-modal textarea {
  width: 100%;
  margin-top: 8px;
  padding: 6px 8px;
  font-family: inherit;
  font-size: 13px;
  border: 1px solid #d0d7de;
  border-radius: 6px;
  resize: vertical;
}
.modal-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 10px; }
.log-modal { width: 640px; max-width: 92vw; }
.log-list { list-style: none; margin: 0; padding: 4px; max-height: 55vh; overflow: auto; border: 1px solid #d0d7de; border-radius: 8px; }
.log-list li { display: flex; align-items: center; gap: 12px; padding: 8px 10px; border-radius: 6px; font-size: 13px; }
.log-list li + li { border-top: 1px solid #eaeef2; }
.log-list li:hover { background: #f6f8fa; }
.log-list .lh { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; color: #0969da; font-weight: 600; flex: 0 0 66px; font-size: 12px; }
.log-list .ls { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: #1f2328; }
.log-list .la { color: #57606a; font-size: 12px; flex: 0 1 auto; text-align: right; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 45%; }
html.dark .log-list { border-color: #30363d; }
html.dark .log-list li + li { border-color: #30363d; }
html.dark .log-list li:hover { background: #22272e; }
html.dark .log-list .lh { color: #58a6ff; }
html.dark .log-list .ls { color: #e6edf3; }
html.dark .log-list .la { color: #8b949e; }
.clone-base { width: 100%; padding: 6px 8px; margin-top: 4px; font-size: 13px; }
.changes-tip {
  position: fixed;
  z-index: 300;
  background: #fff;
  border: 1px solid #d0d7de;
  border-radius: 8px;
  box-shadow: 0 6px 20px #0000002e;
  padding: 8px 10px;
  width: 420px;
  max-width: 90vw;
  font-size: 12px;
}
.changes-tip .ct-title { font-weight: 600; margin-bottom: 6px; color: #1f2328; }
.changes-tip .ct-list { max-height: 220px; overflow: auto; display: flex; flex-direction: column; gap: 3px; }
.changes-tip .ct-file { display: flex; align-items: flex-start; gap: 6px; }
.changes-tip .ct-file .cp { white-space: normal; word-break: break-all; line-height: 1.4; }
.changes-tip .ct-err { color: #cf222e; white-space: normal; word-break: break-all; line-height: 1.5; padding: 2px 0; }
html.dark .changes-tip .ct-err { color: #ff7b72; }
html.dark .changes-tip { background: #1c2128; border-color: #30363d; box-shadow: 0 6px 20px #000a; }
html.dark .changes-tip .ct-title { color: #e6edf3; }
html.dark .commit-list { border-color: #30363d; }
html.dark .commit-modal textarea { border-color: #30363d; background: #161b22; color: #e6edf3; }
html.dark .cst { background: #1f6feb33; color: #58a6ff; }
html.dark .cst.unt { background: #9e6a0333; color: #d29922; }
.modal { background: #fff; border-radius: 10px; padding: 20px 24px; width: 380px; box-shadow: 0 8px 30px #0004; }
.modal h2 { margin: 0 0 6px; font-size: 18px; }
.modal .ver { color: #57606a; font-size: 13px; margin: 0 0 8px; }
.modal .desc { color: #57606a; font-size: 13px; margin: 0 0 12px; }
.modal .features { margin: 0 0 16px; padding-left: 18px; color: #1f2328; font-size: 13px; line-height: 1.9; }
html.dark .modal { background: #1c2128; color: #e6edf3; }
html.dark .modal .ver, html.dark .modal .desc { color: #8b949e; }
html.dark .modal .features { color: #e6edf3; }
</style>
