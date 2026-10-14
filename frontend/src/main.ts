// ClipVault 面板控制器（U4）。
//
// 负责：调用 IPC → 渲染列表、tab 切换、键盘导航（↑↓/Enter/⌘E/⌘P/Esc）、
// 100ms 防抖搜索、加载态（保留上一帧 + 顶部进度条）。
//
// 后续单元接管的动作在此仅调用类型化 IPC 桩并给出 toast 提示：
// Enter→paste_entry(U8)、⌘E→open_note_editor(U6)、⌘P→toggle_pin(U7)。

// 纯副作用 import：styles.css 不被任何模块引用，只能靠它进入 Vite 的依赖图，
// 否则构建产物里不会出现 CSS 资产（面板/模态全部无样式）。删掉这一行不会报错，
// 只会静默失去全部样式 —— 别顺手清理「看起来没人用」的这一行。
import "./styles.css";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  dispatchHotkeyAction,
  handleVaultKey,
  loadKeymap,
  matches,
  registerKeymapHandlers,
} from "./keymap";
import {
  type CategoryInfo,
  type EntryDto,
  EYE_SVG,
  type TagInfo,
  renderCategories,
  renderEntries,
  renderFilterHeader,
  renderTags,
  renderVaultList,
} from "./panel";
import { initPlatform, keys, whenPlatformReady } from "./platform";
import {
  type PasteResult,
  clearUndecryptable,
  getGuestMode,
  getPanelOpacity,
  listVault,
  moveToVault,
  refreshAccessibilityBanner,
  refreshKeychainBanner,
  renderHotkeysSection,
  setGuestMode,
  setPanelOpacity,
  skipNextCopy,
} from "./settings";
import {
  closeEditor,
  isEditorOpen,
  openEditor,
  setEditorCloseHandler,
} from "./editor";

const LIMIT = 50;
const SEARCH_DEBOUNCE_MS = 100;

type TabId = "recent" | "all" | "tag" | "category";
const TABS: { id: TabId; label: string }[] = [
  { id: "recent", label: "最近" },
  { id: "all", label: "全部" },
  { id: "tag", label: "标签" },
  { id: "category", label: "类别" },
];

let currentTab: TabId = "recent";
let entries: EntryDto[] = [];
let tagList: TagInfo[] = [];
let catList: CategoryInfo[] = [];
// 「标签」/「类别」tab 下已选中的筛选项；null 表示仍在展示聚合列表。
let selectedTag: string | null = null;
let selectedCategory: string | null = null;
let selected = 0;
// 缓存当前选中的行元素，避免每次键盘导航都 querySelector(".row.selected")。
// 列表全量重画（load 完成 / tab 切换 / 过滤变化）时由 `paintEntries` 重建并重置。
let selectedEl: HTMLElement | null = null;
// 进行中的 load 请求；新请求发起前 abort 旧的，避免慢响应覆盖快的新结果。
let loadAbort: AbortController | null = null;
let searchTimer: number | undefined;
let toastTimer: number | undefined;
// 访客模式当前状态（以后端为准，通过 getGuestMode 同步；U10）。
let guestMode = false;

const searchEl = document.querySelector<HTMLInputElement>("#search")!;
const tabsEl = document.querySelector<HTMLDivElement>("#tabs")!;
const listEl = document.querySelector<HTMLDivElement>("#list")!;
const bannerEl = document.querySelector<HTMLDivElement>("#banner")!;
const vaultBannerEl = document.querySelector<HTMLDivElement>("#vault-banner")!;
const guestNoticeEl = document.querySelector<HTMLDivElement>("#guest-notice")!;
const progressEl = document.querySelector<HTMLDivElement>("#progress")!;
const toastEl = document.querySelector<HTMLDivElement>("#toast")!;

