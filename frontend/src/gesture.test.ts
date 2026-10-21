// frontend/src/gesture.test.ts
//
// 「等触发键松开再回贴」的回归护栏（⌘1–9 在浏览器里的快捷键冲突）。
//
// 这个文件存在的直接理由：回贴会把面板藏起来、把键盘焦点交还目标应用。若用户此刻
// 仍按着数字键，交还之后系统发出的按键自动重复会落到目标应用上 —— Edge / Chrome
// 把 ⌘1–9 当作「切到第 N 个标签页」，于是「按 ⌘1 回贴」变成「浏览器切标签页」，
// 随后注入的 ⌘V 也落到被切走的那一页。真机上只表现为「没贴出内容」，看不出是
// 手势尾部的重复按键漏了出去，所以把判定逐条钉在这里：
//
// 1. 触发键松开 → released；松开的是别的键 → 继续等；
// 2. 面板失焦（用户点去别处）→ 取消，不回贴；
// 3. 超时仍未松开 → 取消，宁可这次不回贴，也不把重复按键漏给目标应用；
// 4. 无论走哪条出口，监听器与定时器都要摘干净。
//
// 不引 jsdom：需要的只是 addEventListener / removeEventListener 与定时器。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  GESTURE_RELEASE_MAX_MS,
  type GestureHost,
  waitForKeyRelease,
} from "./gesture";

// ---------------------------------------------------------------------------
// 最小宿主替身：只实现 addEventListener / removeEventListener
// ---------------------------------------------------------------------------

type Listener = (ev: Event) => void;

/** 可派发 keyup / blur 的事件宿主替身。 */
class FakeHost implements GestureHost {
  private readonly listeners = new Map<string, Set<Listener>>();

  addEventListener(type: "keyup" | "blur", listener: Listener): void {
    let set = this.listeners.get(type);
    if (!set) {
      set = new Set();
      this.listeners.set(type, set);
    }
    set.add(listener);
  }

  removeEventListener(type: "keyup" | "blur", listener: Listener): void {
    this.listeners.get(type)?.delete(listener);
  }

  /** 派发一次 `keyup`（`key` 为 `KeyboardEvent.key`）。 */
  keyUp(key: string): void {
    for (const fn of [...(this.listeners.get("keyup") ?? [])]) {
      fn({ key } as KeyboardEvent);
    }
  }

  /** 派发一次 `blur`。 */
  blur(): void {
    for (const fn of [...(this.listeners.get("blur") ?? [])]) {
      fn({} as KeyboardEvent);
    }
  }

  /** 当前挂着的监听器数量（用于钉住「走完所有出口都不泄漏」）。 */
  count(type: "keyup" | "blur"): number {
    return this.listeners.get(type)?.size ?? 0;
  }
}

/** 让已 resolve 的 Promise 链跑一拍，便于断言「还没结束」。 */
const flush = (): Promise<void> => Promise.resolve();

describe("waitForKeyRelease", () => {
  let host: FakeHost;

  beforeEach(() => {
    host = new FakeHost();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("触发键松开 → released", async () => {
    const end = waitForKeyRelease("1", host);
    host.keyUp("1");
    await expect(end).resolves.toBe("released");
  });

  it("松开的是别的键 → 继续等，不放行回贴", async () => {
    let result: string | null = null;
    void waitForKeyRelease("1", host).then((r) => {
      result = r;
    });
    host.keyUp("Meta");
    host.keyUp("2");
    await flush();
    expect(result).toBeNull();
  });

  it("面板失焦 → cancelled（用户已经点去别处，不该再回贴）", async () => {
    const end = waitForKeyRelease("1", host);
    host.blur();
    await expect(end).resolves.toBe("cancelled");
  });

  it("超时仍未松开 → cancelled，不把重复按键漏给目标应用", async () => {
    vi.useFakeTimers();
    const end = waitForKeyRelease("1", host);
    host.keyUp("Meta");
    vi.advanceTimersByTime(GESTURE_RELEASE_MAX_MS);
    await expect(end).resolves.toBe("cancelled");
  });

  it("released / cancelled / 超时三种出口都摘干净监听器与定时器", async () => {
    vi.useFakeTimers();

    // released
    const a = new FakeHost();
    const pa = waitForKeyRelease("1", a);
    a.keyUp("1");
    await expect(pa).resolves.toBe("released");
    expect(a.count("keyup")).toBe(0);
    expect(a.count("blur")).toBe(0);

    // cancelled（失焦）
    const b = new FakeHost();
    const pb = waitForKeyRelease("1", b);
    b.blur();
    await expect(pb).resolves.toBe("cancelled");
    expect(b.count("keyup")).toBe(0);
    expect(b.count("blur")).toBe(0);

    // cancelled（超时）
    const c = new FakeHost();
    const pc = waitForKeyRelease("1", c);
    vi.advanceTimersByTime(GESTURE_RELEASE_MAX_MS);
    await expect(pc).resolves.toBe("cancelled");
    expect(c.count("keyup")).toBe(0);
    expect(c.count("blur")).toBe(0);

    // 定时器都已清掉：没有任何待处理的假定时器残留。
    expect(vi.getTimerCount()).toBe(0);
  });
});
