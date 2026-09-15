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
  confirmDiscard: string;
  showMsgHist: boolean;
  conflictState: "idle" | "checking" | "done" | "error" | "auth";
  conflictFiles: string[];
  conflictMsg: string;
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
  confirmCherry: string;
  search: string;
}
interface GraphCommit {
  hash: string;
  short: string;
  parents: string[];
  author: string;
  time: number;
  subject: string;
  refs: string[];
}
interface GraphModal {
  path: string;
  name: string;
  currentBranch: string;
  list: GraphCommit[];
  loading: boolean;
  error: string;
}
interface BranchInfo {
  name: string;
  is_local: boolean;
  is_remote: boolean;
  is_current: boolean;
}
interface BranchModal {
  path: string;
  name: string;
  current: string;
  branches: BranchInfo[];
  loading: boolean;
  error: string;
  opMsg: string;
  newName: string;
  busy: boolean;
  confirmDel: string;
}
interface TagModal {
  path: string;
  name: string;
  list: string[];
  loading: boolean;
  error: string;
  opMsg: string;
  newName: string;
  busy: boolean;
  confirmDel: string;
}
interface RemoteInfo {
  name: string;
  url: string;
}
interface RemoteModal {
  path: string;
  name: string;
  list: RemoteInfo[];
  loading: boolean;
  error: string;
  opMsg: string;
  busy: boolean;
  newName: string;
  newUrl: string;
  editName: string | null;
  editUrl: string;
  confirmDel: string;
}
interface StashInfo {
  index: string;
  subject: string;
}
interface StashModal {
  path: string;
  name: string;
  list: StashInfo[];
  loading: boolean;
  error: string;
  opMsg: string;
  newLabel: string;
  busy: boolean;
  confirmDrop: string;
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
// 工具栏下拉：批量功能面板 + 更多菜单
const batchOpen = ref(false);
const batchTab = ref<"cmd" | "switch" | "replace">("cmd");
const moreOpen = ref(false);
const branchOptions = ref<string[]>([]);
const branchSrc = ref("");
const autoRefresh = ref(false);
const countdown = ref(0);
const lastRefresh = ref("");
const busy = ref(false);
const pendingConfirm = ref<"" | "push" | "replace" | "stashpop">("");
// 最近提交信息历史（本地持久化，供提交弹窗一键复用）
const commitMsgHistory = ref<string[]>([]);
try {
  const saved = localStorage.getItem("repopilot-commit-msgs");
  if (saved) commitMsgHistory.value = JSON.parse(saved);
} catch {
  /* 忽略 */
}
function saveCommitMsg(msg: string) {
  const m = msg.trim();
  if (!m) return;
  commitMsgHistory.value = [m, ...commitMsgHistory.value.filter((x) => x !== m)].slice(0, 10);
  try {
    localStorage.setItem("repopilot-commit-msgs", JSON.stringify(commitMsgHistory.value));
  } catch {
    /* 忽略 */
  }
}
const selected = ref<Set<string>>(new Set());
const expanded = ref<Set<string>>(new Set());
// 分支切换成功的行内高亮（短暂标记）
const flashPaths = ref<Set<string>>(new Set());
const dark = ref(false);
try {
  dark.value = localStorage.getItem("repopilot-dark") === "1";
} catch {
  /* 忽略 */
}
const showAbout = ref(false);
const appVersion = ref("0.1.0");
const commitModal = ref<CommitModal | null>(null);
// 提交弹窗左右结构：左侧文件列表 + 右侧 diff 预览（按 Hunk 块渲染，支持块级暂存）
interface HunkInfo {
  patch: string;
  lines: string[];
}
interface FileHunks {
  header: string[];
  hunks: HunkInfo[];
  partial: boolean;
  untracked: boolean;
  content?: string | null;
  err?: string;
}
const commitHunks = ref<Record<number, FileHunks>>({});
const hunksLoading = ref(false);
const activeDiff = ref<number>(-1);
const activeHunks = computed(() =>
  activeDiff.value >= 0 ? (commitHunks.value[activeDiff.value] ?? null) : null
);
// 给单行 diff 文本打高亮类别（+ 新增 / - 删除 / @@ 变更块 / 元信息 / 普通）
function lineClass(line: string): string {
  if (line.startsWith("+++") || line.startsWith("---") || line.startsWith("diff --git") || line.startsWith("index ") || line.startsWith("(新文件")) return "meta";
  if (line.startsWith("@@")) return "hunk";
  if (line.startsWith("+")) return "add";
  if (line.startsWith("-")) return "del";
  return "ctx";
}
// 把整段 diff 文本拆成带类型的高亮行（用于无法提供 hunk 时的只读查看）
// 提交弹窗左侧文件列表宽度（百分比，可拖动分隔条调整，持久化；旧版存的是像素值，>1 时按 /960 换算）
function readCommitSplit(): number {
  const raw = Number(localStorage.getItem("repoPilot.commitSplit"));
  if (!raw) return 0.38;
  const v = raw > 1 ? raw / 960 : raw; // 旧像素值换算为比例（默认弹窗宽 960）
  return Math.min(Math.max(v, 0.2), 0.5);
}
const commitSplitW = ref<number>(readCommitSplit());
function startCommitSplit(e: MouseEvent) {
  e.preventDefault();
  const startX = e.clientX;
  const startW = commitSplitW.value;
  const move = (ev: MouseEvent) => {
    // 按弹窗宽度换算为百分比；下限 20% 保证可读，上限 50% 保证右侧 diff 区有空间
    const modalEl = document.querySelector<HTMLElement>(".modal.commit-modal");
    const modalW = modalEl ? modalEl.clientWidth : 960;
    commitSplitW.value = Math.min(Math.max(startW + (ev.clientX - startX) / modalW, 0.2), 0.5);
  };
  const up = () => {
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", up);
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
    localStorage.setItem("repoPilot.commitSplit", String(commitSplitW.value));
  };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", up);
  document.body.style.cursor = "col-resize";
  document.body.style.userSelect = "none";
}
function diffLines(text: string): { text: string; cls: string }[] {
  return text.split("\n").map((line) => ({ text: line, cls: lineClass(line) }));
}
// 选中左侧文件并加载/切换其 Hunk 结构
async function selectDiff(i: number) {
  const m = commitModal.value;
  if (!m) return;
  activeDiff.value = i;
  if (commitHunks.value[i] !== undefined) return;
  hunksLoading.value = true;
  try {
    const h = await invoke<FileHunks>("get_hunks", {
      path: m.repo.path,
      file: m.changes[i].path,
    });
    commitHunks.value = { ...commitHunks.value, [i]: h };
  } catch (e) {
    commitHunks.value = {
      ...commitHunks.value,
      [i]: { header: [], hunks: [], partial: false, untracked: false, err: String(e) },
    };
  } finally {
    hunksLoading.value = false;
  }
}
// 重新拉取该仓库改动列表并清理失效的缓存下标（Hunk 暂存后刷新）
async function reloadCommitChanges() {
  const m = commitModal.value;
  if (!m) return;
  const changes = await invoke<ChangeFile[]>("list_changes", { path: m.repo.path });
  m.changes = changes;
  const keep: Record<number, FileHunks> = {};
  for (let i = 0; i < changes.length; i++) {
    if (commitHunks.value[i] !== undefined) keep[i] = commitHunks.value[i];
  }
  commitHunks.value = keep;
  if (activeDiff.value >= changes.length) activeDiff.value = -1;
}
// 提交弹窗文件按 已暂存 / 未暂存 / 未跟踪 分区（部分暂存 MM 归入已暂存区，其未暂存部分靠「放弃」按钮处理）
const commitFileQuery = ref("");
// 搜索过滤后的分区（只影响列表显示；全选/计数/全部放弃仍作用于全部文件）
const filteredCommitSections = computed(() => {
  const q = commitFileQuery.value.trim().toLowerCase();
  if (!q) return commitFileSections.value;
  return commitFileSections.value
    .map((sec) => ({
      ...sec,
      items: sec.items.filter((it) => it.c.path.toLowerCase().includes(q)),
    }))
    .filter((sec) => sec.items.length);
});
// 当前可见（未过滤掉的）文件索引序列，方向键在其上移动
function visibleCommitOrder(): number[] {
  const q = commitFileQuery.value.trim().toLowerCase();
  if (!q) return commitModal.value?.changes.map((_, i) => i) ?? [];
  const out: number[] = [];
  for (const sec of filteredCommitSections.value) for (const it of sec.items) out.push(it.i);
  return out;
}
const commitFileSections = computed(() => {
  const m = commitModal.value;
  if (!m) return [];
  const staged: { c: ChangeFile; i: number }[] = [];
  const unstaged: { c: ChangeFile; i: number }[] = [];
  const untracked: { c: ChangeFile; i: number }[] = [];
  m.changes.forEach((c, i) => {
    if (c.status.trim() === "??") untracked.push({ c, i });
    else if (isStaged(c.status)) staged.push({ c, i });
    else unstaged.push({ c, i });
  });
  const sections: { key: string; title: string; items: { c: ChangeFile; i: number }[] }[] = [];
  if (staged.length) sections.push({ key: "staged", title: t("stagedGroup"), items: staged });
  if (unstaged.length) sections.push({ key: "unstaged", title: t("unstagedGroup"), items: unstaged });
  if (untracked.length) sections.push({ key: "untracked", title: t("untrackedGroup"), items: untracked });
  return sections;
});
// 块级暂存：把某个 Hunk 应用到暂存区
async function stageHunkOp(patch: string) {
  const m = commitModal.value;
  const i = activeDiff.value;
  if (!m || i < 0) return;
  const name = repoName(m.repo.path);
  try {
    const res = await invoke<OpResult>("stage_hunk", { path: m.repo.path, patch });
    addLog(`${res.ok ? "✅" : "❌"} ${name}: ${res.message}`);
    await reloadCommitChanges();
    const h = { ...commitHunks.value };
    delete h[i];
    commitHunks.value = h;
    await selectDiff(i);
  } catch (e) {
    addLog(tr("logStageHunkFail", { e: String(e) }));
  }
}
// porcelain 状态：X 位有内容且非未跟踪 → 已暂存
function isStaged(status: string) {
  const x = status[0] ?? "";
  return x !== " " && x !== "?" && x !== "";
}
// 有未暂存部分（工作树改动）或未跟踪
function hasUnstaged(status: string) {
  if (status.trim() === "??") return true;
  const y = status[1] ?? "";
  return y !== " " && y !== "";
}
// 撤销暂存指定文件
async function unstageFileOp(i: number) {
  const m = commitModal.value;
  if (!m || i < 0) return;
  const name = repoName(m.repo.path);
  try {
    const res = await invoke<OpResult>("unstage_file", {
      path: m.repo.path,
      file: m.changes[i].path,
    });
    addLog(`${res.ok ? "✅" : "❌"} ${name}: ${res.message}`);
    await reloadCommitChanges();
    const h = { ...commitHunks.value };
    delete h[i];
    commitHunks.value = h;
    if (i < m.changes.length) await selectDiff(i);
  } catch (e) {
    addLog(tr("logUnstageFail", { e: String(e) }));
  }
}
// 放弃改动（二次确认后执行）
async function discardFileOp(i: number) {
  const m = commitModal.value;
  if (!m || i < 0) return;
  const file = m.changes[i].path;
  if (m.confirmDiscard !== file) {
    m.confirmDiscard = file;
    return;
  }
  m.confirmDiscard = "";
  const name = repoName(m.repo.path);
  try {
    const res = await invoke<OpResult>("discard_file", {
      path: m.repo.path,
      file,
    });
    addLog(`${res.ok ? "✅" : "❌"} ${name}: ${res.message}`);
    await reloadCommitChanges();
    const h = { ...commitHunks.value };
    delete h[i];
    commitHunks.value = h;
    if (i < m.changes.length) await selectDiff(i);
  } catch (e) {
    addLog(tr("logDiscardFail", { e: String(e) }));
  }
}
const cloneModal = ref<{ url: string; base: string; busy: boolean } | null>(null);
const logModal = ref<LogModal | null>(null);
// 提交历史搜索过滤（提交信息 / 作者 / hash）
const filteredLogList = computed(() => {
  const m = logModal.value;
  if (!m) return [];
  const q = (m.search || "").trim().toLowerCase();
  if (!q) return m.list;
  return m.list.filter(
    (c) =>
      c.subject.toLowerCase().includes(q) ||
      c.author.toLowerCase().includes(q) ||
      c.hash.toLowerCase().includes(q)
  );
});
const graphModal = ref<GraphModal | null>(null);
const branchModal = ref<BranchModal | null>(null);
const tagModal = ref<TagModal | null>(null);
const remoteModal = ref<RemoteModal | null>(null);
const stashModal = ref<StashModal | null>(null);
// 提交历史弹窗内按提交 hash 缓存 diff
const logDiff = ref<Record<string, string>>({});
const logDiffLoading = ref(false);
// 分支图谱配色（按 lane 循环取色）
const GRAPH_COLORS = ["#0969da", "#cf222e", "#1a7f37", "#9a6700", "#8250df", "#0550ae", "#d1242f", "#6f42c1", "#0e8a16", "#953800"];
function laneColor(lane: number) {
  return GRAPH_COLORS[lane % GRAPH_COLORS.length];
}
// 计算每个提交的 lane（纵向轨道）位置
function computeGraphLayout(commits: GraphCommit[]) {
  const rowOf = new Map<string, number>();
  commits.forEach((c, i) => rowOf.set(c.hash, i));
  const lanes: (string | null)[] = [];
  const assign: Record<string, number> = {};
  for (let i = 0; i < commits.length; i++) {
    const c = commits[i];
    let lane = lanes.indexOf(c.hash);
    if (lane === -1) {
      lane = lanes.indexOf(null);
      if (lane === -1) {
        lane = lanes.length;
        lanes.push(c.hash);
      } else lanes[lane] = c.hash;
    }
    assign[c.hash] = lane;
    if (c.parents.length === 0) lanes[lane] = null;
    else {
      lanes[lane] = c.parents[0];
      for (let pi = 1; pi < c.parents.length; pi++) {
        const p = c.parents[pi];
        let pl = lanes.indexOf(p);
        if (pl === -1) {
          pl = lanes.indexOf(null);
          if (pl === -1) {
            pl = lanes.length;
            lanes.push(p);
          } else lanes[pl] = p;
        }
      }
    }
  }
  return { assign, rowOf, maxLanes: lanes.length };
}
// 分支图谱弹窗视图（SVG 拓扑 + HTML 行对齐）
const graphView = computed(() => {
  const m = graphModal.value;
  if (!m || !m.list.length) return null;
  const ROW_H = 26;
  const LANE_W = 20;
  const PAD_TOP = 20;
  const { assign, rowOf, maxLanes } = computeGraphLayout(m.list);
  const edges: { x1: number; y1: number; x2: number; y2: number; color: string }[] = [];
  const nodes = m.list.map((c, i) => {
    const lane = assign[c.hash];
    const x = lane * LANE_W + LANE_W / 2 + 6;
    const y = PAD_TOP + i * ROW_H + ROW_H / 2;
    for (const p of c.parents) {
      const pr = rowOf.get(p);
      if (pr === undefined) continue;
      const x2 = assign[p] * LANE_W + LANE_W / 2 + 6;
      const y2 = PAD_TOP + pr * ROW_H + ROW_H / 2;
      edges.push({ x1: x, y1: y, x2, y2, color: laneColor(lane) });
    }
    return { ...c, lane, x, y };
  });
  return {
    nodes,
    edges,
    laneW: maxLanes * LANE_W + 14,
    svgHeight: PAD_TOP + m.list.length * ROW_H + 10,
    ROW_H,
    PAD_TOP,
  };
});
const graphRefsCount = computed(() => {
  const m = graphModal.value;
  if (!m) return 0;
  const s = new Set<string>();
  for (const c of m.list) for (const r of c.refs) s.add(r);
  return s.size;
});
const changesTip = ref<{ path: string; files: ChangeFile[]; x: number; y: number; err?: string } | null>(null);
const changesCache = new Map<string, ChangeFile[]>();
const progress = ref<{ done: number; total: number; ok: number; path: string } | null>(null);
const batchSummary = ref<{ ok: number; fail: number } | null>(null);
// 最近一次批量操作的失败仓库路径，用于点击汇总条时定位高亮
const failPaths = ref<Set<string>>(new Set());
// 批量操作取消请求中
const cancelling = ref(false);
// 最近一次 runOnRepos 批量操作（供“重试失败项”复用）
const lastBatch = ref<{ action: "pull" | "push" | "stash" | "stashpop" | "cmd"; cmd?: string } | null>(null);
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
    batchBtn: "批量",
    batchBtnTip: "批量执行命令 / 切换分支 / 替换 Remote",
    batchTabCmd: "批量命令",
    batchTabSwitch: "批量切换分支",
    batchTabReplace: "批量替换 Remote",
    batchSelected: "已勾选 {n} 个仓库，可执行",
    batchNeedSelect: "未勾选仓库——先在左侧列表勾选要操作的仓库，批量按钮即可用",
    aboutTitle: "关于 RepoPilot",
    scanning: "扫描中…",
    refresh: "刷新状态",
    removeRoot: "移除该根目录",
    logRootAdded: "已添加根目录：{p}",
    logRootRemoved: "已移除根目录：{p}",
    logRootExists: "目录已在列表中：{p}",
    logRootMissing: "❌ 路径不存在：{p}",
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
    logCopyManualFail: "复制失败，请手动复制",
    autoRefresh: "自动刷新",
    lastRefresh: "上次刷新 {t}",
    searchPlaceholder: "搜索仓库/路径/分支/地址…",
    busyTip: "⏳ 处理中，界面已可操作…",
    batchDone: "完成：成功 {ok} / 失败 {fail}",
    batchLocateTip: "有失败项，点击高亮并定位失败仓库",
    cancelling: "取消中…",
    cancelTip: "停止剩余仓库的操作（已开始的会执行完）",
    logCancel: "已请求取消，未开始的仓库将跳过",
    retryFail: "重试失败项",
    retryTip: "重新勾选上次失败的仓库，执行同一种操作",
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
    refreshing: "刷新中…",
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
    errClickTip: "点击查看错误详情",
    errTitle: "仓库错误：{name}",
    errCopy: "复制错误",
    errRetry: "重试状态",
    bDirty: "有改动",
    bClean: "干净",
    sync: "同步",
    noRemote: "无远程",
    cmdTitle: "批量执行自定义命令（对选中的每个仓库运行同一命令）",
    cmdPlaceholder: "如：git fetch --prune / npm update / git switch dev",
    run: "批量执行",
    cmdHint: "命令会在每个选中仓库的目录下执行，等同你在该目录的终端里手动运行。",
    swTitle: "批量切换分支（对选中的仓库执行 git switch）",
    branchSwitchHint: "下拉切换该仓库的分支",
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
    logClear: "清空",
    logClearTip: "清空操作日志",
    logCopyTip: "点击复制该条日志",
    logCopiedMsg: "已复制：",
    ctxPull: "⇩ Pull",
    ctxPush: "⇧ Push",
    ctxOnly: "◉ 仅选中此仓库",
    ctxFinder: "📂 Finder 显示",
    ctxTerm: ">_ 打开终端",
    ctxRefresh: "刷新此仓库状态",
    logOneRefreshed: "已刷新 {name} 状态",
    logOneRefreshFail: "刷新 {name} 失败",
    ctxWeb: "🌐 打开远程页",
    ctxCommit: "📝 提交…",
    ctxLog: "🕘 提交历史",
    ctxGraph: "📊 分支图谱",
    ctxBranch: "🌿 分支管理",
    ctxTag: "🏷 标签管理",
    ctxRemote: "🔗 远程仓库",
    ctxStash: "🗂 Stash 管理",
    ctxCopyPath: "复制路径",
    logPathCopied: "已复制路径：",
    logCopyFail: "复制失败：{e}",
    stashTitle: "🗂 {name} · Stash 管理",
    stashNewPlaceholder: "暂存当前改动，备注（如 wip: 功能开发中）",
    stashNewBtn: "暂存改动",
    stashNewTip: "把当前已跟踪改动暂存起来（可随时恢复）",
    stashPopBtn: "恢复",
    stashPopTip: "恢复该 stash 到工作区（stash pop）",
    stashDropBtn: "删除",
    stashConfirmDrop: "确认删除？",
    stashDropTip: "删除该 stash（不可恢复）",
    stashEmpty: "还没有 stash，可输入备注暂存当前改动",
    logStashCreateFail: "暂存改动失败：{e}",
    logStashPopFail: "恢复 stash 失败：{e}",
    logStashDropFail: "删除 stash 失败：{e}",
    remoteTitle: "🔗 {name} · 远程仓库",
    remoteNewName: "远程名（如 origin）",
    remoteNewUrl: "远程地址",
    remoteAdd: "添加",
    remoteEdit: "改址",
    remoteEditTip: "修改远程地址",
    remoteSave: "保存",
    remoteDel: "删除",
    remoteConfirmDel: "确认删除？",
    remoteDelTip: "删除该远程",
    remoteEmpty: "该仓库还没有配置远程",
    editConfigFile: "编辑配置文件…",
    editConfigTip: "打开该仓库的 .git/config 文件进行高级编辑",
    editConfigFail: "打开配置文件失败：{e}",
    tagTitle: "🏷 {name} · 标签",
    tagNewPlaceholder: "新标签名，回车创建",
    tagNewBtn: "创建",
    tagPush: "推送",
    tagPushTip: "推送到 origin",
    tagDel: "删除",
    tagConfirmDel: "确认删除？",
    tagDelTip: "删除本地标签",
    tagEmpty: "该仓库还没有标签",
    ctxAlias: "✎ 设置别名",
    branchTitle: "🌿 {name} · 分支管理",
    branchNewPlaceholder: "新分支名，回车创建",
    branchNewBtn: "创建",
    branchMerge: "合并",
    branchDel: "删除",
    branchConfirmDel: "确认删除？",
    branchSwitch: "切换",
    branchLocal: "本地",
    branchRemote: "远程",
    branchCurrent: "当前",
    branchSwitchTip: "切换到该分支",
    branchMergeTip: "把该分支合并到当前分支",
    branchDelTip: "删除该本地分支",
    logDiffBtn: "改动",
    logDiffHide: "收起",
    logCherry: "摘取",
    logConfirmCherry: "确认摘取？",
    logCherryTip: "把该提交摘取（Cherry-pick）到当前分支",
    aliasPlaceholder: "输入别名，回车保存（清空则移除）",
    logHistTitle: "提交历史 · {name}",
    logHistEmpty: "该仓库没有提交记录",
    logHistLoading: "加载中…",
    logSearchPlaceholder: "搜索提交信息 / 作者 / hash",
    logSearchEmpty: "没有匹配的提交",
    commitTitle: "提交 · {name}",
    commitPlaceholder: "提交信息，如 fix: 修复登录 bug",
    commitRun: "提交",
    diffHint: "点击查看该文件的改动内容",
    keyboardNavTip: "↑/↓ 方向键切换文件，右侧同步预览 diff",
    commitSearchPlaceholder: "搜索文件名/路径…",
    commitSearchNoMatch: "没有匹配的文件",
    conflictChecking: "正在比对远程分支改动…",
    conflictBadge: "远程已改",
    conflictTip: "远程分支也改动了这个文件，pull 时可能冲突",
    conflictSummary: "远程分支有改动，{n} 个文件可能冲突",
    conflictFail: "远程比对失败（悬停看原因）",
    conflictAuth: "远程需要认证，未比对（悬停看原因）",
    conflictRefresh: "重新比对",
    conflictRefreshTip: "手动重新拉取远程并比对改动",
    conflictOk: "已同步，无冲突",
    failTitle: "失败/跳过详情（{n} 个仓库）",
    failLocate: "定位仓库",
    failNoChanges: "（无未提交改动）",
    openCommit: "打开提交",
    loadingTip: "加载中…",
    authTitle: "远程需要认证",
    authTitleBatch: "{label} 需要认证：{n} 个仓库",
    authUsername: "用户名 / Token",
    authPassword: "密码 / Token",
    authSave: "记住凭据（写入 macOS 钥匙串，之后自动使用）",
    authConfirm: "确定并重试",
    splitterTip: "拖动调整文件列表宽度",
    refreshFiles: "刷新文件",
    refreshFilesTip: "重新扫描仓库的文件改动（保留勾选，新文件默认勾选）",
    logCommitRefreshed: "提交文件已刷新：{n} 个改动",
    discardSelected: "放弃选中 ({n})",
    discardSelectedTip: "放弃勾选文件中未暂存/未跟踪的改动（已暂存的不动，5 秒内二次点击确认）",
    discardAllConfirm: "确认放弃？",
    logDiscardAllDone: "已放弃 {n} 个文件的改动",
    logDiscardNoneSelected: "请先勾选要放弃的文件（已暂存的不受影响）",
    diffSelectHint: "点击左侧文件查看改动",
    diffLoading: "加载中…",
    diffNoChanges: "该文件没有可暂存的改动块",
    hunkTag: "改动块",
    hunkStageBtn: "暂存此块",
    hunkStageTip: "仅把这一块改动暂存到暂存区",
    hunkPartial: "该文件已有部分改动在暂存区，块级暂存不可用，请整文件处理",
    hunkUntracked: "未跟踪新文件，块级暂存不可用，请勾选整文件提交",
    logStageHunkFail: "暂存改动块失败：{e}",
    unstageBtn: "取消暂存",
    unstageTip: "把该文件移出暂存区（改动保留在工作区）",
    discardBtn: "放弃",
    discardConfirm: "确认放弃？",
    discardTip: "放弃该文件的改动（不可恢复）",
    logUnstageFail: "取消暂存失败：{e}",
    logDiscardFail: "放弃改动失败：{e}",
    msgHistBtn: "历史消息",
    msgHistTip: "复用最近使用的提交信息",
    msgHistEmpty: "还没有历史提交信息",
    stagedGroup: "已暂存",
    unstagedGroup: "未暂存",
    untrackedGroup: "未跟踪",
    maxWin: "最大化",
    restoreWin: "还原",
    graphTitle: "📊 {name} · 分支图谱",
    graphLoading: "图谱加载中…",
    graphEmpty: "该仓库暂无提交",
    graphCount: "最近 {n} 条提交 · {refs} 个分支/标签",
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
    aboutDesc: "本地多仓库批量管理工具 · Tauri 2 + Vue 3 · 本地优先",
    aboutSec1: "总览与分组",
    aboutA1: "多根目录扫描，嵌套仓库树形展开，子仓库脏状态汇总",
    aboutA2: "分组树 / 收藏 / 仓库别名 / 搜索筛选 / 多列排序",
    aboutA3: "状态展示：分支 · 领先落后 · 最后提交 · 改动数，自动刷新",
    aboutSec2: "批量操作",
    aboutB1: "批量 pull / push / stash / 恢复 / 切换分支 / 自定义命令 / 替换 Remote",
    aboutB2: "实时进度、可中途取消、失败详情弹窗（文件清单 + 一键打开提交）",
    aboutB3: "智能跳过：有未提交改动且远程无新提交时无需 pull；远程领先才拦截",
    aboutB4: "认证弹窗：http 仓库需认证时自动弹窗重试，可记住凭据到 macOS 钥匙串",
    aboutSec3: "提交与版本控制",
    aboutC1: "提交弹窗：左右分栏 diff、Hunk 块级暂存、文件搜索、方向键导航",
    aboutC2: "远程冲突比对：本地与远程都改过的文件红色徽标预警",
    aboutC3: "提交历史 + 一键复用提交信息、Cherry-pick、可视化分支图谱",
    aboutC4: "分支 / 标签 / 远程 / Stash 管理弹窗，克隆仓库（超时保护）",
    aboutSec4: "体验与安全",
    aboutD1: "深色 / 浅色主题，中 / 英双语，Dock 脏仓角标",
    aboutD2: "命令超时保护、配置导出 / 导入、操作日志持久化",
    aboutD3: "快捷键：⌘R 刷新 · ⌘⇧A 全选 · ⌘⇧D 取消 · ⌘P Pull · ⌘U Push",
    aboutD4: "内置自动更新，发布后应用内一键升级",
    closeWin: "关闭弹窗",
    close: "关闭",
    emptyNoRepo: "还没有仓库——先输入根目录点“扫描仓库”",
    emptyNoStatus: "没有“{s}”状态的仓库",
    emptyNoMatch: "没有匹配的仓库",
    emptyNoUngrouped: "没有未分组的仓库",
    favs: "收藏",
    newSubGroup: "新建子分组",
    subGroupPlaceholder: "子分组名（单级）",
    logGroupNoSlash: "分组名不能包含 /，每次只创建一级",
    ctxMoveGroup: "移动到分组",
    logGroupMoved: "已移动 {n} 个仓库到「{g}」",
    logGroupMovedOut: "已移动 {n} 个仓库到未分组",
    sbCollapse: "收起分组侧栏",
    sbExpand: "展开分组侧栏",
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
    logScanStart: "开始扫描 {n} 个根目录…",
    logScanDone: "扫描完成：找到 {n} 个仓库，耗时 {ms} 秒",
    logScanFail: "扫描失败: {e}",
    logRestoreDir: "已恢复上次的根目录：{p}",
    logRefreshed: "状态已刷新：{n} 个仓库",
    logRefreshFail: "刷新失败：{e}",
    logNoRepos: "暂无仓库可刷新",
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
    batchBtn: "Batch",
    batchBtnTip: "Run command / switch branch / replace remote",
    batchTabCmd: "Batch command",
    batchTabSwitch: "Switch branch",
    batchTabReplace: "Replace remote",
    batchSelected: "{n} repo(s) selected, ready to run",
    batchNeedSelect: "No repos selected — check repos in the list first to enable batch buttons",
    aboutTitle: "About RepoPilot",
    scanning: "Scanning…",
    refresh: "Refresh",
    removeRoot: "Remove this root",
    logRootAdded: "Added root: {p}",
    logRootRemoved: "Removed root: {p}",
    logRootExists: "Root already in list: {p}",
    logRootMissing: "❌ Path does not exist: {p}",
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
    logCopyManualFail: "Copy failed, please copy manually",
    autoRefresh: "Auto refresh",
    lastRefresh: "Last refresh {t}",
    searchPlaceholder: "Search name/path/branch/url…",
    busyTip: "⏳ Working…",
    batchDone: "Done: {ok} ok / {fail} failed",
    batchLocateTip: "Has failures - click to highlight & locate failed repos",
    cancelling: "Cancelling…",
    cancelTip: "Stop remaining repos (already-running ones will finish)",
    logCancel: "Cancel requested; remaining repos will be skipped",
    retryFail: "Retry failed",
    retryTip: "Re-select last failed repos and run the same action",
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
    refreshing: "Refreshing…",
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
    errClickTip: "Click to view error details",
    errTitle: "Repo error: {name}",
    errCopy: "Copy error",
    errRetry: "Retry status",
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
    branchSwitchHint: "Switch this repo's branch from dropdown",
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
    logClear: "Clear",
    logClearTip: "Clear operation log",
    logCopyTip: "Click to copy this line",
    logCopiedMsg: "Copied: ",
    ctxPull: "⇩ Pull",
    ctxPush: "⇧ Push",
    ctxOnly: "◉ Select only this",
    ctxFinder: "📂 Reveal in Finder",
    ctxTerm: ">_ Open terminal",
    ctxRefresh: "Refresh this repo's status",
    logOneRefreshed: "Refreshed {name} status",
    logOneRefreshFail: "Failed to refresh {name}",
    ctxWeb: "🌐 Open remote page",
    ctxCommit: "📝 Commit…",
    ctxLog: "🕘 History",
    ctxGraph: "📊 Branch graph",
    ctxBranch: "🌿 Branches",
    ctxTag: "🏷 Tags",
    ctxRemote: "🔗 Remotes",
    ctxStash: "🗂 Stash",
    ctxCopyPath: "Copy path",
    logPathCopied: "Copied path: ",
    logCopyFail: "Copy failed: {e}",
    stashTitle: "🗂 {name} · Stash",
    stashNewPlaceholder: "Stash current changes with a label (e.g. wip: feature)",
    stashNewBtn: "Stash",
    stashNewTip: "Stash current tracked changes (recoverable)",
    stashPopBtn: "Pop",
    stashPopTip: "Restore this stash to worktree (stash pop)",
    stashDropBtn: "Drop",
    stashConfirmDrop: "Confirm drop?",
    stashDropTip: "Delete this stash (irreversible)",
    stashEmpty: "No stashes yet — add a label to stash current changes",
    logStashCreateFail: "Failed to stash: {e}",
    logStashPopFail: "Failed to pop stash: {e}",
    logStashDropFail: "Failed to drop stash: {e}",
    remoteTitle: "🔗 {name} · Remotes",
    remoteNewName: "Remote name (e.g. origin)",
    remoteNewUrl: "Remote URL",
    remoteAdd: "Add",
    remoteEdit: "Edit",
    remoteEditTip: "Edit remote URL",
    remoteSave: "Save",
    remoteDel: "Delete",
    remoteConfirmDel: "Confirm delete?",
    remoteDelTip: "Remove this remote",
    remoteEmpty: "No remotes configured",
    editConfigFile: "Edit Config File…",
    editConfigTip: "Open this repo's .git/config for advanced editing",
    editConfigFail: "Failed to open config file: {e}",
    tagTitle: "🏷 {name} · Tags",
    tagNewPlaceholder: "New tag name, Enter to create",
    tagNewBtn: "Create",
    tagPush: "Push",
    tagPushTip: "Push to origin",
    tagDel: "Delete",
    tagConfirmDel: "Confirm delete?",
    tagDelTip: "Delete local tag",
    tagEmpty: "No tags in this repo",
    ctxAlias: "✎ Set alias",
    branchTitle: "🌿 {name} · Branches",
    branchNewPlaceholder: "New branch name, Enter to create",
    branchNewBtn: "Create",
    branchMerge: "Merge",
    branchDel: "Delete",
    branchConfirmDel: "Confirm delete?",
    branchSwitch: "Switch",
    branchLocal: "Local",
    branchRemote: "Remote",
    branchCurrent: "current",
    branchSwitchTip: "Switch to this branch",
    branchMergeTip: "Merge this branch into current",
    branchDelTip: "Delete this local branch",
    logDiffBtn: "Diff",
    logDiffHide: "Hide",
    logCherry: "Cherry-pick",
    logConfirmCherry: "Confirm?",
    logCherryTip: "Cherry-pick this commit to current branch",

