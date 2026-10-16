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
// 最后两条收的是同一个根因（录制态监听器挂在 `document` 上）漏掉的两条路径：关掉设置
// 面板没有收掉录制态，用户回到列表敲的第一个字符被吞掉并静默改绑；重建行与取消录制
// 之间还隔着等 keymap 的轮询窗口，窗口里点「重绑」照样留下一份隐形监听器。
//
// 另有录制**成功 / 失败**两条：按下新组合键 → set_hotkey →（成功时 reloadKeymap）→ cancelRecording。
// 两条此前都只有人工验收 —— 成功那条钉的是顺序（reload 必须在 cancel 之前，否则 refreshRow
// 先把旧绑定写回键帽、之后再没人纠正）；失败那条钉的是「写库没成功就别 reload」以及
// 「给出提示 + 键帽回退到库里那一份」。
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
  /** `#settings` 模态替身：`classList.add("hidden")` = 用户关掉了设置。 */
  settingsModal: FakeEl;
  /** `#toast` 替身：失败的录制会往这里写提示。 */
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
  const settingsModal = new FakeEl();
  const toast = new FakeEl();
  const byId = new Map<string, FakeEl>([
    ["settings", settingsModal],
    ["toast", toast],
    ["hotkey-group-global", new FakeEl()],
    ["hotkey-list-global", new FakeList(["open_panel"], rows)],
    ["hotkey-list-panel", new FakeList(PANEL_ACTIONS, rows)],
    ["hotkey-reset", resetBtn],
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
  return { rows, resetBtn, settingsModal, toast, document, keydownListeners: () => keydown };
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

/** 最近一次 `freshSettings()` 装上的 IPC 替身：用来断言「没有发生静默改绑」。 */
let invokeMock: ReturnType<typeof vi.fn>;

/** `get_hotkeys` 当下要返回的线格式。改它 = 模拟后端已经写入新绑定。 */
let wireToReturn: unknown = DEFAULT_WIRE;

/** 非 null 时 `set_hotkey` 抛这个错误 = 模拟后端拒绝这次改绑。 */
let setHotkeyError: string | null = null;

/** 后端写入新绑定之后的线格式：`paste` 由 Enter 改成 ⇧⌘K。 */
const NEW_WIRE = {
  version: 1,
  actions: {
    ...DEFAULT_WIRE.actions,
    paste: { mods: ["meta", "shift"], key: "k" },
  },
};
async function freshSettings(): Promise<typeof import("./settings")> {
  vi.resetModules();
  const invoke = vi.fn(async (cmd: string) => {
    switch (cmd) {
      case "platform_info":
        return { os: "macos" };
      case "get_hotkeys":
        return JSON.stringify(wireToReturn);
      case "set_hotkey":
        if (setHotkeyError !== null) throw setHotkeyError;
        return undefined;
      default:
        return undefined;
    }
  });
  invokeMock = invoke;
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
  wireToReturn = DEFAULT_WIRE;
  setHotkeyError = null;
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

  it("等 keymap 的轮询窗口里点「重绑」，也留不下隐形监听器", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { renderHotkeysSection, startRecording } = await freshSettings();

    const rendering = renderHotkeysSection(); // 不 await：停在等 keymap 的轮询里
    await new Promise((resolve) => setTimeout(resolve, 0)); // 让渲染跑到轮询处
    startRecording("paste"); // 窗口内用户点了「重绑」
    expect(dom.keydownListeners()).toHaveLength(1);

    await rendering; // 轮询结束 → 重建行
    expect(dom.keydownListeners()).toHaveLength(0);
  });
});

