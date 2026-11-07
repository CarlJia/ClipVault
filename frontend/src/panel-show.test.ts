/// <reference types="vite/client" />
// frontend/src/panel-show.test.ts
//
// 「面板被唤出」信号的回归护栏（#42）。
//
// 这个 bug 的形状很容易复发：面板唤出时前端要刷新，而**唯一**的触发点曾经是
// WebView 的 DOM `focus` 事件 —— 那条信号在「隐藏后重显」时根本不派发（macOS 上
// 隐藏时若 App 仍然活跃，页面不会失焦），于是用户看到「打开面板不刷新，鼠标点一下
// 才刷新」。这里钉住三件事：
//
// 1. 闸门语义（`createShowResetGate`）：后端信号**永不被吞**，DOM focus 只在
//    「最近没有收到过后端信号」时才补一次重置；
// 2. 载荷语义（`readFresh`）：`fresh` 决定这次要不要做破坏性复位；
// 3. **跨语言接线**：后端 Rust 侧确实发了这个事件、事件名与载荷字段与前端一致、
//    前端确实监听了 —— 任一端改名 / 漏接线，前端就会静默地退回「不刷新」。
//
// 3 里那几条断言直接读 Rust 源码（`?raw`），与 `keymap.test.ts` 里「命名键 token 的
// 跨语言对照」同一手法：不给前端引 `@types/node`，也不必为了改名去改两边。

import { describe, expect, it } from "vitest";
import mainTs from "./main.ts?raw";
import panelRs from "../../app/src/panel/mod.rs?raw";
import commandsRaw from "../../app/src/commands.rs?raw";
import {
  createShowResetGate,
  DEFAULT_SHOW_GRACE_MS,
  PANEL_SHOWN_EVENT,
  readFresh,
} from "./panel-show";

describe("唤出去重闸门（show-reset gate）", () => {
  it("没收到过后端信号时，DOM focus 要补一次重置", () => {
    // 后端信号丢了（页面还没就绪）时的兜底路径 —— 这条不能没有。
    const gate = createShowResetGate();
    expect(gate.shouldRunOnFocus(1_000)).toBe(true);
  });

  it("收到过后端信号后，窗口期内的 DOM focus 视为冗余", () => {
    // 同一次唤出会派发两条信号（后端事件 + DOM focus）。两条都跑一遍等于把
    // 搜索框清空两次、列表拉两次。
    const gate = createShowResetGate();
    gate.noteShown(1_000);
    expect(gate.shouldRunOnFocus(1_001)).toBe(false);
    expect(gate.shouldRunOnFocus(1_000 + DEFAULT_SHOW_GRACE_MS)).toBe(false);
  });

  it("窗口期过后，DOM focus 又能兜底", () => {
    // 窗口不是永久的：后端信号真丢了的时候，晚到的 focus 仍要能补上。
    const gate = createShowResetGate();
    gate.noteShown(1_000);
    expect(gate.shouldRunOnFocus(1_000 + DEFAULT_SHOW_GRACE_MS + 1)).toBe(true);
  });

  it("后端信号本身永不被吞", () => {
    // 闸门只拦 DOM focus 那条冗余路径：`noteShown` 不返回「要不要执行」，
    // 调用方无条件执行 —— 吞掉它就等于又回到「打开面板不刷新」。
    const gate = createShowResetGate();
    gate.noteShown(1_000);
    gate.noteShown(1_010);
    expect(gate.shouldRunOnFocus(1_020)).toBe(false);
  });

  it("默认窗口足够宽，且不是永久", () => {
    expect(DEFAULT_SHOW_GRACE_MS).toBeGreaterThanOrEqual(500);
    const gate = createShowResetGate();
    gate.noteShown(0);
    expect(gate.shouldRunOnFocus(DEFAULT_SHOW_GRACE_MS)).toBe(false);
    expect(gate.shouldRunOnFocus(DEFAULT_SHOW_GRACE_MS + 1)).toBe(true);
  });
});

describe("载荷语义（readFresh）", () => {
  it("fresh=true：一次真正的唤出（面板此前是隐藏的）", () => {
    expect(readFresh({ fresh: true })).toBe(true);
  });

  it("fresh=false：面板本来就开着，只刷新数据不动用户输入", () => {
    expect(readFresh({ fresh: false })).toBe(false);
  });

  it("载荷缺失或形状不对时按 fresh=true 处理", () => {
    // 宁可多做一次复位，也不要让面板停在上一次的状态 —— 那正是本次要修的现象。
    expect(readFresh(undefined)).toBe(true);
    expect(readFresh(null)).toBe(true);
    expect(readFresh({})).toBe(true);
    expect(readFresh({ fresh: "yes" })).toBe(true);
    expect(readFresh("panel-shown")).toBe(true);
  });
});

describe("跨语言接线（后端 Rust ↔ 前端 TS）", () => {
  it("事件名两边逐字一致", () => {
    // 后端改名而前端没改 → 前端永远收不到信号，且没有任何本地用例会红。
    const m = /pub const PANEL_SHOWN_EVENT: &str = "([^"]+)";/.exec(panelRs);
    expect(m, "Rust 侧应导出 PANEL_SHOWN_EVENT 常量").not.toBeNull();
    expect(m?.[1]).toBe(PANEL_SHOWN_EVENT);
  });

  it("载荷字段名两边一致", () => {
    // 前端读 `fresh`；Rust 侧字段改名 → 前端读到 undefined → 退回「总是复位」，
    // 表现为「面板开着时按热键会把用户正在输入的搜索词抹掉」。
    expect(panelRs).toContain("pub fresh: bool");
    expect(panelRs).toContain("pub struct PanelShownPayload");
  });

  it("后端确实在唤出入口发了这个事件", () => {
    // `show_panel` 是唤出入口；它不发光信号，前端就只剩不可靠的 DOM focus。
    const showPanel = /pub\(crate\) fn show_panel[\s\S]*?\n\}/.exec(panelRs)?.[0] ?? "";
    expect(showPanel).not.toBe("");
    expect(showPanel).toContain("notify_shown(&window, fresh)");
    // 且 `fresh` 必须在 show() 之前取（show 之后恒为可见）。
    expect(showPanel.indexOf("is_hidden(&window)")).toBeLessThan(showPanel.indexOf("window.show()"));
  });

  it("回贴降级分支重新显示面板时也发了信号", () => {
    // 那三条分支（window_switched / clipboard_only / no_accessibility）自己
    // `window.show()`，不经过 `show_panel` —— 不发信号的话，面板会带着上一次的
    // 列表重新出现（WebView 不重载）。
    const commandsRs = commandsRaw;
    expect(commandsRs).toContain("crate::panel::notify_shown(&window, fresh)");
    // 三处降级分支各发一次。
    expect(commandsRs.split("crate::panel::notify_shown(&window, fresh)").length - 1).toBe(3);
  });

  it("前端监听了这个事件，并按 fresh 分流", () => {
    expect(mainTs).toContain("listen(PANEL_SHOWN_EVENT");
    expect(mainTs).toContain("resetPanelOnShow(readFresh(event.payload))");
    // DOM focus 保留为兜底：后端信号丢了时仍要能刷新。
    expect(mainTs).toContain('window.addEventListener("focus"');
    expect(mainTs).toContain("showGate.shouldRunOnFocus()");
  });
});
