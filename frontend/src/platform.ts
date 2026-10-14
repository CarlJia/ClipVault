// 平台标识与平台化文案（U7 / KTD16）。
//
// 平台由后端 `platform_info` 命令给出，**不**在前端猜：WebView2 的 UA 串会随版本漂移，
// 而后端的 `std::env::consts::OS` 是事实。启动时取一次并缓存；取不到就按 macOS 渲染
// （与 settings.ts 里各 helper 的保守回退一致 —— 宁可显示 ⌘，也不要显示空白或乱码）。

import { invoke } from "@tauri-apps/api/core";

/** 后端 `platform_info` 的返回体。 */
interface PlatformInfo {
  /** `"macos"` | `"windows"` | `"linux"` | … */
  os: string;
}

let os = "macos";
let ready: Promise<void> | undefined;

/** 启动时调用一次；重复调用安全（第二次直接拿到同一个 promise）。 */
export function initPlatform(): Promise<void> {
  ready ??= (async () => {
    try {
      os = (await invoke<PlatformInfo>("platform_info")).os;
    } catch {
      os = "macos";
    }
    localizeKeycaps(document);
  })();
  return ready;
}

/**
 * 等平台标识就绪。任何**依赖平台**的渲染都先 await 它 —— 否则初始化期间的那次
 * IPC 往返会让首屏先用默认（macOS）文案渲染出来，而之后没人重渲染。
 */
export async function whenPlatformReady(): Promise<void> {
  await initPlatform();
}

/** 当前是否 Windows。 */
export function isWindows(): boolean {
  return os === "windows";
}

/** 当前是否 Linux。 */
export function isLinux(): boolean {
  return os === "linux";
}

/** 修饰键/按键符号 → Windows 写法的映射。 */
const KEY_LABELS: ReadonlyArray<readonly [string, string]> = [
  ["⌘", "Ctrl+"],
  ["⌥", "Alt+"],
  ["⇧", "Shift+"],
  ["↩", "Enter"],
];

/**
 * 把一段「按键说明」本地化：macOS 原样返回，Windows 换成 `Ctrl+` / `Shift+` / `Enter`。
 *
 * 入参是**完整**的按键写法（含修饰键），与 `<kbd>` 里的文本同形：
 * `keys("⌘E")` → `⌘E` / `Ctrl+E`，`keys("⌘⇧V")` → `Ctrl+Shift+V`，`keys("↩")` → `Enter`。
 * 用一个通用函数而不是只换 ⌘：`⌘⇧V`、`⌘⇧G` 这类双修饰键组合同样要换，拆成两个函数
 * 只会让调用点各写一遍、迟早漏一处。
 */
export function keys(spec: string): string {
  if (!isWindows()) return spec;
  let out = spec;
  for (const [symbol, label] of KEY_LABELS) out = out.split(symbol).join(label);
  // `esc` 是小写纯文本（不是符号），单独处理。
  return out.replace(/^esc$/i, "Esc");
}

/**
 * 把容器里所有 `<kbd>` 键帽本地化。四处提示条（主面板底部 + 三个 modal）的键帽
 * 已是结构化标记，所以启动时遍历改写即可，不需要 `data-key-hint` 之类的新标记。
 *
 * macOS 下直接返回：文案原样，连 DOM 都不碰。
 */
export function localizeKeycaps(root: ParentNode): void {
  if (!isWindows()) return;
  for (const kbd of root.querySelectorAll("kbd")) {
    kbd.textContent = keys((kbd.textContent ?? "").trim());
  }
}
