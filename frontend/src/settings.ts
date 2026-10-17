// ClipVault 设置 / 权限层（U4 建横幅槽位；U8 补全首启引导与授权后复检；
// U10 补全访客模式 / 「本次不记录」开关的持久化对接）。
//
// 负责 Accessibility 权限横幅（AE7）的完整首启引导 + 访客模式 / 「本次不记录」开关。
// 访客模式经后端持久化到 settings 表并同步共享原子 / 托盘；skip-once 为纯运行时。

import { invoke } from "@tauri-apps/api/core";
import { WARN_SVG, type EntryDto } from "./panel";
import { isWindows, isLinux, keys, whenPlatformReady } from "./platform";
import {
  actionOwningCombo,
  comboEquals,
  currentKeyMap,
  localizedCombo,
  parseKeyCombo,
  reloadKeymap,
  setHotkey,
  setHotkeyRecording,
  type HotkeyAction,
  type KeyCombo,
} from "./keymap";

/** `paste_entry` 的返回体：区分回贴成功分支与对应固定 toast 文案。 */
export interface PasteResult {
  /** `pasted` | `window_switched` | `clipboard_only`。 */
  status: string;
  /** 需要展示的提示（空串表示无需提示）。 */
  message: string;
}

/** 打开「系统设置 → 隐私与安全性 → 辅助功能」的深链。 */
const ACCESSIBILITY_PREF_URL =
  "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/** 点击深链后自动复检授权的延时（give the user time to grant）。 */
const RECHECK_DELAY_MS = 15_000;

let recheckTimer: number | undefined;

/** 查询后端 `check_accessibility()`（即 `AXIsProcessTrusted()`）。 */
async function isTrusted(): Promise<boolean> {
  try {
    return await invoke<boolean>("check_accessibility");
  } catch {
    // 检测失败时不打扰用户，视为已授权。
    return true;
  }
}

/**
 * 刷新 Accessibility 横幅：未授权时展示首启引导，已授权时隐藏并清空。
 *
 * 引导内容（计划 U8）：
 * - 主按钮「打开系统设置」→ 深链;点击后 15s 自动复检并在授权后清横幅。
 * - 次按钮「暂时只写到剪贴板」→ 让用户知道可退化为手动 ⌘V(实际回贴时若仍
 *   未授权,`paste_entry` 会返回 `missing-accessibility`,内容仍写入剪贴板)。
 * - 「重新检查」→ 立即复检。
 */
export async function refreshAccessibilityBanner(
  banner: HTMLElement,
): Promise<void> {
  // 文案里有修饰键（⌘V / Ctrl+V），等平台标识就绪再渲染。
  await whenPlatformReady();

  // Windows 上没有「辅助功能授权」这个概念（RW8）：后端 `check_accessibility` 恒为
  // 已授权，横幅本就不会显示。这里显式短路是为了两件事：不让下面那个 15 秒自动复检
  // 定时器空转，也不让 macOS 的系统设置深链与文案有任何机会漏到 Windows 上。
  if (isWindows()) {
    if (recheckTimer !== undefined) {
      window.clearTimeout(recheckTimer);
      recheckTimer = undefined;
    }
    banner.classList.add("hidden");
    banner.innerHTML = "";
    return;
  }

  const trusted = await isTrusted();

  if (trusted) {
    if (recheckTimer !== undefined) {
      window.clearTimeout(recheckTimer);
      recheckTimer = undefined;
    }
    banner.classList.add("hidden");
    banner.innerHTML = "";
    return;
  }

  banner.classList.remove("hidden");
  banner.innerHTML = `
    <span class="banner-icon">${WARN_SVG}</span>
    <div class="banner-body">
      <div class="banner-title">需要「辅助功能」权限才能自动回贴</div>
      <div class="banner-sub">ClipVault 通过模拟 ${keys("⌘V")} 把条目粘到目标应用,需在系统设置中授权。</div>
    </div>
    <div class="banner-actions">
      <button id="banner-open" class="banner-btn banner-btn-primary">打开系统设置</button>
      <button id="banner-clip" class="banner-btn">暂时只写到剪贴板</button>
      <button id="banner-recheck" class="banner-btn">重新检查</button>
    </div>`;

  banner
    .querySelector<HTMLButtonElement>("#banner-open")
    ?.addEventListener("click", () => {
      void invoke("open_accessibility_settings").catch(() => {
        // 深链打开失败时退回到 window.open（同一 URL scheme）。
        window.open(ACCESSIBILITY_PREF_URL);
      });
      // 授权是异步的用户操作:延时自动复检,授权后自动清横幅。
      if (recheckTimer !== undefined) window.clearTimeout(recheckTimer);
      recheckTimer = window.setTimeout(() => {
        void refreshAccessibilityBanner(banner);
      }, RECHECK_DELAY_MS);
    });

  banner
    .querySelector<HTMLButtonElement>("#banner-clip")
    ?.addEventListener("click", () => {
      // 仅提示用户当前处于「只写剪贴板」的退化模式;真正的写入发生在回贴时。
      const sub = banner.querySelector<HTMLElement>(".banner-sub");
      if (sub) {
        sub.textContent =
          `已切换为「只写剪贴板」:回贴时内容仅写入剪贴板,请在目标应用手动 ${keys("⌘V")}。`;
      }
    });

  banner
    .querySelector<HTMLButtonElement>("#banner-recheck")
    ?.addEventListener("click", () => {
      void refreshAccessibilityBanner(banner);
    });
}