// 加密区管理模态元素（U9）。
const vaultEl = document.querySelector<HTMLDivElement>("#vault")!;
const vaultListEl = document.querySelector<HTMLDivElement>("#vault-list")!;
const vaultSubEl = document.querySelector<HTMLDivElement>("#vault-sub")!;
const vaultCloseEl = document.querySelector<HTMLButtonElement>("#vault-close")!;
const vaultClearEl = document.querySelector<HTMLButtonElement>("#vault-clear")!;
// 加密区模态内的条目状态：列表 + 选中索引 + 选中行元素缓存。
// 加密区条目同样要能选中回贴 / 编辑备注（AE5），故与主列表并列维护一套选中态。
let vaultEntries: EntryDto[] = [];
let vaultSelected = 0;
let vaultSelectedEl: HTMLElement | null = null;

// 设置模态元素（U12）。⌘, 唤出,托盘菜单也可触发。
const settingsEl = document.querySelector<HTMLDivElement>("#settings")!;
const settingsCloseEl = document.querySelector<HTMLButtonElement>("#settings-close")!;
const opacityEl = document.querySelector<HTMLInputElement>("#settings-opacity")!;
const opacityValueEl = document.querySelector<HTMLSpanElement>("#settings-opacity-value")!;

const isVaultOpen = (): boolean => !vaultEl.classList.contains("hidden");
const isSettingsOpen = (): boolean => !settingsEl.classList.contains("hidden");

// 当前视图是否在展示「条目列表」（可键盘导航 / 回贴）：
// recent、all，或 标签/类别 已选中筛选项时。
const showingEntries = (): boolean =>
  currentTab === "recent" ||
  currentTab === "all" ||
  (currentTab === "tag" && selectedTag !== null) ||
  (currentTab === "category" && selectedCategory !== null);

const currentEntry = (): EntryDto | undefined =>
  selected >= 0 ? entries[selected] : undefined;

/** 传给 `search_results` 的 tab 参数：筛选态编码为 `tag:<name>` / `category:<name>`。 */
function tabArg(): string {
  if (currentTab === "tag" && selectedTag !== null) return `tag:${selectedTag}`;
  if (currentTab === "category" && selectedCategory !== null)
    return `category:${selectedCategory}`;
  return currentTab; // recent | all
}

function showProgress(active: boolean): void {
  progressEl.classList.toggle("active", active);
}

function toast(msg: string): void {
  toastEl.textContent = msg;
  toastEl.classList.remove("hidden");
  if (toastTimer !== undefined) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    toastEl.classList.add("hidden");
  }, 1600);
}

/** 多类型 plan U7 + R20：URL ⌘O 打开。 */
async function openUrlAction(e: EntryDto): Promise<void> {
  if (e.auto_category !== "URL") return;
  try {
    await invoke("open_url", { id: e.id });
  } catch (err) {
    toast(`打开失败：${String(err)}`);
  }
}

/**
 * 刷新访客模式提示条（U10 / R10）：开启时在面板顶部显示一条常驻提示，
 * 说明「不记录新条目、已有条目只读可访问」，从而在「最近」等视图里对
 * 缺失的新条目做出解释（计划：已有条目保留可访问，只是不出现新条目）。
 * 同步内部 `guestMode` 状态。
 */
async function refreshGuestNotice(): Promise<void> {
  // 文案里有修饰键（⌘⇧G / Ctrl+Shift+G），等平台标识就绪再渲染。
  await whenPlatformReady();
  guestMode = await getGuestMode();
  if (guestMode) {
    guestNoticeEl.classList.remove("hidden");
    guestNoticeEl.innerHTML = `
      <span class="banner-icon">${EYE_SVG}</span>
      <div class="banner-body">
        <div class="banner-title">访客模式 · 不记录新条目</div>
        <div class="banner-sub">新复制的内容不会入库；已有条目仍只读可访问。${keys("⌘⇧G")} 关闭。</div>
      </div>`;
  } else {
    guestNoticeEl.classList.add("hidden");
    guestNoticeEl.innerHTML = "";
  }
}

