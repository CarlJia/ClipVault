// frontend/src/keymap.test.ts
//
/// <reference types="vite/client" />
//
// keymap.ts 的回归护栏。这个文件存在的直接理由：`get_hotkeys` 的**线格式**
// （`{"mods":["meta"],"key":"e"}`，`key` 是字符串）与**内部表示**（`key` 是带标签的
// `Key`）曾被 `JSON.parse(json) as HotkeyMap` 的裸断言混为一谈，后果是面板内
// 全部快捷键静默失效、设置行显示 `⌘+undefined`；`parseKeyCombo` 读 `ev.key` 又让
// macOS 上含 Option 的绑定既录不进也匹配不上（⌥V 的 `ev.key` 是 `√`）。
// 这里把那两条边界钉成可执行断言 —— 删掉对应的生产代码，这些用例必须变红。

import { beforeEach, describe, expect, it, vi } from "vitest";

// 命名键 token 的跨语言对照要读后端的源码（见文件末尾的 describe）。
// `?raw` 由 Vite 提供，运行时（vitest）与 tsc 都认 —— 不必给前端引入 @types/node，
// 前端跑在 WebView 里，本来就不该有 node 内置模块可用。
import coreKeymapRs from "../../clipvault-core/src/keymap.rs?raw";

/** `KeyboardEvent` 的最小替身：keymap 只读这几个字段，故测试不需要 DOM 环境。 */
type EvInit = Partial<
  Pick<
    KeyboardEvent,
    "key" | "code" | "metaKey" | "ctrlKey" | "shiftKey" | "altKey" | "repeat"
  >
> & { key: string };

function ev(init: EvInit): KeyboardEvent {
  return {
    code: "",
    metaKey: false,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    repeat: false,
    // handleVaultKey 会 preventDefault；matches / parseKeyCombo 不碰它。
    preventDefault: () => undefined,
    ...init,
  } as unknown as KeyboardEvent;
}

/** 后端 `clipvault_core::keymap::serialize` 产出的线格式（`key` 为字符串）。 */
interface WireMap {
  version: number;
  actions: Record<string, { mods: string[]; key: string }>;
}