/** 「本次不记录」：置位一次性 skip flag（R11；watcher 消费后复位，永不持久化）。 */
export async function skipNextCopy(): Promise<void> {
  await invoke("set_skip_next");
}

/**
 * 切换访客模式（R10）。后端持久化到 settings 表并同步共享原子 / 托盘（U10）。
 * 返回最终状态：成功即为传入值，失败则回退为原值以便 UI 保持一致。
 */
export async function setGuestMode(enabled: boolean): Promise<boolean> {
  try {
    await invoke("set_guest_mode", { enabled });
    return enabled;
  } catch {
    return !enabled;
  }
}

/** 读取当前访客模式（以后端运行时状态为准，已与持久化对齐）。 */
export async function getGuestMode(): Promise<boolean> {
  try {
    return await invoke<boolean>("get_guest_mode");
  } catch {
    return false;
  }
}

// ----------------------------------------------------------------
// 加密区 / Vault（U9 / R12）
// ----------------------------------------------------------------

/** 查询 Keychain 数据密钥是否可用（决定加密区是否可用 + 是否显示横幅）。 */
export async function keychainAvailable(): Promise<boolean> {
  try {
    return await invoke<boolean>("keychain_available");
  } catch {
    return false;
  }
}

/** 把条目移入加密区（AES-GCM + Keychain）。Keychain 不可用时抛出错误。 */
export async function moveToVault(id: number): Promise<void> {
  await invoke("move_to_vault", { id });
}

/** 列出加密区条目（元数据；content 已被后端清空）。 */
export async function listVault(): Promise<EntryDto[]> {
  return await invoke<EntryDto[]>("list_vault");
}

/** 批量删除不可解密条目，返回删除条数。 */
export async function clearUndecryptable(): Promise<number> {
  return await invoke<number>("clear_undecryptable");
}

/** 删除单条记录（主列表与加密区管理共用），返回实际删掉的行数（`0` = 该 id 已不存在）。 */
export async function deleteEntry(id: number): Promise<number> {
  return await invoke<number>("delete_entry", { id });
}

/**
 * 刷新「密钥托管不可用」横幅（plan U9）：数据密钥取不到时持续显示，
 * 提示加密区不可用；恢复后自动隐藏。
 *
 * 密钥托管的叫法分平台（RW8/RW9）：macOS 是 Keychain（系统钥匙串），Windows 是
 * 凭据管理器 —— 后端命令名 `keychain_available` 保留，它在 Windows 上的语义就是
 * 「凭据管理器是否可用」。
 */
