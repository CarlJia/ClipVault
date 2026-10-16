// frontend/src/settings.test.ts
//
// 快捷键设置区的回归护栏。这个文件存在的直接理由：进入录制态时行内键帽被换成占位符
// 「请按新键…」，而取消录制只还原了 `recording` 类和按钮文案，忘了把键帽写回去 ——
// 「点重绑 → 不按键 → 点取消/Esc」之后那一行永远显示「请按新键…」，用户看到的就是
// 「快捷键丢了」（绑定还在，丢的是显示，直到重开设置被重渲染救回来）。
//
// 另外两条钉住同一次修复里收口的重复渲染副作用：`#hotkey-reset` 每次打开设置都会被
// 再绑一个监听器；进行中的录制态在重建行时不清理，会留下「按任意键就悄悄改绑」的
// 隐形 keydown 监听器。
//
// 末尾一组收紧最后一条静默路径：录制到的组合已经被**另一条 action** 占着。写进去不
// 会报错（后端只查语法与全局热键的 OS 注册），但 `matches()` 只会派发给排序靠前的那
// 条 —— 另一条永远不触发，而它那一行还显示着这个组合。正确行为是拦下并说出占用者。
//
// settings.ts 是重 DOM 的模块，但这里只覆盖这几段：用最小 DOM 替身驱动，不引 jsdom
// （与 keymap.test.ts 里手写 `ev()` 替身同一思路）。

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { HotkeyAction } from "./keymap";

// ---------------------------------------------------------------------------
// 最小 DOM 替身：只实现 settings.ts 真正用到的成员
// ---------------------------------------------------------------------------

/** 带 `textContent` 与 `classList` 的元素替身。 */
class FakeEl {
  textContent: string;
  private readonly classes = new Set<string>();
  readonly classList = {
    add: (c: string): void => void this.classes.add(c),
    remove: (c: string): void => void this.classes.delete(c),
    contains: (c: string): boolean => this.classes.has(c),
    toggle: (c: string, force?: boolean): void => {
      if (force === undefined ? !this.classes.has(c) : force) this.classes.add(c);
      else this.classes.delete(c);
    },
  };

  constructor(text = "") {
    this.textContent = text;
  }
}

/** 按钮替身：记录绑上来的 click 监听器，用来断言「只绑一次」。 */
class FakeButton extends FakeEl {
  readonly clickListeners: EventListener[] = [];
  private readonly row: FakeRow | null;

  constructor(text = "", row: FakeRow | null = null) {
    super(text);
    this.row = row;
  }

  addEventListener(type: string, fn: EventListener): void {
    if (type === "click") this.clickListeners.push(fn);
  }

  closest(sel: string): FakeRow | null {
    return sel.includes("hotkey-row") ? this.row : null;
  }
}

/** 一行快捷键：`<li data-action>` + 键帽 `<kbd>` + 重绑按钮。 */
class FakeRow extends FakeEl {
  readonly dataset: { action: string };
  readonly kbd = new FakeEl();
  readonly btn: FakeButton;

  constructor(action: string) {
    super();
    this.dataset = { action };
    this.btn = new FakeButton("重绑", this);
  }

  scrollIntoView(): void {
    /* noop */
  }

  querySelector(sel: string): FakeEl | null {
    if (sel.includes("kbd")) return this.kbd;
    if (sel.includes("hotkey-rebind")) return this.btn;
    return null;
  }
}

/** `<ul class="hotkey-list">` 替身：`innerHTML` 的渲染产物不参与断言，只记录。 */
class FakeList extends FakeEl {
  innerHTML = "";
  readonly buttons: FakeButton[];

  constructor(actions: HotkeyAction[], rows: Map<string, FakeRow>) {
    super();
    this.buttons = actions.map((a) => rows.get(a)!.btn);
  }

  querySelectorAll(sel: string): FakeButton[] {
    return sel.includes("hotkey-rebind") ? this.buttons : [];
  }
}

