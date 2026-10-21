// frontend/src/editor.test.ts
//
// 「按 Esc 关闭备注编辑器」在 Windows 11 上偶发失灵（#30）的回归护栏。
//
// 修复前的隐性前提 —— 「焦点必须在 #editor 子元素内」：监听器挂在 #editor 根元素上
// （冒泡阶段，editor.ts:260），注释承诺 capture 但实现漏了第三个参数。同时 main.ts:947
// 在 isEditorOpen() 时早 return 整条 keymap 路径。两者依赖同一个隐含前提：
//
//   keydown.target ∈ #editor 子元素 → editor.ts 接到
//                                 → main.ts 早 return → Esc 关闭
//
// 当面板失焦被收起（panel::handle_window_event 的 Focused(false)）又重新唤出后，
// main.ts:1015 的 window focus 处理器把焦点拽回搜索框 —— #editor 子元素内不再有焦点。
// 此时 keydown.target 是 #search：editor.ts 收不到，main.ts 因 isEditorOpen() 早 return，
// Esc 死路。再加上编辑器模态没在 panel 收起时同步 close，下方"把搜索框挡在编辑器模态
// 后面"就是这条 bug 的可见形态。
//
// 修复后：监听器迁到 document + capture 阶段，加 ev.isComposing 守卫；
// main.ts:1015 在 isEditorOpen() 时也 closeEditor()。下面这 5 条用例钉住关键不变量。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

interface InvokeCall {
  cmd: string;
  args?: Record<string, unknown>;
}

// ---------------------------------------------------------------------------
// 最小 DOM 替身：只实现 editor.ts 真正用到的成员
// ---------------------------------------------------------------------------

class FakeClassList {
  private readonly set_ = new Set<string>();
  add(c: string): void {
    void this.set_.add(c);
  }
  remove(c: string): void {
    void this.set_.delete(c);
  }
  contains(c: string): boolean {
    return this.set_.has(c);
  }
  /** `toggle(c, force)`：force 为 `true` 强制 add，`false` 强制 remove；与 `add` 同义。 */
  toggle(c: string, force?: boolean): "added" | "removed" {
    if (force === true) {
      void this.set_.add(c);
      return "added";
    }
    if (force === false) {
      void this.set_.delete(c);
      return "removed";
    }
    if (this.set_.has(c)) {
      void this.set_.delete(c);
      return "removed";
    }
    void this.set_.add(c);
    return "added";
  }
}

class FakeEl {
  readonly classList = new FakeClassList();
  readonly dataset: Record<string, string> = {};
  value = "";
  textContent = "";
  innerHTML = "";
  readonly childNodes: FakeEl[] = [];
  focus(): void {
    /* noop */
  }
  blur(): void {
    /* noop */
  }
  select(): void {
    /* noop */
  }
  setSelectionRange(_start: number, _end: number): void {
    /* noop */
  }
  addEventListener(_type: string, _fn: EventListener): void {
    /* noop */
  }
  removeEventListener(_type: string, _fn: EventListener): void {
    /* noop */
  }
  querySelector(_sel: string): FakeEl | null {
    return null;
  }
  querySelectorAll(_sel: string): FakeEl[] {
    return [];
  }
  appendChild(c: FakeEl): void {
    void this.childNodes.push(c);
  }
  removeChild(c: FakeEl): void {
    void (this.childNodes.indexOf(c) >= 0 && this.childNodes.splice(this.childNodes.indexOf(c), 1));
  }
  insertBefore(c: FakeEl, ref: FakeEl | null): void {
    if (ref === null) {
      void this.childNodes.push(c);
      return;
    }
    const idx = this.childNodes.indexOf(ref);
    if (idx < 0) {
      void this.childNodes.push(c);
    } else {
      void this.childNodes.splice(idx, 0, c);
    }
  }
}

class FakeTextArea extends FakeEl {
  // 当前测试不需要 textarea 特有成员，留作占位以便区分。
}

class FakeInput extends FakeEl {
  // 当前测试不需要 input 特有成员。
}

class FakeButton extends FakeEl {
  clickListeners: EventListener[] = [];
  addEventListener(type: string, fn: EventListener): void {
    if (type === "click") void this.clickListeners.push(fn);
  }
}

interface KeydownListener {
  fn: EventListener;
  capture: boolean;
}

interface FakeDom {
  editorRoot: FakeEl;
  stateEl: FakeEl;
  previewEl: FakeEl;
  noteEl: FakeTextArea;
  counterEl: FakeEl;
  chipsEl: FakeEl;
  tagInputEl: FakeInput;
  retryEl: FakeButton;
  fakeDocument: {
    querySelector: (sel: string) => FakeEl | null;
    addEventListener: (
      type: string,
      fn: EventListener,
      opts?: boolean | { capture?: boolean },
    ) => void;
  };
  keydownListeners: KeydownListener[];
}