export async function refreshKeychainBanner(banner: HTMLElement): Promise<void> {
  await whenPlatformReady();
  const ok = await keychainAvailable();
  if (ok) {
    banner.classList.add("hidden");
    banner.innerHTML = "";
    return;
  }
  const store = isWindows() ? "凭据管理器" : "Keychain";
  const storeWhere = isWindows() ? "凭据管理器" : "系统钥匙串";
  banner.classList.remove("hidden");
  banner.innerHTML = `
    <span class="banner-icon">${WARN_SVG}</span>
    <div class="banner-body">
      <div class="banner-title">${store} 不可用，加密区暂不可用</div>
      <div class="banner-sub">无法读取或创建数据密钥；移入加密区与解密回贴将失败。请检查${storeWhere}访问权限。</div>
    </div>`;
}

// ----------------------------------------------------------------
// 面板透明度（U12）
// ----------------------------------------------------------------

/**
 * 默认透明度（0-100 整数百分比），与后端 `privacy::DEFAULT_PANEL_OPACITY`、
 * `styles.css` 的 `--panel-alpha` 默认值 0.78（`--bg` 由它派生）对齐。前端任何
 * fallback 都走这个值，避免后端拉取失败时显示空背景。
 */
export const DEFAULT_PANEL_OPACITY = 78;

/** 读取面板透明度（0-100 整数百分比）。后端失败时回退默认。 */
export async function getPanelOpacity(): Promise<number> {
  try {
    return await invoke<number>("get_panel_opacity");
  } catch {
    return DEFAULT_PANEL_OPACITY;
  }
}

/**
 * 设置面板透明度（0-100 整数百分比）。后端会 clamp 到
 * `[MIN_PANEL_OPACITY=30, MAX_PANEL_OPACITY=100]` 区间。
 * 返回 clamp 后的最终值（前端 slider 应回填此值），失败返回 -1。
 */
export async function setPanelOpacity(v: number): Promise<number> {
  try {
    return await invoke<number>("set_panel_opacity", { opacity: v });
  } catch {
    return -1;
  }
}

// ----------------------------------------------------------------
// 诊断日志（U4）
// ----------------------------------------------------------------

/** 后端 `read_log` 返回的 DTO 形状（对应 Rust `log::LogInfo`）。 */
export interface LogInfoDto {
  size_bytes: number;
  backup_size_bytes: number;
  level: string;
  path: string;
  /** `.1` + current 拼接的尾部,后端已 cap 64 KiB / 400 行。 */
  lines: string;
}

/** 拉取一次日志元信息 + 尾部内容。失败时返回 null,UI 静默。 */
export async function readLog(): Promise<LogInfoDto | null> {
  try {
    return await invoke<LogInfoDto>("read_log");
  } catch {
    return null;
  }
}

/** 清空日志。失败抛错,UI 弹 toast。 */
export async function clearLog(): Promise<void> {
  await invoke("clear_log");
}

/** 把日志文件路径写进系统剪贴板(走 Rust 侧平台后端,与回贴写剪贴板同一条路径)。
 *  IPC 失败会抛错(由 UI toast 说明);平台侧写入是否真的成功则回传不了 —— 见
 *  `commands::copy_log_path` 的「已知边界」。 */
export async function copyLogPath(): Promise<void> {
  await invoke("copy_log_path");
}

/** 读取当前日志级别字符串 ("info" / "verbose")。失败回落 "info"。 */
export async function getLogLevel(): Promise<string> {
  try {
    return await invoke<string>("get_log_level");
  } catch {
    return "info";
  }
}

/** 设置日志级别。返回后端 clamp 后的值,失败回落原值。 */
export async function setLogLevel(level: string): Promise<string> {
  try {
    return await invoke<string>("set_log_level", { level });
  } catch {
    return level === "verbose" ? "info" : "verbose";
  }
}

/** 把全部 14 个热键重置为出厂默认。失败抛错(由 UI toast 展示)。 */
export async function resetHotkeys(): Promise<void> {
  await invoke("reset_hotkeys");
}

// ---------------------------------------------------------------------------
// 快捷键设置区块（U4）
// ---------------------------------------------------------------------------