/** 与 `settings.ts` 的分组一致（断言只需要动作集合本身）。 */
const PANEL_ACTIONS: HotkeyAction[] = [
  "paste",
  "edit_note",
  "toggle_pin",
  "move_to_vault",
  "open_url",
  "open_vault",
  "toggle_guest_mode",
  "skip_once",
  "open_settings",
  "close",
  "move_up",
  "move_down",
];
const ALL_ACTIONS: HotkeyAction[] = ["open_panel", ...PANEL_ACTIONS];

interface FakeDom {
  rows: Map<string, FakeRow>;
  resetBtn: FakeButton;
  /** `#toast` 替身：被拦下的重绑会往这里写提示。 */
  toast: FakeEl;
  document: {
    querySelector: (sel: string) => FakeEl | null;
    getElementById: (id: string) => FakeEl | null;
    addEventListener: (type: string, fn: EventListener) => void;
    removeEventListener: (type: string, fn: EventListener) => void;
  };
  /** 当前挂在 `document` 上的 keydown 监听器（录制态注册的那一个）。 */
  keydownListeners: () => EventListener[];
}

function makeDom(): FakeDom {
  const rows = new Map<string, FakeRow>(ALL_ACTIONS.map((a) => [a, new FakeRow(a)]));
  const resetBtn = new FakeButton("重置默认");
  const toast = new FakeEl();
  const byId = new Map<string, FakeEl>([
    ["hotkey-group-global", new FakeEl()],
    ["hotkey-list-global", new FakeList(["open_panel"], rows)],
    ["hotkey-list-panel", new FakeList(PANEL_ACTIONS, rows)],
    ["hotkey-reset", resetBtn],
    ["toast", toast],
  ]);
  let keydown: EventListener[] = [];
  const document = {
    querySelector: (sel: string): FakeEl | null => {
      const m = /\[data-action="([^"]+)"\]/.exec(sel);
      return m ? rows.get(m[1]) ?? null : null;
    },
    getElementById: (id: string): FakeEl | null => byId.get(id) ?? null,
    addEventListener: (type: string, fn: EventListener): void => {
      if (type === "keydown") keydown = [...keydown, fn];
    },
    removeEventListener: (type: string, fn: EventListener): void => {
      if (type === "keydown") keydown = keydown.filter((f) => f !== fn);
    },
  };
  return { rows, resetBtn, toast, document, keydownListeners: () => keydown };
}

// ---------------------------------------------------------------------------
// 夹具：mock IPC + 平台标识 + 默认 keymap
// ---------------------------------------------------------------------------

/** 与 `HotkeyMap::default_map()` 一致的默认映射（`key` 为线格式字符串）。 */
const DEFAULT_WIRE = {
  version: 1,
  actions: {
    close: { mods: [], key: "escape" },
    edit_note: { mods: ["meta"], key: "e" },
    move_down: { mods: [], key: "arrow_down" },
    move_to_vault: { mods: ["meta"], key: "l" },
    move_up: { mods: [], key: "arrow_up" },
    open_panel: { mods: ["meta", "alt"], key: "v" },
    open_settings: { mods: ["meta"], key: "comma" },
    open_url: { mods: ["meta"], key: "o" },
    open_vault: { mods: ["meta", "shift"], key: "v" },
    paste: { mods: [], key: "enter" },
    skip_once: { mods: ["meta", "shift"], key: "n" },
    toggle_guest_mode: { mods: ["meta", "shift"], key: "g" },
    toggle_pin: { mods: ["meta"], key: "p" },
  },
};

/** `set_hotkey` 收到的 IPC 入参（时间序）—— 用来断言「被拦下的重绑一次都没写库」。 */
let setHotkeyCalls: Array<{ action: string; combo: { mods: string[]; key: string } }>;

