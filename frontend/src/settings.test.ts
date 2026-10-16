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
// 条 —— 另一条永远不触发，而它那一行还显示着这个组合。处理是弹框让用户选：确认 =
// 强行换绑（原来占着它的那条变成「未绑定」），取消 = 什么都不写。
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

  /** `askForceRebind` 打开弹框时会把焦点给「强行换绑」按钮。 */
  focus(): void {
    /* noop */
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
  "delete_entry",
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
  /** 快捷键列表容器（`renderRow` 的产物写在 `innerHTML` 里，只做文本断言）。 */
  globalList: FakeList;
  panelList: FakeList;
  /** `#toast` 替身：重绑 / 换绑失败会往这里写提示。 */
  toast: FakeEl;
  /** 「强行换绑」弹框：遮罩 + 文案 + 两个按钮。 */
  confirmOverlay: FakeEl;
  confirmMsg: FakeEl;
  confirmNote: FakeEl;
  confirmOk: FakeButton;
  confirmCancel: FakeButton;
  document: {
    querySelector: (sel: string) => FakeEl | null;
    getElementById: (id: string) => FakeEl | null;
    addEventListener: (type: string, fn: EventListener) => void;
    removeEventListener: (type: string, fn: EventListener) => void;
  };
  /** 当前挂在 `document` 上的 keydown 监听器（录制态或弹框态注册的那一个）。 */
  keydownListeners: () => EventListener[];
}