/** 与 `HotkeyMap::default_map()` 一致的 14 条默认映射（`key` 为线格式字符串）。 */
const DEFAULT_WIRE: WireMap = {
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

type KeymapModule = typeof import("./keymap");

/** `set_hotkey` 收到的 IPC 入参（时间序）。 */
let setHotkeyCalls: Array<{ action: string; combo: { mods: string[]; key: string | null } }>;

/**
 * 取一份**全新的** keymap 模块。
 *
 * `platform.ts` 的平台标识与 `keymap.ts` 的 `currentKeyMap` 都是模块级状态，跨用例
 * 复用会串味，所以每个用例都 `resetModules()` 后从零 import 一次；顺带把后端 IPC
 * 换成桩，让 `platform_info` / `get_hotkeys` / `set_hotkey` 可控、可断言。
 */
async function freshKeymap(
  options: { os?: string; wire?: WireMap } = {},
): Promise<KeymapModule> {
  vi.resetModules();
  setHotkeyCalls = [];
  const invoke = vi.fn(
    async (
      cmd: string,
      args?: { action: string; combo: { mods: string[]; key: string | null } },
    ) => {
      switch (cmd) {
        case "platform_info":
          return { os: options.os ?? "macos" };
        case "get_hotkeys":
          return JSON.stringify(options.wire ?? DEFAULT_WIRE);
        case "set_hotkey":
          if (args) setHotkeyCalls.push({ action: args.action, combo: args.combo });
          return undefined;
        default:
          return undefined;
      }
    },
  );
  vi.doMock("@tauri-apps/api/core", () => ({ invoke }));
  // `initPlatform()` 结尾会 `localizeKeycaps(document)`（Windows 分支遍历 <kbd>）。
  // node 环境没有 document，给一个只为 querySelectorAll 而存在的最小替身 —— 不引 jsdom。
  vi.stubGlobal("document", { querySelectorAll: () => [] });
  const platform = await import("./platform");
  await platform.initPlatform(); // 先定平台标识，再加载 keymap（eventMods 依赖它）
  const keymap = await import("./keymap");
  await keymap.loadKeymap();
  return keymap;
}

beforeEach(() => {
  vi.resetModules();
  vi.unstubAllGlobals();
});

// ---------------------------------------------------------------------------
// 线格式 → 内部表示
// ---------------------------------------------------------------------------

describe("loadKeymap：线格式 key 字符串 → 内部带标签 Key", () => {
  it("把字符串 key 归一化成 Key，而不是原样保留", async () => {
    const km = await freshKeymap();
    expect(km.currentKeyMap?.actions.edit_note).toEqual({
      mods: ["meta"],
      key: { kind: "char", ch: "e" },
    });
    expect(km.currentKeyMap?.actions.paste).toEqual({ mods: [], key: { kind: "enter" } });
    expect(km.currentKeyMap?.actions.open_settings).toEqual({
      mods: ["meta"],
      key: { kind: "comma" },
    });
  });

  it("14 条默认映射逐条都能命中（修复前这里全为 null）", async () => {
    const km = await freshKeymap();
    const cases: Array<[string, EvInit, string]> = [
      ["⌘E", { key: "e", code: "KeyE", metaKey: true }, "edit_note"],
      ["⌘P", { key: "p", code: "KeyP", metaKey: true }, "toggle_pin"],
      ["⌘L", { key: "l", code: "KeyL", metaKey: true }, "move_to_vault"],
      ["⌘⌫", { key: "Backspace", code: "Backspace", metaKey: true }, "delete_entry"],
      ["⌘O", { key: "o", code: "KeyO", metaKey: true }, "open_url"],
      ["⌘⇧V", { key: "V", code: "KeyV", metaKey: true, shiftKey: true }, "open_vault"],
      ["⌘⇧G", { key: "G", code: "KeyG", metaKey: true, shiftKey: true }, "toggle_guest_mode"],
      ["⌘⇧N", { key: "N", code: "KeyN", metaKey: true, shiftKey: true }, "skip_once"],
      ["⌘,", { key: ",", code: "Comma", metaKey: true }, "open_settings"],
      ["↩", { key: "Enter", code: "Enter" }, "paste"],
      ["esc", { key: "Escape", code: "Escape" }, "close"],
      ["↑", { key: "ArrowUp", code: "ArrowUp" }, "move_up"],
      ["↓", { key: "ArrowDown", code: "ArrowDown" }, "move_down"],
      ["⌥⌘V", { key: "√", code: "KeyV", metaKey: true, altKey: true }, "open_panel"],
    ];
    for (const [name, init, expected] of cases) {
      expect(km.matches(ev(init)), name).toBe(expected);
    }
    // 这张表必须覆盖 `DEFAULT_WIRE` 的每一个动作：漏一条就是有一个默认键
    // 悄悄失效（本文件存在的理由），多一条则是表里留了已删动作的幽灵。
    expect(cases.map((c) => c[2]).sort()).toEqual(Object.keys(DEFAULT_WIRE.actions).sort());
  });

  it("6 个命名键 token 解析成非 char 的 kind，并能原样回写成同一 token", async () => {
    // 这张列表必须与 `clipvault-core/src/keymap.rs` 的
    // `CharKind::from_json_str` / `to_json_str` 的命名键集合一致：
    // 往这里加一项却没改后端，往返断言就会红。
    const namedTokens = ["enter", "escape", "arrow_up", "arrow_down", "comma", "backspace"];
    const owners: Record<string, string> = {
      enter: "paste",
      escape: "close",
      arrow_up: "move_up",
      arrow_down: "move_down",
      comma: "open_settings",
      backspace: "delete_entry",
    };
    const wire: WireMap = {
      version: 1,
      actions: {
        paste: { mods: [], key: "enter" },
        close: { mods: [], key: "escape" },
        move_up: { mods: [], key: "arrow_up" },
        move_down: { mods: [], key: "arrow_down" },
        open_settings: { mods: [], key: "comma" },
        delete_entry: { mods: [], key: "backspace" },
      },
    };
    const km = await freshKeymap({ wire });
    const map = km.currentKeyMap!;

    for (const token of namedTokens) {
      const action = owners[token] as Parameters<typeof km.setHotkey>[0];
      const combo = map.actions[action];
      expect(combo, token).toBeDefined();
      expect(combo.key, token).toEqual({ kind: token });
      await km.setHotkey(action, combo);
    }
    // keyToWire(keyFromWire(t)) === t
    expect(setHotkeyCalls.map((c) => c.combo.key)).toEqual(namedTokens);
  });

  it("单字符键双向往返：线格式 \"x\" ↔ {kind:\"char\",ch:\"x\"}", async () => {
    const km = await freshKeymap({
      wire: { version: 1, actions: { toggle_pin: { mods: ["meta"], key: "x" } } },
    });
    const combo = km.currentKeyMap!.actions.toggle_pin;
    expect(combo.key).toEqual({ kind: "char", ch: "x" });
    await km.setHotkey("toggle_pin", combo);
    expect(setHotkeyCalls[0]).toEqual({
      action: "toggle_pin",
      combo: { mods: ["meta"], key: "x" },
    });
  });
});

// ---------------------------------------------------------------------------
// 录制态：物理键 code 优先（macOS Option 组合字符）
// ---------------------------------------------------------------------------

describe("parseKeyCombo：以物理键 ev.code 判定", () => {
  it("Option 组合字符不参与判定（⌥V 的 ev.key 是 √）", async () => {
    const km = await freshKeymap();
    expect(km.parseKeyCombo(ev({ key: "√", code: "KeyV", metaKey: true, altKey: true }))).toEqual(
      { mods: ["meta", "alt"], key: { kind: "char", ch: "v" } },
    );
    expect(km.parseKeyCombo(ev({ key: "≈", code: "KeyX", altKey: true }))).toEqual({
      mods: ["alt"],
      key: { kind: "char", ch: "x" },
    });
    expect(km.parseKeyCombo(ev({ key: "≤", code: "Comma", altKey: true }))).toEqual({
      mods: ["alt"],
      key: { kind: "comma" },
    });
    expect(
      km.parseKeyCombo(ev({ key: "#", code: "Digit3", altKey: true, shiftKey: true })),
    ).toEqual({ mods: ["shift", "alt"], key: { kind: "char", ch: "3" } });
  });

  it("录出的含 Alt 绑定回写成后端能接受的 ASCII 单字符", async () => {
    const km = await freshKeymap();
    const combo = km.parseKeyCombo(ev({ key: "≈", code: "KeyX", altKey: true }));
    expect(combo).not.toBeNull();
    await km.setHotkey("toggle_pin", combo!);
    // 后端 `CharKind::from_json_str` 按字节长度校验，`"≈"` 会被判非法键
    expect(setHotkeyCalls).toEqual([
      { action: "toggle_pin", combo: { mods: ["alt"], key: "x" } },
    ]);
  });

  it("命名键与小键盘回车；code 缺失时回落 ev.key", async () => {
    const km = await freshKeymap();
    expect(km.parseKeyCombo(ev({ key: "Enter", code: "NumpadEnter" }))).toEqual({
      mods: [],
      key: { kind: "enter" },
    });
    expect(km.parseKeyCombo(ev({ key: "e", code: "" }))).toEqual({
      mods: [],
      key: { kind: "char", ch: "e" },
    });
    // 纯修饰键仍然拒绝
    expect(km.parseKeyCombo(ev({ key: "Alt", code: "AltLeft", altKey: true }))).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// 平台主修饰键：Ctrl ≡ meta 只在 Windows 成立
// ---------------------------------------------------------------------------

describe("平台主修饰键规则", () => {
  it("macOS：Control 是独立修饰键，⌃⌥L 不含 meta", async () => {
    const km = await freshKeymap({ os: "macos" });
    expect(
      km.parseKeyCombo(ev({ key: "¬", code: "KeyL", ctrlKey: true, altKey: true }))?.mods,
    ).toEqual(["alt", "ctrl"]);
  });

  it("Windows：Ctrl 是主修饰键，同一事件含 meta", async () => {
    const km = await freshKeymap({ os: "windows" });
    expect(km.parseKeyCombo(ev({ key: "l", code: "KeyL", ctrlKey: true }))?.mods).toEqual([
      "meta",
      "ctrl",
    ]);
  });

  it("macOS：绑到 ctrl 的组合能被 ⌃X 命中", async () => {
    const km = await freshKeymap({
      os: "macos",
      wire: { version: 1, actions: { toggle_pin: { mods: ["ctrl"], key: "x" } } },
    });
    expect(km.matches(ev({ key: "x", code: "KeyX", ctrlKey: true }))).toBe("toggle_pin");
  });

  it("macOS：⌃X 不再误命中绑在 ⌘X 上的动作", async () => {
    const km = await freshKeymap({
      os: "macos",
      wire: { version: 1, actions: { toggle_pin: { mods: ["meta"], key: "x" } } },
    });
    expect(km.matches(ev({ key: "x", code: "KeyX", ctrlKey: true }))).toBeNull();
  });

  // 已知缺口，不在本次修复范围内：Windows 上 Ctrl 会同时置 meta 与 ctrl，
  // comboMatches 的「不得有多余修饰键」分支会拒绝 meta-only 的绑定，因此
  // `Ctrl+E` 这类面板快捷键在 Windows 上触发不了（行为与修复前的基线一致）。
  // 修它要先回答「Windows 主修饰键在线格式里到底写 meta 还是 ctrl」——
  // 后端 app/src/hotkey.rs 把 Mods::META 翻成 MOD_WIN，而 Windows plan/RW2
  // 把默认呼出键描述为 Ctrl+Alt+V，两边尚未对齐。见 review finding #1。
  it.todo("Windows：Ctrl+X 能命中线格式 mods=[\"meta\"] 的绑定（待主修饰键语义决定）");
});

// ---------------------------------------------------------------------------
// 加密区模态内的 keydown（handleVaultKey）
// ---------------------------------------------------------------------------
//
// 模态与主面板共用**同一张 keymap**（含用户重绑），所以这里除了钉「默认键照常工作」，
// 还要钉「重绑能传导进模态」—— 曾经的实现把 ⌘⌫ / ⌘E 写死在模态里，用户重绑后
// 模态内按新键没反应、按旧键反而生效。

describe("handleVaultKey：加密区模态内的快捷键", () => {
  /** 记录 handler 调用序与 `preventDefault` 次数的最小 ctx。 */
  function vaultCtx() {
    const calls: string[] = [];
    return {
      calls,
      ctx: {
        closeVault: () => void calls.push("close"),
        moveVaultSelection: (d: number) => void calls.push(`move:${d}`),
        pasteVaultEntry: async () => void calls.push("paste"),
        editVaultNote: () => void calls.push("edit"),
        deleteVaultEntry: async () => void calls.push("delete"),
      },
    };
  }

  it("⌘⌫ 触发删除，并按下 preventDefault", async () => {
    const km = await freshKeymap();
    const { calls, ctx } = vaultCtx();
    const e = ev({ key: "Backspace", code: "Backspace", metaKey: true });
    const prevented = vi.spyOn(e, "preventDefault");

    expect(km.handleVaultKey(e, ctx)).toBe(true);
    expect(calls).toEqual(["delete"]);
    expect(prevented).toHaveBeenCalled();
  });

  it("⌘⇧⌫ 不是删除（也不落到别的 handler）", async () => {
    const km = await freshKeymap();
    const { calls, ctx } = vaultCtx();
    expect(
      km.handleVaultKey(
        ev({ key: "Backspace", code: "Backspace", metaKey: true, shiftKey: true }),
        ctx,
      ),
    ).toBe(false);
    expect(calls).toEqual([]);
  });

  it("按住 ⌘⌫ 的自动重复不再重复删除（事件仍被吞掉）", async () => {
    const km = await freshKeymap();
    const { calls, ctx } = vaultCtx();
    const press = (repeat: boolean) =>
      km.handleVaultKey(
        ev({ key: "Backspace", code: "Backspace", metaKey: true, repeat }),
        ctx,
      );

    expect(press(false)).toBe(true);
    expect(press(true)).toBe(true);
    expect(press(true)).toBe(true);
    expect(calls).toEqual(["delete"]);
  });

  it("既有键不受影响：esc / ↑↓ / ↩ / ⌘E 仍各司其职", async () => {
    const km = await freshKeymap();
    const { calls, ctx } = vaultCtx();
    km.handleVaultKey(ev({ key: "Escape", code: "Escape" }), ctx);
    km.handleVaultKey(ev({ key: "ArrowDown", code: "ArrowDown" }), ctx);
    km.handleVaultKey(ev({ key: "ArrowUp", code: "ArrowUp" }), ctx);
    km.handleVaultKey(ev({ key: "Enter", code: "Enter" }), ctx);
    km.handleVaultKey(ev({ key: "e", code: "KeyE", metaKey: true }), ctx);
    expect(calls).toEqual(["close", "move:1", "move:-1", "paste", "edit"]);
  });

  it("⌘⇧V（open_vault）再按一次收起模态", async () => {
    const km = await freshKeymap();
    const { calls, ctx } = vaultCtx();
    expect(
      km.handleVaultKey(ev({ key: "V", code: "KeyV", metaKey: true, shiftKey: true }), ctx),
    ).toBe(true);
    expect(calls).toEqual(["close"]);
  });

  it("重绑 delete_entry 后模态跟着走：新键删，旧 ⌘⌫ 不再删", async () => {
    const km = await freshKeymap({
      wire: {
        version: 1,
        actions: { delete_entry: { mods: ["meta", "shift"], key: "d" } },
      },
    });
    const { calls, ctx } = vaultCtx();

    // 旧绑定落空：既不该删，也不该落到别的 handler。
    expect(
      km.handleVaultKey(ev({ key: "Backspace", code: "Backspace", metaKey: true }), ctx),
    ).toBe(false);
    expect(calls).toEqual([]);

    // 新绑定生效（且仍吞掉事件）。
    expect(
      km.handleVaultKey(ev({ key: "D", code: "KeyD", metaKey: true, shiftKey: true }), ctx),
    ).toBe(true);
    expect(calls).toEqual(["delete"]);
  });

  it("Esc 是无条件逃生口：close 被重绑走，Esc 仍关得掉模态", async () => {
    const km = await freshKeymap({
      wire: { version: 1, actions: { close: { mods: [], key: "q" } } },
    });
    const { calls, ctx } = vaultCtx();
    expect(km.handleVaultKey(ev({ key: "Escape", code: "Escape" }), ctx)).toBe(true);
    expect(calls).toEqual(["close"]);
  });
});

// ---------------------------------------------------------------------------
// 命名键 token：前后端必须认同一张表（跨语言对照）
// ---------------------------------------------------------------------------

describe("命名键 token 的跨语言对照", () => {
  it("前端 NAMED_KEY_TOKENS 与后端 CharKind::from_json_str 逐项一致", async () => {
    const src = coreKeymapRs;
    // `CharKind::from_json_str` 函数体里的字符串字面量就是后端认的命名键全集
    // （首个匹配是 CharKind 的那个；`_ => None` 是它的收尾）。
    const start = src.indexOf("pub fn from_json_str");
    expect(start, "没找到 CharKind::from_json_str，后端重构后请更新这条测试").toBeGreaterThan(0);
    const body = src.slice(start, src.indexOf("_ => None", start));
    const backendTokens = [...body.matchAll(/"([a-z_]+)"\s*=>/g)].map((m) => m[1]).sort();
    expect(backendTokens).toHaveLength(6);

    const km = await freshKeymap();
    // 后端新增一个命名键而前端没跟 → 这里红；前端多加 token 而没改后端 → 也红。
    expect([...km.NAMED_KEY_TOKENS].sort()).toEqual(backendTokens);
  });

  it("认不出的 token 被丢弃，不再降级成永远匹配不上的多字符 char", async () => {
    const km = await freshKeymap({
      wire: { version: 1, actions: { toggle_pin: { mods: ["meta"], key: "f13" } } },
    });
    // 旧写法会得到 {kind:"char",ch:"f13"}：键帽显示假字符、匹配永远落空。
    // 现在直接丢掉这个动作（设置面板那行显示 `—`，仍可重绑救回来）。
    expect(km.currentKeyMap?.actions.toggle_pin).toBeUndefined();
    expect(km.matches(ev({ key: "f13", code: "F13", metaKey: true }))).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// U2 焦点门：⌘⌫ 该不该让位给文本控件
// ---------------------------------------------------------------------------

describe("textFieldOwnsDeleteKey：破坏性快捷键的焦点门", () => {
  /** 结构性替身：判据只读 tagName / value / type / isContentEditable，不需要 DOM。 */
  const el = (fields: Record<string, unknown>) =>
    ({ tagName: "INPUT", ...fields }) as unknown as EventTarget;

  it("空文本框不让位：原生 ⌘⌫ 无事可做，让位就等于面板里 ⌘⌫ 彻底失效", async () => {
    const km = await freshKeymap();
    expect(km.textFieldOwnsDeleteKey(el({ value: "" }))).toBe(false);
  });

  it("有内容的文本框 / 文本域让位：原生 ⌘⌫ 是「删到行首」", async () => {
    const km = await freshKeymap();
    expect(km.textFieldOwnsDeleteKey(el({ value: "pass" }))).toBe(true);
    expect(km.textFieldOwnsDeleteKey(el({ tagName: "TEXTAREA", value: "备注" }))).toBe(true);
  });

  it("不吃 ⌘⌫ 的元素与空目标都不让位", async () => {
    const km = await freshKeymap();
    // 非文本类 input：滑块 / 勾选框等，value 非空也不代表有可编辑文本。
    expect(km.textFieldOwnsDeleteKey(el({ type: "range", value: "60" }))).toBe(false);
    expect(km.textFieldOwnsDeleteKey(el({ type: "checkbox", value: "on" }))).toBe(false);
    // 列表行 / body（面板的常规焦点在搜索框，但点击列表等路径会换 target）。
    expect(km.textFieldOwnsDeleteKey(el({ tagName: "DIV" }))).toBe(false);
    expect(km.textFieldOwnsDeleteKey(null)).toBe(false);
  });

  it("contenteditable 一律让位（没法便宜地判断里面有没有内容）", async () => {
    const km = await freshKeymap();
    expect(km.textFieldOwnsDeleteKey(el({ tagName: "DIV", isContentEditable: true }))).toBe(true);
  });
});

describe("录制规则：无修饰的 Backspace 不可绑定", () => {
  it("裸 Backspace 被拒——绑上之后在任何输入框里按退格都会触发该动作", async () => {
    const km = await freshKeymap();
    expect(km.parseKeyCombo(ev({ key: "Backspace", code: "Backspace" }))).toBeNull();
  });

  it("带修饰键的 ⌘⌫ 仍可录（默认绑定的形状）", async () => {
    const km = await freshKeymap();
    expect(km.parseKeyCombo(ev({ key: "Backspace", code: "Backspace", metaKey: true }))).toEqual({
      mods: ["meta"],
      key: { kind: "backspace" },
    });
  });
});
