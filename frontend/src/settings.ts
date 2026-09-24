// ClipVault 设置 / 权限层（U4）。
//
// 负责 Accessibility 权限横幅（AE7）与访客模式 / 「本次不记录」的 UI 桩。
// U8 会补上「打开系统设置」深链与授权后自动复检；U10 会补上开关持久化。

import { invoke } from "@tauri-apps/api/core";

/**
 * 刷新 Accessibility 横幅：未授权时展示提示，已授权时隐藏。
 * `check_accessibility` 返回 `AXIsProcessTrusted()`（AE7）。
 */
export async function refreshAccessibilityBanner(
  banner: HTMLElement,
): Promise<void> {
  let trusted = true;
  try {
    trusted = await invoke<boolean>("check_accessibility");
  } catch {
    trusted = true; // 检测失败时不打扰用户。
  }

  if (trusted) {
    banner.classList.add("hidden");
    banner.innerHTML = "";
    return;
  }

  banner.classList.remove("hidden");
  banner.innerHTML = `
    <span class="banner-icon">􀇾</span>
    <span class="banner-text">需要「系统设置 → 隐私与安全性 → 辅助功能」授权才能自动回贴</span>
    <button id="banner-recheck" class="banner-btn">重新检查</button>`;
  const btn = banner.querySelector<HTMLButtonElement>("#banner-recheck");
  btn?.addEventListener("click", () => {
    void refreshAccessibilityBanner(banner);
  });
}

/** 「本次不记录」桩：置位一次性 skip flag（R11；watcher 消费）。 */
export async function skipNextCopy(): Promise<void> {
  try {
    await invoke("set_skip_next");
  } catch {
    /* 忽略：U10 前为尽力而为的桩。 */
  }
}

/** 切换访客模式桩（R10；U10 完成持久化）。返回最终状态。 */
export async function setGuestMode(enabled: boolean): Promise<boolean> {
  try {
    await invoke("set_guest_mode", { enabled });
    return enabled;
  } catch {
    return !enabled;
  }
}