/** Toast 提示（复用 index.html 的 #toast 元素）。 */
function showToast(msg: string): void {
  const toastEl = document.getElementById("toast");
  if (!toastEl) return;
  toastEl.textContent = msg;
  toastEl.classList.remove("hidden");
  if ((toastEl as HTMLElement & { _timer?: number })._timer !== undefined) {
    window.clearTimeout((toastEl as HTMLElement & { _timer: number })._timer);
  }
  const timer = window.setTimeout(() => {
    toastEl.classList.add("hidden");
  }, 2500);
  (toastEl as HTMLElement & { _timer?: number })._timer = timer;
}

/** action → 中文标签 */
const ACTION_LABELS: Record<HotkeyAction, string> = {
  open_panel: "唤出主面板",
  preview: "切换预览栏",
  paste: "粘贴选中条目",
  edit_note: "编辑备注 / 标签",
  toggle_pin: "切换置顶",
  move_to_vault: "移入加密区",
  delete_entry: "删除选中条目",
  open_url: "在浏览器打开 URL",
  open_vault: "打开加密区管理",
  toggle_guest_mode: "切换访客模式",
  skip_once: "本次不记录",
  open_settings: "打开设置",
  close: "关闭面板",
  move_up: "上移选中",
  move_down: "下移选中",
};

// 分组定义。**导出**是为了让 `settings.test.ts` 钉住「设置面板列出的动作 = keymap 里的
// 动作」：这张表是手写的，加动作时漏掉一行，用户就再也重绑不到它。
export const GLOBAL_ACTIONS: HotkeyAction[] = ["open_panel"];
export const PANEL_ACTIONS: HotkeyAction[] = [
  // `preview` 放在「粘贴选中条目」之前：它是在回贴**之前**确认内容的那一步。
  "preview",
  "paste",
  "edit_note",
  "toggle_pin",
  "move_to_vault",
  "delete_entry",
  "open_url",
  "open_vault",
  "toggle_guest_mode",
  "skip_once",
  "open_settings",
  "close",
  "move_up",
  "move_down",
];

/** 当前录制态；null 表示未在录制。 */
let recordingAction: HotkeyAction | null = null;
/** 当前录制态的 keydown 监听器清理函数。 */
let recordingCleanup: (() => void) | null = null;

// ---------------------------------------------------------------------------
// 全局热键的「让位」（快捷键 plan KTD9）
// ---------------------------------------------------------------------------

/** 让位 / 装回 IPC 的串行队列。 */
let recordingIpc: Promise<void> = Promise.resolve();

/**
 * 把「录制态」同步给后端：`true` = 让出全局热键，`false` = 装回。
 *
 * 为什么必须串行：两条 IPC 都是「写绝对状态」（后端按收到的值办事），而 Tauri 的异步
 * 命令不保证按发送顺序执行完 —— 连续切行（先收一行、再开另一行）时若「装回」晚于
 * 「让出」落地，最终状态就反了：用户还在录键，OS 侧却已经把 ⌥⌘V 装了回去。
 * 串成一条链后，落地顺序就等于调用顺序。
 *
 * 失败不打断录制：让位失败只退回修复前的样子（按全局组合没反应，日志里有 warn），
 * 装回失败更严重（⌥⌘V 会一直失灵），所以后者必须让用户看见。
 */
function setRecordingState(active: boolean): void {
  recordingIpc = recordingIpc
    .then(() => setHotkeyRecording(active))
    .catch((e) => {
      if (!active) showToast(`全局热键恢复失败：${String(e)}`);
    });
}

/**
 * 页面重新加载时的兜底：把全局热键装回去（调用点在 `main.ts` 的启动路径）。
 *
 * 页面一重载，前端这份录制态就凭空消失了（dev 期 HMR 每次改代码都会重载），而后端
 * 可能还停在「已让出」—— 不补这一刀，⌥⌘V 会失灵到重启 App。幂等：后端按「有没有真的
 * 让出过」决定动作（`keymap::recording_transition`），没让出过时这条 IPC 是空转。
 */
export function restoreGlobalHotkeys(): void {
  setRecordingState(false);
}

// ---------------------------------------------------------------------------
// 「强行换绑」确认弹框
// ---------------------------------------------------------------------------