/** 切换访客模式（⌘⇧G / 或托盘菜单）。后端持久化并同步托盘，前端刷新提示。 */
async function toggleGuestMode(): Promise<void> {
  const next = await setGuestMode(!guestMode);
  guestMode = next;
  await refreshGuestNotice();
  toast(next ? "已开启访客模式 · 不记录新条目" : "已关闭访客模式");
}

/** 触发「本次不记录」（⌘⇧N / 或托盘菜单）：下一次复制不入库，一次性。 */
async function triggerSkipOnce(): Promise<void> {
  try {
    await skipNextCopy();
    toast("下一次复制将不被记录");
  } catch (err) {
    toast(`操作失败：${String(err)}`);
  }
}

function renderTabs(): void {
  tabsEl.innerHTML = TABS.map(
    (t) =>
      `<button class="tab${t.id === currentTab ? " active" : ""}" data-tab="${t.id}">${t.label}</button>`,
  ).join("");
}

function scrollToSelected(): void {
  selectedEl?.scrollIntoView({ block: "nearest" });
}

/** 渲染条目列表；筛选态下在顶部补一行「‹ 返回」条。
 *  全量重画：仅在数据本身变化（load 完成 / tab 切换 / 筛选变化）时调用。
 *  选中切换走 [`applySelectionPatch`] 走 classList 增量,不重建 DOM。*/
function paintEntries(): void {
  renderEntries(listEl, entries, selected);
  // 重画后缓存当前选中的行元素（如果存在）。下次切换选中时直接复用。
  selectedEl = listEl.querySelector<HTMLElement>(".row.selected");
  if (currentTab === "tag" && selectedTag !== null) {
    listEl.insertAdjacentHTML(
      "afterbegin",
      renderFilterHeader("tag", selectedTag),
    );
  } else if (currentTab === "category" && selectedCategory !== null) {
    listEl.insertAdjacentHTML(
      "afterbegin",
      renderFilterHeader("category", selectedCategory),
    );
  }
}

/** 选中切换的增量路径：仅调整 .selected class,DOM 不重建。
 *  `idx` 越界时静默忽略。前提:列表已由 [`paintEntries`] 渲染,且与 `entries[]` 一一对应。*/
function applySelectionPatch(idx: number): void {
  if (idx < 0 || idx >= entries.length) return;
  const rows = listEl.children;
  // 过滤头行（如果有）会出现在最前面,跳过它。
  const filterRow = listEl.querySelector<HTMLElement>(".filter-back");
  const offset = filterRow ? 1 : 0;
  const target = rows[idx + offset] as HTMLElement | undefined;
  if (!target) return;
  if (selectedEl && selectedEl !== target) selectedEl.classList.remove("selected");
  if (target.classList.contains("selected")) {
    // 已经在选中态,无需操作。
  } else {
    target.classList.add("selected");
  }
  selectedEl = target;
}

async function load(preferId?: number): Promise<void> {
  // 取消上一次还在飞的 load,避免慢响应覆盖快的新结果(连打 / 切 tab / 切筛选)。
  loadAbort?.abort();
  loadAbort = new AbortController();
  const myAbort = loadAbort;
  showProgress(true);
  try {
    if (myAbort.signal.aborted) return;
    if (currentTab === "tag" && selectedTag === null) {
      tagList = await invoke<TagInfo[]>("get_all_tags");
      if (myAbort.signal.aborted) return;
      renderTags(listEl, tagList);
      selected = -1;
      selectedEl = null;
    } else if (currentTab === "category" && selectedCategory === null) {
      catList = await invoke<CategoryInfo[]>("get_categories");
      if (myAbort.signal.aborted) return;
      renderCategories(listEl, catList);
      selected = -1;
      selectedEl = null;
    } else {
      // 条目列表：recent / all / 已选标签 / 已选类别。
      const query = searchEl.value.trim();
      const result = await invoke<EntryDto[]>("search_results", {
        query,
        tab: tabArg(),
        limit: LIMIT,
      });
      if (myAbort.signal.aborted) return;
      entries = result;
      if (selected >= entries.length) selected = entries.length - 1;
      if (selected < 0 && entries.length > 0) selected = 0;
      // 若指定了 preferId（如刚被切换置顶的条目）且仍可见，则优先选中它，
      // 在唯一一次重绘前就定好选中项，避免二次 paint。
      if (preferId !== undefined) {
        const idx = entries.findIndex((x) => x.id === preferId);
        if (idx >= 0) selected = idx;
      }
      paintEntries();
      scrollToSelected();
    }
  } catch (e) {
    // 取消的请求不报错。
    if (myAbort.signal.aborted) return;
    // 保留上一帧不动，仅在完全无内容时提示。
    if (listEl.childElementCount === 0) {
      listEl.innerHTML = `<div class="empty">加载失败：${String(e)}</div>`;
    }
    toast(`加载失败：${String(e)}`);
  } finally {
    if (loadAbort === myAbort) loadAbort = null;
    showProgress(false);
  }
}