function makeDom(): FakeDom {
  const rows = new Map<string, FakeRow>(ALL_ACTIONS.map((a) => [a, new FakeRow(a)]));
  const resetBtn = new FakeButton("重置默认");
  const toast = new FakeEl();
  const globalList = new FakeList(["open_panel"], rows);
  const panelList = new FakeList(PANEL_ACTIONS, rows);
  const confirmOverlay = new FakeEl();
  confirmOverlay.classList.add("hidden"); // index.html 里初始就是 hidden
  const confirmMsg = new FakeEl();
  const confirmNote = new FakeEl();
  const confirmOk = new FakeButton("强行换绑");
  const confirmCancel = new FakeButton("取消");
  const byId = new Map<string, FakeEl>([
    ["hotkey-group-global", new FakeEl()],
    ["hotkey-list-global", globalList],
    ["hotkey-list-panel", panelList],
    ["hotkey-reset", resetBtn],
    ["toast", toast],
    ["hotkey-confirm", confirmOverlay],
    ["hotkey-confirm-msg", confirmMsg],
    ["hotkey-confirm-note", confirmNote],
    ["hotkey-confirm-ok", confirmOk],
    ["hotkey-confirm-cancel", confirmCancel],
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
  return {
    rows,
    resetBtn,
    globalList,
    panelList,
    toast,
    confirmOverlay,
    confirmMsg,
    confirmNote,
    confirmOk,
    confirmCancel,
    document,
    keydownListeners: () => keydown,
  };
}

// ---------------------------------------------------------------------------
// 夹具：mock IPC + 平台标识 + 默认 keymap
// ---------------------------------------------------------------------------

/** 与 `HotkeyMap::default_map()` 一致的默认映射（`key` 为线格式字符串）。 */
const DEFAULT_WIRE = {
  version: 1,
  actions: {
    close: { mods: [], key: "escape" },
    delete_entry: { mods: ["meta"], key: "backspace" },
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

/** 线格式 combo（`key` 为字符串）；`null` 整条 = 该 action 未绑定。 */
type WireCombo = { mods: string[]; key: string } | null;

/** `set_hotkey` 收到的 IPC 入参（时间序）—— 用来断言「被拦下的重绑一次都没写库」。 */
let setHotkeyCalls: Array<{
  action: string;
  combo: { mods: string[]; key: string };
  force: boolean | undefined;
}>;

/** `get_hotkeys` 当前会返回的线格式状态；`set_hotkey` 成功时像后端一样改动它。 */
let wire: { version: number; actions: Record<string, WireCombo> };

/** `mods` 是两个集合（顺序无关）：按固定顺序拼成 token 再比。 */
function modsToken(mods: string[]): string {
  return ["meta", "shift", "alt", "ctrl"].filter((m) => mods.includes(m)).join("+");
}

/** 取一份全新的 settings 模块（模块级录制态 / 弹框态不能跨用例复用）。 */
async function freshSettings(
  options: { wire?: { version: number; actions: Record<string, WireCombo> }; failSetHotkey?: boolean } = {},
): Promise<typeof import("./settings")> {
  vi.resetModules();
  setHotkeyCalls = [];
  wire = options.wire ?? (JSON.parse(JSON.stringify(DEFAULT_WIRE)) as typeof wire);
  const invoke = vi.fn(
    async (
      cmd: string,
      args?: {
        action: string;
        combo: { mods: string[]; key: string };
        force?: boolean;
      },
    ) => {
      switch (cmd) {
        case "platform_info":
          return { os: "macos" };
        case "get_hotkeys":
          return JSON.stringify(wire);
        case "set_hotkey": {
          if (!args) return undefined;
          setHotkeyCalls.push({ action: args.action, combo: args.combo, force: args.force });
          if (options.failSetHotkey) throw "boom";
          // 像后端一样落库：force 时把原来占着这个组合的其它 action 解绑（`claim`）。
          for (const [action, bound] of Object.entries(wire.actions)) {
            if (
              action !== args.action &&
              bound &&
              modsToken(bound.mods) === modsToken(args.combo.mods) &&
              bound.key === args.combo.key
            ) {
              wire.actions[action] = null;
            }
          }
          wire.actions[args.action] = args.combo;
          return undefined;
        }
        default:
          return undefined;
      }
    },
  );
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
// 组合键在表里必须唯一：两条 action 共用一个组合时，`matches()` 只会派发给序列化
// 排序（按 action 名）靠前的那条，另一条从此永远不触发 —— 而设置里那一行仍然显示着
// 这个组合（`paste` 改成 `⌘E` 之后，旧的 `↩` 没了、新的 `⌘E` 又永远不灵，回贴快捷键
// 实际失效，界面却不这么说）。
//
// 处理是「弹框让用户选」：确认 = 强行换绑（组合归本行，原来占着它的那条变成「未绑定」），
// 取消 = 什么都不写、键帽回原绑定。写入的 `force` 标志只有在用户确认后才为 true。

/** 把一次按键派发给**当前最后一个** document keydown 监听器（录制态或弹框态挂的那个），
 *  并回报它有没有 `preventDefault` / `stopPropagation`。 */
function pressKey(
  dom: FakeDom,
  init: { key: string; code: string; metaKey?: boolean; shiftKey?: boolean; altKey?: boolean },
): { prevented: boolean; stopped: boolean } {
  const listeners = dom.keydownListeners();
  const onKeyDown = listeners[listeners.length - 1];
  let prevented = false;
  let stopped = false;
  onKeyDown({
    key: init.key,
    code: init.code,
    metaKey: init.metaKey ?? false,
    shiftKey: init.shiftKey ?? false,
    altKey: init.altKey ?? false,
    ctrlKey: false,
    preventDefault: () => {
      prevented = true;
    },
    stopPropagation: () => {
      stopped = true;
    },
  } as unknown as KeyboardEvent);
  return { prevented, stopped };
}

/** 让挂起的 IPC 链（setHotkey → reloadKeymap → 重画）跑完。 */
function flush(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

describe("重绑到已被占用的组合：弹框让用户选", () => {
  it("弹出确认框，说出占用者与后果；不写库、键帽不变", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    const pasteRow = seedRow(dom, "paste", "↩");
    const editRow = seedRow(dom, "edit_note", "⌘+E");

    startRecording("paste");
    pressKey(dom, { key: "e", code: "KeyE", metaKey: true });

    // 修复前：这一次照样发给 set_hotkey，两行同时显示 ⌘E，没有任何提示。
    expect(setHotkeyCalls).toHaveLength(0);
    expect(dom.confirmOverlay.classList.contains("hidden")).toBe(false);
    expect(dom.confirmMsg.textContent).toBe(
      "⌘+E 已被「编辑备注 / 标签」占用。强行换绑后，「编辑备注 / 标签」会变成「未绑定」，「粘贴选中条目」改用 ⌘+E。",
    );
    expect(dom.confirmNote.textContent).toBe(""); // 占用者不是全局热键 → 没有额外警告
    expect(pasteRow.kbd.textContent).toBe("↩");
    expect(editRow.kbd.textContent).toBe("⌘+E");
    expect(pasteRow.btn.textContent).toBe("重绑");
    expect(pasteRow.classList.contains("recording")).toBe(false);
  });

  it("弹框期间按别的键不会改绑（录制监听器已收掉）", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    const pasteRow = seedRow(dom, "paste", "↩");

    startRecording("paste");
    pressKey(dom, { key: "e", code: "KeyE", metaKey: true }); // 撞上 edit_note 的 ⌘E
    pressKey(dom, { key: "k", code: "KeyK", metaKey: true }); // ⌘K 是空闲的

    expect(setHotkeyCalls).toHaveLength(0);
    expect(pasteRow.kbd.textContent).toBe("↩");
  });

  it("点「取消」：不写库、键帽不变、弹框收掉、无 toast", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    const pasteRow = seedRow(dom, "paste", "↩");
    seedRow(dom, "edit_note", "⌘+E");

    startRecording("paste");
    pressKey(dom, { key: "e", code: "KeyE", metaKey: true });
    dom.confirmCancel.clickListeners[0]({} as Event);
    await flush();

    expect(dom.confirmOverlay.classList.contains("hidden")).toBe(true);
    expect(setHotkeyCalls).toHaveLength(0);
    expect(pasteRow.kbd.textContent).toBe("↩");
    expect(dom.toast.textContent).toBe("");
  });

  it("按 Esc 取消：弹框收掉、不写库，且事件被 stopPropagation（不连带关掉设置面板）", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    seedRow(dom, "paste", "↩");
    seedRow(dom, "edit_note", "⌘+E");

    startRecording("paste");
    pressKey(dom, { key: "e", code: "KeyE", metaKey: true });
    const { prevented, stopped } = pressKey(dom, { key: "Escape", code: "Escape" });
    await flush();

    // main.ts 的「设置里按 Esc 关设置」在冒泡相，靠这两个标志挡在外面。
    expect(prevented).toBe(true);
    expect(stopped).toBe(true);
    expect(dom.confirmOverlay.classList.contains("hidden")).toBe(true);
    expect(setHotkeyCalls).toHaveLength(0);
    expect(dom.keydownListeners()).toHaveLength(0); // 弹框监听器也摘掉了
  });

  it("按 Enter 确认：以 force=true 抢占，抢到的有键、被抢的显示「未绑定」", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    const pasteRow = seedRow(dom, "paste", "↩");
    const editRow = seedRow(dom, "edit_note", "⌘+E");

    startRecording("paste");
    pressKey(dom, { key: "e", code: "KeyE", metaKey: true });
    pressKey(dom, { key: "Enter", code: "Enter" });
    await flush();

    expect(setHotkeyCalls).toEqual([
      { action: "paste", combo: { mods: ["meta"], key: "e" }, force: true },
    ]);
    expect(pasteRow.kbd.textContent).toBe("⌘+E");
    expect(editRow.kbd.textContent).toBe("未绑定");
    expect(dom.confirmOverlay.classList.contains("hidden")).toBe(true);
    expect(dom.toast.textContent).toBe(
      "已强行换绑：「编辑备注 / 标签」变成未绑定，「粘贴选中条目」改用 ⌘+E",
    );
    expect(dom.keydownListeners()).toHaveLength(0);
  });

  it("点「强行换绑」按钮：同样以 force=true 写入", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    seedRow(dom, "paste", "↩");
    const editRow = seedRow(dom, "edit_note", "⌘+E");

    startRecording("paste");
    pressKey(dom, { key: "e", code: "KeyE", metaKey: true });
    dom.confirmOk.clickListeners[0]({} as Event);
    await flush();

    expect(setHotkeyCalls[0].force).toBe(true);
    expect(editRow.kbd.textContent).toBe("未绑定");
  });

  it("抢全局热键的键位：额外警告，确认后那行显示「未绑定」", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    const panelRow = seedRow(dom, "open_panel", "⌘+⌥+V");
    const editRow = seedRow(dom, "edit_note", "⌘+E");

    startRecording("edit_note");
    pressKey(dom, { key: "v", code: "KeyV", metaKey: true, altKey: true });

    expect(dom.confirmMsg.textContent).toBe(
      "⌘+⌥+V 已被「唤出主面板」占用。强行换绑后，「唤出主面板」会变成「未绑定」，「编辑备注 / 标签」改用 ⌘+⌥+V。",
    );
    // 解绑全局热键 = 面板只剩托盘/菜单栏图标能唤出，必须说出来。
    expect(dom.confirmNote.textContent).toContain("全局热键");

    dom.confirmOk.clickListeners[0]({} as Event);
    await flush();

    expect(setHotkeyCalls).toEqual([
      { action: "edit_note", combo: { mods: ["meta", "alt"], key: "v" }, force: true },
    ]);
    expect(editRow.kbd.textContent).toBe("⌘+⌥+V");
    expect(panelRow.kbd.textContent).toBe("未绑定");
  });

  it("全局热键想拿面板动作（⌘E）的键位：一样弹框，确认后面板那行变成未绑定", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    const panelRow = seedRow(dom, "open_panel", "⌘+⌥+V");
    const pasteRow = seedRow(dom, "paste", "↩");

    startRecording("open_panel");
    pressKey(dom, { key: "e", code: "KeyE", metaKey: true });

    expect(dom.confirmOverlay.classList.contains("hidden")).toBe(false);
    expect(dom.confirmMsg.textContent).toContain("「编辑备注 / 标签」");
    expect(setHotkeyCalls).toHaveLength(0);

    dom.confirmOk.clickListeners[0]({} as Event);
    await flush();

    expect(setHotkeyCalls).toEqual([
      { action: "open_panel", combo: { mods: ["meta"], key: "e" }, force: true },
    ]);
    expect(panelRow.kbd.textContent).toBe("⌘+E");
    expect(pasteRow.kbd.textContent).toBe("↩"); // 被抢的是 edit_note，paste 不受影响
    expect(dom.rows.get("edit_note")!.kbd.textContent).toBe("未绑定");
  });

  it("换绑（force）失败：toast 说清楚，两行键帽都不动", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings({ failSetHotkey: true });
    const pasteRow = seedRow(dom, "paste", "↩");
    const editRow = seedRow(dom, "edit_note", "⌘+E");

    startRecording("paste");
    pressKey(dom, { key: "e", code: "KeyE", metaKey: true });
    dom.confirmOk.clickListeners[0]({} as Event);
    await flush();

    expect(setHotkeyCalls[0].force).toBe(true);
    expect(dom.toast.textContent).toBe("换绑失败：boom，已保留原绑定");
    expect(pasteRow.kbd.textContent).toBe("↩");
    expect(editRow.kbd.textContent).toBe("⌘+E");
  });

  it("重开设置时把没答完的弹框按「取消」收掉（不会事后偷偷写库）", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording, renderHotkeysSection } = await freshSettings();
    seedRow(dom, "paste", "↩");
    seedRow(dom, "edit_note", "⌘+E");

    startRecording("paste");
    pressKey(dom, { key: "e", code: "KeyE", metaKey: true });
    expect(dom.confirmOverlay.classList.contains("hidden")).toBe(false);

    await renderHotkeysSection(); // 重建行 → 弹框先被收掉
    expect(dom.confirmOverlay.classList.contains("hidden")).toBe(true);

    dom.confirmOk.clickListeners[0]({} as Event); // 事后点确认不该再改任何东西
    await flush();
    expect(setHotkeyCalls).toHaveLength(0);
  });

  it("重绑成自己当前的组合：放行（不该被自己的旧值挡住）", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    seedRow(dom, "paste", "↩");

    startRecording("paste");
    pressKey(dom, { key: "Enter", code: "Enter" });
    await flush();

    expect(setHotkeyCalls).toEqual([
      { action: "paste", combo: { mods: [], key: "enter" }, force: false },
    ]);
    expect(dom.toast.textContent).toBe("");
    expect(dom.confirmOverlay.classList.contains("hidden")).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// 未绑定（`null`）：行显示成「未绑定」，不是占位符「—」
// ---------------------------------------------------------------------------

describe("未绑定的行", () => {
  it("渲染成「未绑定」（「—」留给 keymap 还没加载的状态）", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { renderHotkeysSection } = await freshSettings({
      wire: {
        version: 1,
        actions: { ...DEFAULT_WIRE.actions, paste: null },
      },
    });

    await renderHotkeysSection();

    expect(dom.panelList.innerHTML).toContain("<kbd>未绑定</kbd>");
    expect(dom.panelList.innerHTML).not.toContain("<kbd>—</kbd>");
  });
});