/** 弹框正在等待的答复；null 表示弹框没开着。 */
let rebindConfirmResolve: ((ok: boolean) => void) | null = null;
/** 弹框自己挂在 `document` 上的 keydown 清理函数。 */
let rebindConfirmCleanup: (() => void) | null = null;

/** 关掉弹框并给出答复；没开着时什么都不做。 */
function closeForceRebindDialog(ok: boolean): void {
  if (rebindConfirmCleanup) {
    rebindConfirmCleanup();
    rebindConfirmCleanup = null;
  }
  document.getElementById("hotkey-confirm")?.classList.add("hidden");
  const resolve = rebindConfirmResolve;
  rebindConfirmResolve = null;
  resolve?.(ok);
}

/**
 * 弹出「强行换绑」确认框，返回用户的选择：`true` = 抢过来，`false` = 取消。
 *
 * 只有两个出口：`Enter` /「强行换绑」→ true，`Esc` /「取消」→ false。弹框期间不再
 * 重绑任何键 —— 录制监听器在调用前已由 `cancelRecording()` 收掉，而弹框自己的
 * keydown 走**捕获相**并 `stopPropagation()`：main.ts 的「设置里按 Esc 关设置」走
 * 冒泡相，不挡掉的话取消弹框会顺手把整个设置面板一起关掉。
 *
 * 导出是为了让 `settings.test.ts` 能直接驱动；生产代码只在录制路径里调用它。
 */
export function askForceRebind(
  action: HotkeyAction,
  owner: HotkeyAction,
  combo: KeyCombo,
): Promise<boolean> {
  const overlay = document.getElementById("hotkey-confirm");
  const msgEl = document.getElementById("hotkey-confirm-msg");
  const noteEl = document.getElementById("hotkey-confirm-note");
  const okBtn = document.getElementById("hotkey-confirm-ok");
  if (!overlay || !msgEl || !okBtn) {
    // DOM 不完整（不该发生）：当作取消 —— 绑定保持不变是最安全的答复。
    return Promise.resolve(false);
  }

  // 上一个弹框还没答复（连着重绑两行）→ 先按「取消」收掉，免得两个 resolver 打架。
  closeForceRebindDialog(false);

  const comboText = comboLabel(combo);
  msgEl.textContent =
    `${comboText} 已被「${ACTION_LABELS[owner]}」占用。` +
    `强行换绑后，「${ACTION_LABELS[owner]}」会变成「未绑定」，` +
    `「${ACTION_LABELS[action]}」改用 ${comboText}。`;
  if (noteEl) {
    // 被抢的是全局热键的键位：解绑后连面板都唤不出来（只剩托盘/菜单栏图标这条路）。
    noteEl.textContent =
      owner === "open_panel"
        ? `注意：「${ACTION_LABELS.open_panel}」是全局热键，解绑后只能从托盘/菜单栏图标打开面板，可以之后再单独给它绑一个键。`
        : "";
  }

  overlay.classList.remove("hidden");
  okBtn.focus();

  const onKeyDown = (ev: KeyboardEvent): void => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      ev.stopPropagation();
      closeForceRebindDialog(false);
      return;
    }
    if (ev.key === "Enter") {
      ev.preventDefault();
      ev.stopPropagation();
      closeForceRebindDialog(true);
    }
  };
  document.addEventListener("keydown", onKeyDown, { capture: true });
  rebindConfirmCleanup = () => document.removeEventListener("keydown", onKeyDown, { capture: true });

  return new Promise<boolean>((resolve) => {
    rebindConfirmResolve = resolve;
  });
}

// 两个按钮常驻在 index.html（不随行的 innerHTML 重建），与 #hotkey-reset 同理在模块
// 作用域绑一次 —— 放进 askForceRebind() 里会让每开一次弹框多绑一个监听器。
document.getElementById("hotkey-confirm-ok")?.addEventListener("click", () => {
  closeForceRebindDialog(true);
});
document.getElementById("hotkey-confirm-cancel")?.addEventListener("click", () => {
  closeForceRebindDialog(false);
});