function makeFakeDom(): FakeDom {
  const editorRoot = new FakeEl();
  // index.html 里初始就是 hidden（与生产一致）。
  void editorRoot.classList.add("hidden");

  const stateEl = new FakeEl();
  const previewEl = new FakeEl();
  const noteEl = new FakeTextArea();
  const counterEl = new FakeEl();
  const chipsEl = new FakeEl();
  const tagInputEl = new FakeInput();
  const retryEl = new FakeButton();

  const byQuery: Record<string, FakeEl> = {
    "#editor": editorRoot,
    "#editor-state": stateEl,
    "#editor-preview": previewEl,
    "#editor-note": noteEl,
    "#editor-counter": counterEl,
    "#editor-chips": chipsEl,
    "#editor-tag-input": tagInputEl,
    "#editor-retry": retryEl,
  };

  const keydownListeners: KeydownListener[] = [];
  const fakeDocument: FakeDom["fakeDocument"] = {
    querySelector: (sel: string): FakeEl | null => byQuery[sel] ?? null,
    addEventListener: (
      type: string,
      fn: EventListener,
      opts?: boolean | { capture?: boolean },
    ): void => {
      if (type !== "keydown") return;
      // addEventListener 的第三参数可以是 `boolean`（capture 开关）或 `EventListenerOptions`
      // 对象；editor.ts 调的是 `document.addEventListener("keydown", fn, true)`，必须两种都认。
      const capture = typeof opts === "boolean" ? opts : opts?.capture === true;
      void keydownListeners.push({ fn, capture });
    },
  };

  return {
    editorRoot,
    stateEl,
    previewEl,
    noteEl,
    counterEl,
    chipsEl,
    tagInputEl,
    retryEl,
    fakeDocument,
    keydownListeners,
  };
}

/**
 * 触发当前模块在 `document` 上、capture 阶段（或 Bubble 阶段）注册的 keydown 监听器。
 *
 * 找不到目标阶段的监听器时**静默返回** —— 这是修复前的预期行为：原实现把监听器挂在
 * #editor 根元素上，fakeDocument 上没有任何 keydown 监听器，`fireKeydown` 返回后断言
 * 失败（isEditorOpen() 仍为 true），把 bug 红出来。修复后监听器迁到 fakeDocument，
 * `fireKeydown` 才能调到。
 */
function fireKeydown(
  fake: FakeDom,
  capture: boolean,
  init: {
    key: string;
    isComposing?: boolean;
    ctrlKey?: boolean;
    metaKey?: boolean;
    target?: unknown;
  },
): { called: boolean } {
  const target = fake.keydownListeners.find((l) => l.capture === capture);
  if (!target) return { called: false };
  const preventDefault = vi.fn();
  const stopPropagation = vi.fn();
  const ev = {
    key: init.key,
    code: "",
    isComposing: init.isComposing ?? false,
    ctrlKey: init.ctrlKey ?? false,
    metaKey: init.metaKey ?? false,
    shiftKey: false,
    altKey: false,
    repeat: false,
    target: init.target,
    preventDefault,
    stopPropagation,
  } as unknown as KeyboardEvent;
  target.fn(ev);
  return { called: true };
}

function stubEntry(
  id = 1,
): Parameters<typeof import("./editor").openEditor>[0] {
  return {
    id,
    content: "stub content",
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
  };
}

async function freshEditor(): Promise<{
  mod: typeof import("./editor");
  dom: FakeDom;
  invokeCalls: InvokeCall[];
}> {
  vi.resetModules();
  const dom = makeFakeDom();
  vi.stubGlobal("document", dom.fakeDocument);
  // rAF 在 openEditor 里被用到；不真跑动画，给个同步占位。
  vi.stubGlobal("window", {
    setTimeout: globalThis.setTimeout,
    clearTimeout: globalThis.clearTimeout,
    requestAnimationFrame: (cb: (v: number) => void): number => {
      cb(0);
      return 0;
    },
  });
  const invokeCalls: InvokeCall[] = [];
  vi.doMock("@tauri-apps/api/core", () => ({
    invoke: async (cmd: string, args?: Record<string, unknown>): Promise<unknown> => {
      invokeCalls.push({ cmd, args });
      // update_entry 返回带 id 的 entry 副本，与生产路径近似。
      if (cmd === "update_entry") {
        return {
          id: args?.id as number,
          content: "stub",
          note: (args?.note as string) ?? "",
          auto_category: "plain",
          tags: (args?.tags as string[]) ?? [],
          is_pinned: false,
          pinned_at: null,
          is_vault: false,
          is_decryptable: true,
          created_at: 0,
          content_kind: "text",
          mime_type: null,
          thumb: null,
          metadata: null,
        };
      }
      return undefined;
    },
  }));
  const mod = await import("./editor");
  return { mod, dom, invokeCalls };
}