// ----------------------------------------------------------------
// 动作（Enter / ⌘E / ⌘P）—— 调用后续单元的 IPC 桩
// ----------------------------------------------------------------

async function activate(): Promise<void> {
  const e = currentEntry();
  if (!e) return;
  await pasteEntry(e);
}

/** ⌘N 快捷键：把选中移到列表第 idx 条并回贴。越界则忽略。 */
async function activateIndex(idx: number): Promise<void> {
  if (!showingEntries() || idx < 0 || idx >= entries.length) return;
  selected = idx;
  applySelectionPatch(selected);
  scrollToSelected();
  await pasteEntry(entries[idx]);
}

/** 回贴前收起所有浮层（加密区模态 / 备注编辑器）。
 *
 *  回贴是「面板的一次性动作」：无论成功（后端直接隐藏面板）还是降级
 *  （内容已在剪贴板），用户都不会再回到模态里，留着它只会挡住下一次唤出，
 *  并让「看得见的是模态、敲进去的是搜索框」。故在每个回贴入口前统一收起。 */
function dismissOverlays(): void {
  if (isVaultOpen()) closeVault();
  if (isEditorOpen()) closeEditor();
}

/** 回贴一条记录：面板**显不显示由后端按回贴结果决定**，前端不再插手。
 *
 *  - `pasted`：后端已隐藏面板、把 ⌘V 落到目标应用，并把键盘焦点还了回去；
 *  - `window_switched` / `clipboard_only` / `no_accessibility`：内容已在剪贴板
 *    但没能自动回贴，后端保持面板可见，前端只负责把提示（toast / 授权横幅）
 *    显示在这个仍然可见的面板上。
 *
 *  这里曾无条件再调一次 `hide_panel`，把后端刚显示出来的降级提示立刻藏掉：
 *  用户既看不到 toast，焦点又被多抢了一次（面板藏在 WebView 里，藏了就没提示）。
 *  真正报错（vault 解密失败等）仍保留面板让用户处理。 */
async function pasteEntry(e: EntryDto): Promise<void> {
  // 入口即收起浮层：主列表 ↩ / ⌘1–9 / 双击，以及加密区模态内的 ↩ / 双击，
  // 都走这里，不需要各自记得关闭自己的模态。
  dismissOverlays();
  try {
    const r = await invoke<PasteResult>("paste_entry", { id: e.id });
    if (r.status === "no_accessibility") {
      // 未授权：内容已进剪贴板，但无法自动回贴。亮授权横幅 + 提示可手动 ⌘V。
      // 面板保持可见；Esc 关闭时后端会把焦点还回原应用，再手动 ⌘V 即可。
      await refreshAccessibilityBanner(bannerEl);
      toast(r.message);
    } else if (r.status !== "pasted") {
      // window_switched / clipboard_only：后端已重新显示面板，toast 引导手动 ⌘V。
      toast(r.message);
    } else {
      // "pasted"：后端已隐藏面板并把 ⌘V 落到目标应用，无需额外提示。
      toast(r.message);
    }
  } catch (err) {
    const msg = String(err);
    if (msg.includes("missing-accessibility")) {
      // 兜底:后端仍以错误形式返回未授权时,展示 Accessibility 横幅。
      await refreshAccessibilityBanner(bannerEl);
      toast("需要「辅助功能」授权才能自动回贴");
    } else if (msg.includes("加密条目解密失败")) {
      // vault 解密失败:留面板让用户去加密区处理(保留密文不出错)。
      toast(`加密区条目解密失败:${msg}`);
    } else {
      toast(`回贴失败:${msg}`);
    }
  }
}