/**
 * 设置模态是否已经关掉（`#settings` 带上 `hidden`）。
 *
 * 录制态的监听器挂在 `document` 上，模态关掉之后它照样活着：用户回到主列表敲下的
 * 第一个字符会被它吞掉，并静默写成那个 action 的新绑定（后端只拦 modifier-only）。
 * 元素取不到时按「没关」处理 —— 这条只给录制态兜底，不该在非预期环境下改变既有行为。
 */
function settingsModalHidden(): boolean {
  return document.getElementById("settings")?.classList.contains("hidden") ?? false;
}

/**
 * 开始录制指定 action 的新快捷键。
 * - 全局 keydown 监听持续有效直到 capture 或 cancel。
 * - Escape 或再次点击「取消」退出录制。
 * - 有效按键 parseKeyCombo 后调用 setHotkey IPC。
 *
 * 导出是为了让 `settings.test.ts` 能直接驱动录制态；生产代码只在行内按钮的
 * click handler 里调用它。
 */
export function startRecording(action: HotkeyAction): void {
  cancelRecording(); // 确保没有残留监听

  recordingAction = action;
  const row = document.querySelector<HTMLElement>(`[data-action="${action}"]`);
  if (row) {
    row.classList.add("recording");
    // 进入录制态的行自动滚入可见区（R11）：若该行接近内容区（.settings-body）
    // 底部，用户看不到「请按新键…」。block:"nearest" 只在该行不可见时才滚动，
    // 已可见则不跳动；.hotkey-row 的 scroll-margin-block 留出贴边余量。
    row.scrollIntoView({ block: "nearest" });
  }
  const kbd = row?.querySelector<HTMLElement>(".hotkey-row-combo kbd");
  if (kbd) kbd.textContent = "请按新键…";

  const btn = row?.querySelector<HTMLButtonElement>(".hotkey-rebind");
  if (btn) btn.textContent = "取消";

  const onKeyDown = (ev: KeyboardEvent): void => {
    // 模态已经关掉（用户点了「关闭」）：这次录制作废。先自我了断再 return —— 不能
    // preventDefault，否则用户在主列表里敲的这个字符就没了。
    if (settingsModalHidden()) {
      cancelRecording();
      return;
    }

    if (ev.key === "Escape") {
      ev.preventDefault();
      ev.stopPropagation();
      cancelRecording();
      return;
    }

    // 忽略纯修饰键
    if (
      (ev.metaKey || ev.ctrlKey || ev.shiftKey || ev.altKey) &&
      ["Meta", "Control", "Shift", "Alt"].includes(ev.key)
    ) {
      return;
    }

    ev.preventDefault();
    ev.stopPropagation();

    const combo = parseKeyCombo(ev);
    if (!combo) {
      // KTD5：解析不出的键要说出来，别静默吞掉（F5 这类命名键、无修饰的 ⌫ 都属于
      // 这一类）。修复前这里是 `return`，用户按下去只看到「什么都没发生」。
      showToast(`这个键不能用作快捷键：${ev.code || ev.key}`);
      return;
    }

    // 采到的就是当前绑定：写进去是 no-op（后端对同一 `(action, combo)` 直接 Ok），
    // 行上一个字都不会变 —— 「按了但什么都没发生」正是这次 bug 最难描述的地方，
    // 所以显式说一句，让「没改动」和「没收到按键」在界面上分得开。
    const bound = currentKeyMap?.actions[action];
    const unchanged = bound ? comboEquals(bound, combo) : false;

    // 表内冲突：这个组合已经被另一条 action 占着。写进去不会报错（后端只查语法与
    // 全局热键的 OS 注册），但两条 action 同组合时 `matches()` 只会派发给排序靠前的
    // 那条 —— 另一条从此永远不触发，而它那一行仍然显示着这个组合（把 `paste` 改成
    // `⌘E` 之后，旧的 `↩` 没了、新的 `⌘E` 又永远不灵，回贴快捷键实际失效）。
    // 处理：先收掉录制态（弹框期间不该还能改绑），再让用户选 —— 确认则「强行换绑」：
    // 组合归本行，原来占着它的那条变成「未绑定」（后端 `HotkeyMap::claim`）；
    // 取消则什么都不写，键帽回到原绑定。
    const owner = actionOwningCombo(combo, action);
    if (owner) {
      cancelRecording();
      void (async () => {
        if (!(await askForceRebind(action, owner, combo))) return; // 取消：绑定不变
        const result = await setHotkey(action, combo, true);
        if (result.ok) await reloadKeymap();
        // 顺序同下面那条路径：reload 在前、重画在后，键帽不会先回旧值再切到新值。
        // 强行换绑会同时改两行（抢到的那行 + 变成「未绑定」的那行），故整区重画。
        refreshAllRows();
        showToast(
          result.ok
            ? `已强行换绑：「${ACTION_LABELS[owner]}」变成未绑定，「${ACTION_LABELS[action]}」改用 ${comboLabel(combo)}`
            : `换绑失败：${result.err}，已保留原绑定`,
        );
      })();
      return;
    }

    void (async () => {
      const result = await setHotkey(action, combo);
      // 顺序：成功路径必须 reloadKeymap() 在前、cancelRecording() 在后 —— 后者里的
      // refreshRow 写回当前绑定，颠倒顺序会让键帽先回旧值再切到新值，闪一下。
      if (result.ok) await reloadKeymap();
      cancelRecording();
      if (!result.ok) {
        showToast(`重绑失败：${result.err}，已回退到原绑定`);
      } else if (unchanged) {
        showToast(`「${ACTION_LABELS[action]}」已经绑在 ${comboLabel(combo)} 上，未做改动`);
      }
    })();
  };

  document.addEventListener("keydown", onKeyDown, { capture: true });
  recordingCleanup = () => document.removeEventListener("keydown", onKeyDown, { capture: true });

  // 让出全局热键：不这么做的话，用户按「当前正被 OS 占着」的组合（默认就是 ⌥⌘V）时
  // 按键根本到不了上面那个监听器 —— 那一行会永远停在「请按新键…」（快捷键 plan KTD9）。
  // IPC 是异步的，但本地往返在微秒级，远早于用户抬手按键。
  setRecordingState(true);
}

