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
import { isWindows, keys } from "./platform";

// ---------------------------------------------------------------------------
// TS 类型 —— 与 clipvault_core::keymap 一一对应
// ---------------------------------------------------------------------------
//
// **两套表示，别混用**：
// - 线格式（`Wire*`）：`get_hotkeys` / `set_hotkey` 走的 JSON，`key` 是**字符串**
//   （`"e"` / `"enter"` / `"arrow_up"` …），由 `clipvault_core::keymap::serialize` 产出。
// - 内部表示（`Key` / `KeyCombo`）：`key` 是带标签的联合类型，`matches` /
//   `localizedCombo` / 录制态都用它。
// 两者只在 `loadKeymap` / `serializeComboForBackend` 两个边界上转换。

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

/** 线格式 combo：`key` 为字符串（见上）。 */
interface WireCombo {
  mods: Mod[];
  key: string;
}

/**
 * 线格式 HotkeyMap：`actions` 按 action 字符串索引。
 *
 * 某条 action 的值是 `null` → 该 action **未绑定**（用户「强行换绑」后原来占着这个
 * 组合的那条会变成这样）。注意与 `{ mods: [], key: null }` 区分：后者是非法 entry
 * （modifier-only），后端 `parse` 会拒绝整份 JSON。
 *
 * 未知 action 会被原样保留（`dispatchHotkeyAction` 的 switch 没有对应 case，
 * 派发时静默跳过）—— 「未知 action 丢 silently」是后端 `clipvault_core::keymap::parse`
 * 的行为，前端这一层只做表示转换，不做过滤。
 */
interface WireHotkeyMap {
  version: number;
  actions: Record<string, WireCombo | null>;
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
  /** `null` = 该 action 未绑定（没有快捷键）。 */
  actions: Record<HotkeyAction, KeyCombo | null>;
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
 * 把 KeyboardEvent 的修饰键解析成四个平台无关的布尔。
 *
 * **平台主修饰键**：macOS 是 Cmd(meta)、Windows 是 Ctrl，所以「Ctrl ≡ meta」的
 * 等价规则只在 Windows 上成立；macOS 的 Control 是独立修饰键（plan AE1 的 ⌃⌥L，
 * 无条件合并会把 ⌃⌥L 记成 ⌘⌥⌃L）。
 *
 * `parseKeyCombo`（录制）与 `comboMatches`（匹配）必须走同一个函数 —— 两处各写一份
 * 就会「录出来的」和「能触发的」对不上，这正是本次修复要关掉的那类失效。
 */
function eventMods(ev: KeyboardEvent): {
  meta: boolean;
  shift: boolean;
  alt: boolean;
  ctrl: boolean;
} {
  return {
    meta: ev.metaKey || (isWindows() && ev.ctrlKey),
    shift: ev.shiftKey,
    alt: ev.altKey,
    ctrl: ev.ctrlKey && !ev.metaKey,
  };
}

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

  const m = eventMods(ev);
  const mods: Mod[] = [];
  if (m.meta) mods.push("meta");
  if (m.shift) mods.push("shift");
  if (m.alt) mods.push("alt");
  if (m.ctrl) mods.push("ctrl");

  const key = keyFromEvent(ev);
  if (!key) return null; // 不认识的键

  return { mods, key };
}

/**
 * KeyboardEvent → 内部 `Key`（匹配与录制共用同一条判定）。
 *
 * **优先 `ev.code`（物理键）而不是 `ev.key`**：macOS 上按住 Option 会把 `ev.key`
 * 换成组合字符（⌥V→`√`、⌥X→`≈`、⌥,→`≤`），只有 `code` 还是那把物理键。读 `ev.key`
 * 会让含 Alt 的绑定两头都坏 —— 匹配不上（`keyMatches` 拿 `√` 去比 `v`），录制也录不出
 * （组合字符是多字节，后端 `CharKind::from_json_str` 按**字节**长度校验，直接判非法键，
 * 实测 `set_hotkey` 返回 `unrecognised key '≈'`）。
 *
 * `code` 缺失或不认识（合成事件 / 非常规键位）时回落 `ev.key`，行为与修复前一致。
 */
