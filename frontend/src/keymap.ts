// frontend/src/keymap.ts
//
// U3: 数据驱动的快捷键 dispatch。
// - 类型镜像 `clipvault_core::keymap`
// - KeyMatcher.matches(ev) -> HotkeyAction | null  查表派发
// - parseKeyCombo(ev) -> KeyCombo | null  反向解析（录制态用）
// - loadKeymap() / reloadKeymap()  从 get_hotkeys IPC 拉取
// - dispatchHotkeyAction(action)  单一派发入口
// - handleVaultKey(ev)  加密区模态内 keydown（保留原有逻辑）

import { invoke } from "@tauri-apps/api/core";
import { keys } from "./platform";

// ---------------------------------------------------------------------------
// TS 类型 —— 与 clipvault_core::keymap 一一对应
// ---------------------------------------------------------------------------

export type Mod = "meta" | "shift" | "alt" | "ctrl";

export type Key =
  | { kind: "char"; ch: string }
  | { kind: "enter" }
  | { kind: "escape" }
  | { kind: "arrow_up" }
  | { kind: "arrow_down" }
  | { kind: "comma" };

export interface KeyCombo {
  mods: Mod[];
  key: Key;
}

export type HotkeyAction =
  | "open_panel"
  | "paste"
  | "edit_note"
  | "toggle_pin"
  | "move_to_vault"
  | "open_url"
  | "open_vault"
  | "toggle_guest_mode"
  | "skip_once"
  | "open_settings"
  | "close"
  | "move_up"
  | "move_down";

export interface HotkeyMap {
  actions: Record<HotkeyAction, KeyCombo>;
}

// ---------------------------------------------------------------------------
// Module-level 状态
// ---------------------------------------------------------------------------

// 启动时为空；loadKeymap() / reloadKeymap() 填充后生效。
export let currentKeyMap: HotkeyMap | null = null;

// ---------------------------------------------------------------------------
// parseKeyCombo — 从 KeyboardEvent 反向构建 KeyCombo
// 用于前端录制态（U4），也用于 validate。
// ---------------------------------------------------------------------------

/**
 * 把 KeyboardEvent 解析为 KeyCombo。
 * - modifier-only（Meta/Control/Shift/Alt）→ 返回 null（拒绝）。
 * - 其余情况返回 { mods, key }。
 */
export function parseKeyCombo(ev: KeyboardEvent): KeyCombo | null {
  // 拒绝纯修饰键
  if (
    (ev.metaKey || ev.ctrlKey || ev.shiftKey || ev.altKey) &&
    ["Meta", "Control", "Shift", "Alt"].includes(ev.key)
  ) {
    return null;
  }

  const mods: Mod[] = [];
  // cross-platform 规则：meta = Mac Cmd 或 Win Ctrl
  if (ev.metaKey || ev.ctrlKey) mods.push("meta");
  if (ev.shiftKey) mods.push("shift");
  if (ev.altKey) mods.push("alt");
  // 仅 Win Ctrl（不含 Mac meta）的场景：add "ctrl" separately so the list has both
  // when ev.ctrlKey && !ev.metaKey — needed when user binds "Ctrl+X" explicitly
  if (ev.ctrlKey && !ev.metaKey) mods.push("ctrl");

  const key = parseKeyKind(ev.key);
  if (!key) return null; // 不认识的键

  return { mods, key };
}

function parseKeyKind(key: string): Key | null {
  switch (key) {
    case "Enter":
      return { kind: "enter" };
    case "Escape":
      return { kind: "escape" };
    case "ArrowUp":
      return { kind: "arrow_up" };
    case "ArrowDown":
      return { kind: "arrow_down" };
    case ",":
      return { kind: "comma" };
    default:
      if (key.length === 1) return { kind: "char", ch: key.toLowerCase() };
      return null;
  }
}

// ---------------------------------------------------------------------------
// KeyMatcher — 数据驱动的 keydown 匹配
// ---------------------------------------------------------------------------

/**
 * 检查一个 KeyboardEvent 是否匹配给定的 KeyCombo。
 * - mods：event 必须有 ALL listed mods，且不能有额外的 mods（但 meta||ctrl cross-platform 规则除外）。
 * - key：按 CharKind 分类匹配。
 */