function editNote(): void {
  const e = currentEntry();
  if (!e) return;
  openEditor(e, (dto) => {
    // 保存成功：就地更新对应行并重绘，无需整表刷新。
    const idx = entries.findIndex((x) => x.id === dto.id);
    if (idx >= 0) {
      entries[idx] = dto;
      paintEntries();
      scrollToSelected();
    }
  });
}

async function togglePin(): Promise<void> {
  const e = currentEntry();
  if (!e) return;
  const toggledId = e.id;
  try {
    // 切换置顶。后端返回刷新后的 DTO，但真正的「Pin 优先」顺序以服务端为准，
    // 故直接重新拉取当前 tab，让服务端排序权威地反映新顺序（R17 / AE8）。
    // 把被切换的条目 id 交给 load()，在唯一一次重绘前定好选中项。
    await invoke<EntryDto>("toggle_pin", { id: toggledId });
    await load(toggledId);
  } catch (err) {
    toast(`置顶失败：${String(err)}`);
  }
}

function moveSelection(delta: number): void {
  if (!showingEntries() || entries.length === 0) return;
  selected = Math.min(Math.max(selected + delta, 0), entries.length - 1);
  // 选中切换走 classList 增量,DOM 不重建(jsdom 基准:50 行 ~0.02ms)。
  applySelectionPatch(selected);
  scrollToSelected();
}

// ----------------------------------------------------------------
// 加密区（U9）：移入加密区 + 加密区管理模态
// ----------------------------------------------------------------

/** 把当前选中条目移入加密区（⌘L）。Keychain 不可用时提示并刷新横幅。 */
async function moveCurrentToVault(): Promise<void> {
  const e = currentEntry();
  if (!e) return;
  if (e.is_vault) {
    toast("该条目已在加密区");
    return;
  }
  try {
    await moveToVault(e.id);
    toast("已移入加密区 🔒");
    await load(); // vault 条目已退出普通列表。
  } catch (err) {
    // 多为 Keychain 不可用：条目保持明文未加密。
    await refreshKeychainBanner(vaultBannerEl);
    toast(`移入加密区失败：${String(err)}`);
  }
}

/** 打开加密区管理模态（⌘⇧V）：列出 vault 条目、按需暴露「清除不可解密」。 */
async function openVault(): Promise<void> {
  vaultEl.classList.remove("hidden");
  try {
    const items = await listVault();
    vaultEntries = items;
    if (vaultSelected >= vaultEntries.length) vaultSelected = vaultEntries.length - 1;
    if (vaultSelected < 0) vaultSelected = 0;
    paintVaultList();
    const undec = items.filter((x) => !x.is_decryptable).length;
    vaultSubEl.textContent =
      items.length === 0
        ? `把敏感条目移入加密区（列表中按 ${keys("⌘L")}）后，内容以 AES-256-GCM 加密存储。`
        : `共 ${items.length} 条${undec > 0 ? ` · ${undec} 条无法解密` : ""}`;
    vaultClearEl.classList.toggle("hidden", undec === 0);
  } catch (err) {
    vaultEntries = [];
    vaultSelectedEl = null;
    vaultListEl.innerHTML = `<div class="empty">加载加密区失败：${String(err)}</div>`;
  }
}

