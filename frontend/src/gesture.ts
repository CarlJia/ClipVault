// frontend/src/gesture.ts
//
// 「等触发键松开」的小工具：回贴要把键盘焦点交还目标应用，交还之前先确认
// 触发这次回贴的那个键已经松开。
//
// 背景（真机 bug）：在 Edge / Chrome 里用 ⌘1–9 回贴时，面板会先隐藏、焦点交还
// 浏览器。若用户此刻仍按着数字键，交还之后系统发出的**按键自动重复**会继续落到
// 浏览器上，被它当成自己的「切到第 N 个标签页」—— 用户看到的是「按 ⌘1 没贴出
// 内容，浏览器却切了标签页」，随后注入的 ⌘V 也落到被切走的那一页。
//
// 为什么只等触发键、不等修饰键：自动重复由「键仍被按着」驱动，修饰键自身不会
// 重复。数字键一松开，目标应用就再也不可能收到 ⌘+数字；继续按着的 ⌘ 不构成
// 这个快捷键。反过来，依赖修饰键的 `keyup` 才是有风险的（少数情况下收不到就会
// 一路等到超时），没有必要冒这个险。
//
// 这个模块只做「等」这一件事：纯事件监听 + 一个有界定时器，不碰 DOM 结构，
// 因而可以在没有 jsdom 的环境里直测（见 gesture.test.ts）。

/** 手势的结束方式。 */
export type GestureEnd = "released" | "cancelled";

/**
 * 等触发键松开的上限。
 *
 * 超时按**取消**处理，而不是「照常回贴」：照常回贴等于把仍被按着的 ⌘+数字漏给
 * 目标应用，正是要修的那个 bug。宁可这一次不回贴（用户再按一次即可），也不切走
 * 用户正在看的标签页。
 */
export const GESTURE_RELEASE_MAX_MS = 1000;

/** 监听 keyup / blur 的宿主。生产传 `window`；测试传最小替身。 */
export interface GestureHost {
  addEventListener(type: "keyup" | "blur", listener: (ev: Event) => void): void;
  removeEventListener(type: "keyup" | "blur", listener: (ev: Event) => void): void;
}

/**
 * 等到 `key`（`KeyboardEvent.key`）被松开。
 *
 * - 松开 → `released`，调用方可以回贴；
 * - 面板失焦（用户点去别处 / 窗口被收起）→ `cancelled`；
 * - 超过 `maxMs` 仍未松开 → `cancelled`。
 *
 * 无论走哪条出口都会摘掉监听器与定时器，不泄漏。
 */
export function waitForKeyRelease(
  key: string,
  host: GestureHost,
  maxMs: number = GESTURE_RELEASE_MAX_MS,
): Promise<GestureEnd> {
  return new Promise<GestureEnd>((resolve) => {
    let settled = false;

    const finish = (end: GestureEnd): void => {
      if (settled) return;
      settled = true;
      host.removeEventListener("keyup", onKeyUp);
      host.removeEventListener("blur", onCancel);
      clearTimeout(timer);
      resolve(end);
    };
    const onKeyUp = (ev: Event): void => {
      if ((ev as KeyboardEvent).key === key) finish("released");
    };
    const onCancel = (): void => finish("cancelled");
    const timer = setTimeout(onCancel, maxMs);

    host.addEventListener("keyup", onKeyUp);
    host.addEventListener("blur", onCancel);
  });
}