function keyFromEvent(ev: KeyboardEvent): Key | null {
  switch (ev.code) {
    case "Enter":
    case "NumpadEnter":
      return { kind: "enter" };
    case "Escape":
      return { kind: "escape" };
    case "ArrowUp":
      return { kind: "arrow_up" };
    case "ArrowDown":
      return { kind: "arrow_down" };
    case "Comma":
      return { kind: "comma" };
    default:
      break;
  }
  // KeyV → v，Digit5 → 5（`code` 与键盘布局无关，也与修饰键无关）。
  if (ev.code.startsWith("Key") && ev.code.length === 4) {
    return { kind: "char", ch: ev.code.slice(3).toLowerCase() };
  }
  if (ev.code.startsWith("Digit") && ev.code.length === 6) {
    return { kind: "char", ch: ev.code.slice(5) };
  }
  return keyFromKeyString(ev.key);
}

/** `code` 认不出时的回落：按 `ev.key` 字面量判定。 */
function keyFromKeyString(key: string): Key | null {
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
      return key.length === 1 ? { kind: "char", ch: key.toLowerCase() } : null;
  }
}

/** `Key` 的可比较 token：字符键看字符，命名键看 kind。 */
function keyToken(key: Key): string {
  return key.kind === "char" ? `char:${key.ch}` : key.kind;
}

// ---------------------------------------------------------------------------
// KeyMatcher — 数据驱动的 keydown 匹配
// ---------------------------------------------------------------------------

/**
 * 检查一个 KeyboardEvent 是否匹配给定的 KeyCombo。
 * - mods：event 必须有 ALL listed mods，且不能有额外的 mods（Windows 下 `metaKey ||
 *   ctrlKey` 的等价规则除外）。
 * - key：经 [`keyFromEvent`]（物理键 `code` 优先）判定。
 */
function comboMatches(combo: KeyCombo, ev: KeyboardEvent): boolean {
  // Modifiers。与 parseKeyCombo 共用 eventMods，保证「录出来的」与「能匹配的」一致。
  const { meta: evMeta, ctrl: evCtrl, shift: evShift, alt: evAlt } = eventMods(ev);

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
  return keyMatches(combo.key, ev);
}

/** 与 [`keyFromEvent`] 同一条判定 —— 录制得到的键与能被触发的键必须是同一套解释。 */
function keyMatches(key: Key, ev: KeyboardEvent): boolean {
  const got = keyFromEvent(ev);
  return got !== null && keyToken(got) === keyToken(key);
}

/**
 * 用当前 in-memory currentKeyMap 匹配 KeyboardEvent。
 * 返回第一个命中的 HotkeyAction，或 null。未绑定的 action（`null`）不参与匹配。
 */
export function matches(ev: KeyboardEvent): HotkeyAction | null {
  if (!currentKeyMap) return null;
  for (const [action, combo] of Object.entries(currentKeyMap.actions) as [
    HotkeyAction,
    KeyCombo | null,
  ][]) {
    if (combo && comboMatches(combo, ev)) return action;
  }
  return null;
}

/** 四个修饰键的规范顺序（`mods` 的顺序不做承诺，比较前要先归一化）。 */
const MOD_ORDER: Mod[] = ["meta", "shift", "alt", "ctrl"];

/** 把 `mods` 归一化成与顺序无关的 token。 */
function modsToken(mods: Mod[]): string {
  return MOD_ORDER.filter((m) => mods.includes(m)).join("+");
}

/** 组合键结构相等：mods 集合相同，且键是同一个键（`keyToken` 同一套解释）。 */
function comboEquals(a: KeyCombo, b: KeyCombo): boolean {
  return keyToken(a.key) === keyToken(b.key) && modsToken(a.mods) === modsToken(b.mods);
}

/**
 * 查「已经占着 `combo` 的那条 action」（`except` 自己不算）。
 *
 * 组合键在表里必须唯一：两条 action 共用同一个组合时，[`matches`] 只会派发给迭代
 * 顺序靠前的那条（线格式由后端 `serialize` 的 `BTreeMap` 按 action 名排序，前端
 * `keyMapFromWire` 原样保留该顺序），另一条从此永远不触发 —— 而设置里那一行仍然
 * 显示着这个组合。录制路径据此把这种重绑拦下来，弹出「强行换绑」确认框。
 *
 * 迭代顺序与 [`matches`] 相同，所以返回的确实是「真会拿到派发的那条」。
 * 未绑定的 action（`null`）不占用任何组合，跳过。
 */