/** 渲染加密区列表并重建选中行缓存。全量重画，仅在数据变化时调用。 */
function paintVaultList(): void {
  renderVaultList(vaultListEl, vaultEntries, vaultSelected);
  vaultSelectedEl = vaultListEl.querySelector<HTMLElement>(".vault-row.selected");
}

/** 加密区选中切换的增量路径：仅调整 .selected class，不重建 DOM。 */
function applyVaultSelectionPatch(idx: number): void {
  if (idx < 0 || idx >= vaultEntries.length) return;
  const target = vaultListEl.children[idx] as HTMLElement | undefined;
  if (!target) return;
  if (vaultSelectedEl && vaultSelectedEl !== target) vaultSelectedEl.classList.remove("selected");
  target.classList.add("selected");
  vaultSelectedEl = target;
  target.scrollIntoView({ block: "nearest" });
}

function moveVaultSelection(delta: number): void {
  if (vaultEntries.length === 0) return;
  const next = Math.min(Math.max(vaultSelected + delta, 0), vaultEntries.length - 1);
  if (next === vaultSelected) return;
  vaultSelected = next;
  applyVaultSelectionPatch(vaultSelected);
}

const currentVaultEntry = (): EntryDto | undefined => vaultEntries[vaultSelected];

/**
 * 回贴当前选中的加密区条目（↩ / 双击）。后端 `paste_entry` 负责解密 +
 * 写剪贴板 + 模拟 ⌘V，与非加密条目同一路径（AE5）。
 * 不可解密的条目（Keychain 密钥缺失 / 变更）直接提示，不发起回贴。
 */
async function pasteVaultEntry(): Promise<void> {
  const e = currentVaultEntry();
  if (!e) return;
  if (!e.is_decryptable) {
    toast("该条目无法解密：Keychain 密钥缺失或已变更，密文无法还原");
    return;
  }
  // 收起模态交给 pasteEntry 的 dismissOverlays()：所有回贴入口行为一致。
  await pasteEntry(e);
}

/** ⌘E：在加密区内编辑选中条目的备注 / 标签，保存后就地重绘该行。 */
function editVaultNote(): void {
  const e = currentVaultEntry();
  if (!e) return;
  openEditor(e, (dto) => {
    const idx = vaultEntries.findIndex((x) => x.id === dto.id);
    if (idx >= 0) {
      vaultEntries[idx] = dto;
      paintVaultList();
    }
  });
}

function closeVault(): void {
  vaultEl.classList.add("hidden");
  vaultSelectedEl = null;
  searchEl.focus();
}

vaultCloseEl.addEventListener("click", closeVault);
// 加密区条目选中 / 双击回贴：与主列表同样的鼠标语义（单击选中，双击回贴）。
vaultListEl.addEventListener("click", (ev) => {
  const row = (ev.target as HTMLElement).closest<HTMLElement>(".vault-row");
  if (!row) return;
  const idx = Number(row.dataset.index);
  if (!Number.isInteger(idx)) return;
  vaultSelected = idx;
  applyVaultSelectionPatch(idx);
});
vaultListEl.addEventListener("dblclick", (ev) => {
  // 单击已把 vaultSelected 指向被双击的行，这里直接回贴当前选中项。
  const row = (ev.target as HTMLElement).closest<HTMLElement>(".vault-row");
  if (row) vaultSelected = Number(row.dataset.index);
  ev.preventDefault();
  void pasteVaultEntry();
});
vaultClearEl.addEventListener("click", () => {
  void (async () => {
    try {
      const n = await clearUndecryptable();
      toast(`已清除 ${n} 条不可解密条目`);
      await openVault(); // 重新渲染。
    } catch (err) {
      toast(`清除失败：${String(err)}`);
    }
  })();
});

// ----------------------------------------------------------------
// 设置（U12）：透明度滑块 + 启动一次性对齐 CSS 变量
// ----------------------------------------------------------------