describe("设置面板关掉后录制态必须失效", () => {
  it("放行按键：不吞键、不静默改绑，键帽回到当前绑定", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    const row = seedRow(dom, "paste", "↩");

    startRecording("paste");
    expect(row.kbd.textContent).toBe("请按新键…");

    dom.settingsModal.classList.add("hidden"); // 用户点了「关闭」

    const listeners = dom.keydownListeners();
    const onKeyDown = listeners[listeners.length - 1];
    let prevented = false;
    let stopped = false;
    onKeyDown({
      key: "a",
      code: "KeyA",
      metaKey: false,
      ctrlKey: false,
      shiftKey: false,
      altKey: false,
      preventDefault: () => {
        prevented = true;
      },
      stopPropagation: () => {
        stopped = true;
      },
    } as unknown as KeyboardEvent);

    // 录制态自我了断：这一下按键不属于它，要留给下面的面板快捷键。
    // 修复前 `a` 会被吞掉（preventDefault）并写成 paste 的新绑定。
    expect(prevented).toBe(false);
    expect(stopped).toBe(false);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "set_hotkey")).toHaveLength(0);
    expect(dom.keydownListeners()).toHaveLength(0);
    expect(row.kbd.textContent).toBe("↩");
    expect(row.btn.textContent).toBe("重绑");
  });
});

/** 把一次 ⇧⌘K 派发给当前录制监听器；返回它有没有吞掉这一下。 */
function pressRecordedKey(dom: FakeDom): { prevented: boolean; stopped: boolean } {
  const listeners = dom.keydownListeners();
  const onKeyDown = listeners[listeners.length - 1];
  let prevented = false;
  let stopped = false;
  onKeyDown({
    key: "k",
    code: "KeyK",
    metaKey: true,
    shiftKey: true,
    altKey: false,
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

describe("录制成功：按下新组合键", () => {
  it("一次画对新绑定：键帽是新值、录制态收干净、IPC 拿到新组合", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    const row = seedRow(dom, "paste", "↩"); // paste 当前是 Enter
    wireToReturn = NEW_WIRE; // set_hotkey 之后 get_hotkeys 会读到的新绑定

    startRecording("paste");
    const { prevented } = pressRecordedKey(dom);

    // 录制态先吞掉这一下（它就是被录进去的那一下），再把组合键交给 IPC。
    expect(prevented).toBe(true);
    // 等 set_hotkey → reloadKeymap → cancelRecording 这条链跑完。
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(invokeMock).toHaveBeenCalledWith("set_hotkey", {
      action: "paste",
      combo: { mods: ["meta", "shift"], key: "k" },
    });
    // 顺序护栏：refreshRow 必须读到 reload 之后的新 keymap —— 反过来（先 cancel 再
    // reload）会把旧值「↩」写回键帽，再也没人纠正它。
    expect(row.kbd.textContent).toBe("⌘+⇧+K");
    expect(row.classList.contains("recording")).toBe(false);
    expect(row.btn.textContent).toBe("重绑");
    expect(dom.keydownListeners()).toHaveLength(0);
    // 成功路径不该弹失败提示。
    expect(dom.toast.textContent).toBe("");
  });
});

describe("录制失败：后端拒绝这次改绑", () => {
  it("键帽回退到原绑定并给出提示，录制态一样收干净", async () => {
    const dom = makeDom();
    vi.stubGlobal("document", dom.document);
    const { startRecording } = await freshSettings();
    const row = seedRow(dom, "paste", "↩"); // paste 当前是 Enter
    setHotkeyError = "unrecognised key '≈'"; // 后端拒绝（wireToReturn 不变 = 库里没改）

    startRecording("paste");
    const { prevented } = pressRecordedKey(dom);
    expect(prevented).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 0));

    // 写库失败 → 不 reload → 键帽回到库里那一份（旧绑定），而不是刚按下的组合。
    expect(row.kbd.textContent).toBe("↩");
    expect(row.classList.contains("recording")).toBe(false);
    expect(row.btn.textContent).toBe("重绑");
    expect(dom.keydownListeners()).toHaveLength(0);
    expect(dom.toast.textContent).toBe("重绑失败：unrecognised key '≈'，已回退到原绑定");
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