    aliasPlaceholder: "Alias, Enter to save (empty removes)",
    logHistTitle: "History · {name}",
    logHistEmpty: "No commits in this repo",
    logHistLoading: "Loading…",
    logSearchPlaceholder: "Search commits (subject / author / hash)",
    logSearchEmpty: "No matching commits",
    commitTitle: "Commit · {name}",
    commitPlaceholder: "Commit message, e.g. fix: fix login bug",
    commitRun: "Commit",
    diffHint: "Click to view this file's diff",
    keyboardNavTip: "Use ↑/↓ arrows to switch files, diff previews on the right",
    commitSearchPlaceholder: "Search file name/path…",
    commitSearchNoMatch: "No matching files",
    conflictChecking: "Comparing with remote branch…",
    conflictBadge: "remote",
    conflictTip: "Remote branch also changed this file; pull may conflict",
    conflictSummary: "Remote has changes in {n} file(s), may conflict",
    conflictFail: "Remote comparison failed (hover for reason)",
    conflictAuth: "Remote requires authentication, skipped (hover for reason)",
    conflictRefresh: "Re-compare",
    conflictRefreshTip: "Manually fetch remote and re-compare changes",
    conflictOk: "In sync, no conflicts",
    failTitle: "Failed/skipped details ({n} repos)",
    failLocate: "Locate repos",
    failNoChanges: "(no unstaged changes)",
    openCommit: "Open commit",
    loadingTip: "Loading…",
    authTitle: "Remote requires authentication",
    authTitleBatch: "{label} requires authentication: {n} repo(s)",
    authUsername: "Username / Token",
    authPassword: "Password / Token",
    authSave: "Remember credentials (save to macOS Keychain, used automatically later)",
    authConfirm: "OK & retry",
    splitterTip: "Drag to resize the file list",
    refreshFiles: "Refresh files",
    refreshFilesTip: "Re-scan repo changes (keeps selection; new files checked by default)",
    logCommitRefreshed: "Commit files refreshed: {n} changes",
    discardSelected: "Discard selected ({n})",
    discardSelectedTip: "Discard unstaged/untracked changes in checked files (staged untouched; click again within 5s to confirm)",
    discardAllConfirm: "Confirm discard?",
    logDiscardAllDone: "Discarded changes in {n} files",
    logDiscardNoneSelected: "Check files to discard first (staged files are untouched)",
    diffSelectHint: "Click a file on the left to view its diff",
    diffLoading: "Loading…",
    diffNoChanges: "No staggable hunks in this file",
    hunkTag: "Hunk",
    hunkStageBtn: "Stage",
    hunkStageTip: "Stage only this hunk",
    hunkPartial: "File has some staged changes; hunk staging unavailable",
    hunkUntracked: "Untracked file; stage the whole file instead",
    logStageHunkFail: "Stage hunk failed: {e}",
    unstageBtn: "Unstage",
    unstageTip: "Move this file out of the index (changes kept in worktree)",
    discardBtn: "Discard",
    discardConfirm: "Confirm discard?",
    discardTip: "Discard this file's changes (irreversible)",
    logUnstageFail: "Failed to unstage: {e}",
    logDiscardFail: "Failed to discard: {e}",
    msgHistBtn: "History",
    msgHistTip: "Reuse a recent commit message",
    msgHistEmpty: "No recent commit messages",
    stagedGroup: "Staged",
    unstagedGroup: "Changes",
    untrackedGroup: "Untracked",
    maxWin: "Maximize",
    restoreWin: "Restore",
    graphTitle: "📊 {name} · Branch graph",
    graphLoading: "Loading graph…",
    graphEmpty: "No commits in this repo",
    graphCount: "Latest {n} commits · {refs} branches/tags",
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
    aboutDesc: "Local multi-repo batch manager · Tauri 2 + Vue 3 · local-first",
    aboutSec1: "Overview & Groups",
    aboutA1: "Multi-root scanning, nested repo tree, child dirty-status summary",
    aboutA2: "Group tree / favorites / repo alias / search filter / multi-column sort",
    aboutA3: "Status: branch · ahead/behind · last commit · changes, auto refresh",
    aboutSec2: "Batch Operations",
    aboutB1: "Batch pull / push / stash / pop / switch branch / custom command / replace remote",
    aboutB2: "Live progress, cancellable, failure-detail dialog (file list + open commit)",
    aboutB3: "Smart skip: local changes with no remote updates = no pull needed; intercept only when remote is ahead",
    aboutB4: "Auth dialog: auto-retry http repos, remember credentials in macOS Keychain",
    aboutSec3: "Commit & Version Control",
    aboutC1: "Commit dialog: split diff, hunk-level staging, file search, arrow-key nav",
    aboutC2: "Remote conflict compare: red badges for files changed on both sides",
    aboutC3: "History + reuse commit message, cherry-pick, visual branch graph",
    aboutC4: "Branch / tag / remote / stash managers, clone with timeout guard",
    aboutSec4: "Experience & Safety",
    aboutD1: "Dark / light theme, zh / en UI, Dock dirty-repo badge",
    aboutD2: "Command timeout guard, config export / import, persisted logs",
    aboutD3: "Shortcuts: ⌘R refresh · ⌘⇧A select all · ⌘⇧D deselect · ⌘P pull · ⌘U push",
    aboutD4: "Built-in auto-update, one-click upgrade in-app",
    closeWin: "Close dialog",
    close: "Close",
    emptyNoRepo: "No repos yet — enter a root directory and click Scan",
    emptyNoStatus: "No repos with status “{s}”",
    emptyNoMatch: "No matching repos",
    emptyNoUngrouped: "No ungrouped repos",
    favs: "Favorites",
    newSubGroup: "New sub-group",
    subGroupPlaceholder: "Sub-group name (single level)",
    logGroupNoSlash: "Group name cannot contain /, create one level at a time",
    ctxMoveGroup: "Move to group",
    logGroupMoved: "Moved {n} repo(s) to “{g}”",
    logGroupMovedOut: "Moved {n} repo(s) to ungrouped",
    sbCollapse: "Collapse group sidebar",
    sbExpand: "Expand group sidebar",
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
    logScanStart: "Scanning {n} root(s)…",
    logScanDone: "Scan done: found {n} repos in {ms}s",
    logScanFail: "Scan failed: {e}",
    logRestoreDir: "Restored last roots: {p}",
    logRefreshed: "Status refreshed: {n} repos",
    logRefreshFail: "Refresh failed: {e}",
    logNoRepos: "No repos to refresh",
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
// 刷新进行中标记：防止自动刷新并发重入，同时驱动刷新按钮的"刷新中…"状态
const refreshing = ref(false);

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
  const name = (p: string) => repoName(p);
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
// 分组侧栏折叠（持久化）
const sidebarCollapsed = ref(localStorage.getItem("repoPilot.sbCollapsed") === "1");
watch(sidebarCollapsed, (v) => localStorage.setItem("repoPilot.sbCollapsed", v ? "1" : "0"));
const favs = ref<Set<string>>(new Set());
// 仓库别名：仓库路径 -> 显示名（可空）
const aliases = ref<Record<string, string>>({});
const editingAlias = ref("");
const aliasDraft = ref("");
const aliasInput = ref<HTMLInputElement | null>(null);
function repoDisplayName(r: RepoStatus): string {
  return aliases.value[r.path] || repoName(r.path) || r.path;
}
function startAliasEdit(path: string) {
  editingAlias.value = path;
  aliasDraft.value = aliases.value[path] || "";
  nextTick(() => aliasInput.value?.focus());
}
function saveAlias() {
  const path = editingAlias.value;
  if (!path) return;
  const a = { ...aliases.value };
  if (aliasDraft.value.trim()) a[path] = aliasDraft.value.trim();
  else delete a[path];
  aliases.value = a;
  editingAlias.value = "";
  invoke("save_aliases", { state: a }).catch((err) =>
    addLog(tr("logFavFail", { e: String(err) }))
  );
}
// 弹窗最大化状态（按弹窗类型区分），支持最大化/还原
const maximized = ref<Record<string, boolean>>({});
function isMax(k: string) {
  return !!maximized.value[k];
}
function toggleMax(k: string) {
  maximized.value = { ...maximized.value, [k]: !maximized.value[k] };
}
// 取仓库名：兼容 Windows（\）与 macOS/Linux（/）路径分隔符
function repoName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}
function closeModal(k: string) {
  if (k === "clone") cloneModal.value = null;
  else if (k === "log") logModal.value = null;
  else if (k === "graph") graphModal.value = null;
  else if (k === "branch") branchModal.value = null;
  else if (k === "tag") tagModal.value = null;
  else if (k === "remote") remoteModal.value = null;
  else if (k === "stash") stashModal.value = null;
  else if (k === "commit") commitModal.value = null;
  else if (k === "about") showAbout.value = false;
}
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
    // 空分组也显示：否则新建的空分组既看不到也无法选中删除（用户实测反馈）
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
// 批量移动到分组：勾选了仓库则移动勾选集合，否则移动右键的仓库
function moveSelectedToGroup(name: string) {
  const m = ctxMenu.value;
  const targets = selected.value.size ? [...selected.value] : m ? [m.repo.path] : [];
  if (!targets.length) return;
  for (const p of targets) setRepoGroup(p, name);
  addLog(
    name
      ? tr("logGroupMoved", { n: targets.length, g: name })
      : tr("logGroupMovedOut", { n: targets.length })
  );
  closeCtx();
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
// 新建分组的父级（""=顶级；非空=该组下新建子分组）
const newGroupParent = ref("");
// 规范化分组名：去首尾斜杠、压缩连续斜杠；无效（空/纯斜杠）返回 ""
function normalizeGroupName(name: string): string {
  return name
    .trim()
    .replace(/^\/+|\/+$/g, "")
    .replace(/\/+/g, "/");
}
function openNewGroup(parent = "") {
  newGroupParent.value = parent;
  showNewGroup.value = true;
  newGroupName.value = "";
  nextTick(() => newGrpInput.value?.focus());
}
function confirmNewGroup() {
  // 只创建一级：输入名不允许包含 "/"（层级通过"新建子分组"逐级创建）
  const n = normalizeGroupName(newGroupName.value);
  if (!n) return;
  if (n.includes("/")) {
    addLog(tr("logGroupNoSlash"));
    return;
  }
  const full = newGroupParent.value ? newGroupParent.value + "/" + n : n;
  if (!groupNames.value.includes(full)) {
    groupNames.value = [...groupNames.value, full];
    addLog(tr("logGroupCreated", { g: full }));
  } else {
    addLog(tr("logGroupRenameConflict", { g: full }));
  }
  activeGroup.value = full;
  showNewGroup.value = false;
  newGroupName.value = "";
  persistGroups();
}
// 删除分组及其所有子分组
function doRemoveGroup(name: string) {
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
  const cur = activeGroup.value;
  renameGroupName.value = cur.includes("/")
    ? cur.slice(cur.lastIndexOf("/") + 1)
    : cur;
  showRename.value = true;
  nextTick(() => renameGrpInput.value?.focus());
}
function confirmRename() {
  // 只改当前层级名（不允许 "/"），父级前缀自动保留
  const leaf = normalizeGroupName(renameGroupName.value);
  const oldName = activeGroup.value;
  showRename.value = false;
  if (!leaf) return;
  if (leaf.includes("/")) {
    addLog(tr("logGroupNoSlash"));
    return;
  }
  const parentPrefix = oldName.includes("/")
    ? oldName.slice(0, oldName.lastIndexOf("/") + 1)
    : "";
  const newName = parentPrefix + leaf;
  if (newName === oldName) return;
  // 不能重命名成自己的后代（如 工作 → 工作/前端/新），否则前缀替换会产生错乱路径
  if (newName.startsWith(oldName + "/")) {
    addLog(tr("logGroupRenameConflict", { g: newName }));
    return;
  }
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
      [r.path, repoName(r.path), aliases.value[r.path] ?? "", r.branch, r.remote_url]
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
    // 容器行只有在"展开"时才输出子行（与普通行一致），否则折叠后子行仍显示"收不起来"
    if (hasCh && !expanded.value.has(c)) emitChildren(c, 1, seq);
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
    addLog(t("logCopyManualFail"));
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
      aliases: aliases.value,
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
      aliases?: Record<string, string>;
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
    if (cfg.aliases && typeof cfg.aliases === "object") {
      aliases.value = cfg.aliases;
      await invoke("save_aliases", { state: cfg.aliases }).catch(() => {});
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
    commitHunks.value = {}; // 清空上次的 diff 缓存
    activeDiff.value = -1;
    commitModal.value = {
      repo: r,
      changes,
      selected: new Set(changes.map((_, i) => i)),
      message: "",
      busy: false,
      confirmDiscard: "",
      showMsgHist: false,
      conflictState: "idle",
      conflictFiles: [] as string[],
      conflictMsg: "",
    };
    maximized.value = { ...maximized.value, commit: true }; // 提交弹窗默认最大化
    nextTick(() => commitFilesRef.value?.focus()); // 自动聚焦文件列表，方向键可直接移动
    if (changes.length) void selectDiff(0); // 默认查看第一个文件的改动
    void checkRemoteConflicts(); // 后台比对远程分支改动（不阻塞弹窗）
  } catch (e) {
    addLog(tr("logListFail", { e: String(e) }));
  }
}
// 认证弹窗（SourceTree 风格）：远程操作需要认证时输入用户名/密码重试，可写入钥匙串
interface AuthTarget {
  path: string;
  name: string;
  args: string[];
}
const authRe =
  /could not read Username|Authentication failed|Device not configured|401|403|not authorized|credential|用户名|认证/i;
const authModal = ref<{
  visible: boolean;
  title: string;
  remoteUrl: string;
  targets: AuthTarget[];
  username: string;
  password: string;
  save: boolean;
  busy: boolean;
  onDone: ((ok: boolean) => void) | null;
} | null>(null);
function openAuthModal(opts: {
  title: string;
  remoteUrl: string;
  targets: AuthTarget[];
  onDone?: (ok: boolean) => void;
}) {
  authModal.value = {
    visible: true,
    title: opts.title,
    remoteUrl: opts.remoteUrl,
    targets: opts.targets,
    username: "",
    password: "",
    save: true,
    busy: false,
    onDone: opts.onDone ?? null,
  };
}
async function confirmAuthModal() {
  const a = authModal.value;
  if (!a || a.busy) return;
  a.busy = true;
  let allOk = true;
  for (const t of a.targets) {
    const res = await invoke<OpResult>("run_git_auth", {
      path: t.path,
      args: t.args,
      username: a.username,
      password: a.password,
      save: a.save,
    });
    addLog(`${res.ok ? "✅" : "❌"} ${t.name}: ${res.message}`);
    if (!res.ok) allOk = false;
  }
  a.visible = false;
  a.busy = false;
  if (a.onDone) a.onDone(allOk);
}
function closeAuthModal() {
  const a = authModal.value;
  if (!a || a.busy) return;
  a.visible = false;
  const cb = a.onDone;
  authModal.value = null;
  if (cb) cb(false);
}
// 远程冲突比对：标注「远程分支也改过」的文件（本地也改 → pull 时可能冲突）
type RemoteConflictResult = { ok: boolean; degraded: boolean; message: string; remote_changes: string[] };
async function checkRemoteConflicts(auth?: { username: string; password: string; save: boolean }) {
  const m = commitModal.value;
  if (!m || m.conflictState === "checking") return;
  m.conflictState = "checking";
  m.conflictMsg = t("conflictChecking");
  try {
    const res = await invoke<RemoteConflictResult>("check_remote_conflicts", {
      path: m.repo.path,
      authUser: auth?.username ?? null,
      authPass: auth?.password ?? null,
      authSave: auth?.save ?? false,
    });
    m.conflictFiles = res.remote_changes;
    m.conflictMsg = res.message;
    if (res.ok) {
      if (res.degraded && !res.remote_changes.length) {
        // 认证仓库降级比对且无差异：结果不可靠，保持"需认证"中性提示
        m.conflictState = "auth";
        return;
      }
      m.conflictState = "done";
      return;
    }
    if (authRe.test(res.message)) {
      // 认证类错误：弹认证窗，确认后带凭据重试比对
      m.conflictState = "auth";
      const repo = repos.value.find((x) => x.path === m.repo.path);
      openAuthModal({
        title: t("authTitle"),
        remoteUrl: repo?.remote_url ?? "",
        targets: [],
        onDone: (ok) => {
          if (!ok) return;
          const a = authModal.value;
          if (!a) return;
          void checkRemoteConflicts({
            username: a.username,
            password: a.password,
            save: a.save,
          });
        },
      });
    } else {
      m.conflictState = "error";
    }
  } catch (e) {
    m.conflictState = "error";
    m.conflictMsg = String(e);
  }
}
const commitConflictSet = computed(() => {
  const m = commitModal.value;
  return new Set(m?.conflictFiles ?? []);
});
const commitConflictCount = computed(() => {
  const m = commitModal.value;
  if (!m) return 0;
  const s = commitConflictSet.value;
  return m.changes.filter((c) => s.has(c.path)).length;
});
// 提交弹窗内刷新文件列表：新改动进来、已消失的移除；按路径保留勾选，新文件默认勾选
const commitRefreshing = ref(false);async function refreshCommitFiles() {
  const m = commitModal.value;
  if (!m || commitRefreshing.value) return;
  commitRefreshing.value = true;
  try {
    const changes = await invoke<ChangeFile[]>("list_changes", { path: m.repo.path });
    const oldByPath = new Map<string, number>();
    m.changes.forEach((c, i) => oldByPath.set(c.path, i));
    const selected = new Set<number>();
    changes.forEach((c, i) => {
      const oldIdx = oldByPath.get(c.path);
      if (oldIdx === undefined || m.selected.has(oldIdx)) selected.add(i);
    });
    const activePath = m.changes[activeDiff.value]?.path;
    commitHunks.value = {}; // 清空 diff 缓存，避免内容过期
    activeDiff.value = -1;
    m.changes = changes;
    m.selected = selected;
    m.confirmDiscard = "";
    if (activePath) {
      const ni = changes.findIndex((c) => c.path === activePath);
      if (ni >= 0) void selectDiff(ni);
      else if (changes.length) void selectDiff(0);
    } else if (changes.length) void selectDiff(0);
    addLog(tr("logCommitRefreshed", { n: changes.length }));
    void checkRemoteConflicts(); // 文件变化后重新比对远程改动
  } catch (e) {
    modalOrLog(e);
  } finally {
    commitRefreshing.value = false;
  }
}
// 放弃选中：对勾选文件中未暂存/未跟踪的逐个还原（已暂存的不动，避免误伤暂存内容）
const commitDiscardAll = ref(false);
const discardTargetCount = computed(() => {
  const m = commitModal.value;
  if (!m) return 0;
  return m.changes.filter((c, i) => m.selected.has(i) && hasUnstaged(c.status)).length;
});
async function discardSelectedFiles() {
  const m = commitModal.value;
  if (!m || m.busy) return;
  const targets = m.changes.filter((c, i) => m.selected.has(i) && hasUnstaged(c.status));
  if (!targets.length) return addLog(t("logDiscardNoneSelected"));
  if (!commitDiscardAll.value) {
    commitDiscardAll.value = true;
    setTimeout(() => {
      commitDiscardAll.value = false;
    }, 5000);
    return;
  }
  commitDiscardAll.value = false;
  m.busy = true;
  const name = repoName(m.repo.path);
  let ok = 0;
  try {
    // 串行执行，避免 git 索引锁冲突
    for (const c of targets) {
      const res = await invoke<OpResult>("discard_file", {
        path: m.repo.path,
        file: c.path,
      });
      if (res.ok) ok++;
      addLog(`${res.ok ? "✅" : "❌"} ${name}: ${res.message}`);
    }
    addLog(tr("logDiscardAllDone", { n: ok }));
    await refreshCommitFiles();  } catch (e) {
    modalOrLog(e);
  } finally {
    m.busy = false;
  }
}
// 提交弹窗文件列表：方向键 ↑/↓ 移动选中（高亮 + diff 联动，循环到底/顶）
const commitFilesRef = ref<HTMLElement | null>(null);
function onCommitFilesKeydown(e: KeyboardEvent) {
  const m = commitModal.value;
  if (!m || !m.changes.length) return;
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
  e.preventDefault();
  const delta = e.key === "ArrowDown" ? 1 : -1;
  const order = visibleCommitOrder();
  if (!order.length) return;
  const pos = order.indexOf(activeDiff.value);
  const next =
    pos < 0 ? (delta > 0 ? 0 : order.length - 1) : (pos + delta + order.length) % order.length;
  void selectDiff(order[next]);
  nextTick(() => {
    document.querySelector(".commit-file.sel")?.scrollIntoView({ block: "nearest" });
  });
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
      saveCommitMsg(m.message);
      addLog(
        tr("logCommitDone", {
          name: repoName(m.repo.path),
          n: files.length,
        })
      );
      commitModal.value = null;
      // 只刷新该仓库的状态（提交单个仓库却全量刷新几百个会明显变慢，用户感知不到"自动刷新"）
      await refreshOne(m.repo.path);
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
  logModal.value = { path: r.path, name: repoName(r.path), list: [], loading: true, confirmCherry: "", search: "" };
  logDiff.value = {};
  try {
    const list = await invoke<CommitInfo[]>("get_log", { path: r.path, count: 20 });
    if (logModal.value && logModal.value.path === r.path) {
      logModal.value.list = list;
      logModal.value.loading = false;
    }
  } catch (e) {
    if (logModal.value && logModal.value.path === r.path) {
      logModal.value.loading = false;
      modalOrLog(e);
    }
  }
}

// 分支图谱
async function openGraph(r: RepoStatus) {
  graphModal.value = {
    path: r.path,
    name: repoDisplayName(r),
    currentBranch: r.branch || "",
    list: [],
    loading: true,
    error: "",
  };
  try {
    const list = await invoke<GraphCommit[]>("get_graph", { path: r.path, count: 1000 });
    if (graphModal.value && graphModal.value.path === r.path) {
      graphModal.value.list = list;
      graphModal.value.loading = false;
    }
  } catch (e) {
    if (graphModal.value && graphModal.value.path === r.path) {
      graphModal.value.error = String(e);
      graphModal.value.loading = false;
    }
  }
}
function graphRowTitle(n: GraphCommit): string {
  const refs = n.refs.length ? n.refs.join(", ") : "";
  return `${n.short} ${n.subject}\n${n.author} · ${new Date(n.time * 1000).toLocaleString()}${refs ? "\nrefs: " + refs : ""}`;
}

// 分支管理
async function openBranch(r: RepoStatus) {
  branchModal.value = {
    path: r.path,
    name: repoDisplayName(r),
    current: r.branch || "",
    branches: [],
    loading: true,
    error: "",
    opMsg: "",
    newName: "",
    busy: false,
    confirmDel: "",
  };
  try {
    const list = await invoke<BranchInfo[]>("get_branches", { path: r.path });
    if (branchModal.value && branchModal.value.path === r.path) {
      branchModal.value.branches = list;
      const cur = list.find((b) => b.is_current);
      if (cur) branchModal.value.current = cur.name;
      branchModal.value.loading = false;
    }
  } catch (e) {
    if (branchModal.value && branchModal.value.path === r.path) {
      branchModal.value.error = String(e);
      branchModal.value.loading = false;
    }
  }
}
async function reloadBranches() {
  const m = branchModal.value;
  if (!m) return;
  m.loading = true;
  m.opMsg = "";
  try {
    m.branches = await invoke<BranchInfo[]>("get_branches", { path: m.path });
    const cur = m.branches.find((b) => b.is_current);
    if (cur) m.current = cur.name;
    m.error = "";
  } catch (e) {
    m.error = String(e);
  } finally {
    m.loading = false;
  }
}
async function createBranchOp() {
  const m = branchModal.value;
  if (!m || !m.newName.trim() || m.busy) return;
  m.busy = true;
  try {
    const res = await invoke<OpResult>("create_branch", { path: m.path, name: m.newName.trim() });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    if (res.ok) m.newName = "";
    await reloadBranches();
  } catch (e) {
    modalOrLog(e);
  } finally {
    m.busy = false;
  }
}
async function mergeBranchOp(name: string) {
  const m = branchModal.value;
  if (!m || m.busy || m.current === name) return;
  m.busy = true;
  try {
    const res = await invoke<OpResult>("merge_branch", { path: m.path, name });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    await reloadBranches();
    await refreshOne(m.path);
  } catch (e) {
    modalOrLog(e);
  } finally {
    m.busy = false;
  }
}
async function deleteBranchOp(name: string) {
  const m = branchModal.value;
  if (!m || m.busy) return;
  if (m.confirmDel !== name) {
    m.confirmDel = name;
    return;
  }
  m.confirmDel = "";
  m.busy = true;
  try {
    const res = await invoke<OpResult>("delete_branch", { path: m.path, name, force: false });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    await reloadBranches();
    await refreshOne(m.path);
  } catch (e) {
    modalOrLog(e);
  } finally {
    m.busy = false;
  }
}
async function switchBranchOp(name: string) {
  const m = branchModal.value;
  if (!m || m.busy || m.current === name) return;
  m.busy = true;
  try {
    const resArr = await invoke<OpResult[]>("switch_branches", { paths: [m.path], branch: name });
    const res = resArr[0] as OpResult;
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    if (res.ok) m.current = name;
    await reloadBranches();
    await refreshOne(m.path);
  } catch (e) {
    modalOrLog(e);
  } finally {
    m.busy = false;
  }
}
// 标签管理
async function openTag(r: RepoStatus) {
  tagModal.value = {
    path: r.path,
    name: repoDisplayName(r),
    list: [],
    loading: true,
    error: "",
    opMsg: "",
    newName: "",
    busy: false,
    confirmDel: "",
  };
  try {
    const list = await invoke<string[]>("get_tags", { path: r.path });
    if (tagModal.value && tagModal.value.path === r.path) {
      tagModal.value.list = list;
      tagModal.value.loading = false;
    }
  } catch (e) {
    if (tagModal.value && tagModal.value.path === r.path) {
      tagModal.value.error = String(e);
      tagModal.value.loading = false;
    }
  }
}
async function reloadTags() {
  const m = tagModal.value;
  if (!m) return;
  m.loading = true;
  m.opMsg = "";
  try {
    m.list = await invoke<string[]>("get_tags", { path: m.path });
    m.error = "";
  } catch (e) {
    m.error = String(e);
  } finally {
    m.loading = false;
  }
}
async function createTagOp() {
  const m = tagModal.value;
  if (!m || !m.newName.trim() || m.busy) return;
  m.busy = true;
  try {
    const res = await invoke<OpResult>("create_tag", { path: m.path, name: m.newName.trim() });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    if (res.ok) m.newName = "";
    await reloadTags();
  } catch (e) {
    modalOrLog(e);
  } finally {
    m.busy = false;
  }
}
async function deleteTagOp(name: string) {
  const m = tagModal.value;
  if (!m || m.busy) return;
  if (m.confirmDel !== name) {
    m.confirmDel = name;
    return;
  }
  m.confirmDel = "";
  m.busy = true;
  try {
    const res = await invoke<OpResult>("delete_tag", { path: m.path, name });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    await reloadTags();
  } catch (e) {
    modalOrLog(e);
  } finally {
    m.busy = false;
  }
}
async function pushTagOp(name: string) {
  const m = tagModal.value;
  if (!m || m.busy) return;
  m.busy = true;
  try {
    const res = await invoke<OpResult>("push_tag", { path: m.path, name });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
  } catch (e) {
    modalOrLog(e);
  } finally {
    m.busy = false;
  }
}
// 远程仓库管理
async function openRemote(r: RepoStatus) {
  remoteModal.value = {
    path: r.path,
    name: repoDisplayName(r),
    list: [],
    loading: true,
    error: "",
    opMsg: "",
    busy: false,
    newName: "",
    newUrl: "",
    editName: null,
    editUrl: "",
    confirmDel: "",
  };
  try {
    const list = await invoke<RemoteInfo[]>("get_remotes", { path: r.path });
    if (remoteModal.value && remoteModal.value.path === r.path) {
      remoteModal.value.list = list;
      remoteModal.value.loading = false;
    }
  } catch (e) {
    if (remoteModal.value && remoteModal.value.path === r.path) {
      remoteModal.value.error = String(e);
      remoteModal.value.loading = false;
    }
  }
}
async function reloadRemotes() {
  const m = remoteModal.value;
  if (!m) return;
  m.loading = true;
  m.opMsg = "";
  try {
    m.list = await invoke<RemoteInfo[]>("get_remotes", { path: m.path });
    m.error = "";
  } catch (e) {
    m.error = String(e);
  } finally {
    m.loading = false;
  }
}
async function openGitConfig() {
  const m = remoteModal.value;
  if (!m || m.busy) return;
  m.busy = true;
  try {
    await invoke("open_git_config", { path: m.path });
    addLog(`📝 已打开 ${m.name} 的 .git/config`);
  } catch (e) {
    m.opMsg = tr("editConfigFail", { e: String(e) });
  } finally {
    m.busy = false;
  }
}
async function addRemoteOp() {
  const m = remoteModal.value;
  if (!m || m.busy || !m.newName.trim() || !m.newUrl.trim()) return;
  m.busy = true;
  try {
    const res = await invoke<OpResult>("add_remote", { path: m.path, name: m.newName.trim(), url: m.newUrl.trim() });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    if (res.ok) {
      m.newName = "";
      m.newUrl = "";
    }
    await reloadRemotes();
  } catch (e) {
    modalOrLog(e);
  } finally {
    m.busy = false;
  }
}
function startRemoteEdit(name: string, url: string) {
  const m = remoteModal.value;
  if (!m) return;
  m.editName = name;
  m.editUrl = url;
}
async function saveRemoteUrlOp() {
  const m = remoteModal.value;
  if (!m || m.busy || m.editName === null) return;
  m.busy = true;
  const name = m.editName;
  try {
    const res = await invoke<OpResult>("set_remote_url", { path: m.path, name, url: m.editUrl });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    if (res.ok) m.editName = null;
    await reloadRemotes();
  } catch (e) {
    modalOrLog(e);
  } finally {
    m.busy = false;
  }
}
async function removeRemoteOp(name: string) {
  const m = remoteModal.value;
  if (!m || m.busy) return;
  if (m.confirmDel !== name) {
    m.confirmDel = name;
    return;
  }
  m.confirmDel = "";
  m.busy = true;
  try {
    const res = await invoke<OpResult>("remove_remote", { path: m.path, name });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    await reloadRemotes();
  } catch (e) {
    modalOrLog(e);
  } finally {
    m.busy = false;
  }
}
// Stash 管理
async function openStash(r: RepoStatus) {
  stashModal.value = {
    path: r.path,
    name: repoDisplayName(r),
    list: [],
    loading: true,
    error: "",
    opMsg: "",
    newLabel: "",
    busy: false,
    confirmDrop: "",
  };
  try {
    const list = await invoke<StashInfo[]>("get_stash_list", { path: r.path });
    if (stashModal.value && stashModal.value.path === r.path) {
      stashModal.value.list = list;
      stashModal.value.loading = false;
    }
  } catch (e) {
    if (stashModal.value && stashModal.value.path === r.path) {
      stashModal.value.error = String(e);
      stashModal.value.loading = false;
    }
  }
}
async function reloadStashes() {
  const m = stashModal.value;
  if (!m) return;
  m.loading = true;
  m.error = "";
  try {
    m.list = await invoke<StashInfo[]>("get_stash_list", { path: m.path });
  } catch (e) {
    m.error = String(e);
  } finally {
    m.loading = false;
  }
}
async function createStashOp() {
  const m = stashModal.value;
  if (!m || m.busy || !m.newLabel.trim()) return;
  m.busy = true;
  m.opMsg = "";
  try {
    const res = await invoke<OpResult>("stash_create", { path: m.path, label: m.newLabel.trim() });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    if (res.ok) m.newLabel = "";
    await reloadStashes();
  } catch (e) {
    const msg = String(e);
    m.opMsg = msg;
    addLog(tr("logStashCreateFail", { e: msg }));
  } finally {
    m.busy = false;
  }
}
async function popStashOp(index: string) {
  const m = stashModal.value;
  if (!m || m.busy) return;
  m.busy = true;
  m.opMsg = "";
  try {
    const res = await invoke<OpResult>("stash_pop_one", { path: m.path, index });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    await reloadStashes();
  } catch (e) {
    const msg = String(e);
    m.opMsg = msg;
    addLog(tr("logStashPopFail", { e: msg }));
  } finally {
    m.busy = false;
  }
}
async function dropStashOp(index: string) {
  const m = stashModal.value;
  if (!m || m.busy) return;
  if (m.confirmDrop !== index) {
    m.confirmDrop = index;
    return;
  }
  m.confirmDrop = "";
  m.busy = true;
  m.opMsg = "";
  try {
    const res = await invoke<OpResult>("stash_drop", { path: m.path, index });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    await reloadStashes();
  } catch (e) {
    const msg = String(e);
    m.opMsg = msg;
    addLog(tr("logStashDropFail", { e: msg }));
  } finally {
    m.busy = false;
  }
}
// 提交历史：查看某次提交的改动
async function logToggleDiff(hash: string) {
  const m = logModal.value;
  if (!m) return;
  if (logDiff.value[hash] !== undefined) {
    const d = { ...logDiff.value };
    delete d[hash];
    logDiff.value = d;
    return;
  }
  logDiffLoading.value = true;
  try {
    const text = await invoke<string>("get_commit_diff", { path: m.path, hash });
    logDiff.value = { ...logDiff.value, [hash]: text };
  } catch (e) {
    logDiff.value = { ...logDiff.value, [hash]: String(e) };
  } finally {
    logDiffLoading.value = false;
  }
}
// 提交历史：Cherry-pick 某次提交到当前分支（二次确认）
async function cherryPickOp(hash: string) {
  const m = logModal.value;
  if (!m) return;
  if (m.confirmCherry !== hash) {
    m.confirmCherry = hash;
    return;
  }
  m.confirmCherry = "";
  try {
    const res = await invoke<OpResult>("cherry_pick", { path: m.path, hash });
    addLog(`${res.ok ? "✅" : "❌"} ${m.name}: ${res.message}`);
    await refreshOne(m.path);
  } catch (e) {
    modalOrLog(e);
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
      // 克隆产生新仓库，仅刷新旧列表不会带出新仓库，需重新扫描
      await scan();
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
  sub: "move" | null;
} | null>(null);
function openCtx(e: MouseEvent, r: RepoStatus) {
  e.preventDefault();
  const x = Math.min(e.clientX, window.innerWidth - 210);
  const y = Math.min(e.clientY, window.innerHeight - 280);
  ctxMenu.value = { x, y, repo: r, confirm: false, sub: null };
}
// 分组节点右键菜单
const groupMenu = ref<{
  x: number;
  y: number;
  path: string;
  hasChildren: boolean;
  confirm: boolean;
} | null>(null);
function openGroupCtx(e: MouseEvent, path: string, hasChildren: boolean) {
  e.preventDefault();
  const x = Math.min(e.clientX, window.innerWidth - 190);
  const y = Math.min(e.clientY, window.innerHeight - 220);
  groupMenu.value = { x, y, path, hasChildren, confirm: false };
  activeGroup.value = path;
}
// 新建子分组：选中当前组并弹出输入框
function gmNewChild() {
  const m = groupMenu.value;
  openNewGroup(m?.path ?? "");
  groupMenu.value = null;
}
function gmRename() {
  const m = groupMenu.value;
  if (m) {
    activeGroup.value = m.path;
    openRename();
  }
  groupMenu.value = null;
}
function gmDelete() {
  const m = groupMenu.value;
  if (!m) return;
  if (!m.confirm) {
    groupMenu.value = { ...m, confirm: true };
    return;
  }
  doRemoveGroup(m.path);
  groupMenu.value = null;
}
function gmToggle() {
  const m = groupMenu.value;
  if (m) toggleCollapse(m.path);
  groupMenu.value = null;
}
function closeCtx() {
  ctxMenu.value = null;
  groupMenu.value = null;
  batchOpen.value = false;
  moreOpen.value = false;
}
async function copyText(txt: string, prefix: string) {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(txt);
    } else {
      const ta = document.createElement("textarea");
      ta.value = txt;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
    }
    addLog(`${prefix}${txt.length > 40 ? txt.slice(0, 40) + "…" : txt}`);
  } catch (e) {
    addLog(tr("logCopyFail", { e: String(e) }));
  }
}
function copyPath(p: string) {
  void copyText(p, t("logPathCopied"));
}
function copyLogLine(l: string) {
  void copyText(l, t("logCopiedMsg"));
}
function clearLog() {
  log.value = [];
  try {
    localStorage.setItem("repopilot-log", JSON.stringify([]));
  } catch {
    /* 忽略 */
  }
}
function ctxAction(
  kind: "pull" | "push" | "finder" | "term" | "web" | "only" | "commit" | "log" | "alias" | "graph" | "branch" | "tag" | "remote" | "stash" | "copypath"
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
  if (kind === "copypath") {
    void copyPath(r.path);
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
  else if (kind === "graph") void openGraph(r);
  else if (kind === "branch") void openBranch(r);
  else if (kind === "tag") void openTag(r);
  else if (kind === "remote") void openRemote(r);
  else if (kind === "stash") void openStash(r);
  else if (kind === "alias") startAliasEdit(r.path);
}

// 操作失败：若对应管理弹窗开着则就地显示，否则只进日志
function modalOrLog(e: unknown) {
  const msg = String(e);
  if (branchModal.value) branchModal.value.opMsg = msg;
  else if (tagModal.value) tagModal.value.opMsg = msg;
  else if (remoteModal.value) remoteModal.value.opMsg = msg;
  else if (stashModal.value) stashModal.value.opMsg = msg;
  addLog(tr("logFail", { e: msg }));
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
async function addRoot() {
  const p = rootInput.value.trim();
  if (!p) return;
  if (roots.value.includes(p)) {
    addLog(tr("logRootExists", { p }));
    rootInput.value = "";
    return;
  }
  // 校验路径存在（~ 展开为家目录）
  let real = p;
  if (p.startsWith("~/")) real = (await invoke<string>("home_dir").catch(() => p)) + p.slice(1);
  const ok = await invoke<boolean>("path_exists", { path: real }).catch(() => false);
  if (!ok) {
    addLog(tr("logRootMissing", { p }));
    return; // 保留输入框内容，方便修改
  }
  roots.value = [...roots.value, p];
  addLog(tr("logRootAdded", { p }));
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
  const t0 = Date.now();
  addLog(tr("logScanStart", { n: roots.value.length }));
  try {
    const all = new Map<string, RepoStatus>();
    // 并行扫描所有根目录，避免串行等待
    const results = await Promise.all(
      roots.value.map(async (r) => {
        const entries: { path: string; parent?: string | null }[] = await invoke("scan_repos", { root: r });
        const parentMap = new Map<string, string | null>();
        for (const e of entries) parentMap.set(e.path, e.parent ?? null);
        const paths = entries.map((e) => e.path);
        if (!paths.length) return new Map<string, RepoStatus>();
        const statuses: RepoStatus[] = await invoke("get_statuses", { paths });
        const m = new Map<string, RepoStatus>();
        for (const s of statuses) {
          s.parent = parentMap.get(s.path) ?? null;
          m.set(s.path, s);
        }
        return m;
      })
    );
    for (const m of results) for (const [k, v] of m) all.set(k, v);
    repos.value = [...all.values()];
    // 仓库集合已变化，旧的改动文件缓存作废
    changesCache.clear();
    // 默认不勾选任何仓库，需要操作时用「全选当前」或手动勾选（安全优先）
    selected.value = new Set();
    // 记住本次目录，下次打开自动恢复
    await saveRoots();
    void updateBadge();
    addLog(tr("logScanDone", { n: repos.value.length, ms: ((Date.now() - t0) / 1000).toFixed(1) }));
  } catch (e) {
    addLog(tr("logScanFail", { e: String(e) }));
  } finally {
    scanning.value = false;
  }
}

function onKeydown(e: KeyboardEvent) {
  // ESC 关闭最上层浮层/弹窗
  if (e.key === "Escape") {
    if (ctxMenu.value || groupMenu.value) closeCtx();
    else if (commitModal.value?.showMsgHist) commitModal.value.showMsgHist = false;
    else if (commitModal.value) commitModal.value = null;
    else if (logModal.value) logModal.value = null;
    else if (graphModal.value) graphModal.value = null;
    else if (branchModal.value) branchModal.value = null;
    else if (tagModal.value) tagModal.value = null;
    else if (remoteModal.value) remoteModal.value = null;
    else if (stashModal.value) stashModal.value = null;
    else if (cloneModal.value) cloneModal.value = null;
    else if (errModal.value) errModal.value = null;
    else if (showAbout.value) showAbout.value = false;
    return;
  }
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
    // 清理历史脏数据：规范化分组名（去首尾/重复斜杠），并把仓库赋值里的
    // 分组合并进 names，保证树能显示它们
    const seen = new Set<string>();
    const cleanNames: string[] = [];
    for (const n of st.names ?? []) {
      const nn = normalizeGroupName(n);
      if (nn && !seen.has(nn)) {
        seen.add(nn);
        cleanNames.push(nn);
      }
    }
    const cleanAssign: Record<string, string> = {};
    for (const k in st.assign ?? {}) {
      const v = normalizeGroupName(st.assign[k]);
      if (!v) continue;
      cleanAssign[k] = v;
      if (!seen.has(v)) {
        seen.add(v);
        cleanNames.push(v);
      }
    }
    groupNames.value = cleanNames;
    groups.value = cleanAssign;
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
    const savedAliases = await invoke<Record<string, string>>("load_aliases");
    aliases.value = savedAliases ?? {};
  } catch {
    /* 无别名配置 */
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
  if (!paths.length) {
    if (!silent) addLog(t("logNoRepos"));
    return;
  }
  if (refreshing.value) return;
  refreshing.value = true;
  try {
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
    if (!silent) addLog(tr("logRefreshed", { n: statuses.length }));
  } catch (e) {
    if (!silent) addLog(tr("logRefreshFail", { e: String(e) }));
  } finally {
    refreshing.value = false;
  }
}

/// 仅刷新单个仓库的状态并就地更新，用于提交等单仓库操作后的快速反馈
/// 返回是否成功，供行内刷新按钮给出日志反馈
async function refreshOne(path: string): Promise<boolean> {
  try {
    const s = await invoke<RepoStatus[]>("get_statuses", { paths: [path] });
    if (!s.length) return false;
    const old = repos.value.find((x) => x.path === path);
    s[0].parent = old?.parent ?? null;
    repos.value = repos.value.map((x) => (x.path === path ? s[0] : x));
    changesCache.delete(path);
    void updateBadge();
    return true;
  } catch {
    return false;
  }
}
// 错误详情弹窗：点击"错误"徽标查看完整错误，可复制/重试
const errModal = ref<{ path: string; name: string; error: string } | null>(null);
function openErr(r: RepoStatus) {
  errModal.value = { path: r.path, name: repoName(r.path), error: r.error ?? "" };
}
async function retryErr() {
  const m = errModal.value;
  if (!m) return;
  addLog(`⟳ 重试 ${m.name} 状态…`);
  const ok = await refreshOne(m.path);
  if (ok) {
    addLog(`✅ ${m.name} 状态已更新`);
    errModal.value = null;
  } else {
    addLog(`❌ ${m.name} 仍失败，请查看详情`);
  }
}
// 行内"刷新此仓库状态"按钮：刷新单个仓库并给出日志反馈
async function refreshOneRow(path: string) {
  const ok = await refreshOne(path);
  const name = repoName(path);
  addLog(ok ? tr("logOneRefreshed", { name }) : tr("logOneRefreshFail", { name }));
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
        // refreshStatus 内部通过 refreshing 防并发重入，这里直接调用即可
        if (repos.value.length) {
          refreshStatus(true).catch(() => {});
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
  // 记录本次批量操作，供“重试失败项”复用
  lastBatch.value = {
    action,
    cmd: action === "cmd" ? customCmd.value.trim() : undefined,
  };
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
  failPaths.value = new Set();
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
    const authTargets: { path: string; name: string; args: string[] }[] = [];
    for (const r of results) {
      if (r.ok) ok++;
      else if (
        (action === "pull" || action === "push") &&
        authRe.test(r.message)
      ) {
        authTargets.push({
          path: r.path,
          name: r.path.split("/").pop() ?? r.path,
          args: action === "pull" ? ["pull", "--no-rebase"] : ["push"],
        });
      }
      addLog(`${r.ok ? "✅" : "❌"} ${r.path}: ${r.message}`);
    }
    applyBatchResults(results);
    addLog(tr("logDone", { label, ok, total: results.length }));
    if (authTargets.length) {
      const repo = repos.value.find((x) => x.path === authTargets[0].path);
      openAuthModal({
        title: tr("authTitleBatch", { label, n: authTargets.length }),
        remoteUrl: repo?.remote_url ?? "",
        targets: authTargets,
        onDone: (okAll) => {
          if (okAll) void refreshStatus(true);
        },
      });
    }
    if (action !== "cmd") await refreshStatus(true);
  } catch (e) {
    modalOrLog(e);
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
  failPaths.value = new Set();
  addLog(tr("logSwitchStart", { b: branch, n: paths.length }));
  try {
    const results: OpResult[] = await invoke("switch_branches", { paths, branch });
    let ok = 0;
    for (const r of results) {
      if (r.ok) ok++;
      addLog(`${r.ok ? "✅" : "❌"} ${r.path}: ${r.message}`);
    }
    applyBatchResults(results);
    addLog(tr("logSwitchDone", { ok, total: results.length }));
    await refreshStatus(true);
  } catch (e) {
    modalOrLog(e);
  } finally {
    busy.value = false;
  }
}

// 弹窗打开时自动聚焦输入框
const vFocus = {
  mounted: (el: HTMLElement) => {
    el.focus();
  },
};

// 单仓库分支下拉：懒加载该仓库的分支列表（缓存，避免 382 个仓库全部拉取）
const branchOpts = ref<Record<string, string[]>>({});
async function loadBranchOpts(path: string) {
  if (branchOpts.value[path]) return;
  try {
    const list = await invoke<string[]>("list_branches", { path });
    branchOpts.value = { ...branchOpts.value, [path]: list };
  } catch {
    /* 拉取失败时下拉只有当前分支 */
  }
}
async function switchRepoBranch(path: string, branch: string) {
  if (!branch) return;
  const cur = repos.value.find((x) => x.path === path)?.branch;
  if (branch === cur) return;
  try {
    const resArr = await invoke<OpResult[]>("switch_branches", { paths: [path], branch });
    const res = resArr[0] as OpResult;
    addLog(`${res.ok ? "✅" : "❌"} ${path.split("/").pop()}: ${res.message}`);
    await refreshOne(path);
    if (res.ok) {
      // 行内短暂高亮反馈
      flashPaths.value = new Set(flashPaths.value).add(path);
      setTimeout(() => {
        const s = new Set(flashPaths.value);
        s.delete(path);
        flashPaths.value = s;
      }, 1600);
    }
  } catch (e) {
    modalOrLog(e);
  }
}

// 批量操作结果汇总：更新汇总条 + 记录失败路径（供定位）
function applyBatchResults(results: OpResult[]): { ok: number; fail: number } {
  let ok = 0;
  for (const r of results) if (r.ok) ok++;
  const fail = results.length - ok;
  batchSummary.value = { ok, fail };
  failPaths.value = new Set(
    fail > 0 ? results.filter((r) => !r.ok).map((r) => r.path) : []
  );
  lastFailResults.value = results.filter((r) => !r.ok);
  return { ok, fail };
}

// 失败详情弹窗：列出失败/跳过仓库及其未提交文件清单，可一键打开提交弹窗
const failModal = ref<{
  visible: boolean;
  loading: boolean;
  items: { path: string; name: string; message: string; changes: ChangeFile[] | null }[];
} | null>(null);
const lastFailResults = ref<OpResult[]>([]);
async function openFailDetail() {
  const paths = [...failPaths.value];
  if (!paths.length) return;
  const msgByPath = new Map(lastFailResults.value.map((r) => [r.path, r.message]));
  failModal.value = { visible: true, loading: true, items: [] };
  const items: { path: string; name: string; message: string; changes: ChangeFile[] | null }[] = [];
  for (const p of paths) {
    const name = p.split("/").pop() ?? p;
    try {
      const changes = await invoke<ChangeFile[]>("list_changes", { path: p });
      items.push({ path: p, name, message: msgByPath.get(p) ?? "", changes });
    } catch (e) {
      items.push({ path: p, name, message: String(e), changes: null });
    }
  }
  if (failModal.value) {
    failModal.value.items = items;
    failModal.value.loading = false;
  }
}
function openCommitForPath(path: string) {
  const repo = repos.value.find((x) => x.path === path);
  if (!repo) return addLog(t("logNoRepo"));
  closeFailDetail();
  void openCommit(repo);
}
function closeFailDetail() {
  if (failModal.value) failModal.value.visible = false;
}

// 多路径短暂高亮（复用行内分支切换的高亮机制）
function flashPathsTemporarily(paths: string[], ms = 2200) {
  const s = new Set(flashPaths.value);
  for (const p of paths) s.add(p);
  flashPaths.value = s;
  setTimeout(() => {
    const ns = new Set(flashPaths.value);
    for (const p of paths) ns.delete(p);
    flashPaths.value = ns;
  }, ms);
}

// 点击失败汇总条：高亮并滚动到第一个失败仓库
function locateFailRepos() {
  const paths = [...failPaths.value];
  if (!paths.length) return;
  flashPathsTemporarily(paths);
  const el = document.querySelector<HTMLElement>(
    `tr[data-path="${CSS.escape(paths[0])}"]`
  );
  el?.scrollIntoView({ behavior: "smooth", block: "center" });
}

// 请求取消当前批量操作：Rust 侧置取消标志，未开始的仓库直接跳过
async function cancelBatch() {
  if (cancelling.value) return;
  cancelling.value = true;
  try {
    await invoke("cancel_batch");
    addLog(t("logCancel"));
  } catch {
    /* 命令通道失败时忽略，批量操作本身会结束 */
  }
}
watch(busy, (b) => {
  if (!b) cancelling.value = false;
});

// 重试失败项：重新勾选上次失败的仓库，执行同一种批量操作
function retryFailed() {
  const lb = lastBatch.value;
  if (!lb || !failPaths.value.size) return;
  selected.value = new Set(failPaths.value);
  if (lb.action === "cmd" && lb.cmd) customCmd.value = lb.cmd;
  void runOnRepos(lb.action);
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
  failPaths.value = new Set();
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
    applyBatchResults(results);
    addLog(tr("logReplaceDone", { ok, total: results.length }));
    await refreshStatus(true);
  } catch (e) {
    modalOrLog(e);
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
            <button class="ghost" @click="refreshStatus(false)" :disabled="!repos.length || refreshing" :title="t('titleRefresh')">
              {{ refreshing ? t("refreshing") : t("refresh") }}
            </button>
          </div>
          <div class="tool-group">
            <button class="ghost" :class="{ on: batchOpen }" @click.stop="batchOpen = !batchOpen; moreOpen = false" :title="t('batchBtnTip')">
              {{ t("batchBtn") }} ▾
            </button>
            <button class="ghost" :class="{ on: moreOpen }" @click.stop="moreOpen = !moreOpen; batchOpen = false" title="More">
              ⋯
            </button>
          </div>
        </div>
        <div v-if="batchOpen" class="drop-panel" @click.stop>
          <div class="tab-row">
            <button
              v-for="tb in [['cmd', 'batchTabCmd'], ['switch', 'batchTabSwitch'], ['replace', 'batchTabReplace']] as [string, string][]"
              :key="tb[0]"
              :class="{ on: batchTab === tb[0] }"
              @click="batchTab = tb[0] as 'cmd' | 'switch' | 'replace'"
            >{{ t(tb[1]) }}</button>
          </div>
          <div class="batch-status">
            <span v-if="selected.size" class="ok">{{ tr("batchSelected", { n: selected.size }) }}</span>
            <span v-else class="warn">{{ t("batchNeedSelect") }}</span>
          </div>
          <div v-if="batchTab === 'cmd'" class="drop-body">
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
          <div v-else-if="batchTab === 'switch'" class="drop-body">
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
          <div v-else class="drop-body">
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
        </div>
        <div v-if="moreOpen" class="drop-panel more-panel" @click.stop>
          <div class="more-row">
            <button class="ghost" @click="lang = lang === 'zh' ? 'en' : 'zh'">
              {{ lang === "zh" ? "EN" : "中文" }}
            </button>
            <button class="ghost" @click="dark = !dark">{{ dark ? t("light") : t("dark") }}</button>
            <button class="ghost" @click="checkUpdate(true)">🔄 {{ t("updateCheck") }}</button>
            <button class="ghost" @click="exportConfig">{{ t("exportCfg") }}</button>
            <button class="ghost" @click="importConfig">{{ pendingImport ? t("importConfirmShort") : t("importCfg") }}</button>
            <button class="ghost" @click="showAbout = true">ⓘ {{ t("aboutTitle") }}</button>
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
      <div class="sidebar" :class="{ collapsed: sidebarCollapsed }">
        <div class="tree-title">
          <template v-if="!sidebarCollapsed">
            <span>{{ t("groups") }}</span>
            <span class="tt-btns">
              <button class="sb-toggle" :title="t('newGroup')" @click="openNewGroup()">＋</button>
              <button class="sb-toggle" :title="t('sbCollapse')" @click="sidebarCollapsed = true">◀</button>
            </span>
          </template>
        </div>
        <template v-if="!sidebarCollapsed">
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
            @contextmenu.prevent="openGroupCtx($event, node.path, hasChildren(node.path))"
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
              :placeholder="newGroupParent ? t('subGroupPlaceholder') : t('groupPlaceholder')"
              @keyup.enter="confirmNewGroup"
              @keyup.esc="showNewGroup = false"
            />
            <button @click="confirmNewGroup">{{ t("ok") }}</button>
            <button class="ghost" @click="showNewGroup = false">{{ t("cancel") }}</button>
          </div>
        </div>
        </template>
      </div>

      <div class="content">
      <div class="toolbar">
        <button v-if="sidebarCollapsed" class="sb-open" :title="t('sbExpand')" @click="sidebarCollapsed = false">☰</button>
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
        <span v-else-if="batchSummary" class="busy-tip" :class="{ bad: batchSummary.fail > 0, locate: batchSummary.fail > 0 }" :title="batchSummary.fail > 0 ? t('batchLocateTip') : ''" @click="batchSummary.fail > 0 ? openFailDetail() : undefined">
          {{ tr("batchDone", { ok: batchSummary.ok, fail: batchSummary.fail }) }}
        </span>
        <span v-if="updateInfo" class="busy-tip upd" @click="installUpdate">
          {{ updateInfo.installing ? t("updateInstalling") : tr("updateAvailable", { v: updateInfo.version }) }}
        </span>
        <button
          v-if="busy && progress"
          class="ghost mini danger"
          :disabled="cancelling"
          :title="t('cancelTip')"
          @click="cancelBatch"
        >
          {{ cancelling ? t("cancelling") : t("cancel") }}
        </button>
        <button
          v-if="!busy && batchSummary && batchSummary.fail > 0 && lastBatch"
          class="ghost mini"
          :title="t('retryTip')"
          @click="retryFailed"
        >
          ↻ {{ t("retryFail") }}
        </button>
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
              :data-path="row.r.path"
              :class="{ off: !selected.has(row.r.path), follow: !!row.follow, flash: flashPaths.has(row.r.path) }"
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
                  <template v-if="editingAlias === row.r.path">
                    <input
                      ref="aliasInput"
                      v-model="aliasDraft"
                      class="alias-input"
                      :placeholder="t('aliasPlaceholder')"
                      @keyup.enter="saveAlias"
                      @keyup.esc="editingAlias = ''"
                      @blur="saveAlias"
                    />
                  </template>
                  <span
                    v-else
                    class="tree-name"
                    :title="[row.follow ? t('followHint') : row.container ? t('containerHint') : '', row.r.path].filter(Boolean).join(' · ')"
                    @dblclick="startAliasEdit(row.r.path)"
                  >{{ repoDisplayName(row.r) }}</span>
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
                  :title="row.container ? t('containerHint') : ''"
                  @change="setRepoGroup(row.r.path, ($event.target as HTMLSelectElement).value)"
                >
                  <option value="">{{ t("ungrouped") }}</option>
                  <option v-for="node in groupNodes" :key="node.path" :value="node.path">
                    {{ "　".repeat(node.depth) }}{{ node.path }}
                  </option>
                </select>
              </td>
              <td>
                <select
                  v-if="row.r.branch"
                  class="branch-select"
                  :value="row.r.branch"
                  :title="t('branchSwitchHint')"
                  @mouseenter="loadBranchOpts(row.r.path)"
                  @click="loadBranchOpts(row.r.path)"
                  @change="switchRepoBranch(row.r.path, ($event.target as HTMLSelectElement).value)"
                >
                  <option :value="row.r.branch">{{ row.r.branch }}</option>
                  <template v-for="b in branchOpts[row.r.path] || []" :key="b">
                    <option v-if="b !== row.r.branch" :value="b">{{ b }}</option>
                  </template>
                </select>
                <span v-else>—</span>
              </td>
              <td>
                <span
                  v-if="row.r.error"
                  class="badge err tipable"
                  :title="t('errClickTip')"
                  @mouseenter="showErrTip($event, row.r)"
                  @mouseleave="hideChangesDelayed"
                  @click.stop="openErr(row.r)"
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
                <button class="mini ref-one" :title="t('ctxRefresh')" @click="refreshOneRow(row.r.path)">⟳</button>
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
      <div class="panel log">
        <div class="log-head">
          <h3>{{ t("logTitle") }}</h3>
          <button v-if="log.length" class="mini" :title="t('logClearTip')" @click="clearLog">{{ t("logClear") }}</button>
        </div>
        <ul v-if="log.length">
          <li v-for="(l, i) in log" :key="i" :title="t('logCopyTip')" @click="copyLogLine(l)">{{ l }}</li>
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
      <div class="ctx-title">{{ repoName(ctxMenu.repo.path) }}</div>
      <button @click="ctxAction('pull')">{{ t("ctxPull") }}</button>
      <button @click="ctxAction('push')">
        {{ ctxMenu.confirm ? t("confirmPush") : t("ctxPush") }}
      </button>
      <button @click="ctxAction('only')">{{ t("ctxOnly") }}</button>
      <button
        @mouseenter="ctxMenu.sub = 'move'"
        @click="ctxMenu.sub = ctxMenu.sub === 'move' ? null : 'move'"
      >{{ t("ctxMoveGroup") }} ▸</button>
      <div v-if="ctxMenu.sub === 'move'" class="ctx-sub" @mouseleave="ctxMenu.sub = null">
        <button v-for="n in groupNodes" :key="n.path" @click="moveSelectedToGroup(n.path)">
          {{ "　".repeat(n.depth) }}{{ n.path }}
        </button>
        <button @click="moveSelectedToGroup('')">{{ t("ungrouped") }}</button>
      </div>
      <div class="ctx-sep"></div>
      <button @click="ctxAction('commit')">{{ t("ctxCommit") }}</button>
      <button @click="ctxAction('log')">{{ t("ctxLog") }}</button>
      <button @click="ctxAction('graph')">{{ t("ctxGraph") }}</button>
      <button @click="ctxAction('branch')">{{ t("ctxBranch") }}</button>
      <button @click="ctxAction('tag')">{{ t("ctxTag") }}</button>
      <button @click="ctxAction('remote')">{{ t("ctxRemote") }}</button>
      <button @click="ctxAction('stash')">{{ t("ctxStash") }}</button>
      <button @click="ctxAction('copypath')">{{ t("ctxCopyPath") }}</button>
      <button @click="ctxAction('alias')">{{ t("ctxAlias") }}</button>
      <div class="ctx-sep"></div>
      <button @click="ctxAction('finder')">{{ t("ctxFinder") }}</button>
      <button @click="ctxAction('term')">{{ t("ctxTerm") }}</button>
      <button @click="ctxAction('web')">{{ t("ctxWeb") }}</button>
    </div>

    <!-- 分组节点右键菜单 -->
    <div
      v-if="groupMenu"
      class="ctx-menu"
      :style="{ left: groupMenu.x + 'px', top: groupMenu.y + 'px' }"
      @click.stop
    >
      <div class="ctx-title">{{ groupMenu.path }}</div>
      <button @click="gmNewChild">＋ {{ t("newSubGroup") }}</button>
      <button @click="gmRename">{{ t("renameGroup") }}</button>
      <button @click="gmDelete">
        {{ groupMenu.confirm ? t("confirmDelete") : t("deleteGroup") }}
      </button>
      <button v-if="groupMenu.hasChildren" @click="gmToggle">
        {{ collapsed.has(groupMenu.path) ? t("expand") : t("collapse") }}
      </button>
    </div>

    <div v-if="cloneModal" class="modal-mask" @click.self="cloneModal = null">
      <div class="modal" :class="{ max: isMax('clone') }">
        <button
          class="max-btn"
          :title="isMax('clone') ? t('restoreWin') : t('maxWin')"
          @click="toggleMax('clone')"
        >{{ isMax('clone') ? "⤢" : "⛶" }}</button>
        <button class="max-btn close-x" :title="t('closeWin')" @click="closeModal('clone')">✕</button>
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
      <div class="modal log-modal" :class="{ max: isMax('log') }">
        <button
          class="max-btn"
          :title="isMax('log') ? t('restoreWin') : t('maxWin')"
          @click="toggleMax('log')"
        >{{ isMax('log') ? "⤢" : "⛶" }}</button>
        <button class="max-btn close-x" :title="t('closeWin')" @click="closeModal('log')">✕</button>
        <h2>{{ tr("logHistTitle", { name: logModal.name }) }}</h2>
        <div v-if="logModal.loading" class="hint">{{ t("logHistLoading") }}</div>
        <template v-else>
          <input
            v-if="logModal.list.length"
            v-model="logModal.search"
            class="log-search"
            :placeholder="t('logSearchPlaceholder')"
          />
          <ul v-if="filteredLogList.length" class="log-list">
            <template v-for="(c, i) in filteredLogList" :key="i">
              <li>
                <span class="lh">{{ c.hash }}</span>
                <span class="ls" :title="c.subject">{{ c.subject }}</span>
                <button
                  class="mini"
                  :disabled="logDiffLoading && logDiff[c.hash] === undefined"
                  :title="t('diffHint')"
                  @click="logToggleDiff(c.hash)"
                >{{ logDiff[c.hash] !== undefined ? t("logDiffHide") : t("logDiffBtn") }}</button>
                <button
                  class="mini"
                  :class="{ danger: logModal.confirmCherry === c.hash }"
                  :title="t('logCherryTip')"
                  @click="cherryPickOp(c.hash)"
                >{{ logModal.confirmCherry === c.hash ? t("logConfirmCherry") : t("logCherry") }}</button>
                <span class="la">{{ c.author }} · {{ relTime(c.time) }}</span>
              </li>
              <li v-if="logDiff[c.hash] !== undefined" class="log-diff">
                <pre>{{ logDiff[c.hash] }}</pre>
              </li>
            </template>
          </ul>
          <p v-else-if="logModal.list.length" class="hint">{{ t("logSearchEmpty") }}</p>
          <p v-else class="hint">{{ t("logHistEmpty") }}</p>
        </template>
        <div class="modal-actions">
          <button class="ghost" @click="logModal = null">{{ t("close") }}</button>
        </div>
      </div>
    </div>

    <div v-if="graphModal" class="modal-mask" @click.self="graphModal = null">
      <div class="modal graph-modal" :class="{ max: isMax('graph') }">
        <button
          class="max-btn"
          :title="isMax('graph') ? t('restoreWin') : t('maxWin')"
          @click="toggleMax('graph')"
        >{{ isMax('graph') ? "⤢" : "⛶" }}</button>
        <button class="max-btn close-x" :title="t('closeWin')" @click="closeModal('graph')">✕</button>
        <h2>{{ tr("graphTitle", { name: graphModal.name }) }}</h2>
        <p v-if="graphModal.loading" class="hint">{{ t("graphLoading") }}</p>
        <p v-else-if="graphModal.error" class="hint err-text">{{ graphModal.error }}</p>
        <template v-else-if="graphView && graphModal.list.length">
          <p class="hint">{{ tr("graphCount", { n: graphModal.list.length, refs: graphRefsCount }) }}</p>
          <div class="graph-scroll">
            <div style="display:flex; min-width:max-content;">
              <svg :width="graphView.laneW" :height="graphView.svgHeight" style="flex:0 0 auto; display:block;">
                <line
                  v-for="(e, ei) in graphView.edges"
                  :key="ei"
                  :x1="e.x1" :y1="e.y1" :x2="e.x2" :y2="e.y2"
                  :stroke="e.color" stroke-width="1.5"
                />
                <circle
                  v-for="n in graphView.nodes"
                  :key="n.hash"
                  :cx="n.x" :cy="n.y" r="4.5"
                  :fill="laneColor(n.lane)" stroke="#fff" stroke-width="1"
                />
              </svg>
              <div style="flex:1 1 auto; min-width:0;">
                <div class="graph-rows" :style="{ marginTop: graphView.PAD_TOP + 'px' }">
                  <div
                    v-for="n in graphView.nodes"
                    :key="n.hash"
                    class="graph-row"
                    :style="{ height: graphView.ROW_H + 'px' }"
                    :title="graphRowTitle(n)"
                  >
                    <span class="g-refs">
                      <span
                        v-for="(r, ri) in n.refs"
                        :key="ri"
                        class="g-ref"
                        :class="{ head: r === graphModal.currentBranch }"
                      >{{ r }}</span>
                    </span>
                    <span class="g-hash">{{ n.short }}</span>
                    <span class="g-subj">{{ n.subject }}</span>
                    <span class="g-meta">{{ n.author }} · {{ relTime(n.time) }}</span>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </template>
        <p v-else class="hint">{{ t("graphEmpty") }}</p>
        <div class="modal-actions">
          <button class="ghost" @click="graphModal = null">{{ t("close") }}</button>
        </div>
      </div>
    </div>

    <div v-if="branchModal" class="modal-mask" @click.self="branchModal = null">
      <div class="modal branch-modal" :class="{ max: isMax('branch') }">
        <button
          class="max-btn"
          :title="isMax('branch') ? t('restoreWin') : t('maxWin')"
          @click="toggleMax('branch')"
        >{{ isMax('branch') ? "⤢" : "⛶" }}</button>
        <button class="max-btn close-x" :title="t('closeWin')" @click="closeModal('branch')">✕</button>
        <h2>{{ tr("branchTitle", { name: branchModal.name }) }}</h2>
        <p v-if="branchModal.loading" class="hint">{{ t("graphLoading") }}</p>
        <p v-else-if="branchModal.error" class="hint err-text">{{ branchModal.error }}</p>
        <template v-else>
          <div class="branch-new">
            <input
              v-model="branchModal.newName"
              v-focus
              :placeholder="t('branchNewPlaceholder')"
              @keyup.enter="createBranchOp"
            />
            <button
              class="primary"
              :disabled="branchModal.busy || !branchModal.newName.trim()"
              @click="createBranchOp"
            >{{ t("branchNewBtn") }}</button>
          </div>
          <p v-if="branchModal.opMsg" class="hint err-text">{{ branchModal.opMsg }}</p>
          <ul class="branch-list">
            <li v-for="b in branchModal.branches" :key="b.name">
              <span class="b-name" :class="{ cur: b.is_current }">
                <span class="b-dot"></span>{{ b.name }}
                <span class="b-tag" :class="{ cur: b.is_current, local: b.is_local && !b.is_current, remote: b.is_remote }">
                  {{ b.is_current ? t("branchCurrent") : b.is_local ? t("branchLocal") : t("branchRemote") }}
                </span>
              </span>
              <span class="b-ops">
                <button
                  class="mini"
                  :disabled="branchModal.busy || b.is_current"
                  :title="t('branchSwitchTip')"
                  @click="switchBranchOp(b.name)"
                >{{ t("branchSwitch") }}</button>
                <button
                  class="mini"
                  :disabled="branchModal.busy || b.is_current || b.is_remote"
                  :title="t('branchMergeTip')"
                  @click="mergeBranchOp(b.name)"
                >{{ t("branchMerge") }}</button>
                <button
                  class="mini"
                  :class="{ danger: branchModal.confirmDel === b.name }"
                  :disabled="branchModal.busy || b.is_current || b.is_remote"
                  :title="t('branchDelTip')"
                  @click="deleteBranchOp(b.name)"
                >{{ branchModal.confirmDel === b.name ? t("branchConfirmDel") : t("branchDel") }}</button>
              </span>
            </li>
            <p v-if="!branchModal.branches.length" class="hint">{{ t("graphEmpty") }}</p>
          </ul>
        </template>
        <div class="modal-actions">
          <button class="ghost" @click="branchModal = null">{{ t("close") }}</button>
        </div>
      </div>
    </div>

    <div v-if="tagModal" class="modal-mask" @click.self="tagModal = null">
      <div class="modal branch-modal" :class="{ max: isMax('tag') }">
        <button
          class="max-btn"
          :title="isMax('tag') ? t('restoreWin') : t('maxWin')"
          @click="toggleMax('tag')"
        >{{ isMax('tag') ? "⤢" : "⛶" }}</button>
        <button class="max-btn close-x" :title="t('closeWin')" @click="closeModal('tag')">✕</button>
        <h2>{{ tr("tagTitle", { name: tagModal.name }) }}</h2>
        <p v-if="tagModal.loading" class="hint">{{ t("graphLoading") }}</p>
        <p v-else-if="tagModal.error" class="hint err-text">{{ tagModal.error }}</p>
        <template v-else>
          <div class="branch-new">
            <input
              v-model="tagModal.newName"
              v-focus
              :placeholder="t('tagNewPlaceholder')"
              @keyup.enter="createTagOp"
            />
            <button
              class="primary"
              :disabled="tagModal.busy || !tagModal.newName.trim()"
              @click="createTagOp"
            >{{ t("tagNewBtn") }}</button>
          </div>
          <p v-if="tagModal.opMsg" class="hint err-text">{{ tagModal.opMsg }}</p>
          <ul class="branch-list">
            <li v-for="tg in tagModal.list" :key="tg">
              <span class="b-name">
                <span class="b-dot"></span>{{ tg }}
              </span>
              <span class="b-ops">
                <button
                  class="mini"
                  :disabled="tagModal.busy"
                  :title="t('tagPushTip')"
                  @click="pushTagOp(tg)"
                >{{ t("tagPush") }}</button>
                <button
                  class="mini"
                  :class="{ danger: tagModal.confirmDel === tg }"
                  :disabled="tagModal.busy"
                  :title="t('tagDelTip')"
                  @click="deleteTagOp(tg)"
                >{{ tagModal.confirmDel === tg ? t("tagConfirmDel") : t("tagDel") }}</button>
              </span>
            </li>
            <p v-if="!tagModal.list.length" class="hint">{{ t("tagEmpty") }}</p>
          </ul>
        </template>
        <div class="modal-actions">
          <button class="ghost" @click="tagModal = null">{{ t("close") }}</button>
        </div>
      </div>
    </div>

    <div v-if="remoteModal" class="modal-mask" @click.self="remoteModal = null">
      <div class="modal branch-modal" :class="{ max: isMax('remote') }">
        <button
          class="max-btn"
          :title="isMax('remote') ? t('restoreWin') : t('maxWin')"
          @click="toggleMax('remote')"
        >{{ isMax('remote') ? "⤢" : "⛶" }}</button>
        <button class="max-btn close-x" :title="t('closeWin')" @click="closeModal('remote')">✕</button>
        <h2>{{ tr("remoteTitle", { name: remoteModal.name }) }}</h2>
        <p v-if="remoteModal.loading" class="hint">{{ t("graphLoading") }}</p>
        <p v-else-if="remoteModal.error" class="hint err-text">{{ remoteModal.error }}</p>
        <template v-else>
          <div class="branch-new">
            <input
              v-model="remoteModal.newName"
              v-focus
              class="rm-name"
              :placeholder="t('remoteNewName')"
            />
            <input
              v-model="remoteModal.newUrl"
              class="rm-url"
              :placeholder="t('remoteNewUrl')"
              @keyup.enter="addRemoteOp"
            />
            <button
              class="primary"
              :disabled="remoteModal.busy || !remoteModal.newName.trim() || !remoteModal.newUrl.trim()"
              @click="addRemoteOp"
            >{{ t("remoteAdd") }}</button>
          </div>
          <p v-if="remoteModal.opMsg" class="hint err-text">{{ remoteModal.opMsg }}</p>
          <ul class="branch-list rm-list">
            <li v-for="rm in remoteModal.list" :key="rm.name">
              <div class="rm-head">
                <span class="b-dot"></span>
                <span class="b-name">{{ rm.name }}</span>
                <span class="b-ops">
                  <template v-if="remoteModal.editName === rm.name">
                    <button class="mini" :disabled="remoteModal.busy" @click="saveRemoteUrlOp">{{ t("remoteSave") }}</button>
                    <button class="mini" @click="remoteModal.editName = null">{{ t("cancel") }}</button>
                  </template>
                  <template v-else>
                    <button class="mini" :disabled="remoteModal.busy" :title="t('remoteEditTip')" @click="startRemoteEdit(rm.name, rm.url)">{{ t("remoteEdit") }}</button>
                    <button
                      class="mini"
                      :class="{ danger: remoteModal.confirmDel === rm.name }"
                      :disabled="remoteModal.busy"
                      :title="t('remoteDelTip')"
                      @click="removeRemoteOp(rm.name)"
                    >{{ remoteModal.confirmDel === rm.name ? t("remoteConfirmDel") : t("remoteDel") }}</button>
                  </template>
                </span>
              </div>
              <div v-if="remoteModal.editName === rm.name" class="rm-edit">
                <input
                  v-model="remoteModal.editUrl"
                  class="rm-url"
                  :placeholder="t('remoteNewUrl')"
                  @keyup.enter="saveRemoteUrlOp"
                />
              </div>
              <div v-else class="rm-url-full" :title="rm.url">{{ rm.url }}</div>
            </li>
            <p v-if="!remoteModal.list.length" class="hint">{{ t("remoteEmpty") }}</p>
          </ul>
        </template>
        <div class="modal-actions">
          <button
            class="ghost"
            :disabled="remoteModal.busy"
            :title="t('editConfigTip')"
            @click="openGitConfig"
          >{{ t("editConfigFile") }}</button>
          <span class="flex-spacer"></span>
          <button class="ghost" @click="remoteModal = null">{{ t("close") }}</button>
        </div>
      </div>
    </div>

    <div v-if="stashModal" class="modal-mask" @click.self="stashModal = null">
      <div class="modal branch-modal" :class="{ max: isMax('stash') }">
        <button
          class="max-btn"
          :title="isMax('stash') ? t('restoreWin') : t('maxWin')"
          @click="toggleMax('stash')"
        >{{ isMax('stash') ? "⤢" : "⛶" }}</button>
        <button class="max-btn close-x" :title="t('closeWin')" @click="closeModal('stash')">✕</button>
        <h2>{{ tr("stashTitle", { name: stashModal.name }) }}</h2>
        <p v-if="stashModal.loading" class="hint">{{ t("graphLoading") }}</p>
        <p v-else-if="stashModal.error" class="hint err-text">{{ stashModal.error }}</p>
        <template v-else>
          <div class="branch-new">
            <input
              v-model="stashModal.newLabel"
              v-focus
              :placeholder="t('stashNewPlaceholder')"
              @keyup.enter="createStashOp"
            />
            <button
              class="primary"
              :disabled="stashModal.busy || !stashModal.newLabel.trim()"
              :title="t('stashNewTip')"
              @click="createStashOp"
            >{{ t("stashNewBtn") }}</button>
          </div>
          <p v-if="stashModal.opMsg" class="hint err-text">{{ stashModal.opMsg }}</p>
          <ul class="branch-list" v-if="stashModal.list.length">
            <li v-for="s in stashModal.list" :key="s.index">
              <span class="b-name">
                <span class="b-dot"></span><span class="stash-idx">{{ s.index }}</span>
                <span class="stash-subj" :title="s.subject">{{ s.subject }}</span>
              </span>
              <span class="b-ops">
                <button class="mini" :disabled="stashModal.busy" :title="t('stashPopTip')" @click="popStashOp(s.index)">{{ t("stashPopBtn") }}</button>
                <button
                  class="mini"
                  :class="{ danger: stashModal.confirmDrop === s.index }"
                  :disabled="stashModal.busy"
                  :title="t('stashDropTip')"
                  @click="dropStashOp(s.index)"
                >{{ stashModal.confirmDrop === s.index ? t("stashConfirmDrop") : t("stashDropBtn") }}</button>
              </span>
            </li>
          </ul>
          <p v-else class="hint">{{ t("stashEmpty") }}</p>
        </template>
        <div class="modal-actions">
          <button class="ghost" @click="stashModal = null">{{ t("close") }}</button>
        </div>
      </div>
    </div>

    <div v-if="changesTip" class="changes-tip" :style="{ left: changesTip.x + 'px', top: changesTip.y + 'px' }" @mouseenter="keepChanges" @mouseleave="hideChangesDelayed">
      <div class="ct-title">{{ repoName(changesTip.path) }}<template v-if="changesTip.err !== undefined"> · {{ t("bErr") }}</template><template v-else> · {{ changesTip.files.length }}</template></div>
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
      <div class="modal commit-modal" :class="{ max: isMax('commit') }">
        <button
          class="max-btn"
          :title="isMax('commit') ? t('restoreWin') : t('maxWin')"
          @click="toggleMax('commit')"
        >{{ isMax('commit') ? "⤢" : "⛶" }}</button>
        <button class="max-btn close-x" :title="t('closeWin')" @click="closeModal('commit')">✕</button>
        <h2>{{ tr("commitTitle", { name: commitModal.repo.path.split("/").pop() ?? "" }) }}</h2>
        <div class="commit-top">
          <span class="ct-side">
            <button class="ghost" @click="toggleAllCommitFiles">
              {{ allCommitSelected() ? t("commitDeselectAll") : t("commitSelectAll") }}
            </button>
            <span class="cnt">{{ commitModal.selected.size }} / {{ commitModal.changes.length }}</span>
          </span>
          <span class="ct-side">
            <button
              class="ghost mini"
              :disabled="commitRefreshing"
              :title="t('refreshFilesTip')"
              @click="refreshCommitFiles"
            >{{ commitRefreshing ? t("refreshing") : "⟳ " + t("refreshFiles") }}</button>
            <button
              class="ghost mini"
              :disabled="commitModal.conflictState === 'checking'"
              :title="t('conflictRefreshTip')"
              @click="checkRemoteConflicts()"
            >⟳ {{ t("conflictRefresh") }}</button>
            <button
              class="ghost mini danger"
              :disabled="commitModal.busy || commitRefreshing"
              :title="t('discardSelectedTip')"
              @click="discardSelectedFiles"
            >{{ commitDiscardAll ? t("discardAllConfirm") : tr("discardSelected", { n: discardTargetCount }) }}</button>
          </span>
        </div>
        <div class="commit-body">
          <!-- 左：文件列表（宽度可拖动调整；方向键 ↑/↓ 移动选中；内嵌搜索过滤） -->
          <div
            ref="commitFilesRef"
            class="commit-files"
            :style="{ width: commitSplitW * 100 + '%' }"
            tabindex="0"
            :title="t('keyboardNavTip')"
            @keydown="onCommitFilesKeydown"
          >
            <input
              v-model="commitFileQuery"
              class="commit-search"
              type="text"
              :placeholder="t('commitSearchPlaceholder')"
              @keydown.stop
            />
            <div class="conflict-row">
              <div
                v-if="commitModal.conflictState === 'checking'"
                class="conflict-line"
              >{{ t("conflictChecking") }}</div>
              <div
                v-else-if="commitModal.conflictState === 'error'"
                class="conflict-line warn"
                :title="commitModal.conflictMsg"
              >{{ t("conflictFail") }}</div>
              <div
                v-else-if="commitModal.conflictState === 'auth'"
                class="conflict-line"
                :title="commitModal.conflictMsg"
              >{{ t("conflictAuth") }}</div>
              <div
                v-else-if="commitConflictCount > 0"
                class="conflict-line warn"
              >{{ tr("conflictSummary", { n: commitConflictCount }) }}</div>
              <div v-else class="conflict-line ok">{{ t("conflictOk") }}</div>
            </div>
            <div class="commit-files-list">
              <template v-for="sec in filteredCommitSections" :key="sec.key">
                <div class="cf-title">{{ sec.title }} · {{ sec.items.length }}</div>
                <div
                  v-for="it in sec.items"
                  :key="it.i"
                  class="commit-file"
                  :class="{ sel: activeDiff === it.i }"
                  :title="t('diffHint')"
                  @click="selectDiff(it.i)"
                >
                  <input
                    type="checkbox"
                    :checked="commitModal.selected.has(it.i)"
                    @click.stop
                    @change="toggleCommitFile(it.i)"
                  />
                  <span class="cst" :class="{ unt: it.c.status.trim() === '??' }">{{ statusLabel(it.c.status) }}</span>
                  <span
                    v-if="commitConflictSet.has(it.c.path)"
                    class="cc-badge"
                    :title="t('conflictTip')"
                  >{{ t("conflictBadge") }}</span>
                  <span class="cp" :title="it.c.path">{{ it.c.path }}</span>
                  <span class="commit-file-ops">
                    <button
                      v-if="isStaged(it.c.status)"
                      class="mini"
                      :disabled="commitModal.busy"
                      :title="t('unstageTip')"
                      @click.stop="unstageFileOp(it.i)"
                    >{{ t("unstageBtn") }}</button>
                    <button
                      v-if="hasUnstaged(it.c.status)"
                      class="mini"
                      :class="{ danger: commitModal.confirmDiscard === it.c.path }"
                      :disabled="commitModal.busy"
                      :title="t('discardTip')"
                      @click.stop="discardFileOp(it.i)"
                    >{{ commitModal.confirmDiscard === it.c.path ? t("discardConfirm") : t("discardBtn") }}</button>
                  </span>
                </div>
              </template>
              <p v-if="!commitModal.changes.length" class="hint">{{ t("commitNoChanges") }}</p>
              <p v-else-if="!filteredCommitSections.length && commitFileQuery.trim()" class="hint">{{ t("commitSearchNoMatch") }}</p>
            </div>
          </div>
          <div class="commit-splitter" :title="t('splitterTip')" @mousedown="startCommitSplit"></div>
          <!-- 右：diff 预览（按 Hunk 渲染，支持块级暂存） -->
          <div class="commit-diff-pane">
            <template v-if="activeHunks && !activeHunks.err">
              <div class="commit-diff">
                <!-- 有 hunk：文件头 + 每个块的工具条与内容 -->
                <template v-if="activeHunks.hunks.length">
                  <span v-for="(line, li) in activeHunks.header" :key="'h' + li" class="dl" :class="lineClass(line)">{{ line }}</span>
                  <template v-for="(hk, hi) in activeHunks.hunks" :key="'k' + hi">
                    <div class="hunk-bar">
                      <span class="hunk-tag">{{ t("hunkTag") }} {{ hi + 1 }}</span>
                      <button
                        class="mini"
                        :title="t('hunkStageTip')"
                        @click="stageHunkOp(hk.patch)"
                      >{{ t("hunkStageBtn") }}</button>
                    </div>
                    <span v-for="(line, li) in hk.lines" :key="li" class="dl" :class="lineClass(line)">{{ line }}</span>
                  </template>
                </template>
                <!-- 无 hunk：未跟踪/部分暂存文件只读查看 -->
                <template v-else-if="activeHunks.content">
                  <span
                    v-for="(l, li) in diffLines(activeHunks.content)"
                    :key="li"
                    class="dl"
                    :class="l.cls"
                  >{{ l.text }}</span>
                </template>
                <p v-else-if="activeHunks.hunks.length === 0" class="hint">{{ t("diffNoChanges") }}</p>
              </div>
              <p v-if="activeHunks.partial" class="hint warn">{{ t("hunkPartial") }}</p>
              <p v-else-if="activeHunks.untracked" class="hint warn">{{ t("hunkUntracked") }}</p>
            </template>
            <p v-else-if="activeHunks && activeHunks.err" class="hint err-text">{{ activeHunks.err }}</p>
            <p v-else class="hint">{{ activeDiff < 0 ? t("diffSelectHint") : t("diffLoading") }}</p>
          </div>
        </div>
        <p class="hint">{{ t("commitSelectHint") }}</p>
        <div class="commit-msg-hist">
          <button
            v-if="commitMsgHistory.length"
            class="mini"
            :title="t('msgHistTip')"
            @click="commitModal.showMsgHist = !commitModal.showMsgHist"
          >{{ t("msgHistBtn") }} ▾</button>
          <div v-if="commitModal.showMsgHist" class="msg-hist-list">
            <div
              v-for="(msg, i) in commitMsgHistory"
              :key="i"
              class="msg-hist-item"
              :title="msg"
              @click="commitModal.message = msg; commitModal.showMsgHist = false"
            >{{ msg }}</div>
            <p v-if="!commitMsgHistory.length" class="hint">{{ t("msgHistEmpty") }}</p>
          </div>
        </div>
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
      <div class="modal about-modal" :class="{ max: isMax('about') }">
        <button
          class="max-btn"
          :title="isMax('about') ? t('restoreWin') : t('maxWin')"
          @click="toggleMax('about')"
        >{{ isMax('about') ? "⤢" : "⛶" }}</button>
        <button class="max-btn close-x" :title="t('closeWin')" @click="closeModal('about')">✕</button>
        <h2>{{ t("appTitle") }}</h2>
        <p class="ver">{{ tr("aboutVer", { v: appVersion }) }}</p>
        <p class="desc">{{ t("aboutDesc") }}</p>
        <div class="about-sec">
          <h4>{{ t("aboutSec1") }}</h4>
          <ul>
            <li>{{ t("aboutA1") }}</li>
            <li>{{ t("aboutA2") }}</li>
            <li>{{ t("aboutA3") }}</li>
          </ul>
        </div>
        <div class="about-sec">
          <h4>{{ t("aboutSec2") }}</h4>
          <ul>
            <li>{{ t("aboutB1") }}</li>
            <li>{{ t("aboutB2") }}</li>
            <li>{{ t("aboutB3") }}</li>
            <li>{{ t("aboutB4") }}</li>
          </ul>
        </div>
        <div class="about-sec">
          <h4>{{ t("aboutSec3") }}</h4>
          <ul>
            <li>{{ t("aboutC1") }}</li>
            <li>{{ t("aboutC2") }}</li>
            <li>{{ t("aboutC3") }}</li>
            <li>{{ t("aboutC4") }}</li>
          </ul>
        </div>
        <div class="about-sec">
          <h4>{{ t("aboutSec4") }}</h4>
          <ul>
            <li>{{ t("aboutD1") }}</li>
            <li>{{ t("aboutD2") }}</li>
            <li>{{ t("aboutD3") }}</li>
            <li>{{ t("aboutD4") }}</li>
          </ul>
        </div>
        <button @click="showAbout = false">{{ t("close") }}</button>
      </div>
    </div>

    <!-- 仓库错误详情弹窗 -->
    <div v-if="errModal" class="modal-mask" @click.self="errModal = null">
      <div class="modal err-modal">
        <h2>{{ tr("errTitle", { name: errModal.name }) }}</h2>
        <pre class="err-pre">{{ errModal.error }}</pre>
        <div class="modal-actions">
          <button class="ghost" @click="copyText(errModal.error, '错误信息')">📋 {{ t("errCopy") }}</button>
          <button class="ghost" @click="retryErr">⟳ {{ t("errRetry") }}</button>
          <span class="flex-spacer"></span>
          <button class="ghost" @click="errModal = null">{{ t("close") }}</button>
        </div>
      </div>
    </div>

    <!-- 批量失败详情弹窗：列出失败仓库的文件清单，一键打开提交弹窗 -->
    <div v-if="failModal?.visible" class="modal-mask" @click.self="closeFailDetail">
      <div class="modal fail-modal">
        <h2>{{ tr("failTitle", { n: failModal.items.length }) }}</h2>
        <p v-if="failModal.loading" class="hint">{{ t("loadingTip") }}</p>
        <div v-else class="fail-list">
          <div v-for="it in failModal.items" :key="it.path" class="fail-item">
            <div class="fail-head">
              <span class="fail-name" :title="it.path">📦 {{ it.name }}</span>
              <span class="fail-msg" :title="it.message">{{ it.message }}</span>
              <button class="mini" @click="openCommitForPath(it.path)">{{ t("openCommit") }}</button>
            </div>
            <div v-if="it.changes?.length" class="fail-changes">
              <div v-for="c in it.changes" :key="c.path" class="fail-file">
                <span class="cst" :class="{ unt: c.status.trim() === '??' }">{{ statusLabel(c.status) }}</span>
                <span class="fail-file-path" :title="c.path">{{ c.path }}</span>
              </div>
            </div>
            <div v-else class="hint">{{ it.message || t("failNoChanges") }}</div>
          </div>
        </div>
        <div class="modal-actions">
          <button class="ghost" @click="locateFailRepos">{{ t("failLocate") }}</button>
          <button class="ghost" @click="closeFailDetail">{{ t("close") }}</button>
        </div>
      </div>
    </div>

    <!-- 认证弹窗（SourceTree 风格）：远程操作需要认证时输入用户名/密码重试 -->
    <div v-if="authModal?.visible" class="modal-mask auth-mask" @click.self="closeAuthModal">
      <div class="modal auth-modal">
        <h2>{{ authModal.title }}</h2>
        <div v-if="authModal.remoteUrl" class="auth-url" :title="authModal.remoteUrl">{{ authModal.remoteUrl }}</div>
        <div v-if="authModal.targets.length" class="auth-repos">
          <div v-for="tp in authModal.targets" :key="tp.path" class="auth-repo" :title="tp.path">📦 {{ tp.name }}</div>
        </div>
        <input
          v-model="authModal.username"
          class="auth-input"
          :placeholder="t('authUsername')"
          autofocus
          @keydown.enter="confirmAuthModal"
        />
        <input
          v-model="authModal.password"
          class="auth-input"
          type="password"
          :placeholder="t('authPassword')"
          @keydown.enter="confirmAuthModal"
        />
        <label class="auth-save">
          <input type="checkbox" v-model="authModal.save" />
          <span>{{ t("authSave") }}</span>
        </label>
        <div class="modal-actions">
          <button class="ghost" :disabled="authModal.busy" @click="closeAuthModal">{{ t("cancel") }}</button>
          <button
            class="primary"
            :disabled="authModal.busy || !authModal.username || !authModal.password"
            @click="confirmAuthModal"
          >{{ authModal.busy ? t("running") : t("authConfirm") }}</button>
        </div>
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
.sidebar { width: 230px; flex-shrink: 0; background: #fff; border: 1px solid #d0d7de; border-radius: 8px; padding: 10px; display: flex; flex-direction: column; gap: 8px; overflow: auto; transition: width .15s ease; }
.sidebar.collapsed { width: 0; padding: 0; border-width: 0; overflow: hidden; }
.sb-toggle {
  border: none; background: transparent; cursor: pointer; font-size: 11px;
  color: #57606a; padding: 2px 6px; border-radius: 4px; line-height: 1;
}
.sb-toggle:hover { background: #eaeef2; color: #0969da; }
.sb-open {
  border: 1px solid #d0d7de; background: #fff; cursor: pointer;
  font-size: 13px; color: #57606a; padding: 4px 10px; border-radius: 6px; line-height: 1;
}
.sb-open:hover { border-color: #0969da; color: #0969da; }
html.dark .sb-open { background: #1c2128; border-color: #30363d; color: #8b949e; }
html.dark .sb-open:hover { border-color: #58a6ff; color: #58a6ff; }
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
.tree-title { font-size: 12px; color: #57606a; font-weight: 600; padding: 2px 6px; display: flex; align-items: center; justify-content: space-between; }
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
.branch-select { padding: 2px 4px; font-size: 12px; max-width: 120px; }
.alias-input { padding: 1px 4px; font-size: 12px; width: 150px; }
.new-grp-row { display: flex; gap: 8px; align-items: center; }
.new-grp-row input { flex: 1; min-width: 0; padding: 5px 10px; }
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
.busy-tip.locate { cursor: pointer; text-decoration: underline dotted; }
.busy-tip.locate:hover { background: #ffd7d5; }
.busy-tip.upd { color: #fff; background: #0969da; cursor: pointer; font-weight: 600; }
.busy-tip.upd:hover { background: #0a5cc0; }
html.dark .busy-tip.bad { background: #f8514926; color: #f85149; }
html.dark .busy-tip.locate:hover { background: #f8514933; }
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
tr.follow td { background: rgba(127, 127, 127, 0.06); }
html.dark tr.follow td { background: rgba(255, 255, 255, 0.03); }
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
.ops { width: 180px; text-align: center; white-space: nowrap; }
.ops button.mini {
  display: inline-flex; align-items: center; justify-content: center;
  height: 28px; min-width: 28px; padding: 0 5px;
  font-size: 13px; line-height: 1; vertical-align: middle;
}
.ops .ref-one { font-size: 24px; font-weight: 700; padding: 0 6px; color: #0969da; background: #ddf4ff; border-color: #54aeff66; }
.mini { padding: 1px 7px; font-size: 12px; line-height: 1.7; border-color: #d0d7de; }
.ops .ref-one { font-size: 24px; line-height: 1; font-weight: 700; padding: 0 6px; color: #0969da; background: #ddf4ff; border-color: #54aeff66; }
html.dark .ops .ref-one { color: #58a6ff; background: #1f6feb26; border-color: #58a6ff66; }
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
.panels-grid .log { grid-column: 1 / -1; }
.drop-panel {
  position: absolute; top: calc(100% - 6px); right: 18px; z-index: 90;
  background: #fff; border: 1px solid #d0d7de; border-radius: 8px;
  box-shadow: 0 8px 24px rgba(0,0,0,.18); padding: 10px 12px;
  width: 600px; max-width: 88vw;
}
html.dark .drop-panel { background: #1c2128; border-color: #30363d; box-shadow: 0 8px 24px #000a; }
.drop-panel .tab-row { display: flex; gap: 6px; margin-bottom: 8px; flex-wrap: wrap; }
.drop-panel .tab-row button { border: 1px solid #d0d7de; border-radius: 6px; padding: 3px 10px; font-size: 12px; background: #fff; color: #57606a; cursor: pointer; }
.drop-panel .tab-row button.on { background: #0969da; border-color: #0969da; color: #fff; }
html.dark .drop-panel .tab-row button { background: #0d1117; border-color: #30363d; color: #8b949e; }
html.dark .drop-panel .tab-row button.on { background: #1f6feb; border-color: #1f6feb; color: #fff; }
.drop-panel .drop-body h3 { margin: 0 0 6px; font-size: 13px; }
.batch-status { font-size: 12px; margin-bottom: 8px; padding: 4px 8px; border-radius: 6px; background: #f6f8fa; }
.batch-status .ok { color: #1a7f37; }
.batch-status .warn { color: #9a6700; }
html.dark .batch-status { background: #0d1117; }
html.dark .batch-status .ok { color: #3fb950; }
html.dark .batch-status .warn { color: #d29922; }
.more-panel { width: 420px; }
.more-row { display: flex; flex-wrap: wrap; gap: 6px; }
/* drop-panel 是浅色浮层，按钮不能用 topbar 的浅色字（白底浅字看不清） */
.drop-panel button.ghost { background: #fff; border-color: #d0d7de; color: #24292f; }
.drop-panel button.ghost:hover { border-color: #0969da; color: #0969da; background: #f6f8fa; }
html.dark .drop-panel button.ghost { background: #0d1117; border-color: #444c56; color: #adbac7; }
html.dark .drop-panel button.ghost:hover { border-color: #58a6ff; color: #58a6ff; background: #161b22; }
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
.log li { cursor: pointer; border-radius: 3px; padding-left: 4px; }
.log li:hover { background: #f0f3f6; }
html.dark .log li:hover { background: #21262d; }
.log-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
.log-head h3 { margin: 0 0 6px; font-size: 13px; }
.log-head .mini { margin: 0 0 6px; }
tr.flash td { background: #ddf4ff !important; transition: background .6s ease; }
html.dark tr.flash td { background: #1f6feb33 !important; }

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
.ctx-sub {
  position: absolute; left: calc(100% - 4px); top: -6px; z-index: 101;
  background: #fff; border: 1px solid #d0d7de; border-radius: 8px;
  box-shadow: 0 4px 16px #0000002e; padding: 6px; min-width: 200px;
  max-height: 320px; overflow: auto;
}
html.dark .ctx-sub { background: #1c2128; border-color: #30363d; }
.tt-btns { display: inline-flex; gap: 2px; }
html.dark .ctx-menu { background: #1c2128; border-color: #30363d; box-shadow: 0 4px 16px #000a; }
html.dark .ctx-menu .ctx-title { color: #8b949e; border-color: #30363d; }
html.dark .ctx-menu button:hover { background: #22272e; color: #539bf5; }
html.dark .ctx-menu .ctx-sep { background: #30363d; }
.modal-mask { position: fixed; inset: 0; background: #000a; display: flex; align-items: center; justify-content: center; z-index: 200; }
.modal.commit-modal { width: 960px; max-width: 96vw; }
.commit-top { display: flex; align-items: center; justify-content: space-between; margin-bottom: 6px; }
.commit-top .ct-side { display: inline-flex; align-items: center; gap: 8px; }
.commit-top .cnt { font-size: 12px; color: #57606a; }
html.dark .commit-top .cnt { color: #8b949e; }
.commit-body { display: flex; gap: 0; align-items: stretch; }
.commit-splitter {
  flex: 0 0 auto; width: 8px; margin: 0 3px; cursor: col-resize;
  align-self: stretch; border-radius: 4px; touch-action: none;
}
.commit-splitter:hover { background: #d0d7de; }
html.dark .commit-splitter:hover { background: #30363d; }
.commit-files {
  width: 320px; flex: 0 0 auto; display: flex; flex-direction: column;
  max-height: 55vh; overflow: hidden;
  border: 1px solid #d0d7de;
  border-radius: 6px;
  padding: 4px 6px;
}
.commit-files:focus { outline: 1px solid #0969da66; outline-offset: -1px; }
html.dark .commit-files:focus { outline-color: #58a6ff66; }
.commit-search {
  flex: 0 0 auto; margin-bottom: 4px; padding: 4px 8px;
  font-size: 12px; border: 1px solid #d0d7de; border-radius: 4px;
  background: #fff; color: inherit; min-width: 0;
}
html.dark .commit-search { background: #0d1117; border-color: #30363d; }
.commit-files-list { flex: 1 1 auto; min-height: 0; overflow-y: auto; overflow-x: hidden; }
.conflict-row { flex: 0 0 auto; display: flex; align-items: center; gap: 6px; margin-bottom: 4px; }
.conflict-line {
  flex: 1 1 auto; min-width: 0; font-size: 11px; color: #57606a;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.conflict-line.warn { color: #d1242f; }
.conflict-line.ok { color: #1a7f37; }
html.dark .conflict-line { color: #8b949e; }
html.dark .conflict-line.warn { color: #f85149; }
html.dark .conflict-line.ok { color: #3fb950; }
.cc-badge {
  flex: 0 0 auto; font-size: 10px; line-height: 1;
  color: #d1242f; border: 1px solid #ff818266; border-radius: 8px;
  padding: 2px 5px; margin-left: 4px; white-space: nowrap;
}
html.dark .cc-badge { color: #f85149; border-color: #f8514966; }
.commit-file { display: flex; align-items: center; gap: 8px; padding: 4px 6px; font-size: 13px; border-radius: 4px; cursor: pointer; }
.cf-title { font-size: 11px; font-weight: 600; color: #57606a; text-transform: uppercase; letter-spacing: .04em; padding: 6px 6px 4px; }
.cf-title:not(:first-child) { margin-top: 4px; }
html.dark .cf-title { color: #8b949e; }
.commit-file-ops { flex: 0 0 auto; display: inline-flex; gap: 4px; margin-left: auto; }
.commit-file-ops .mini { margin: 0; }
.commit-msg-hist { position: relative; margin-top: 6px; }
.commit-msg-hist .mini { margin: 0; }
.msg-hist-list { position: absolute; left: 0; top: 28px; z-index: 50; min-width: 320px; max-width: 100%; max-height: 180px; overflow-y: auto; background: #fff; border: 1px solid #d0d7de; border-radius: 6px; box-shadow: 0 4px 12px rgba(0,0,0,.12); padding: 4px; }
.msg-hist-item { padding: 5px 8px; font-size: 12px; border-radius: 4px; cursor: pointer; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.msg-hist-item:hover { background: #f0f3f6; }
html.dark .msg-hist-list { background: #161b22; border-color: #30363d; }
html.dark .msg-hist-item:hover { background: #21262d; }
.commit-file:hover { background: #f6f8fa; }
.commit-file.sel { background: #ddf4ff; }
html.dark .commit-file:hover { background: #21262d; }
html.dark .commit-file.sel { background: #1f6feb33; }
.cst { flex: 0 0 auto; font-size: 11px; padding: 1px 7px; border-radius: 8px; background: #ddf4ff; color: #0969da; }
.cst.unt { background: #fff8c5; color: #9a6700; }
.cp { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1 1 auto; min-width: 0; }
.commit-diff-pane {
  flex: 1 1 auto; min-width: 0;
  max-height: 55vh; overflow: auto;
  border: 1px solid #d0d7de;
  border-radius: 6px;
  background: #f6f8fa;
}
.commit-diff { font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, monospace; font-size: 12px; line-height: 1.6; white-space: pre; }
.hunk-bar { display: flex; align-items: center; gap: 8px; padding: 4px 8px; margin: 4px 0; background: #f6f8fa; border: 1px solid #d0d7de; border-radius: 6px; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
.hunk-tag { font-size: 11px; color: #57606a; }
.hunk-bar .mini { padding: 2px 10px; font-size: 11px; }
.hint.warn { color: #9a6700; }
html.dark .hunk-bar { background: #161b22; border-color: #30363d; }
html.dark .hunk-tag { color: #8b949e; }
html.dark .hint.warn { color: #d29922; }
.commit-diff pre { margin: 0; padding: 8px 10px; font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, monospace; font-size: 12px; line-height: 1.6; white-space: pre; }
.commit-diff .dl { display: block; }
.commit-diff .dl.add { background: #e6ffec; color: #1a7f37; }
.commit-diff .dl.del { background: #ffebe9; color: #cf222e; }
.commit-diff .dl.hunk { background: #ddf4ff; color: #0969da; }
.commit-diff .dl.meta { color: #57606a; }
html.dark .commit-diff .dl.add { background: #12261a; color: #7ee787; }
html.dark .commit-diff .dl.del { background: #2d1518; color: #ff7b72; }
html.dark .commit-diff .dl.hunk { background: #122a3a; color: #58a6ff; }
html.dark .commit-diff .dl.meta { color: #8b949e; }
.commit-diff-pane .hint { padding: 24px; text-align: center; }
html.dark .commit-diff-pane { background: #0d1117; border-color: #30363d; }
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
.flex-spacer { flex: 1 1 auto; }
.err-modal { width: 560px; max-width: 92vw; }
.err-pre {
  background: #f6f8fa; border: 1px solid #d0d7de; border-radius: 6px;
  padding: 10px 12px; font-size: 12px; line-height: 1.5; color: #cf222e;
  white-space: pre-wrap; word-break: break-all; max-height: 50vh; overflow: auto;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace; margin: 8px 0 0;
}
html.dark .err-pre { background: #0d1117; border-color: #30363d; color: #f85149; }
.modal.log-modal { width: 640px; max-width: 92vw; }
.modal.graph-modal { width: 920px; max-width: 96vw; }
.modal.branch-modal { width: 760px; max-width: 94vw; }
.branch-new { display: flex; gap: 8px; margin-bottom: 10px; }
.rm-name { flex: 0 0 110px !important; }
.rm-url { flex: 1 1 auto; min-width: 0 !important; }
/* 远程管理列表：两行布局（上行 名+操作，下行 完整地址/编辑框） */
.branch-list.rm-list > li { flex-direction: column; align-items: stretch; gap: 4px; }
.rm-head { display: flex; align-items: center; gap: 8px; }
.rm-head .b-name { font-weight: 600; flex: 0 0 auto; }
.rm-head .b-ops { margin-left: auto; }
.rm-url-full {
  font-size: 12px; color: #57606a; word-break: break-all; line-height: 1.5;
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}
.rm-edit { display: flex; gap: 6px; }
.rm-edit .rm-url {
  flex: 1 1 auto; min-width: 0; padding: 5px 8px; font-size: 13px;
  border: 1px solid #d0d7de; border-radius: 6px; background: #fff; color: inherit;
}
html.dark .rm-url-full { color: #8b949e; }
html.dark .rm-edit .rm-url { background: #0d1117; border-color: #30363d; }
.stash-idx { color: #57606a; font-size: 12px; font-family: monospace; }
.stash-subj { color: inherit; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 55%; }
html.dark .stash-idx { color: #8b949e; }
.branch-new input { flex: 1; min-width: 0; padding: 5px 8px; font-size: 13px; border: 1px solid #d0d7de; border-radius: 6px; }
.branch-new button { flex: 0 0 auto; }
.branch-list { list-style: none; margin: 0; padding: 4px; max-height: 50vh; overflow: auto; border: 1px solid #d0d7de; border-radius: 8px; }
.modal.max .branch-list { max-height: none; flex: 1 1 auto; }
.branch-list li { display: flex; align-items: center; gap: 8px; padding: 6px 8px; border-radius: 6px; font-size: 13px; }
.branch-list li + li { border-top: 1px solid #eaeef2; }
.branch-list li:hover { background: #f6f8fa; }
.b-name { display: flex; align-items: center; gap: 6px; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }
.b-name.cur { color: #0969da; }
.b-dot { flex: 0 0 auto; width: 8px; height: 8px; border-radius: 50%; background: #d0d7de; }
.b-name.cur .b-dot { background: #0969da; }
.b-tag { flex: 0 0 auto; font-size: 10px; padding: 0 6px; border-radius: 8px; font-weight: 400; }
.b-tag.cur { background: #0969da; color: #fff; }
.b-tag.local { background: #ddf4ff; color: #0969da; }
.b-tag.remote { background: #fff8c5; color: #9a6700; }
.b-ops { flex: 0 0 auto; display: flex; gap: 4px; }
.mini.danger { color: #cf222e; border-color: #ff8182; }
.mini.danger:hover { background: #ffebe9; }
.log-list li.log-diff { display: block; border: none; padding: 0; }
.log-diff pre { margin: 0 0 6px 6px; padding: 8px 10px; font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, monospace; font-size: 12px; line-height: 1.5; white-space: pre; overflow: auto; background: #f6f8fa; border: 1px solid #d0d7de; border-radius: 6px; max-height: 260px; }
html.dark .branch-new input { background: #161b22; color: #e6edf3; border-color: #30363d; }
html.dark .branch-list { border-color: #30363d; }
html.dark .branch-list li + li { border-color: #30363d; }
html.dark .branch-list li:hover { background: #22272e; }
html.dark .b-name.cur { color: #58a6ff; }
html.dark .b-name.cur .b-dot { background: #58a6ff; }
html.dark .b-dot { background: #30363d; }
html.dark .b-tag.cur { background: #1f6feb; color: #fff; }
html.dark .b-tag.local { background: #1f6feb33; color: #58a6ff; }
html.dark .b-tag.remote { background: #9e6a0333; color: #d29922; }
html.dark .log-diff pre { background: #0d1117; border-color: #30363d; }
html.dark .mini.danger { color: #ff7b72; border-color: #ff8182; }
html.dark .mini.danger:hover { background: #2d1518; }
.graph-scroll { max-height: 60vh; overflow: auto; border: 1px solid #d0d7de; border-radius: 8px; }
.modal.max .graph-scroll { max-height: none; flex: 1 1 auto; }
.graph-row { display: flex; align-items: center; gap: 8px; padding-right: 12px; }
.graph-row:hover { background: #f6f8fa; }
.g-refs { flex: 0 0 auto; min-width: 90px; max-width: 220px; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; }
.g-ref { display: inline-block; font-size: 10px; padding: 0 5px; border-radius: 6px; background: #ddf4ff; color: #0969da; margin-right: 4px; }
.g-ref.head { background: #0969da; color: #fff; }
.g-hash { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11px; color: #57606a; flex: 0 0 52px; }
.g-subj { font-size: 12px; color: #1f2328; flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.g-meta { font-size: 11px; color: #8c959f; flex: 0 0 auto; max-width: 200px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.err-text { color: #cf222e; }
html.dark .graph-scroll { border-color: #30363d; }
html.dark .graph-row:hover { background: #22272e; }
html.dark .g-ref { background: #1f6feb33; color: #58a6ff; }
html.dark .g-ref.head { background: #1f6feb; color: #fff; }
html.dark .g-hash { color: #8b949e; }
html.dark .g-subj { color: #e6edf3; }
html.dark .g-meta { color: #8b949e; }
.log-search { width: 100%; padding: 5px 8px; margin-bottom: 8px; font-size: 13px; border: 1px solid #d0d7de; border-radius: 6px; }
html.dark .log-search { background: #161b22; color: #e6edf3; border-color: #30363d; }
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
html.dark .commit-files { border-color: #30363d; }
html.dark .commit-modal textarea { border-color: #30363d; background: #161b22; color: #e6edf3; }
html.dark .cst { background: #1f6feb33; color: #58a6ff; }
html.dark .cst.unt { background: #9e6a0333; color: #d29922; }
.modal { background: #fff; border-radius: 10px; padding: 20px 24px; width: 380px; box-shadow: 0 8px 30px #0004; position: relative; }
.about-modal { width: 620px; max-width: 92vw; max-height: 86vh; overflow-y: auto; }
.about-modal .ver { font-size: 12px; color: #57606a; margin: 2px 0 4px; }
.about-modal .desc { font-size: 13px; margin: 0 0 10px; }
.about-modal .about-sec { margin: 10px 0; }
.about-modal .about-sec h4 { margin: 0 0 4px; font-size: 13px; color: #0969da; }
.about-modal .about-sec ul { margin: 0; padding-left: 18px; display: flex; flex-direction: column; gap: 3px; }
.about-modal .about-sec li { font-size: 12px; line-height: 1.5; }
html.dark .about-modal .ver { color: #8b949e; }
html.dark .about-modal .about-sec h4 { color: #58a6ff; }
.auth-mask { z-index: 500; }
.fail-modal { width: 560px; max-width: 92vw; }
.fail-list { max-height: 55vh; overflow-y: auto; display: flex; flex-direction: column; gap: 10px; margin-top: 8px; }
.fail-item { border: 1px solid #d0d7de; border-radius: 6px; padding: 8px 10px; }
html.dark .fail-item { border-color: #30363d; }
.fail-head { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
.fail-name { font-weight: 600; font-size: 13px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.fail-msg { flex: 1 1 auto; min-width: 0; font-size: 11px; color: #57606a; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
html.dark .fail-msg { color: #8b949e; }
.fail-changes { max-height: 160px; overflow-y: auto; display: flex; flex-direction: column; gap: 2px; }
.fail-file { display: flex; align-items: center; gap: 6px; font-size: 12px; }
.fail-file-path { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.auth-modal { width: 420px; }
.auth-input { width: 100%; padding: 7px 10px; margin-top: 8px; font-size: 13px; border: 1px solid #d0d7de; border-radius: 6px; box-sizing: border-box; background: #fff; color: inherit; }
html.dark .auth-input { background: #161b22; border-color: #30363d; }
.auth-url { font-size: 11px; color: #57606a; word-break: break-all; margin-top: 6px; }
.auth-repos { margin-top: 8px; max-height: 110px; overflow-y: auto; display: flex; flex-direction: column; gap: 2px; }
.auth-repo { font-size: 12px; color: #57606a; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
html.dark .auth-url, html.dark .auth-repo { color: #8b949e; }
.auth-save { display: flex; align-items: center; gap: 6px; margin-top: 10px; font-size: 12px; color: #57606a; cursor: pointer; user-select: none; }
html.dark .auth-save { color: #8b949e; }
.max-btn { position: absolute; top: 12px; right: 44px; border: none; background: transparent; cursor: pointer; font-size: 15px; line-height: 1; color: #57606a; padding: 4px 6px; border-radius: 4px; }
.max-btn.close-x { right: 16px; font-size: 14px; }
.max-btn:hover { background: #f6f8fa; color: #0969da; }
.max-btn.close-x:hover { color: #cf222e; }
html.dark .max-btn { color: #8b949e; }
html.dark .max-btn:hover { background: #22272e; color: #58a6ff; }
.modal.max { width: 92vw !important; max-width: 92vw !important; height: 90vh; max-height: 90vh; display: flex; flex-direction: column; }
.modal.max .commit-body { flex: 1 1 auto; min-height: 0; }
.modal.max .commit-files,
.modal.max .commit-diff-pane { max-height: none; }
.modal.max .log-list { max-height: none; flex: 1 1 auto; }
.modal.max .clone-base { max-height: none; flex: 1 1 auto; }
.modal h2 { margin: 0 0 6px; font-size: 18px; }
.modal .ver { color: #57606a; font-size: 13px; margin: 0 0 8px; }
.modal .desc { color: #57606a; font-size: 13px; margin: 0 0 12px; }
.modal .features { margin: 0 0 16px; padding-left: 18px; color: #1f2328; font-size: 13px; line-height: 1.9; }
html.dark .modal { background: #1c2128; color: #e6edf3; }
html.dark .modal .ver, html.dark .modal .desc { color: #8b949e; }
html.dark .modal .features { color: #e6edf3; }
</style>