/**
 * 把 0-100 百分比透明度即时应用到 `--bg` CSS 变量。
 * `.panel` (`styles.css:49`) 用 `background: var(--bg)`，只动变量就一处生效，
 * `backdrop-filter: blur(20px)` 不被破坏、不触发重排。
 */
function applyPanelOpacity(percent: number): void {
  const alpha = Math.max(0, Math.min(100, percent)) / 100;
  document.documentElement.style.setProperty(
    "--bg",
    `rgba(28, 28, 32, ${alpha})`,
  );
}

/** 打开设置模态，并把 slider 位置 / 文本对齐到当前后端值。 */
async function openSettings(): Promise<void> {
  const v = await getPanelOpacity();
  opacityEl.value = String(v);
  opacityValueEl.textContent = `${v}%`;
  settingsEl.classList.remove("hidden");
  // 渲染快捷键区块（U4）
  void renderHotkeysSection();
  // 焦点交还搜索框：modal 内部无其它交互，esc 关闭即可。
  searchEl.focus();
}

function closeSettings(): void {
  settingsEl.classList.add("hidden");
  searchEl.focus();
}

settingsCloseEl.addEventListener("click", closeSettings);
// `input` 事件即时改 CSS 变量 + 文本预览；`change` 事件再写库，
// 避免拖动过程产生数十次 IPC。
opacityEl.addEventListener("input", () => {
  const v = Number(opacityEl.value);
  opacityValueEl.textContent = `${v}%`;
  applyPanelOpacity(v);
});
opacityEl.addEventListener("change", () => {
  void setPanelOpacity(Number(opacityEl.value));
});

// ----------------------------------------------------------------
// 事件接线
// ----------------------------------------------------------------

searchEl.addEventListener("input", () => {
  if (searchTimer !== undefined) window.clearTimeout(searchTimer);
  searchTimer = window.setTimeout(() => {
    selected = 0;
    void load();
  }, SEARCH_DEBOUNCE_MS);
});

tabsEl.addEventListener("click", (ev) => {
  const btn = (ev.target as HTMLElement).closest<HTMLElement>(".tab");
  if (!btn) return;
  currentTab = btn.dataset.tab as TabId;
  // 切 tab 时清空标签/类别筛选，回到聚合列表。
  selectedTag = null;
  selectedCategory = null;
  selected = 0;
  renderTabs();
  void load();
});

listEl.addEventListener("click", (ev) => {
  const target = ev.target as HTMLElement;

  // 聚合列表 / 返回条（标签、类别 tab）。
  const agg = target.closest<HTMLElement>(".agg-row");
  if (agg) {
    const kind = agg.dataset.kind;
    if (kind === "back") {
      selectedTag = null;
      selectedCategory = null;
      selected = 0;
      void load();
      return;
    }
    const idx = Number(agg.dataset.index);
    if (kind === "tag" && tagList[idx]) {
      selectedTag = tagList[idx].tag;
      selected = 0;
      void load();
    } else if (kind === "category" && catList[idx]) {
      selectedCategory = catList[idx].category;
      selected = 0;
      void load();
    }
    return;
  }

  // 条目行选中。
  const row = target.closest<HTMLElement>(".row");
  if (!row) return;
  selected = Number(row.dataset.index);
  applySelectionPatch(selected);
});

listEl.addEventListener("dblclick", (ev) => {
  // 注意:单击处理器已在此之前重绘过列表(paintEntries 替换 innerHTML),
  // 故 ev.target 可能已是脱离文档的旧节点,不能再靠 closest(".row") 取行。
  // 单击已把 selected 指向被双击的行,这里直接回贴当前选中项即可。
  const row = (ev.target as HTMLElement).closest<HTMLElement>(".row");
  if (row) selected = Number(row.dataset.index);
  ev.preventDefault();
  void activate();
});