describe("动作基数与分组：设置面板列出的 = keymap 里有的", () => {
  it("1 全局 + 13 面板 = 14，且与 keymap 的动作集合逐项一致", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { GLOBAL_ACTIONS, PANEL_ACTIONS: panel } = await freshSettings();

    // 这条断言是「基数」类散文（README 的「面板内 13 项」、plan 里的变体数）唯一的
    // 锚点：动作加了却漏进设置面板 → 用户再也重绑不到它，这里先红。
    expect(GLOBAL_ACTIONS).toEqual(["open_panel"]);
    expect(GLOBAL_ACTIONS).toHaveLength(1);
    expect(panel).toHaveLength(13);
    expect(Object.keys(DEFAULT_WIRE.actions)).toHaveLength(14);
    expect([...GLOBAL_ACTIONS, ...panel].sort()).toEqual(
      Object.keys(DEFAULT_WIRE.actions).sort(),
    );

    // 分组本身不能有重复项，也不能把全局动作混进面板组（会渲染出两行同一动作）。
    expect(new Set(panel).size).toBe(panel.length);
    expect(panel).not.toContain("open_panel");

    // 本文件上方的 DOM 替身按自己的 ALL_ACTIONS 建行，必须覆盖真实分组 ——
    // 否则 `renderRow` 在真实分组里取不到行（那边的 `!` 会直接抛）。
    for (const action of [...GLOBAL_ACTIONS, ...panel]) {
      expect(ALL_ACTIONS, `测试夹具缺少 ${action} 的行`).toContain(action);
    }
  });
});