/** 取一份全新的 settings 模块（模块级录制态不能跨用例复用）。 */
async function freshSettings(): Promise<typeof import("./settings")> {
  vi.resetModules();
  setHotkeyCalls = [];
  const invoke = vi.fn(async (cmd: string, args?: { action: string; combo: { mods: string[]; key: string } }) => {
    switch (cmd) {
      case "platform_info":
        return { os: "macos" };
      case "get_hotkeys":
        return JSON.stringify(DEFAULT_WIRE);
      case "set_hotkey":
        if (args) setHotkeyCalls.push({ action: args.action, combo: args.combo });
        return undefined;
      default:
        return undefined;
    }
  });
  vi.doMock("@tauri-apps/api/core", () => ({ invoke }));
  vi.stubGlobal("window", {
    setTimeout: globalThis.setTimeout,
    clearTimeout: globalThis.clearTimeout,
  });
  const settings = await import("./settings");
  const platform = await import("./platform");
  await platform.initPlatform();
  const keymap = await import("./keymap");
  await keymap.loadKeymap();
  return settings;
}

/** 模拟 `renderHotkeysSection` 渲染完一行后的状态：键帽已是当前绑定。 */
function seedRow(dom: FakeDom, action: string, comboText: string): FakeRow {
  const row = dom.rows.get(action)!;
  row.kbd.textContent = comboText;
  return row;
}

beforeEach(() => {
  vi.resetModules();
  vi.unstubAllGlobals();
});

describe("录制态取消：键帽必须回到当前绑定", () => {
  it("cancelRecording 后行显示当前绑定，而不是停在「请按新键…」", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording, cancelRecording } = await freshSettings();
    const row = seedRow(dom, "paste", "↩"); // paste 默认是 Enter → 「↩」

    startRecording("paste");
    expect(row.kbd.textContent).toBe("请按新键…");
    expect(row.btn.textContent).toBe("取消");
    expect(row.classList.contains("recording")).toBe(true);

    cancelRecording();
    expect(row.classList.contains("recording")).toBe(false);
    expect(row.btn.textContent).toBe("重绑");
    // 修复前这里是「请按新键…」—— 用户报的「快捷键丢了」。
    expect(row.kbd.textContent).toBe("↩");
  });

  it("按 Escape 取消（走 document 的捕获监听器）同样还原键帽", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    const row = seedRow(dom, "paste", "↩");

    startRecording("paste");
    const listeners = dom.keydownListeners();
    const onKeyDown = listeners[listeners.length - 1];
    onKeyDown({
      key: "Escape",
      preventDefault: () => {},
      stopPropagation: () => {},
    } as unknown as KeyboardEvent);

    expect(row.kbd.textContent).toBe("↩");
    expect(dom.keydownListeners()).toHaveLength(0);
  });

  it("录制中直接改绑另一行：前一行的键帽不会停在占位符上", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    const pasteRow = seedRow(dom, "paste", "↩");
    seedRow(dom, "open_settings", "⌘+,");

    startRecording("paste");
    expect(pasteRow.kbd.textContent).toBe("请按新键…");

    startRecording("open_settings"); // 内部先 cancelRecording()
    expect(pasteRow.kbd.textContent).toBe("↩");
  });
});

describe("重复渲染设置区", () => {
  it("不会给「重置默认」重复绑监听器（打开 N 次 ≠ 点一下跑 N 遍）", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { renderHotkeysSection } = await freshSettings();

    await renderHotkeysSection();
    await renderHotkeysSection();

    expect(dom.resetBtn.clickListeners).toHaveLength(1);
  });

  it("收掉进行中的录制态，不留隐形 keydown 监听器", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { renderHotkeysSection, startRecording } = await freshSettings();

    await renderHotkeysSection();
    startRecording("paste");
    expect(dom.keydownListeners()).toHaveLength(1);

    await renderHotkeysSection(); // 重建行 → 必须先把录制态收掉
    expect(dom.keydownListeners()).toHaveLength(0);
    expect(dom.rows.get("paste")!.btn.textContent).toBe("重绑");
  });
});