// U3: 启动后从 get_hotkeys IPC 拉取快捷键映射。
// 注册 handlers（需要 activate/moveSelection 等 closure）。
registerKeymapHandlers({
  activate,
  moveSelection,
  editNote,
  togglePin,
  moveCurrentToVault,
  openUrlAction,
  openVault,
  toggleGuestMode,
  triggerSkipOnce,
  openSettings,
  closeSettings,
  currentEntry,
});
void loadKeymap();

document.addEventListener("keydown", (ev) => {
  // 编辑器打开时，列表快捷键让位给编辑器（其 Esc / ⌘S 已在 editor.ts 处理）。
  if (isEditorOpen()) return;
  // 加密区模态打开时：handleVaultKey 处理。
  if (isVaultOpen()) {
    if (
      handleVaultKey(ev, {
        closeVault,
        moveVaultSelection,
        pasteVaultEntry,
        editVaultNote,
      })
    )
      return;
    return;
  }
  // 设置模态打开时，仅处理 Esc 关闭(U12)。
  if (isSettingsOpen()) {
    if (ev.key === "Escape") {
      ev.preventDefault();
      closeSettings();
    }
    return;
  }
  // 数据驱动匹配
  const action = matches(ev);
  if (action) {
    ev.preventDefault();
    dispatchHotkeyAction(action);
    return;
  }
  // ⌘1 – ⌘9：直接选中并回贴列表中第 N 条（保留，不在 keymap 中）。
  if (
    (ev.metaKey || ev.ctrlKey) &&
    !ev.shiftKey &&
    !ev.altKey &&
    ev.key >= "1" &&
    ev.key <= "9"
  ) {
    ev.preventDefault();
    void activateIndex(Number(ev.key) - 1);
  }
});

// 面板每次被唤出（窗口获得焦点）时：回到主列表、聚焦搜索框、复检权限、刷新列表。
// 收起加密区模态：面板被隐藏（热键 / 托盘）时模态可能仍处于打开态，而搜索框已获焦点，
// 保持模态会让「看得见的是加密区、敲进去的是搜索框」，故每次唤出都从主列表开始。
window.addEventListener("focus", () => {
  closeVault();
  searchEl.focus();
  searchEl.select();
  void refreshAccessibilityBanner(bannerEl);
  void refreshKeychainBanner(vaultBannerEl);
  void refreshGuestNotice();
  void load();
});

// ----------------------------------------------------------------
// 初始化
// ----------------------------------------------------------------

renderTabs();
searchEl.focus();
// 编辑器 Esc 关闭后把焦点交还搜索框 / 列表。
// 若编辑器是从加密区模态内唤起的（⌘⇧V → ⌘E），焦点留在模态里：搜索框在模态之下。
setEditorCloseHandler(() => {
  if (isVaultOpen()) return;
  searchEl.focus();
});
// U12：托盘「设置…」菜单触发后端 `emit("open-settings")`，前端转呼 openSettings()。
// listen 返回的 Promise unhandled rejection 静默丢弃：失败属异常路径，丢给 window 错误兜底。
void listen("open-settings", () => {
  void openSettings();
});
// U12：启动时一次性把 `--bg` CSS 变量对齐到后端持久化的透明度，
// 即使模态不打开，刷新面板也能直接看到上次设置；getPanelOpacity 失败回退默认 78。
void getPanelOpacity().then(applyPanelOpacity);
// U7：平台标识（`platform_info`）先到位再首屏渲染 —— 底部键帽、行序号徽标（⌘1–9）、
// 各横幅文案都含平台差异，晚一步就会先用 macOS 文案画出来而之后没人重渲染。
// `initPlatform()` 同时会遍历改写 `<kbd>` 键帽；三个 refresh 函数内部也会
// `await whenPlatformReady()`，所以后续的 focus 路径同样安全。
void initPlatform().then(() => {
  void refreshAccessibilityBanner(bannerEl);
  void refreshKeychainBanner(vaultBannerEl);
  void refreshGuestNotice();
  void load();
});