function comboMatches(combo: KeyCombo, ev: KeyboardEvent): boolean {
  // Modifiers
  const evMeta = ev.metaKey || ev.ctrlKey; // cross-platform: Win Ctrl 当 Mac Cmd
  const evCtrl = ev.ctrlKey && !ev.metaKey; // 仅 Win Ctrl（不含 Win Meta）
  const evShift = ev.shiftKey;
  const evAlt = ev.altKey;

  const wantMeta = combo.mods.includes("meta");
  const wantShift = combo.mods.includes("shift");
  const wantAlt = combo.mods.includes("alt");
  const wantCtrl = combo.mods.includes("ctrl");

  // 空 mods → event 必须没有任何修饰键
  if (combo.mods.length === 0) {
    if (evMeta || evCtrl || evShift || evAlt) return false;
  } else {
    // ALL wanted mods must be present
    if (wantMeta && !evMeta) return false;
    if (wantShift && !evShift) return false;
    if (wantAlt && !evAlt) return false;
    if (wantCtrl && !evCtrl) return false;
    // NO extra unwanted mods (except cross-platform tolerance)
    // 如果 combo 里没有 meta，但 event 有 meta||ctrl 算不算 extra？
    // 按 spec：combo.mods 是 wanted list；extra mod = 不在 list 里的 mod。
    // cross-platform 规则："meta" 在 combo 里时，ev.metaKey||ev.ctrlKey 都接受。
    // 如果 combo 里没有 meta，但 event 有 meta||ctrl，那是 extra（不应该匹配纯 ctrl）
    if (!wantMeta && evMeta) return false;
    if (!wantShift && evShift) return false;
    if (!wantAlt && evAlt) return false;
    // 纯 ctrl（不在 combo 的 meta 里）：只有 combo 明确要 "ctrl" 才匹配
    if (!wantCtrl && evCtrl) return false;
  }

  // Key
  return keyMatches(combo.key, ev.key);
}

function keyMatches(key: Key, evKey: string): boolean {
  switch (key.kind) {
    case "char":
      return evKey === key.ch || evKey === key.ch.toUpperCase();
    case "enter":
      return evKey === "Enter";
    case "escape":
      return evKey === "Escape";
    case "arrow_up":
      return evKey === "ArrowUp";
    case "arrow_down":
      return evKey === "ArrowDown";
    case "comma":
      return evKey === ",";
  }
}

/**
 * 用当前 in-memory currentKeyMap 匹配 KeyboardEvent。
 * 返回第一个命中的 HotkeyAction，或 null。
 */
export function matches(ev: KeyboardEvent): HotkeyAction | null {
  if (!currentKeyMap) return null;
  for (const [action, combo] of Object.entries(currentKeyMap.actions) as [
    HotkeyAction,
    KeyCombo,
  ][]) {
    if (comboMatches(combo, ev)) return action;
  }
  return null;
}

// ---------------------------------------------------------------------------
// loadKeymap / reloadKeymap — IPC 拉取
// ---------------------------------------------------------------------------

/**
 * 启动时调用一次：从 get_hotkeys IPC 拉取 HotkeyMap。
 * 失败时保留既有值（不静默回落），console.warn。
 */
export async function loadKeymap(): Promise<void> {
  try {
    // get_hotkeys 返回 JSON 字符串
    const json = await invoke<string>("get_hotkeys");
    currentKeyMap = JSON.parse(json) as HotkeyMap;
  } catch (err) {
    console.warn("[keymap] loadKeymap 失败，保留既有映射：", err);
  }
}

/**
 * reloadKeymap：与 loadKeymap 相同，供 U4 在 set_hotkey 成功后调用以刷新 in-memory。
 */
export async function reloadKeymap(): Promise<void> {
  await loadKeymap();
}

// ---------------------------------------------------------------------------
// setHotkey — IPC 写 + 返回 discriminated union（U4）
// ---------------------------------------------------------------------------

/** 将 KeyCombo 序列化为后端 set_hotkey 期望的 JSON 结构。 */
function serializeComboForBackend(combo: KeyCombo): { mods: string[]; key: string | null } {
  const key = (() => {
    switch (combo.key.kind) {
      case "char":
        return combo.key.ch;
      case "enter":
        return "enter";
      case "escape":
        return "escape";
      case "arrow_up":
        return "arrow_up";
      case "arrow_down":
        return "arrow_down";
      case "comma":
        return "comma";
    }
  })();
  return { mods: combo.mods as string[], key };
}

/**
 * 调用 set_hotkey IPC。
 * 返回 { ok: true } | { ok: false, err: string }。
 */
export async function setHotkey(
  action: HotkeyAction,
  combo: KeyCombo,
): Promise<{ ok: true } | { ok: false; err: string }> {
  try {
    await invoke("set_hotkey", {
      action,
      combo: serializeComboForBackend(combo),
    });
    return { ok: true };
  } catch (err) {
    return { ok: false, err: String(err) };
  }
}

// ---------------------------------------------------------------------------
// localizedCombo — 用于 <kbd> 显示（U4）
// ---------------------------------------------------------------------------

/**
 * 将 KeyCombo 渲染为平台化的按键说明字符串（如 `⌘+Shift+E` / `Ctrl+Shift+E`）。
 * 先用 Mac 字形构建，再过 `keys()` 做 Windows 替换。
 */