export function actionOwningCombo(combo: KeyCombo, except: HotkeyAction): HotkeyAction | null {
  if (!currentKeyMap) return null;
  for (const [action, bound] of Object.entries(currentKeyMap.actions) as [
    HotkeyAction,
    KeyCombo | null,
  ][]) {
    if (action === except) continue;
    if (bound && comboEquals(bound, combo)) return action;
  }
  return null;
}

// ---------------------------------------------------------------------------
// 线格式 → 内部表示（IPC 边界）
// ---------------------------------------------------------------------------

/**
 * 命名键的线格式 token 集合。
 *
 * `clipvault_core::keymap::CharKind` 序列化出来的 token **与内部 `kind` 同名**，
 * 所以这一张表同时是「线格式 → 内部」的识别集合与「内部 → 线格式」的写法来源：
 * 新增命名键时改这里一处，两个方向同时生效（否则两个方向会各维护一份 switch，
 * 迟早只改一边 —— 那正是本次修复要关掉的那类失效）。
 */
const NAMED_KEY_KINDS = ["enter", "escape", "arrow_up", "arrow_down", "comma"] as const;

function isNamedKeyKind(raw: string): raw is (typeof NAMED_KEY_KINDS)[number] {
  return (NAMED_KEY_KINDS as readonly string[]).includes(raw);
}

/**
 * 线格式的 `key` 字符串 → 内部 `Key`。
 *
 * 命名键按 [`NAMED_KEY_KINDS`] 识别；其余一律当单字符处理（不校验长度，与后端
 * `serialize` 的实际产出 —— 命名键或单字符 —— 一致）。
 */
function keyFromWire(raw: string): Key {
  return isNamedKeyKind(raw) ? { kind: raw } : { kind: "char", ch: raw.toLowerCase() };
}

/** 内部 `Key` → 线格式的 `key` 字符串（[`keyFromWire`] 的逆，共用同一张表）。 */
function keyToWire(key: Key): string {
  return key.kind === "char" ? key.ch : key.kind;
}

/**
 * 线格式 HotkeyMap → 内部 HotkeyMap。
 *
 * **不要**退回 `JSON.parse(json) as HotkeyMap` 的裸断言：线格式的 `key` 是字符串、
 * 内部是带标签的 `Key`，断言只是把差异藏进类型系统，运行时 `key.kind` 恒为
 * undefined —— 所有快捷键静默失效、设置行显示 `⌘+undefined`。
 *
 * 线格式的 `null`（未绑定）原样转成内部的 `null`。
 */
function keyMapFromWire(wire: WireHotkeyMap): HotkeyMap {
  const actions = {} as Record<HotkeyAction, KeyCombo | null>;
  for (const [action, combo] of Object.entries(wire.actions)) {
    actions[action as HotkeyAction] = combo
      ? { mods: combo.mods, key: keyFromWire(combo.key) }
      : null;
  }
  return { actions };
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
    // get_hotkeys 返回线格式 JSON 字符串，必须过 keyMapFromWire 转成内部表示。
    const json = await invoke<string>("get_hotkeys");
    currentKeyMap = keyMapFromWire(JSON.parse(json) as WireHotkeyMap);
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

/** 将 KeyCombo 序列化为后端 set_hotkey 期望的 JSON 结构（[`keyFromWire`] 的逆）。 */
function serializeComboForBackend(combo: KeyCombo): { mods: string[]; key: string } {
  return { mods: combo.mods as string[], key: keyToWire(combo.key) };
}

/**
 * 调用 set_hotkey IPC。
 * 返回 { ok: true } | { ok: false, err: string }。
 *
 * `force = true`：该组合已被另一条 action 占用时，抢过来并把原来占着它的那条解绑成
 * 「未绑定」（后端 `HotkeyMap::claim`）。只有用户在「强行换绑」弹框里确认过才传 true
 * —— 默认 `false`，被占用时后端会拒绝。
 */
export async function setHotkey(
  action: HotkeyAction,
  combo: KeyCombo,
  force = false,
): Promise<{ ok: true } | { ok: false; err: string }> {
  try {
    await invoke("set_hotkey", {
      action,
      combo: serializeComboForBackend(combo),
      force,
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
