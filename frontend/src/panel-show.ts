// ClipVault「面板被唤出」信号（#42）。
//
// ## 为什么需要这条信号
//
// 面板唤出时前端要做的收尾（收回浮层、清空搜索框、重拉列表）原来**只**挂在 WebView
// 的 DOM `focus` 事件上（`main.ts`）。但那条信号并不可靠：
//
// - 面板收起只是 `window.hide()`（macOS 的 `orderOut`），WebView 不重载；
// - macOS 上隐藏时若 App 仍然活跃，**页面不会失焦**；之后 `show()` + `set_focus()`
//   也不会让页面重新拿到 DOM 焦点 —— 于是**不再派发 DOM `focus`**，
//   前端收不到任何唤出信号，列表就停在上一次的内容；
// - 面板本来就开着时（用户按了一次热键想「让它刷新」），`show()` / `set_focus()`
//   都是空操作，更不会有任何焦点事件。
//
// 用户看到的现象就是：**打开面板不刷新，鼠标点一下才刷新**。
//
// 后端因此补了一条显式的「面板被唤出」信号（`app/src/panel/mod.rs` 的
// `PANEL_SHOWN_EVENT`）—— 它由唤出入口直接发出，不依赖任何平台焦点语义。
// DOM `focus` 退居兜底：只在「最近没有收到过后端信号」时才补一次重置。

/**
 * 后端「面板被唤出」事件名。
 *
 * 必须与 `app/src/panel/mod.rs` 的 `PANEL_SHOWN_EVENT` 逐字一致 ——
 * `panel-show.test.ts` 会用 `?raw` 读那份 Rust 源码做跨语言对照。
 */
export const PANEL_SHOWN_EVENT = "panel-shown";

/**
 * 兜底窗口：收到后端信号后，多久之内的 DOM `focus` 视为冗余。
 *
 * 与 `overlay-intent.ts` 的 `DEFAULT_OVERLAY_GRACE_MS` 一样，都是为了对付
 * 「平台事件先后不确定」而设的闸门，故取同一个值（1000ms），review 时好对照。
 *
 * **不得低于 500ms**：Windows 上 `show()` 是投递到主线程的异步调用，后端信号与
 * DOM `focus` 谁先到、隔多久都不可控。
 */
export const DEFAULT_SHOW_GRACE_MS = 1000;

/**
 * 「同一次唤出只重置一次」的去重闸门。
 *
 * 语义是**按后端信号去重**，不是「按重置去重」：
 *
 * - 后端信号到达 → **总是**执行重置（权威信号，永不被吞：吞了就等于又回到
 *   「打开面板不刷新」）；
 * - DOM `focus` 到达 → 只有在「最近没有收到过后端信号」时才执行 —— 它是
 *   「后端信号丢了」的兜底，不是第二套刷新逻辑。
 *
 * 反过来（按重置去重）会真的丢一次刷新：两次唤出间隔小于窗口时，第二次的
 * 重置被吞掉，用户按了热键却什么都没发生。
 */
export interface ShowResetGate {
  /** 后端「面板被唤出」信号到达：记下时间戳（供 DOM `focus` 去重用）。 */
  noteShown(now?: number): void;
  /**
   * DOM `focus` 到达时是否需要补一次重置。
   *
   * 最近收到过后端信号 → `false`（冗余）；否则 → `true`（信号可能丢了）。
   */
  shouldRunOnFocus(now?: number): boolean;
}

/** 造一个独立的闸门（不在模块顶层持有状态：测试各用例互不干扰）。 */
export function createShowResetGate(
  graceMs: number = DEFAULT_SHOW_GRACE_MS,
): ShowResetGate {
  let shownAt: number | null = null;
  return {
    noteShown(now: number = performance.now()): void {
      shownAt = now;
    },
    shouldRunOnFocus(now: number = performance.now()): boolean {
      if (shownAt === null) return true;
      // 单调时钟：过期的时间戳不会自己回到窗口内。
      return now - shownAt > graceMs;
    },
  };
}

/**
 * 后端信号的载荷：`{ fresh: boolean }`。
 *
 * `fresh` = 面板**此前是隐藏的**（一次真正的「唤出」）。
 */
export interface PanelShownPayload {
  fresh: boolean;
}

/**
 * 从事件载荷里读 `fresh`。
 *
 * 载荷缺失 / 形状不对时按 `true` 处理（当作一次真正的唤出）：宁可多做一次复位，
 * 也不要让面板停在上一次的状态 —— 这正是本次要修的现象。
 */
export function readFresh(payload: unknown): boolean {
  if (typeof payload !== "object" || payload === null) return true;
  const fresh = (payload as { fresh?: unknown }).fresh;
  return typeof fresh === "boolean" ? fresh : true;
}
