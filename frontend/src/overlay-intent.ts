/**
 * 「刚被托盘点名打开」的浮层豁免窗口。
 *
 * 背景：托盘没有 UI，加密区 / 设置 / 关于三个入口都是「先 `show_panel()` 让窗口
 * 拿到焦点，再 emit 一个事件让前端打开对应浮层」。而前端 `main.ts` 的 focus 处理器
 * 每次唤出都做一次重置 —— 其中一条是「收回加密区，从主列表开始」。
 *
 * 这两件事都在异步事件里，谁先到取决于平台事件循环：macOS 的 key window 通知与
 * Windows 的 WM_ACTIVATE 都可能晚于 `emit` 派发到 JS。于是「刚被托盘点名」的浮层
 * 会被紧随其后的 focus 重置立刻抹掉 —— 表现与「点了没功能」完全一样，而且只有
 * 点托盘才复现（`⌘⇧V` 走的是 keydown，不经过这条路径）。
 *
 * 语义：托盘事件到达时 `mark()` 一个时间戳，focus 处理器在处理重置前问 `isFresh()`。
 *
 * - **不清除**：一次 `show() + set_focus()` 在个别平台会派发不止一次 focus，
 *   一次性消费会让第二次 focus 立刻把浮层收回去（原症状概率性复现）。
 * - **有过期**：否则一次点名会让此后所有唤出都不再重置，加密区会一直挂在屏幕上。
 * - 调用方还要叠加「浮层确实开着」这个条件（`isVaultOpen()`）：focus 先到的那一
 *   顺序里，`mark()` 之后浮层才打开，这期间若发生一次与本请求无关的 focus，光是
 *   时间新鲜并不足以说明「该豁免」。
 */
export const DEFAULT_OVERLAY_GRACE_MS = 1000;

export interface OverlayGate {
  /** 记一次「托盘刚点名要打开某个浮层」。 */
  mark(now?: number): void;
  /** 最近一次点名是否仍在豁免窗口内（纯查询，不改状态）。 */
  isFresh(now?: number): boolean;
}

/** 造一个独立的闸门（不在模块顶层持有状态：测试各用例互不干扰）。 */
export function createOverlayGate(graceMs: number = DEFAULT_OVERLAY_GRACE_MS): OverlayGate {
  let requestedAt: number | null = null;
  return {
    mark(now: number = performance.now()): void {
      requestedAt = now;
    },
    isFresh(now: number = performance.now()): boolean {
      if (requestedAt === null) return false;
      // 单调时钟：过期的戳不会自己回到窗口内，故不必在查询里清状态。
      return now - requestedAt <= graceMs;
    },
  };
}
