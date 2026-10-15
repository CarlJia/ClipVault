// ClipVault 设置 / 权限层（U4 建横幅槽位；U8 补全首启引导与授权后复检；
// U10 补全访客模式 / 「本次不记录」开关的持久化对接）。
//
// 负责 Accessibility 权限横幅（AE7）的完整首启引导 + 访客模式 / 「本次不记录」开关。
// 访客模式经后端持久化到 settings 表并同步共享原子 / 托盘；skip-once 为纯运行时。

import { invoke } from "@tauri-apps/api/core";
import { WARN_SVG, type EntryDto } from "./panel";
import { isWindows, isLinux, keys, whenPlatformReady } from "./platform";
import {
  localizedCombo,
  parseKeyCombo,
  reloadKeymap,
  setHotkey,
  type HotkeyAction,
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
  paste: "粘贴选中条目",
  edit_note: "编辑备注 / 标签",
  toggle_pin: "切换置顶",
  move_to_vault: "移入加密区",
  open_url: "在浏览器打开 URL",
  open_vault: "打开加密区管理",
  toggle_guest_mode: "切换访客模式",
  skip_once: "本次不记录",
  open_settings: "打开设置",
  close: "关闭面板",
  move_up: "上移选中",
  move_down: "下移选中",
};

// 分组定义
const GLOBAL_ACTIONS: HotkeyAction[] = ["open_panel"];
const PANEL_ACTIONS: HotkeyAction[] = [
  "paste",
  "edit_note",
  "toggle_pin",
  "move_to_vault",
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

/**
 * 开始录制指定 action 的新快捷键。
 * - 全局 keydown 监听持续有效直到 capture 或 cancel。
 * - Escape 或再次点击「取消」退出录制。
 * - 有效按键 parseKeyCombo 后调用 setHotkey IPC。
 */
function startRecording(action: HotkeyAction): void {
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
    if (!combo) return;

    void (async () => {
      const result = await setHotkey(action, combo);
      cancelRecording();
      if (!result.ok) {
        showToast(`重绑失败：${result.err}，已回退到原绑定`);
        return;
      }
      // 刷新前端 in-memory keymap
      await reloadKeymap();
      // 刷新本行显示
      refreshRow(action);
    })();
  };

  document.addEventListener("keydown", onKeyDown, { capture: true });
  recordingCleanup = () => document.removeEventListener("keydown", onKeyDown, { capture: true });
}

/** 取消当前录制态，清理监听器，恢复 UI。 */
function cancelRecording(): void {
  if (recordingCleanup) {
    recordingCleanup();
    recordingCleanup = null;
  }
  if (recordingAction !== null) {
    const row = document.querySelector<HTMLElement>(`[data-action="${recordingAction}"]`);
    if (row) row.classList.remove("recording");
    const btn = row?.querySelector<HTMLButtonElement>(".hotkey-rebind");
    if (btn) btn.textContent = "重绑";
    recordingAction = null;
  }
}

/** 刷新单行 combo 显示（从 keymap 最新状态）。 */
async function refreshRow(action: HotkeyAction): Promise<void> {
  const mod = await import("./keymap");
  const combo = mod.currentKeyMap?.actions[action];
  if (!combo) return;
  const row = document.querySelector<HTMLElement>(`[data-action="${action}"]`);
  if (!row) return;
  const kbd = row.querySelector<HTMLElement>(".hotkey-row-combo kbd");
  if (kbd) kbd.textContent = mod.localizedCombo(combo);
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
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let km: any = null;
  await new Promise<void>((resolve) => {
    let attempts = 0;
    const iv = setInterval(async () => {
      const mod = await import("./keymap");
      km = mod;
      if (mod.currentKeyMap || attempts >= 20) {
        clearInterval(iv);
        resolve();
      }
      attempts++;
    }, 50);
  });

  const comboMap = km?.currentKeyMap?.actions;

  function renderRow(action: HotkeyAction): string {
    const combo = comboMap?.[action];
    const comboText = combo ? localizedCombo(combo) : "—";
    return `<li class="hotkey-row" data-action="${action}">
  <span class="hotkey-row-label">${ACTION_LABELS[action]}</span>
  <span class="hotkey-row-combo"><kbd>${comboText}</kbd></span>
  <button class="hotkey-rebind">重绑</button>
</li>`;
  }

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

