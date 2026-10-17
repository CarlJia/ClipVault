// ClipVault「关于」浮层的数据层（U13）。
//
// 关于面板此前不存在：托盘那一项直接落到 `show_panel`（占位），前端也没有任何
// 关于界面 —— 点下去只弹出主列表，用户看到的就是「关于内容不展示」。
//
// 这里只做三件事，保持可单测（about.test.ts）：
// 1. 把后端 `app_info` 的返回归一化（版本缺失不能渲染空串）；
// 2. 把数据写进 DOM（版本 + GitHub 地址）；
// 3. 点地址时调 `open_external_url` 交给系统浏览器。
//
// 版本号由后端给（`tauri.conf.json` 的 `version`，与安装包 / Release tag 同源），
// 不在前端写死：写死就要在发版时同步改第二处，漏改时关于面板会显示旧版本。

import { invoke } from "@tauri-apps/api/core";

/** `app_info` 的返回体。 */
export interface AppInfo {
  /** 版本号（如 `0.8.0`）。 */
  version: string;
  /** 仓库主页（https，可直接交给系统浏览器）。 */
  repository: string;
}

/** 仓库主页。后端 `app_info` 拿不到时用它兜底 —— 地址是产品常量，不该因 IPC 失败而消失。 */
export const REPOSITORY_URL = "https://github.com/CarlJia/ClipVault";

/** 版本号缺失时的展示文案。空串会被当成「界面坏了」，必须给一个明确的字。 */
export const UNKNOWN_VERSION = "未知";

/**
 * 归一化 `app_info` 的返回：去空白、缺省值回退。
 *
 * 后端正常时是恒等变换；它存在的意义是「拿不到 / 拿到空值」的那两条路径 ——
 * 那正是关于面板最容易显示成一片空白的地方。
 */
export function normalizeAppInfo(raw: Partial<AppInfo> | null | undefined): AppInfo {
  const version = raw?.version?.trim() ?? "";
  const repository = raw?.repository?.trim() ?? "";
  return {
    version: version === "" ? UNKNOWN_VERSION : version,
    repository: repository === "" ? REPOSITORY_URL : repository,
  };
}

/** 拉取关于面板数据。IPC 失败时不抛错：关于面板照常打开，版本显示「未知」。 */
export async function fetchAppInfo(): Promise<AppInfo> {
  try {
    return normalizeAppInfo(await invoke<AppInfo>("app_info"));
  } catch {
    return normalizeAppInfo(null);
  }
}

/** 关于面板里由 JS 写入的两个节点。 */
export interface AboutElements {
  /** 版本号节点。 */
  version: HTMLElement;
  /** GitHub 地址（按钮，点它打开浏览器）。 */
  github: HTMLElement;
}

/** 把版本与仓库地址写进关于面板。 */
export function renderAbout(els: AboutElements, info: AppInfo): void {
  els.version.textContent = info.version;
  // 地址直接作为按钮文案：用户要能一眼看到仓库在哪，而不是只有悬停才看得到。
  els.github.textContent = info.repository;
  els.github.title = `${info.repository}（在浏览器打开）`;
}

/** 在系统默认浏览器打开仓库主页。失败由调用方 toast 提示。 */
export async function openRepository(url: string = REPOSITORY_URL): Promise<void> {
  await invoke("open_external_url", { url });
}
