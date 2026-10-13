// ClipVault 设置 / 权限层（U4 建横幅槽位；U8 补全首启引导与授权后复检；
// U10 补全访客模式 / 「本次不记录」开关的持久化对接）。
//
// 负责 Accessibility 权限横幅（AE7）的完整首启引导 + 访客模式 / 「本次不记录」开关。
// 访客模式经后端持久化到 settings 表并同步共享原子 / 托盘；skip-once 为纯运行时。

import { invoke } from "@tauri-apps/api/core";
import { WARN_SVG, type EntryDto } from "./panel";
import { isWindows, keys, whenPlatformReady } from "./platform";

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
 * `styles.css` `--bg` 默认 0.78 三处对齐。前端任何 fallback 都走这个值，
 * 避免后端拉取失败时显示空背景。
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

