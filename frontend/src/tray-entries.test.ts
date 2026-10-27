/// <reference types="vite/client" />
// frontend/src/tray-entries.test.ts
//
// 托盘「借面板画布」的入口（加密区 / 设置… / 关于）的回归护栏。这个文件的正题是
// **加密区**：托盘菜单里一直挂着「加密区」，事件分发却把它并进 `show_panel` 的占位臂
// （注释写着「暂先打开主面板占位」），前端也没有 `open-vault` 的监听 —— 点下去只把
// 主列表弹出来，用户看到的就是「加密区点了没功能」。关于面板刚踩过同一个坑。
//
// 三条契约钉在这里：
// 1. `main.ts` 必须监听 `open-vault` 并转呼 `openVault()` —— 托盘只负责 emit；
// 2. `openVault()` 打开前收掉关于 / 设置 / 编辑器：托盘三个入口共用一块面板，
//    浮层叠着时 Esc 关掉的是被遮住的那层，用户会以为「按了没反应」；
// 3. `#vault` 三个锚点仍在（入口点了要有地方落）。
//
// 为什么还要 `overlay-intent`：`show_panel()` 会先让窗口拿到焦点，而 `main.ts` 的
// focus 处理器一律「回到主列表」（收回加密区）。这两件事都在异步事件里，谁先到取决于
// 平台事件循环 —— 于是「刚被托盘点名」的浮层要豁免 focus 重置。这层时序在 main.ts
// 里没法单测，故抽成纯函数在这里钉住语义。

import { describe, expect, it } from "vitest";
import indexHtml from "../index.html?raw";
import mainTs from "./main.ts?raw";
import { DEFAULT_OVERLAY_GRACE_MS, createOverlayGate } from "./overlay-intent";

/** 去掉所有空白后的源码：断言接线形状时不受换行 / 缩进（prettier 重排）影响。 */
const squeezed = mainTs.replace(/\s+/g, "");

describe("托盘「加密区」入口的源码契约", () => {
  it("前端监听了托盘发的 open-vault", () => {
    // 这条 listen 一删，点「加密区」就只剩面板弹一下 —— 正是用户报的现象。
    expect(mainTs).toContain('listen("open-vault"');
    // 监听体必须是打开加密区浮层，而不是随手 `closeVault()` / 空实现。
    expect(/listen\("open-vault"[\s\S]{0,200}openVault\(\)/.test(mainTs)).toBe(true);
  });

  it("openVault 在显示浮层之前收掉关于 / 设置 / 编辑器", () => {
    const body = /async function openVault\(\): Promise<void> \{([\s\S]*?)\n\}/.exec(mainTs)?.[1] ?? "";
    expect(body).not.toBe("");
    // 顺序也要对：先收底下那层，再显示自己 —— 反过来写等于把浮层又盖回去。
    const shownAt = body.indexOf('classList.remove("hidden")');
    expect(shownAt).toBeGreaterThan(-1);
    for (const call of ["closeAbout()", "closeSettings()", "closeEditor()"]) {
      const at = body.indexOf(call);
      expect(at, call).toBeGreaterThan(-1);
      expect(at, `${call} 应在显示浮层之前`).toBeLessThan(shownAt);
    }
  });

  it("index.html 仍有加密区浮层的锚点", () => {
    // 托盘入口最终落在这三个节点上：容器、列表、关闭按钮（鼠标用户的退出口）。
    expect(indexHtml).toContain('id="vault"');
    expect(indexHtml).toContain('id="vault-list"');
    expect(indexHtml).toContain('id="vault-close"');
  });

  it("focus 重置受豁免窗口保护，且叠加「浮层确实开着」", () => {
    // 精确到接线形状：把 `isFresh()` 的返回值丢掉、或去掉 `isVaultOpen()` 的与运算，
    // 都会让这段保护形同虚设（前者等于什么都没做，后者会把豁免借给不相干的 focus）。
    // 这是唯一能守住时序修复的断言，所以按去空白后的源码比对，避免格式变动假红。
    expect(squeezed).toContain("if(!(isVaultOpen()&&overlayGate.isFresh()))closeVault();");
  });
});

describe("面板唤出时的搜索框复位（#37）", () => {
  // 与上面同一段 focus 处理器：面板收起只是 `window.hide()`，WebView 不重载，
  // 所有「唤起时该复位什么」的契约都由这一个处理器承担，故放在同一文件里守。
  it("focus 重置会清空搜索框，且清空发生在重新拉列表之前", () => {
    // 上一次的查询词原样留在 `input.value` 里，而 `load()` 拿的正是这个值
    // （main.ts 的 `searchEl.value.trim()`），于是再唤出时搜索框和列表都还停在
    // 上次的查询上。只 `focus()` + `select()` 不够：全选只让下一次按键覆盖旧词，
    // 旧词仍可见、仍参与过滤。必须显式写空，而且要写在这一轮 `load()` **之前**，
    // 否则这次唤出先按旧词拉一遍、下一帧才纠正。
    const handler = /window\.addEventListener\("focus"[\s\S]*?\n\}\);/.exec(mainTs)?.[0] ?? "";
    expect(handler).not.toBe("");
    const body = handler.replace(/\s+/g, "");
    const cleared = body.indexOf('searchEl.value="";');
    expect(cleared, "focus 处理器必须清空 searchEl.value").toBeGreaterThan(-1);
    expect(cleared, "清空应在聚焦之前").toBeLessThan(body.indexOf("searchEl.focus()"));
    expect(cleared, "清空应在这次 load() 之前").toBeLessThan(body.lastIndexOf("voidload();"));
  });
});

describe("浮层豁免窗口（overlay-intent）", () => {
  it("托盘点名后紧接着的那次 focus 命中豁免", () => {
    const gate = createOverlayGate();
    gate.mark(1_000);
    expect(gate.isFresh(1_002)).toBe(true);
  });

  it("窗口期内可重复命中：一次唤出可能派发多次 focus", () => {
    // 一次性消费（consume 语义）会让第二次 focus 立刻把浮层收回 —— 原症状概率复现。
    const gate = createOverlayGate();
    gate.mark(1_000);
    expect(gate.isFresh(1_010)).toBe(true);
    expect(gate.isFresh(1_020)).toBe(true);
  });

  it("过期即作废，不会在之后的唤出上「复活」", () => {
    const gate = createOverlayGate(50);
    gate.mark(1_000);
    expect(gate.isFresh(1_051)).toBe(false);
    // 单调时钟：过期之后任何更晚的唤出都不该再命中。
    expect(gate.isFresh(9_999)).toBe(false);
  });

  it("没被点名时不豁免", () => {
    expect(createOverlayGate().isFresh(1_000)).toBe(false);
  });

  it("默认窗口足够宽，且不是永久", () => {
    expect(DEFAULT_OVERLAY_GRACE_MS).toBeGreaterThan(0);
    const gate = createOverlayGate();
    gate.mark(0);
    expect(gate.isFresh(DEFAULT_OVERLAY_GRACE_MS)).toBe(true);
    expect(gate.isFresh(DEFAULT_OVERLAY_GRACE_MS + 1)).toBe(false);
  });
});
