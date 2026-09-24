// ClipVault 面板控制器（U4）。
//
// 负责：调用 IPC → 渲染列表、tab 切换、键盘导航（↑↓/Enter/⌘E/⌘P/Esc）、
// 100ms 防抖搜索、加载态（保留上一帧 + 顶部进度条）。
//
// 后续单元接管的动作在此仅调用类型化 IPC 桩并给出 toast 提示：
// Enter→paste_entry(U8)、⌘E→open_note_editor(U6)、⌘P→toggle_pin(U7)。

import "./styles.css";
import { invoke } from "@tauri-apps/api/core";
import {
  type CategoryInfo,
  type EntryDto,
  type TagInfo,
  renderCategories,
  renderEntries,
  renderFilterHeader,
  renderTags,
  renderVaultList,
} from "./panel";
import {
  type PasteResult,
  clearUndecryptable,
  getGuestMode,
  listVault,
  moveToVault,
  refreshAccessibilityBanner,
  refreshKeychainBanner,
  setGuestMode,
  skipNextCopy,
} from "./settings";
import {
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

const isVaultOpen = (): boolean => !vaultEl.classList.contains("hidden");

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

/**
 * 刷新访客模式提示条（U10 / R10）：开启时在面板顶部显示一条常驻提示，
 * 说明「不记录新条目、已有条目只读可访问」，从而在「最近」等视图里对
 * 缺失的新条目做出解释（计划：已有条目保留可访问，只是不出现新条目）。
 * 同步内部 `guestMode` 状态。
 */
async function refreshGuestNotice(): Promise<void> {
  guestMode = await getGuestMode();
  if (guestMode) {
    guestNoticeEl.classList.remove("hidden");
    guestNoticeEl.innerHTML = `
      <span class="banner-icon">􀩦</span>
      <div class="banner-body">
        <div class="banner-title">访客模式 · 不记录新条目</div>
        <div class="banner-sub">新复制的内容不会入库；已有条目仍只读可访问。⌘⇧G 关闭。</div>
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
  listEl.querySelector<HTMLElement>(".row.selected")?.scrollIntoView({
    block: "nearest",
  });
}

/** 渲染条目列表；筛选态下在顶部补一行「‹ 返回」条。 */
function paintEntries(): void {
  renderEntries(listEl, entries, selected);
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

async function load(): Promise<void> {
  showProgress(true);
  try {
    if (currentTab === "tag" && selectedTag === null) {
      tagList = await invoke<TagInfo[]>("get_all_tags");
      renderTags(listEl, tagList);
      selected = -1;
    } else if (currentTab === "category" && selectedCategory === null) {
      catList = await invoke<CategoryInfo[]>("get_categories");
      renderCategories(listEl, catList);
      selected = -1;
    } else {
      // 条目列表：recent / all / 已选标签 / 已选类别。
      const query = searchEl.value.trim();
      entries = await invoke<EntryDto[]>("search_results", {
        query,
        tab: tabArg(),
        limit: LIMIT,
      });
      if (selected >= entries.length) selected = entries.length - 1;
      if (selected < 0 && entries.length > 0) selected = 0;
      paintEntries();
      scrollToSelected();
    }
  } catch (e) {
    // 保留上一帧不动，仅在完全无内容时提示。
    if (listEl.childElementCount === 0) {
      listEl.innerHTML = `<div class="empty">加载失败：${String(e)}</div>`;
    }
    toast(`加载失败：${String(e)}`);
  } finally {
    showProgress(false);
  }
}

// ----------------------------------------------------------------
// 动作（Enter / ⌘E / ⌘P）—— 调用后续单元的 IPC 桩
// ----------------------------------------------------------------

async function activate(): Promise<void> {
  const e = currentEntry();
  if (!e) return;
  try {
    const r = await invoke<PasteResult>("paste_entry", { id: e.id });
    if (r.status === "pasted") {
      // 回贴成功:后端已隐藏面板并把 ⌘V 落到目标应用,无需额外提示。
      await invoke("hide_panel");
    } else {
      // window_switched / clipboard_only:内容已在剪贴板,后端已重新显示面板,
      // 展示固定提示引导用户手动 ⌘V。
      toast(r.message);
    }
  } catch (err) {
    const msg = String(err);
    if (msg.includes("missing-accessibility")) {
      // AE7:未授权 —— 展示 Accessibility 横幅,不隐藏面板。
      await refreshAccessibilityBanner(bannerEl);
      toast("需要「辅助功能」授权才能自动回贴");
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
    await invoke<EntryDto>("toggle_pin", { id: toggledId });
    await load();
    // 若被切换的条目仍在可见列表内，保持选中它。
    const idx = entries.findIndex((x) => x.id === toggledId);
    if (idx >= 0) {
      selected = idx;
      paintEntries();
      scrollToSelected();
    }
  } catch (err) {
    toast(`置顶失败：${String(err)}`);
  }
}

function moveSelection(delta: number): void {
  if (!showingEntries() || entries.length === 0) return;
  selected = Math.min(Math.max(selected + delta, 0), entries.length - 1);
  paintEntries();
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
    renderVaultList(vaultListEl, items);
    const undec = items.filter((x) => !x.is_decryptable).length;
    vaultSubEl.textContent =
      items.length === 0
        ? "把敏感条目移入加密区（列表中按 ⌘L）后，内容以 AES-256-GCM 加密存储。"
        : `共 ${items.length} 条${undec > 0 ? ` · ${undec} 条无法解密` : ""}`;
    vaultClearEl.classList.toggle("hidden", undec === 0);
  } catch (err) {
    vaultListEl.innerHTML = `<div class="empty">加载加密区失败：${String(err)}</div>`;
  }
}

function closeVault(): void {
  vaultEl.classList.add("hidden");
  searchEl.focus();
}

vaultCloseEl.addEventListener("click", closeVault);
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
  paintEntries();
});

listEl.addEventListener("dblclick", (ev) => {
  const row = (ev.target as HTMLElement).closest<HTMLElement>(".row");
  if (!row) return;
  selected = Number(row.dataset.index);
  void activate();
});

document.addEventListener("keydown", (ev) => {
  // 编辑器打开时，列表快捷键让位给编辑器（其 Esc / ⌘S 已在 editor.ts 处理）。
  if (isEditorOpen()) return;
  // 加密区模态打开时，仅处理 Esc 关闭。
  if (isVaultOpen()) {
    if (ev.key === "Escape") {
      ev.preventDefault();
      closeVault();
    }
    return;
  }
  if (ev.key === "Escape") {
    ev.preventDefault();
    void invoke("hide_panel");
    return;
  }
  if (ev.key === "ArrowDown") {
    ev.preventDefault();
    moveSelection(1);
    return;
  }
  if (ev.key === "ArrowUp") {
    ev.preventDefault();
    moveSelection(-1);
    return;
  }
  if (ev.key === "Enter") {
    ev.preventDefault();
    void activate();
    return;
  }
  // ⌘⇧V：打开加密区管理（放在 ⌘E 之前判断，避免与其它组合冲突）。
  if ((ev.metaKey || ev.ctrlKey) && ev.shiftKey && (ev.key === "v" || ev.key === "V")) {
    ev.preventDefault();
    void openVault();
    return;
  }
  // ⌘⇧G：切换访客模式（U10 / R10）。
  if ((ev.metaKey || ev.ctrlKey) && ev.shiftKey && (ev.key === "g" || ev.key === "G")) {
    ev.preventDefault();
    void toggleGuestMode();
    return;
  }
  // ⌘⇧N：本次不记录（U10 / R11），一次性。
  if ((ev.metaKey || ev.ctrlKey) && ev.shiftKey && (ev.key === "n" || ev.key === "N")) {
    ev.preventDefault();
    void triggerSkipOnce();
    return;
  }
  if ((ev.metaKey || ev.ctrlKey) && (ev.key === "e" || ev.key === "E")) {
    ev.preventDefault();
    editNote();
    return;
  }
  if ((ev.metaKey || ev.ctrlKey) && (ev.key === "p" || ev.key === "P")) {
    ev.preventDefault();
    void togglePin();
    return;
  }
  // ⌘L：把当前条目移入加密区。
  if ((ev.metaKey || ev.ctrlKey) && !ev.shiftKey && (ev.key === "l" || ev.key === "L")) {
    ev.preventDefault();
    void moveCurrentToVault();
  }
});

// 面板每次被唤出（窗口获得焦点）时：聚焦搜索框、复检权限、刷新列表。
window.addEventListener("focus", () => {
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
setEditorCloseHandler(() => {
  searchEl.focus();
});
void refreshAccessibilityBanner(bannerEl);
void refreshKeychainBanner(vaultBannerEl);
void refreshGuestNotice();
void load();
