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
  renderTags,
} from "./panel";
import { refreshAccessibilityBanner } from "./settings";

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
let selected = 0;
let searchTimer: number | undefined;
let toastTimer: number | undefined;

const searchEl = document.querySelector<HTMLInputElement>("#search")!;
const tabsEl = document.querySelector<HTMLDivElement>("#tabs")!;
const listEl = document.querySelector<HTMLDivElement>("#list")!;
const bannerEl = document.querySelector<HTMLDivElement>("#banner")!;
const progressEl = document.querySelector<HTMLDivElement>("#progress")!;
const toastEl = document.querySelector<HTMLDivElement>("#toast")!;

const isEntryTab = () => currentTab === "recent" || currentTab === "all";
const currentEntry = (): EntryDto | undefined =>
  selected >= 0 ? entries[selected] : undefined;

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

async function load(): Promise<void> {
  showProgress(true);
  try {
    if (currentTab === "tag") {
      const tags = await invoke<TagInfo[]>("get_all_tags");
      renderTags(listEl, tags);
      selected = -1;
    } else if (currentTab === "category") {
      const cats = await invoke<CategoryInfo[]>("get_categories");
      renderCategories(listEl, cats);
      selected = -1;
    } else {
      const query = searchEl.value.trim();
      entries = await invoke<EntryDto[]>("search_results", {
        query,
        tab: currentTab,
        limit: LIMIT,
      });
      if (selected >= entries.length) selected = entries.length - 1;
      if (selected < 0 && entries.length > 0) selected = 0;
      renderEntries(listEl, entries, selected);
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

async function editNote(): Promise<void> {
  const e = currentEntry();
  if (!e) return;
  await invoke<string>("open_note_editor", { id: e.id }).catch(() => "");
  toast("备注 / 标签编辑将在 U6 接入");
}

async function togglePin(): Promise<void> {
  const e = currentEntry();
  if (!e) return;
  await invoke<string>("toggle_pin", { id: e.id }).catch(() => "");
  toast("置顶将在 U7 接入");
}

function moveSelection(delta: number): void {
  if (!isEntryTab() || entries.length === 0) return;
  selected = Math.min(Math.max(selected + delta, 0), entries.length - 1);
  renderEntries(listEl, entries, selected);
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
  selected = 0;
  renderTabs();
  void load();
});

listEl.addEventListener("click", (ev) => {
  const row = (ev.target as HTMLElement).closest<HTMLElement>(".row");
  if (!row) return;
  selected = Number(row.dataset.index);
  renderEntries(listEl, entries, selected);
});

listEl.addEventListener("dblclick", (ev) => {
  const row = (ev.target as HTMLElement).closest<HTMLElement>(".row");
  if (!row) return;
  selected = Number(row.dataset.index);
  void activate();
});

document.addEventListener("keydown", (ev) => {
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
    void editNote();
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
void refreshAccessibilityBanner(bannerEl);
void load();