// ---------------------------------------------------------------------------
// 表内冲突：重绑到另一条 action 已经占着的组合
// ---------------------------------------------------------------------------
//
// 模型里没有「未绑定」状态，组合键没法从另一条 action 上抢过来。放进去的后果是
// 静默的：`matches()` 只会派发给序列化排序（按 action 名）靠前的那条，另一条
// 从此永远不触发 —— 而设置里那一行仍然显示着这个组合（`paste` 改成 `⌘E` 之后，
// 旧的 `↩` 没了、新的 `⌘E` 又永远不灵，回贴快捷键实际失效，界面却不这么说）。

/** 把一次按键派发给当前录制监听器（settings.ts 挂在 `document` 上的捕获监听器）。 */
function pressRecordingKey(
  dom: FakeDom,
  init: { key: string; code: string; metaKey?: boolean; shiftKey?: boolean; altKey?: boolean },
): void {
  const listeners = dom.keydownListeners();
  const onKeyDown = listeners[listeners.length - 1];
  onKeyDown({
    key: init.key,
    code: init.code,
    metaKey: init.metaKey ?? false,
    shiftKey: init.shiftKey ?? false,
    altKey: init.altKey ?? false,
    ctrlKey: false,
    preventDefault: () => {},
    stopPropagation: () => {},
  } as unknown as KeyboardEvent);
}

describe("重绑到已被占用的组合", () => {
  it("不写库、提示占用者、键帽回到原绑定、监听器收干净", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    const pasteRow = seedRow(dom, "paste", "↩");
    const editRow = seedRow(dom, "edit_note", "⌘+E");

    startRecording("paste");
    pressRecordingKey(dom, { key: "e", code: "KeyE", metaKey: true }); // ⌘E = edit_note 的
    await new Promise((resolve) => setTimeout(resolve, 0));

    // 修复前：这一次照样发给 set_hotkey，toast 为空，两行同时显示 ⌘E。
    expect(setHotkeyCalls).toHaveLength(0);
    expect(dom.toast.textContent).toBe("⌘+E 已被「编辑备注 / 标签」占用，已保留原绑定");
    expect(pasteRow.kbd.textContent).toBe("↩");
    expect(pasteRow.btn.textContent).toBe("重绑");
    expect(pasteRow.classList.contains("recording")).toBe(false);
    expect(dom.keydownListeners()).toHaveLength(0);
    expect(editRow.kbd.textContent).toBe("⌘+E"); // 占用者那一行不受影响
  });

  it("重绑成自己当前的组合：放行（不该被自己的旧值挡住）", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    seedRow(dom, "paste", "↩");

    startRecording("paste");
    pressRecordingKey(dom, { key: "Enter", code: "Enter" });
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(setHotkeyCalls).toEqual([{ action: "paste", combo: { mods: [], key: "enter" } }]);
    expect(dom.toast.textContent).toBe("");
  });

  it("面板动作想拿全局热键（⌥⌘V）的组合：也拦", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    seedRow(dom, "edit_note", "⌘+E");

    startRecording("edit_note");
    pressRecordingKey(dom, { key: "v", code: "KeyV", metaKey: true, altKey: true });
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(setHotkeyCalls).toHaveLength(0);
    expect(dom.toast.textContent).toBe("⌘+⌥+V 已被「唤出主面板」占用，已保留原绑定");
    expect(dom.rows.get("edit_note")!.kbd.textContent).toBe("⌘+E");
  });

  it("全局热键想拿面板动作（⌘E）的组合：也拦", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    seedRow(dom, "open_panel", "⌘+⌥+V");

    startRecording("open_panel");
    pressRecordingKey(dom, { key: "e", code: "KeyE", metaKey: true });
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(setHotkeyCalls).toHaveLength(0);
    expect(dom.toast.textContent).toBe("⌘+E 已被「编辑备注 / 标签」占用，已保留原绑定");
    expect(dom.rows.get("open_panel")!.kbd.textContent).toBe("⌘+⌥+V");
  });
});
