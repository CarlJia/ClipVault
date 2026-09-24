// ClipVault 备注 / 标签编辑器（U6）。
//
// 一个在浮层内切换可见性的紧凑模态：多行备注（2000 字符上限 + 计数器）、
// 标签 chip 输入（Enter 添加 / Backspace 删除末尾）。内置保存状态机
//   idle → dirty → saving → saved → error
// ⌘S 立即保存；输入 / 失焦触发 800ms 防抖保存；saved 显示勾号 1.5s 后回
// idle；error 显示红色 toast + 重试按钮；单次保存 5s 无响应自动重试一次。
//
// 保存成功后通过 onSaved 回调把刷新后的 EntryDto 交回 main.ts 重绘对应行。

import { invoke } from "@tauri-apps/api/core";
import { type EntryDto, escapeHtml } from "./panel";

const MAX_NOTE = 2000;
const DEBOUNCE_MS = 800;
const SAVED_MS = 1500;
const TIMEOUT_MS = 5000;

type SaveState = "idle" | "dirty" | "saving" | "saved" | "error";

// —— DOM —— //
const rootEl = document.querySelector<HTMLDivElement>("#editor")!;
const stateEl = document.querySelector<HTMLSpanElement>("#editor-state")!;
const previewEl = document.querySelector<HTMLDivElement>("#editor-preview")!;
const noteEl = document.querySelector<HTMLTextAreaElement>("#editor-note")!;
const counterEl = document.querySelector<HTMLDivElement>("#editor-counter")!;
const chipsEl = document.querySelector<HTMLDivElement>("#editor-chips")!;
const tagInputEl = document.querySelector<HTMLInputElement>("#editor-tag-input")!;
const retryEl = document.querySelector<HTMLButtonElement>("#editor-retry")!;

// —— 状态 —— //
let open = false;
let currentId: number | null = null;
let tags: string[] = [];
let state: SaveState = "idle";
let onSavedCb: ((dto: EntryDto) => void) | null = null;

let debounceTimer: number | undefined;
let savedTimer: number | undefined;
// 保存序号：递增后使更早保存尝试的迟到结果失效（防竞态）。
let saveSeq = 0;

export function isEditorOpen(): boolean {
  return open;
}

/** 打开编辑器，载入某条目的备注 / 标签。 */
export function openEditor(entry: EntryDto, onSaved: (dto: EntryDto) => void): void {
  currentId = entry.id;
  onSavedCb = onSaved;
  tags = [...entry.tags];
  noteEl.value = entry.note;
  previewEl.textContent = entry.content;
  updateCounter();
  renderChips();
  clearTimers();
  saveSeq += 1; // 作废任何在途保存
  setState("idle");
  open = true;
  rootEl.classList.remove("hidden");
  // 聚焦备注框，光标置末尾。
  window.requestAnimationFrame(() => {
    noteEl.focus();
    noteEl.setSelectionRange(noteEl.value.length, noteEl.value.length);
  });
}

/** 关闭编辑器；若有未落盘的编辑则先冲刷保存。返回焦点回列表由 main.ts 负责。 */
export function closeEditor(): void {
  if (!open) return;
  if (state === "dirty") flushSave();
  open = false;
  rootEl.classList.add("hidden");
  clearTimers();
}

// ----------------------------------------------------------------
// 保存状态机
// ----------------------------------------------------------------

function clearTimers(): void {
  if (debounceTimer !== undefined) window.clearTimeout(debounceTimer);
  if (savedTimer !== undefined) window.clearTimeout(savedTimer);
  debounceTimer = undefined;
  savedTimer = undefined;
}

function setState(next: SaveState): void {
  state = next;
  rootEl.dataset.state = next;
  const labels: Record<SaveState, string> = {
    idle: "",
    dirty: "● 未保存",
    saving: "正在保存…",
    saved: "✓ 已保存",
    error: "保存失败",
  };
  stateEl.textContent = labels[next];
  stateEl.className = `editor-state state-${next}`;
  retryEl.classList.toggle("hidden", next !== "error");
}

/** 标记为脏并安排一次防抖保存。 */
function markDirty(): void {
  if (savedTimer !== undefined) {
    window.clearTimeout(savedTimer);
    savedTimer = undefined;
  }
  setState("dirty");
  if (debounceTimer !== undefined) window.clearTimeout(debounceTimer);
  debounceTimer = window.setTimeout(() => void flushSave(), DEBOUNCE_MS);
}

/** 立即触发保存（⌘S / 失焦 / 关闭时使用）。 */
function flushSave(): void {
  if (debounceTimer !== undefined) {
    window.clearTimeout(debounceTimer);
    debounceTimer = undefined;
  }
  if (currentId === null) return;
  if (state !== "dirty" && state !== "error") return;
  runSave();
}

