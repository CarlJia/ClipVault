// frontend/src/preview.test.ts
//
// 预览栏状态机与 kind 派发的回归护栏（列表旁预览栏 plan U3 / U4）。
//
// 这个文件存在的直接理由：预览栏跨了「DOM 渲染 + 一条 IPC + 面板尺寸」三件事，
// 任何一处写错都只表现为「按了 Space 没反应」或「窗口变宽了但栏是空的」——
// 真机看一眼不一定分得出是哪一段坏了。这里逐条钉住：
//
// 1. 开关状态机与尺寸 IPC（开 → expanded，关 → normal，没开时不发 IPC）；
// 2. 三种 content_kind 的渲染路径（图片走 IPC、明文文本不走、其它给提示）；
// 3. 缓存（同一图片第二次打开不再打 IPC，关栏不清缓存）；
// 4. 跟随选中（连按方向键时两个请求的结果不混）；
// 5. 加密区不可解密 → 「无法解密」，且**不**向 IPC 要明文之外的东西。
//
// preview.ts 是轻 DOM 模块，但这里仍用最小替身驱动、不引 jsdom（与 settings.test.ts
// 同一思路）：需要的只是 classList / innerHTML / dataset。

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EntryDto } from "./panel";

// ---------------------------------------------------------------------------
// 最小 DOM 替身
// ---------------------------------------------------------------------------

/** `#preview` 替身：只需要 classList / innerHTML / dataset。 */
class FakePane {
  innerHTML = "";
  readonly dataset: Record<string, string> = {};
  private readonly classes = new Set<string>(["preview-pane", "hidden"]);

  readonly classList = {
    add: (c: string): void => void this.classes.add(c),
    remove: (c: string): void => void this.classes.delete(c),
    contains: (c: string): boolean => this.classes.has(c),
  };
}

/** `EntryDto` 的最小构造器：只给用例关心的字段赋值。 */
function entry(over: Partial<EntryDto> & { id: number }): EntryDto {
  return {
    content: "",
    note: "",
    auto_category: "plain",
    tags: [],
    is_pinned: false,
    pinned_at: null,
    is_vault: false,
    is_decryptable: true,
    created_at: 0,
    content_kind: "text",
    mime_type: null,
    thumb: null,
    metadata: null,
    ...over,
  };
}

const PNG = "data:image/png;base64,AAAA";

type PreviewModule = typeof import("./preview");

/** IPC 调用记录 + 可编排的返回值。 */
interface InvokeLog {
  cmd: string;
  args?: Record<string, unknown>;
}

/**
 * 每个用例一份全新的 preview 模块（模块里有 `open` / `currentId` / 图片缓存三份
 * 模块级状态，跨用例复用会串味），并把 `invoke` 换成可控的桩。
 */
async function freshPreview(
  respond: (cmd: string, args?: Record<string, unknown>) => unknown = () => undefined,
): Promise<{ mod: PreviewModule; pane: FakePane; calls: InvokeLog[] }> {
  vi.resetModules();
  const calls: InvokeLog[] = [];
  const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    calls.push({ cmd, args });
    return respond(cmd, args);
  });
  vi.doMock("@tauri-apps/api/core", () => ({ invoke }));
  const mod = await import("./preview");
  const pane = new FakePane();
  mod.bindPreviewPane(pane as unknown as HTMLElement);
  return { mod, pane, calls };
}

/** 只取某一类 IPC 的调用（顺序敏感）。 */
const modeCalls = (calls: InvokeLog[]): string[] =>
  calls.filter((c) => c.cmd === "set_panel_mode").map((c) => String(c.args?.mode));

const previewCalls = (calls: InvokeLog[]): number[] =>
  calls.filter((c) => c.cmd === "get_entry_preview").map((c) => Number(c.args?.id));

/** 让挂起的 promise 链跑完（预览的 IPC 是 fire-and-forget 的异步补充）。 */
const flush = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  vi.resetModules();
  vi.unstubAllGlobals();
});

// ---------------------------------------------------------------------------
// 开关状态机 + 尺寸 IPC（R1 / R4 / R10）
// ---------------------------------------------------------------------------