export function localizedCombo(combo: KeyCombo): string {
  const modStr =
    combo.mods.length === 0
      ? ""
      : combo.mods
          .map((m) => {
            switch (m) {
              case "meta":
                return "⌘";
              case "shift":
                return "⇧";
              case "alt":
                return "⌥";
              case "ctrl":
                return "⌃";
            }
          })
          .join("+") + "+";

  const keyStr = (() => {
    switch (combo.key.kind) {
      case "char":
        return combo.key.ch.toUpperCase();
      case "enter":
        return "↩";
      case "escape":
        return "esc";
      case "arrow_up":
        return "↑";
      case "arrow_down":
        return "↓";
      case "comma":
        return ",";
    }
  })();

  return keys(modStr + keyStr);
}

// ---------------------------------------------------------------------------
// dispatchHotkeyAction — 单一派发入口
// ---------------------------------------------------------------------------

// 导入 main.ts 中定义的 handler（单向依赖：keymap.ts 不 import main.ts，
// 而是让 main.ts 在调用处传进来，这样可以避免循环 import）。
// 但实际上 dispatch 需要调用 activate/moveSelection/editNote 等，所以我们在
// 这里声明需要从 main.ts 注入的 handler map。
type Handlers = {
  activate: () => Promise<void>;
  moveSelection: (delta: number) => void;
  editNote: () => void;
  togglePin: () => Promise<void>;
  moveCurrentToVault: () => Promise<void>;
  openUrlAction: (e: import("./panel").EntryDto) => Promise<void>;
  openVault: () => Promise<void>;
  toggleGuestMode: () => Promise<void>;
  triggerSkipOnce: () => Promise<void>;
  openSettings: () => Promise<void>;
  closeSettings: () => void;
  currentEntry: () => import("./panel").EntryDto | undefined;
};

let handlers: Handlers | null = null;

/**
 * 注入 main.ts 的 handler 集（在 main.ts 初始化时调用）。
 */
export function registerKeymapHandlers(h: Handlers): void {
  handlers = h;
}

/**
 * 根据 HotkeyAction 派发到对应 handler。
 */
export function dispatchHotkeyAction(action: HotkeyAction): void {
  if (!handlers) {
    console.error("[keymap] handlers 未注册，无法派发 action");
    return;
  }
  switch (action) {
    case "close":
      void invoke("hide_panel");
      break;
    case "paste":
      void handlers.activate();
      break;
    case "move_up":
      handlers.moveSelection(-1);
      break;
    case "move_down":
      handlers.moveSelection(1);
      break;
    case "edit_note":
      handlers.editNote();
      break;
    case "toggle_pin":
      void handlers.togglePin();
      break;
    case "move_to_vault":
      void handlers.moveCurrentToVault();
      break;
    case "open_url": {
      const e = handlers.currentEntry();
      if (e) void handlers.openUrlAction(e);
      break;
    }
    case "open_vault":
      void handlers.openVault();
      break;
    case "toggle_guest_mode":
      void handlers.toggleGuestMode();
      break;
    case "skip_once":
      void handlers.triggerSkipOnce();
      break;
    case "open_settings":
      void handlers.openSettings();
      break;
    case "open_panel":
      // 全局热键由 Rust 端处理，前端 panel 内此 action 是 no-op
      break;
  }
}

// ---------------------------------------------------------------------------
// handleVaultKey — 加密区模态内 keydown
// 逻辑与原有 main.ts:653-685 完全一致，保留以便 keydown listener 调用。
// ---------------------------------------------------------------------------

export function handleVaultKey(
  ev: KeyboardEvent,
  ctx: {
    closeVault: () => void;
    moveVaultSelection: (delta: number) => void;
    pasteVaultEntry: () => Promise<void>;
    editVaultNote: () => void;
  },
): boolean {
  if (ev.key === "Escape") {
    ev.preventDefault();
    ctx.closeVault();
    return true;
  }
  if (ev.key === "ArrowDown") {
    ev.preventDefault();
    ctx.moveVaultSelection(1);
    return true;
  }
  if (ev.key === "ArrowUp") {
    ev.preventDefault();
    ctx.moveVaultSelection(-1);
    return true;
  }
  if (ev.key === "Enter") {
    ev.preventDefault();
    void ctx.pasteVaultEntry();
    return true;
  }
  if ((ev.metaKey || ev.ctrlKey) && (ev.key === "e" || ev.key === "E")) {
    ev.preventDefault();
    ctx.editVaultNote();
    return true;
  }
  // ⌘⇧V：再次按下收起加密区模态
  if ((ev.metaKey || ev.ctrlKey) && ev.shiftKey && (ev.key === "v" || ev.key === "V")) {
    ev.preventDefault();
    ctx.closeVault();
    return true;
  }
  return false;
}