/** 取消当前录制态，清理监听器，恢复 UI（导出以便单测直接驱动，见 startRecording）。 */
export function cancelRecording(): void {
  const wasRecording = recordingCleanup !== null || recordingAction !== null;
  if (recordingCleanup) {
    recordingCleanup();
    recordingCleanup = null;
  }
  if (recordingAction !== null) {
    const row = document.querySelector<HTMLElement>(`[data-action="${recordingAction}"]`);
    if (row) row.classList.remove("recording");
    const btn = row?.querySelector<HTMLButtonElement>(".hotkey-rebind");
    if (btn) btn.textContent = "重绑";
    // 取消的语义是「绑定不变」，所以键帽必须回到当前绑定，否则行会永久停在「请按新键…」占位符上。
    refreshRow(recordingAction);
    recordingAction = null;
  }
  // 录制收工 → 让后端把全局热键装回 OS（快捷键 plan KTD9）。这里是录制态的**唯一收口**：
  // 采到键、Esc、行内「取消」、切到别行、关设置、重建行都经过它，所以装回只需要写这
  // 一处。只有真的收起过录制态才发 IPC —— 打开设置、重建行这类空转路径不该凭空多一次往返。
  if (wasRecording) setRecordingState(false);
}

/** 键帽文本。三种状态：有绑定 → 平台化组合键；未绑定（`null`）→「未绑定」；
 * keymap 尚未加载（`undefined`）→ 与 `renderRow` 同一个占位符。 */
function comboLabel(combo: KeyCombo | null | undefined): string {
  if (combo === null) return "未绑定";
  return combo ? localizedCombo(combo) : "—";
}

/**
 * 把一行重画成「当前绑定」：键帽写回 `currentKeyMap` 里的值。
 *
 * 录制成功与取消录制都走这里 —— 行的显示只能有一个来源（keymap），各写一份
 * 就会再次出现「一边改了另一边没跟」的失效。
 */
function refreshRow(action: HotkeyAction): void {
  const row = document.querySelector<HTMLElement>(`[data-action="${action}"]`);
  if (!row) return;
  const kbd = row.querySelector<HTMLElement>(".hotkey-row-combo kbd");
  if (!kbd) return;
  kbd.textContent = comboLabel(currentKeyMap?.actions[action]);
}

