/// <reference types="vite/client" />
// frontend/src/about.test.ts
//
// 「关于 ClipVault」浮层的回归护栏。这个文件的直接理由：托盘菜单里一直挂着
// 「关于」，事件分发却把它和「加密区」一起指向 `show_panel`（占位），
// 前端也没有任何关于界面 —— 点下去只会弹出主列表，用户看到的就是「关于内容
// 不展示」。占位注释写着「后续」，于是它就这么留到了发版。
//
// 三条契约钉在这里：
// 1. 关于面板必须展示**版本号**与 **GitHub 地址**（产品要求的最低集合）；
// 2. 后端拿不到版本时不渲染空串（空串会被当成「界面坏了」），统一回退「未知」；
// 3. `index.html` 里那四个锚点必须存在 —— 它们是把后端数据写进界面的唯一落点，
//    改名 / 删节点会让 renderAbout 静默写进 null。仓库里 `commands_test.rs` 用
//    源码静态比对守「命令没注册」同属一类缺口，这里守的是 DOM 契约。
//
// 与 settings.test.ts 同一思路：不引 jsdom，用最小元素替身驱动（about.ts 只碰
// textContent / title 这几个成员）。index.html 用 Vite 的 `?raw` 读，避免为此
// 引入 @types/node（keymap.test.ts 已用同一条 `<reference types="vite/client" />`）。

import { beforeEach, describe, expect, it, vi } from "vitest";
import indexHtml from "../index.html?raw";
import mainTs from "./main.ts?raw";
import {
  REPOSITORY_URL,
  UNKNOWN_VERSION,
  fetchAppInfo,
  normalizeAppInfo,
  openRepository,
  renderAbout,
} from "./about";

// IPC 替身：about.ts 的两个 invoke 封装（app_info / open_external_url）光靠人工
// 验收抓不到参数名写错 —— 命令名一改，前端只会在点开时静默失败。
const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

/** 带 `textContent` / `title` 的元素替身。 */
class FakeEl {
  textContent = "";
  title = "";
}

/** about.ts 只碰 textContent / title，替身按这三个成员窄化后交给 renderAbout。 */
const asEl = (el: FakeEl): HTMLElement => el as unknown as HTMLElement;

describe("关于面板的数据归一化", () => {
  it("后端给了版本与仓库地址时原样采用", () => {
    expect(normalizeAppInfo({ version: "0.8.0", repository: REPOSITORY_URL })).toEqual({
      version: "0.8.0",
      repository: REPOSITORY_URL,
    });
  });

  it("拿不到 app_info 时版本回退「未知」，仓库地址仍有值", () => {
    expect(normalizeAppInfo(null)).toEqual({
      version: UNKNOWN_VERSION,
      repository: REPOSITORY_URL,
    });
  });

  it("版本是空白串 / 仓库地址缺失时不渲染空值", () => {
    expect(normalizeAppInfo({ version: "  ", repository: "" }).version).toBe(UNKNOWN_VERSION);
    expect(normalizeAppInfo({ version: " 0.8.0 ", repository: "" })).toEqual({
      version: "0.8.0",
      repository: REPOSITORY_URL,
    });
  });
});

describe("关于面板的渲染", () => {
  it("版本与 GitHub 地址都写进对应节点", () => {
    const version = new FakeEl();
    const github = new FakeEl();
    renderAbout(
      { version: asEl(version), github: asEl(github) },
      { version: "0.8.0", repository: "https://github.com/CarlJia/ClipVault" },
    );
    expect(version.textContent).toBe("0.8.0");
    // 地址要能一眼读出来（而不是只藏在 title / href 里）。
    expect(github.textContent).toBe("https://github.com/CarlJia/ClipVault");
    // 悬停提示说明「点它会发生什么」，否则用户不会去点。
    expect(github.title).toContain("在浏览器打开");
  });

  it("版本缺失时渲染「未知」而不是空串", () => {
    const version = new FakeEl();
    const github = new FakeEl();
    renderAbout({ version: asEl(version), github: asEl(github) }, normalizeAppInfo(null));
    expect(version.textContent).toBe(UNKNOWN_VERSION);
    expect(version.textContent.trim()).not.toBe("");
  });
});

describe("关于面板的 DOM 落点（index.html 契约）", () => {
  it("存在关于浮层与版本 / 地址节点", () => {
    expect(indexHtml).toContain('id="about"');
    expect(indexHtml).toContain('id="about-version"');
    expect(indexHtml).toContain('id="about-github"');
    // 关闭按钮：没有它，非 Esc 路径（比如鼠标用户）关不掉浮层。
    expect(indexHtml).toContain('id="about-close"');
  });

  it("GitHub 地址是按钮（可点击），且指向本仓库", () => {
    const tag = /<button[^>]*id="about-github"[^>]*>/.exec(indexHtml)?.[0] ?? "";
    expect(tag).not.toBe("");
    expect(REPOSITORY_URL).toBe("https://github.com/CarlJia/ClipVault");
    expect(REPOSITORY_URL.startsWith("https://")).toBe(true);
  });

  it("前端确实监听了托盘发的 open-about", () => {
    // 托盘只负责 emit：这条 listen 一删，点「关于」就只剩面板弹一下 ——
    // 正是用户报的那个现象。main.ts 不参与单测，这里守源码契约。
    expect(mainTs).toContain('listen("open-about"');
  });
});

describe("关于面板的 IPC 封装", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("fetchAppInfo 读取 app_info 并归一化", async () => {
    invokeMock.mockResolvedValue({ version: "0.8.0", repository: REPOSITORY_URL });
    await expect(fetchAppInfo()).resolves.toEqual({
      version: "0.8.0",
      repository: REPOSITORY_URL,
    });
    expect(invokeMock).toHaveBeenCalledWith("app_info");
  });

  it("app_info 失败时不抛错，版本回退「未知」", async () => {
    invokeMock.mockRejectedValue(new Error("command app_info not found"));
    await expect(fetchAppInfo()).resolves.toEqual({
      version: UNKNOWN_VERSION,
      repository: REPOSITORY_URL,
    });
  });

  it("openRepository 打开的是传进来的那条地址", async () => {
    invokeMock.mockResolvedValue(undefined);
    await openRepository("https://github.com/CarlJia/ClipVault");
    expect(invokeMock).toHaveBeenCalledWith("open_external_url", {
      url: "https://github.com/CarlJia/ClipVault",
    });
  });
});
