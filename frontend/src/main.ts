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
} from "./panel";
import { refreshAccessibilityBanner } from "./settings";
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

const searchEl = document.querySelector<HTMLInputElement>("#search")!;
const tabsEl = document.querySelector<HTMLDivElement>("#tabs")!;
const listEl = document.querySelector<HTMLDivElement>("#list")!;
const bannerEl = document.querySelector<HTMLDivElement>("#banner")!;
const progressEl = document.querySelector<HTMLDivElement>("#progress")!;
const toastEl = document.querySelector<HTMLDivElement>("#toast")!;

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
    const r = await invoke<string>("paste_entry", { id: e.id });
    if (r.startsWith("unimplemented")) {
      toast("回贴功能将在 U8 接入");
    } else {
      await invoke("hide_panel");
    }
  } catch (err) {
    toast(`回贴失败：${String(err)}`);
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
  await invoke<string>("toggle_pin", { id: e.id }).catch(() => "");
  toast("置顶将在 U7 接入");
}

function moveSelection(delta: number): void {
  if (!showingEntries() || entries.length === 0) return;
  selected = Math.min(Math.max(selected + delta, 0), entries.length - 1);
  paintEntries();
  scrollToSelected();
}

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
  if ((ev.metaKey || ev.ctrlKey) && (ev.key === "e" || ev.key === "E")) {
    ev.preventDefault();
    editNote();
    return;
  }
  if ((ev.metaKey || ev.ctrlKey) && (ev.key === "p" || ev.key === "P")) {
    ev.preventDefault();
    void togglePin();
  }
});

// 面板每次被唤出（窗口获得焦点）时：聚焦搜索框、复检权限、刷新列表。
window.addEventListener("focus", () => {
  searchEl.focus();
  searchEl.select();
  void refreshAccessibilityBanner(bannerEl);
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
void load();