/**
 * 重画每一行的键帽。
 *
 * 强行换绑会同时改两行（抢到组合的那行 + 变成「未绑定」的那行），逐行 `refreshRow`
 * 容易只刷一半 —— 行的显示只有一个来源，整区重画最省事也最不容易漏。
 */
function refreshAllRows(): void {
  for (const action of [...GLOBAL_ACTIONS, ...PANEL_ACTIONS]) refreshRow(action);
}

/**
 * 渲染快捷键设置区块。
 * 在 openSettings() 末尾调用；Linux 上隐藏全局热键组。
 */
export async function renderHotkeysSection(): Promise<void> {
  await whenPlatformReady();

  const globalGroup = document.getElementById("hotkey-group-global");
  const globalList = document.getElementById("hotkey-list-global");
  const panelList = document.getElementById("hotkey-list-panel");

  if (!globalList || !panelList) return;

  // Linux 隐藏全局热键组
  if (globalGroup) {
    globalGroup.classList.toggle("hidden", isLinux());
  }

  // 等待 keymap 就绪（最多轮询 1s）
  await new Promise<void>((resolve) => {
    let attempts = 0;
    const iv = setInterval(() => {
      if (currentKeyMap || attempts >= 20) {
        clearInterval(iv);
        resolve();
      }
      attempts++;
    }, 50);
  });

  function renderRow(action: HotkeyAction): string {
    const comboText = comboLabel(currentKeyMap?.actions[action]);
    return `<li class="hotkey-row" data-action="${action}">
  <span class="hotkey-row-label">${ACTION_LABELS[action]}</span>
  <span class="hotkey-row-combo"><kbd>${comboText}</kbd></span>
  <button class="hotkey-rebind">重绑</button>
</li>`;
  }

  // 重建行前必须收掉录制态：它指向的 DOM 马上被替换，留下的 keydown 监听器会变成
  // 「按任意键悄悄改绑」的隐形状态。清理要贴着 innerHTML，不能放在上面那段轮询之前 ——
  // 轮询最长 1s（keymap 拉不到时走满 20 次），这 1s 内点「重绑」照样会留下一份隐形
  // 监听器：新行没有 `recording` 态，而 recordingAction 还指着它。
  //
  // 「强行换绑」弹框同理一起收掉（它的待答组合来自上一轮的行），也贴着 innerHTML：
  // 弹框的 promise 在轮询窗口里悬着的话，用户看到的是一行已经被替换掉的行。
  cancelRecording();
  closeForceRebindDialog(false);

  globalList.innerHTML = GLOBAL_ACTIONS.map(renderRow).join("");
  panelList.innerHTML = PANEL_ACTIONS.map(renderRow).join("");

  // 绑定重绑按钮
  for (const list of [globalList, panelList]) {
    for (const btn of list.querySelectorAll<HTMLButtonElement>(".hotkey-rebind")) {
      btn.addEventListener("click", () => {
        const row = btn.closest<HTMLElement>(".hotkey-row");
        const action = row?.dataset.action as HotkeyAction | undefined;
        if (!action) return;
        if (recordingAction === action) {
          cancelRecording();
        } else {
          startRecording(action);
        }
      });
    }
  }
}

// 「重置默认」按钮：IPC 把全部 14 条热键还原 + 全局热键 OS 侧重注册，成功后再
// reloadKeymap() + 重渲染本区（沿用 record 流程的 reload 模式）。
//
// 绑在模块作用域：#hotkey-reset 常驻在 index.html 里，不像每行「重绑」按钮那样
// 随 innerHTML 重建；放在 renderHotkeysSection() 里每开一次设置就会多绑一个监听器。
// main.ts 对 #settings-close / #log-clear 等常驻控件同样是模块作用域绑一次。
document.getElementById("hotkey-reset")?.addEventListener("click", () => {
  void (async () => {
    try {
      await resetHotkeys();
      await reloadKeymap();
      await renderHotkeysSection();
      showToast("已重置为默认快捷键");
    } catch (e) {
      showToast(`重置失败：${String(e)}`);
    }
  })();
});