function runSave(): void {
  const seq = ++saveSeq;
  const id = currentId!;
  const note = noteEl.value;
  const payloadTags = [...tags];
  setState("saving");

  let autoRetried = false;

  const attempt = (): void => {
    let settled = false;
    const to = window.setTimeout(() => {
      if (settled || seq !== saveSeq) return;
      if (!autoRetried) {
        autoRetried = true; // 5s 无响应，自动重试一次
        attempt();
      } else {
        setState("error");
      }
    }, TIMEOUT_MS);

    invoke<EntryDto>("update_entry", { id, note, tags: payloadTags })
      .then((dto) => {
        settled = true;
        window.clearTimeout(to);
        if (seq !== saveSeq) return; // 已被更新的保存取代
        onSavedCb?.(dto);
        // 后端归一化后的标签回填（trim/去重）。
        tags = [...dto.tags];
        renderChips();
        setState("saved");
        savedTimer = window.setTimeout(() => {
          if (state === "saved") setState("idle");
        }, SAVED_MS);
      })
      .catch(() => {
        settled = true;
        window.clearTimeout(to);
        if (seq !== saveSeq) return;
        setState("error");
      });
  };

  attempt();
}

// ----------------------------------------------------------------
// 备注 / 计数器
// ----------------------------------------------------------------

function updateCounter(): void {
  const n = [...noteEl.value].length;
  counterEl.textContent = `${n} / ${MAX_NOTE}`;
  counterEl.classList.toggle("at-cap", n >= MAX_NOTE);
}

// ----------------------------------------------------------------
// 标签 chips
// ----------------------------------------------------------------

function renderChips(): void {
  // 清掉已有 chip（保留 input），再按序插入。
  chipsEl.querySelectorAll(".editor-chip").forEach((el) => el.remove());
  tags.forEach((t, i) => {
    const chip = document.createElement("span");
    chip.className = "editor-chip";
    chip.dataset.index = String(i);
    chip.innerHTML = `${escapeHtml(t)}<button class="chip-x" data-index="${i}" title="删除">×</button>`;
    chipsEl.insertBefore(chip, tagInputEl);
  });
}

function addTag(raw: string): void {
  const t = raw.trim();
  if (t === "") return;
  if (tags.includes(t)) return; // 集合去重
  tags.push(t);
  renderChips();
  markDirty();
}

function removeTagAt(i: number): void {
  if (i < 0 || i >= tags.length) return;
  tags.splice(i, 1);
  renderChips();
  markDirty();
}

// ----------------------------------------------------------------
// 事件接线
// ----------------------------------------------------------------

noteEl.addEventListener("input", () => {
  updateCounter();
  markDirty();
});
noteEl.addEventListener("blur", () => {
  if (open) flushSave();
});

tagInputEl.addEventListener("keydown", (ev) => {
  if (ev.key === "Enter") {
    ev.preventDefault();
    addTag(tagInputEl.value);
    tagInputEl.value = "";
  } else if (ev.key === "Backspace" && tagInputEl.value === "") {
    // 输入框空时 Backspace 删除末尾 chip。
    ev.preventDefault();
    removeTagAt(tags.length - 1);
  }
});
tagInputEl.addEventListener("blur", () => {
  // 失焦时把残留输入固化为一个标签，再冲刷保存。
  if (!open) return;
  if (tagInputEl.value.trim() !== "") {
    addTag(tagInputEl.value);
    tagInputEl.value = "";
  }
  flushSave();
});

chipsEl.addEventListener("click", (ev) => {
  const x = (ev.target as HTMLElement).closest<HTMLElement>(".chip-x");
  if (!x) return;
  removeTagAt(Number(x.dataset.index));
  tagInputEl.focus();
});

retryEl.addEventListener("click", () => {
  runSave();
});

// 编辑器自身的键盘处理（⌘S 保存 / Esc 关闭）。用捕获阶段确保先于
// main.ts 的全局 keydown 处理，且只在编辑器打开时生效。
rootEl.addEventListener("keydown", (ev) => {
  if (!open) return;
  if ((ev.metaKey || ev.ctrlKey) && (ev.key === "s" || ev.key === "S")) {
    ev.preventDefault();
    ev.stopPropagation();
    if (state === "error") runSave();
    else flushSave();
    return;
  }
  if (ev.key === "Escape") {
    ev.preventDefault();
    ev.stopPropagation();
    closeEditor();
    onCloseCb?.();
  }
});

// Esc 关闭后把焦点交还列表：由 main.ts 注入回调。
let onCloseCb: (() => void) | null = null;
export function setEditorCloseHandler(cb: () => void): void {
  onCloseCb = cb;
}