describe("开关状态机与面板尺寸", () => {
  it("打开 → 栏可见、状态为开、窗口切 expanded", async () => {
    const { mod, pane, calls } = await freshPreview();
    mod.openPreview(entry({ id: 1, content: "全文" }));

    expect(mod.isPreviewOpen()).toBe(true);
    expect(mod.getPreviewEntryId()).toBe(1);
    expect(pane.classList.contains("hidden")).toBe(false);
    expect(modeCalls(calls)).toEqual(["expanded"]);
  });

  it("关闭 → 栏隐藏、状态复位、窗口收回 normal", async () => {
    const { mod, pane, calls } = await freshPreview();
    mod.openPreview(entry({ id: 1, content: "全文" }));
    await mod.closePreview();

    expect(mod.isPreviewOpen()).toBe(false);
    expect(mod.getPreviewEntryId()).toBeNull();
    expect(pane.classList.contains("hidden")).toBe(true);
    expect(modeCalls(calls)).toEqual(["expanded", "normal"]);
  });

  it("没开着时关一次是空转：不发 set_panel_mode", async () => {
    // 关面板的每条路径都会顺手调一次 closePreview（失焦 / 回贴 / 托盘），
    // 不该为此每次多打一条 IPC。
    const { mod, calls } = await freshPreview();
    await mod.closePreview();
    expect(modeCalls(calls)).toEqual([]);
    expect(calls).toEqual([]);
  });

  it("开着时再开另一条只换内容，不重复切尺寸（避免窗口抖动）", async () => {
    const { mod, calls } = await freshPreview();
    mod.openPreview(entry({ id: 1, content: "第一条" }));
    mod.openPreview(entry({ id: 2, content: "第二条" }));
    expect(modeCalls(calls)).toEqual(["expanded"]);
    expect(mod.getPreviewEntryId()).toBe(2);
  });

  it("toggle：开着就关，关着且有选中条目就开；没有选中条目时什么都不做", async () => {
    const { mod } = await freshPreview();
    mod.togglePreview(undefined);
    expect(mod.isPreviewOpen()).toBe(false);

    mod.togglePreview(entry({ id: 5, content: "x" }));
    expect(mod.isPreviewOpen()).toBe(true);

    mod.togglePreview(entry({ id: 5, content: "x" }));
    expect(mod.isPreviewOpen()).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// kind 派发（R2 / R3）
// ---------------------------------------------------------------------------

describe("按 content_kind 派发", () => {
  it("文本：直接用 DTO 里的 content，不走 IPC", async () => {
    const { mod, pane, calls } = await freshPreview();
    mod.openPreview(entry({ id: 1, content: "这是一段很长的正文\n第二行" }));

    expect(pane.innerHTML).toContain("这是一段很长的正文");
    expect(pane.innerHTML).toContain("第二行");
    expect(pane.dataset.kind).toBe("text");
    expect(previewCalls(calls)).toEqual([]);
  });

  it("图片：先占位、IPC 回来后渲染 data URL", async () => {
    const { mod, pane, calls } = await freshPreview((cmd) =>
      cmd === "get_entry_preview"
        ? { kind: "image", mime: "image/png", data_url: PNG, content: null, width: 1600, height: 800, undecryptable: false }
        : undefined,
    );
    mod.openPreview(entry({ id: 7, content_kind: "image" }));
    expect(pane.innerHTML).toContain("正在加载");
    expect(previewCalls(calls)).toEqual([7]);

    await flush();
    expect(pane.innerHTML).toContain(PNG);
    expect(pane.dataset.kind).toBe("image");
  });

  it("file / rtf / color：显示一句「无预览」提示，不发 IPC", async () => {
    const { mod, pane, calls } = await freshPreview();
    mod.openPreview(entry({ id: 9, content_kind: "color" }));
    expect(pane.innerHTML).toContain("无预览");
    expect(previewCalls(calls)).toEqual([]);
  });

  it("加密区文本：content 为空（密文不下发），必须走后端解密", async () => {
    const { mod, pane, calls } = await freshPreview((cmd) =>
      cmd === "get_entry_preview"
        ? { kind: "text", mime: null, data_url: null, content: "解密后的正文", width: null, height: null, undecryptable: false }
        : undefined,
    );
    mod.openPreview(entry({ id: 11, is_vault: true, content: "" }));

    expect(previewCalls(calls)).toEqual([11]);
    await flush();
    expect(pane.innerHTML).toContain("解密后的正文");
  });
});

// ---------------------------------------------------------------------------
// 缓存（R3 的「来回切不重复取同一张图」）
// ---------------------------------------------------------------------------

describe("图片缓存", () => {
  it("同一条第二次打开不再打 IPC；关栏不清缓存", async () => {
    const { mod, pane, calls } = await freshPreview((cmd) =>
      cmd === "get_entry_preview"
        ? { kind: "image", mime: "image/png", data_url: PNG, content: null, width: 10, height: 10, undecryptable: false }
        : undefined,
    );
    const img = entry({ id: 3, content_kind: "image" });

    mod.openPreview(img);
    await flush();
    await mod.closePreview();

    pane.innerHTML = "";
    mod.openPreview(img); // 命中缓存：应立即有图，且不再发 IPC
    expect(pane.innerHTML).toContain(PNG);
    expect(previewCalls(calls)).toEqual([3]);
  });

  it("resetPreviewCache 之后重新取（面板被重新唤出时用）", async () => {
    const { mod, calls } = await freshPreview((cmd) =>
      cmd === "get_entry_preview"
        ? { kind: "image", mime: "image/png", data_url: PNG, content: null, width: 10, height: 10, undecryptable: false }
        : undefined,
    );
    const img = entry({ id: 4, content_kind: "image" });
    mod.openPreview(img);
    await flush();
    await mod.closePreview();

    mod.resetPreviewCache();
    mod.openPreview(img);
    await flush();
    expect(previewCalls(calls)).toEqual([4, 4]);
  });
});

// ---------------------------------------------------------------------------
// 跟随选中（R3 / AE1）
// ---------------------------------------------------------------------------

describe("跟随选中", () => {
  it("栏开着时切到另一条：内容跟着换，且只显示最新那条", async () => {
    const { mod, pane, calls } = await freshPreview((cmd, args) => {
      if (cmd !== "get_entry_preview") return undefined;
      const id = Number(args?.id);
      return {
        kind: "image",
        mime: "image/png",
        data_url: `data:image/png;base64,IMG${id}`,
        content: null,
        width: 10,
        height: 10,
        undecryptable: false,
      };
    });

    mod.openPreview(entry({ id: 1, content_kind: "image" }));
    await flush();
    expect(pane.innerHTML).toContain("IMG1");

    mod.updatePreviewForSelection(entry({ id: 2, content_kind: "image" }));
    await flush();
    expect(pane.innerHTML).toContain("IMG2");
    expect(pane.innerHTML).not.toContain("IMG1");
    expect(previewCalls(calls)).toEqual([1, 2]);
  });

  it("栏没开时 updatePreviewForSelection 是空转（不偷开）", async () => {
    const { mod, calls } = await freshPreview();
    mod.updatePreviewForSelection(entry({ id: 2, content: "x" }));
    expect(mod.isPreviewOpen()).toBe(false);
    expect(calls).toEqual([]);
  });

  it("选中的条目消失 → 收起预览栏", async () => {
    const { mod } = await freshPreview();
    mod.openPreview(entry({ id: 1, content: "x" }));
    mod.updatePreviewForSelection(undefined);
    expect(mod.isPreviewOpen()).toBe(false);
  });

  it("慢请求回来时已经切走了：旧结果不覆盖新内容（AE1 的「结果不混」）", async () => {
    // id=1 的响应故意延后；期间切到 id=2 并让它的响应先到。
    const resolvers = new Map<number, (v: unknown) => void>();
    const { mod, pane } = await freshPreview((cmd, args) => {
      if (cmd !== "get_entry_preview") return undefined;
      const id = Number(args?.id);
      return new Promise((resolve) => resolvers.set(id, resolve));
    });

    mod.openPreview(entry({ id: 1, content_kind: "image" }));
    mod.updatePreviewForSelection(entry({ id: 2, content_kind: "image" }));

    resolvers.get(2)!({
      kind: "image",
      mime: "image/png",
      data_url: "data:image/png;base64,IMG2",
      content: null,
      width: 10,
      height: 10,
      undecryptable: false,
    });
    await flush();
    resolvers.get(1)!({
      kind: "image",
      mime: "image/png",
      data_url: "data:image/png;base64,IMG1",
      content: null,
      width: 10,
      height: 10,
      undecryptable: false,
    });
    await flush();

    expect(pane.innerHTML).toContain("IMG2");
    expect(pane.innerHTML).not.toContain("IMG1");
  });
});

// ---------------------------------------------------------------------------
// 加密区（R7 / AE3）
// ---------------------------------------------------------------------------

describe("加密区条目", () => {
  it("is_decryptable=false：直接显示「无法解密」，连 IPC 都不发", async () => {
    const { mod, pane, calls } = await freshPreview();
    mod.openPreview(entry({ id: 21, content_kind: "image", is_vault: true, is_decryptable: false }));

    expect(pane.innerHTML).toContain("无法解密");
    expect(previewCalls(calls)).toEqual([]);
  });

  it("后端回 undecryptable=true：同样显示「无法解密」，不渲染任何内容", async () => {
    const { mod, pane } = await freshPreview((cmd) =>
      cmd === "get_entry_preview"
        ? { kind: "text", mime: null, data_url: null, content: null, width: null, height: null, undecryptable: true }
        : undefined,
    );
    mod.openPreview(entry({ id: 22, is_vault: true, content: "" }));
    await flush();
    expect(pane.innerHTML).toContain("无法解密");
    expect(pane.innerHTML).not.toContain("<pre");
  });
});

// ---------------------------------------------------------------------------
// IPC 失败
// ---------------------------------------------------------------------------

describe("IPC 失败", () => {
  it("后端报错时在栏内显示失败原因，而不是留一个空栏", async () => {
    const { mod, pane } = await freshPreview((cmd) => {
      if (cmd === "get_entry_preview") throw new Error("entry 99 不存在");
      return undefined;
    });
    mod.openPreview(entry({ id: 99, content_kind: "image" }));
    await flush();
    expect(pane.innerHTML).toContain("预览失败");
    expect(pane.innerHTML).toContain("不存在");
  });
});