beforeEach(() => {
  vi.unstubAllGlobals();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("编辑器打开时按 Esc 关闭（焦点无关）", () => {
  it("焦点在 #editor 子元素（textarea）时，capture 阶段的 document 监听器接到 Esc 并关闭", async () => {
    const { mod, dom } = await freshEditor();
    mod.openEditor(stubEntry(), () => undefined);
    expect(mod.isEditorOpen()).toBe(true);
    expect(dom.editorRoot.classList.contains("hidden")).toBe(false);

    const r = fireKeydown(dom, true, {
      key: "Escape",
      target: dom.noteEl, // target = textarea（#editor 子元素）
    });
    expect(r.called).toBe(true);
    expect(mod.isEditorOpen()).toBe(false);
    expect(dom.editorRoot.classList.contains("hidden")).toBe(true);
  });

  it("★ 焦点飘到 #editor 外（如搜索框）时，capture 阶段的 document 监听器仍能接到 Esc（#30 核心 bug）", async () => {
    const { mod, dom } = await freshEditor();
    mod.openEditor(stubEntry(), () => undefined);
    // 模拟 main.ts:1015 的 window focus 处理器把焦点拽回 #search —— 此刻 target
    // 路径不再经过 #editor，原实现下 editor.ts 收不到 keydown，Esc 失灵。
    const r = fireKeydown(dom, true, {
      key: "Escape",
      target: { id: "search" }, // target = 搜索框（不在 #editor 内）
    });
    expect(r.called).toBe(true);
    expect(mod.isEditorOpen()).toBe(false);
    expect(dom.editorRoot.classList.contains("hidden")).toBe(true);
  });

  it("IME 激活态（ev.isComposing = true）下 Esc 不关闭编辑器，让 IME 处理", async () => {
    const { mod, dom } = await freshEditor();
    mod.openEditor(stubEntry(), () => undefined);

    fireKeydown(dom, true, {
      key: "Escape",
      isComposing: true,
      target: dom.noteEl,
    });
    expect(mod.isEditorOpen()).toBe(true);
  });

  it("编辑器未打开时，capture 阶段监听器对 Esc 是 noop（不误伤主面板 keymap）", async () => {
    const { mod, dom } = await freshEditor();
    expect(mod.isEditorOpen()).toBe(false);
    const r = fireKeydown(dom, true, { key: "Escape", target: dom.noteEl });
    expect(r.called).toBe(true); // 监听器存在且被调
    expect(mod.isEditorOpen()).toBe(false); // 但因为 open=false，不关任何东西
  });

  it("Ctrl+S keydown 触发 preventDefault + stopPropagation（state=idle 也照拦，键事件不外溢）", async () => {
    const { mod, dom } = await freshEditor();
    mod.openEditor(stubEntry(), () => undefined);

    const listener = dom.keydownListeners.find((l) => l.capture === true);
    expect(listener).toBeDefined();
    const preventDefault = vi.fn();
    const stopPropagation = vi.fn();
    listener!.fn({
      key: "s",
        code: "",
        ctrlKey: true,
        metaKey: false,
        shiftKey: false,
        altKey: false,
        isComposing: false,
        repeat: false,
        target: dom.noteEl,
        preventDefault,
        stopPropagation,
      } as unknown as KeyboardEvent);
    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(stopPropagation).toHaveBeenCalledTimes(1);
  });

  it("Esc keydown 同样触发 preventDefault + stopPropagation（不外溢到 main.ts 的全局 keymap）", async () => {
    const { mod, dom } = await freshEditor();
    mod.openEditor(stubEntry(), () => undefined);

    const listener = dom.keydownListeners.find((l) => l.capture === true);
    expect(listener).toBeDefined();
    const preventDefault = vi.fn();
    const stopPropagation = vi.fn();
    listener!.fn({
      key: "Escape",
        code: "",
        ctrlKey: false,
        metaKey: false,
        shiftKey: false,
        altKey: false,
        isComposing: false,
        repeat: false,
        target: dom.noteEl,
        preventDefault,
        stopPropagation,
      } as unknown as KeyboardEvent);
    expect(preventDefault).toHaveBeenCalledTimes(1);
    expect(stopPropagation).toHaveBeenCalledTimes(1);
    expect(mod.isEditorOpen()).toBe(false);
  });
});