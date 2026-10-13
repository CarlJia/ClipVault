// ClipVault 面板渲染层（U4）。
//
// 负责把后端返回的 DTO 渲染成 Raycast 风格的平铺列表 / 标签列表 / 类别列表。
// 纯渲染，无 IPC；交互与状态由 main.ts 驱动。
// （唯一的外部依赖是 platform.ts 的 `keys()` —— 一个纯函数，用来把行序号徽标的
// ⌘1–9 换成 Windows 的 Ctrl+1–9。）

import { keys } from "./platform";

/** 单条剪贴板记录 DTO（对应 Rust `commands::EntryDto`）。 */
export interface EntryDto {
  id: number;
  content: string;
  note: string;
  auto_category: string;
  tags: string[];
  is_pinned: boolean;
  pinned_at: number | null;
  is_vault: boolean;
  /** 加密区条目是否可解密；`false` → 显示「无法解密」徽标（U9）。 */
  is_decryptable: boolean;
  created_at: number;
  /** 多类型 plan U5：内容类型（text / image / file / rtf / color）。 */
  content_kind?: string;
  /** 多类型 plan U5：MIME for binary kinds。 */
  mime_type?: string | null;
  /** 多类型 plan U5：图片 64×64 PNG base64；其他 kind 为 null。 */
  thumb?: string | null;
  /** 多类型 plan U5：结构化 metadata 字符串（图片维度/文件名/RGBA 等）。 */
  metadata?: string | null;
}

/** 「标签」tab 聚合项。 */
export interface TagInfo {
  tag: string;
  count: number;
}

/** 「类别」tab 聚合项。 */
export interface CategoryInfo {
  category: string;
  count: number;
}

/** HTML 转义，防止内容注入。 */
export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/** 把多行内容压成单行预览。 */
function previewOf(content: string, maxLen = 120): string {
  const oneLine = content.replace(/\s+/g, " ").trim();
  return oneLine.length > maxLen ? `${oneLine.slice(0, maxLen)}…` : oneLine;
}

/** 相对时间格式化。 */
export function formatTime(unixSecs: number): string {
  const now = Date.now() / 1000;
  const diff = Math.max(0, now - unixSecs);
  if (diff < 60) return "刚刚";
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
  const d = new Date(unixSecs * 1000);
  const mm = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  return `${mm}-${dd}`;
}

/**
 * Pin 徽标：SF Symbol `pin.fill` 在 WebView 中无法直接渲染，故用一个
 * 内联 SVG 近似（倾斜图钉，对应 SF `pin.fill` 的斜置形态），13×13px，
 * 通过 `fill="currentColor"` 承接 CSS 颜色——选中行取 `--accent`、
 * 非选中行取 `--muted`（见 styles.css `.pin` / `.row.selected .pin`）。
 */
const PIN_SVG =
  '<svg class="pin" viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">' +
  "<title>已置顶</title>" +
  '<path fill="currentColor" d="M9.828.722a.5.5 0 0 1 .354.146l4.95 4.95a.5.5 0 0 1 0 ' +
  ".707c-.48.48-1.072.588-1.503.588-.177 0-.335-.018-.46-.039l-3.134 3.134a5.927 5.927 0 " +
  "0 1 .16 1.013c.046.702-.032 1.687-.72 2.375a.5.5 0 0 1-.707 0l-2.829-2.828-3.182 " +
  "3.182c-.195.195-1.219.902-1.414.707-.195-.195.512-1.22.707-1.414l3.182-3.182-2.828-2.829a" +
  ".5.5 0 0 1 0-.707c.688-.688 1.673-.767 2.375-.72.343.023.692.077 1.013.16l3.134-3.133a2.772 " +
  '2.772 0 0 1-.04-.461c0-.43.108-1.022.589-1.503a.5.5 0 0 1 .353-.146z"/></svg>';

/**
 * 横幅图标（U7）：原来是 SF Symbols 私有区码点（U+1001FE 等），在 WebView2 / Windows
 * 默认字体下必然是豆腐块。改内联 SVG —— 与 `PIN_SVG` 同样的理由与做法。
 *
 * 描边而非填充：横幅底色是半透明的（叠在毛玻璃面板上），填充图标的「挖空」得知道底色
 * 才能画对，描边没这个问题。颜色仍由外层 `.banner-icon` 的 `color` 经 `currentColor` 承接
 * （授权横幅是黄色、访客横幅是蓝色）。
 */
export const WARN_SVG =
  '<svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" fill="none" ' +
  'stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" stroke-linecap="round">' +
  '<path d="M8 2.4 14.4 13.5H1.6z"/><path d="M8 6.7v3"/><path d="M8 11.6h.01"/></svg>';

/** 访客模式横幅图标（「仅查看，不记录」→ 眼睛）。 */
export const EYE_SVG =
  '<svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" fill="none" ' +
  'stroke="currentColor" stroke-width="1.4" stroke-linecap="round">' +
  '<path d="M1.4 8S4 3.6 8 3.6 14.6 8 14.6 8 12 12.4 8 12.4 1.4 8 1.4 8Z"/>' +
  '<circle cx="8" cy="8" r="1.9"/></svg>';

/** 渲染平铺记录列表。`selected` 为高亮行索引。 */
export function renderEntries(
  container: HTMLElement,
  entries: EntryDto[],
  selected: number,
): void {
  if (entries.length === 0) {
    container.innerHTML = `<div class="empty">没有匹配的记录</div>`;
    return;
  }
  container.innerHTML = entries
    .map((e, i) => {
      const pin = e.is_pinned ? PIN_SVG : "";
      const cat = `<span class="chip chip-cat">${escapeHtml(e.auto_category)}</span>`;
      const tags = e.tags
        .map((t) => `<span class="chip chip-tag">${escapeHtml(t)}</span>`)
        .join("");
      const vault = e.is_vault
        ? `<span class="chip chip-vault" title="加密区条目">🔒 加密</span>`
        : "";
      const undec =
        e.is_vault && !e.is_decryptable
          ? `<span class="chip chip-undec" title="密钥缺失，无法解密">⚠ 无法解密</span>`
          : "";
      const kind = e.content_kind ?? "text";
      const kindBadge = kind === "text" ? "" : `<span class="chip chip-kind" data-kind="${escapeHtml(kind)}">${escapeHtml(kind.toUpperCase())}</span>`;
      const previewHtml = renderKindPreview(e, kind, pin);
      const idxBadge =
        i < 9
          ? `<span class="row-index" title="${escapeHtml(keys(`⌘${i + 1}`))} 直接选中">${escapeHtml(keys(`⌘${i + 1}`))}</span>`
          : "";
      return `
      <div class="row row-kind-${escapeHtml(kind)}${i === selected ? " selected" : ""}" data-id="${e.id}" data-index="${i}" data-kind="${escapeHtml(kind)}">
        ${idxBadge}
        <div class="row-main">
          ${previewHtml}
          <div class="row-meta">${cat}${kindBadge}${tags}${vault}${undec}</div>
        </div>
        <div class="row-time">${formatTime(e.created_at)}</div>
      </div>`;
    })
    .join("");
}

/** 根据 content_kind 渲染不同的主预览块（U5 多类型）。 */
function renderKindPreview(e: EntryDto, kind: string, pin: string): string {
  const hasNote = Boolean(e.note);
  switch (kind) {
    case "image": {
      const thumb = e.thumb;
      const meta = parseMeta(e.metadata);
      const dims = meta && meta.width && meta.height ? `${meta.width}×${meta.height}` : "";
      const size = meta && meta.size_bytes
        ? formatBytes(Number(meta.size_bytes))
        : "";
      const ariaLabel = `图片: ${dims || "未知尺寸"}, ${size || "未知大小"}`;
      const imgHtml = thumb
        ? `<img class="row-thumb" src="data:image/png;base64,${escapeHtml(thumb)}" alt="${escapeHtml(ariaLabel)}" aria-label="${escapeHtml(ariaLabel)}" role="img">`
        : `<div class="row-thumb row-thumb-placeholder" aria-label="${escapeHtml(ariaLabel)}" role="img">${escapeHtml(size || "图片")}</div>`;
      const titleHtml = hasNote
        ? `<div class="row-content row-note-title" title="${escapeHtml(e.note)}">${pin}📝 ${escapeHtml(previewOf(e.note, 80))}</div>`
        : `<div class="row-content row-image-meta">${pin}${escapeHtml(dims)} ${escapeHtml(size)}</div>`;
      return `<div class="row-image-wrap">${imgHtml}${titleHtml}</div>`;
    }
    case "file": {
      const meta = parseMeta(e.metadata);
      const paths: string[] = Array.isArray(meta?.paths) ? (meta!.paths as string[]) : [];
      const first = paths[0] ?? "(无路径)";
      const more = paths.length > 1 ? ` 等 ${paths.length} 项` : "";
      const titleHtml = `<div class="row-content row-file-name">${pin}📄 ${escapeHtml(basenameOf(first))}${escapeHtml(more)}</div>`;
      const subtitle = hasNote
        ? `<div class="row-subtitle" title="${escapeHtml(e.content)}">${escapeHtml(previewOf(e.note, 80))}</div>`
        : `<div class="row-subtitle" title="${escapeHtml(first)}">${escapeHtml(first)}</div>`;
      return titleHtml + subtitle;
    }
    case "rtf": {
      const titleHtml = `<div class="row-content row-rtf-badge">${pin}📝 RTF 富文本</div>`;
      const subtitle = hasNote
        ? `<div class="row-subtitle" title="${escapeHtml(e.content)}">${escapeHtml(previewOf(e.note, 80))}</div>`
        : "";
      return titleHtml + subtitle;
    }
    case "color": {
      const meta = parseMeta(e.metadata);
      const rgba = (meta?.rgba as string) ?? "rgba(0,0,0,1)";
      const swatch = `<span class="row-color-swatch" style="background:${escapeHtml(rgba)}" role="img" aria-label="颜色 ${escapeHtml(rgba)}"></span>`;
      const titleHtml = `<div class="row-content row-color-meta">${pin}${swatch}${escapeHtml(rgba)}</div>`;
      const subtitle = hasNote
        ? `<div class="row-subtitle" title="${escapeHtml(e.content)}">${escapeHtml(previewOf(e.note, 80))}</div>`
        : "";
      return titleHtml + subtitle;
    }
    case "text":
    default: {
      const titleHtml = hasNote
        ? `<div class="row-content row-note-title" title="${escapeHtml(e.note)}">${pin}📝 ${escapeHtml(previewOf(e.note, 80))}</div>`
        : `<div class="row-content">${pin}${escapeHtml(previewOf(e.content))}</div>`;
      const subtitle = hasNote
        ? `<div class="row-subtitle" title="${escapeHtml(e.content)}">${escapeHtml(previewOf(e.content, 80))}</div>`
        : "";
      return titleHtml + subtitle;
    }
  }
}

function parseMeta(s: string | null | undefined): Record<string, unknown> | null {
  if (!s) return null;
  try {
    return JSON.parse(s) as Record<string, unknown>;
  } catch {
    return null;
  }
}

function basenameOf(p: string): string {
  const m = p.split("/").pop() ?? p;
  return m;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** 渲染「标签」聚合列表。可点击筛选。 */
export function renderTags(container: HTMLElement, tags: TagInfo[]): void {
  if (tags.length === 0) {
    container.innerHTML = `<div class="empty">还没有标签</div>`;
    return;
  }
  container.innerHTML = tags
    .map(
      (t, i) => `
      <div class="agg-row" data-index="${i}" data-kind="tag">
        <span class="chip chip-tag">${escapeHtml(t.tag)}</span>
        <span class="agg-count">${t.count}</span>
      </div>`,
    )
    .join("");
}

/** 渲染「类别」聚合列表。可点击筛选。 */
export function renderCategories(
  container: HTMLElement,
  cats: CategoryInfo[],
): void {
  if (cats.length === 0) {
    container.innerHTML = `<div class="empty">还没有记录</div>`;
    return;
  }
  container.innerHTML = cats
    .map(
      (c, i) => `
      <div class="agg-row" data-index="${i}" data-kind="category">
        <span class="chip chip-cat">${escapeHtml(c.category)}</span>
        <span class="agg-count">${c.count}</span>
      </div>`,
    )
    .join("");
}

/** 在筛选后的条目列表顶部渲染一行「‹ 返回」的返回条。 */
export function renderFilterHeader(kind: "tag" | "category", name: string): string {
  const label = kind === "tag" ? "标签" : "类别";
  return `
    <div class="agg-row filter-back" data-kind="back">
      <span class="back-arrow">‹</span>
      <span>${label}：${escapeHtml(name)}</span>
    </div>`;
}

/**
 * 渲染「加密区管理」列表（U9）。密文不下发，只展示元数据：类别、标签、备注、
 * 时间，以及是否可解密。不可解密的条目高亮警示，供用户「清除不可解密条目」。
 *
 * `selected` 为高亮行索引：加密区条目与主列表一样可键盘导航（↑↓）、
 * `Enter` 回贴、`⌘E` 编辑备注，故需要选中态（AE5：移入加密区 → 选中 → Enter）。
 */
export function renderVaultList(
  container: HTMLElement,
  entries: EntryDto[],
  selected = 0,
): void {
  if (entries.length === 0) {
    container.innerHTML = `<div class="empty">加密区暂无条目</div>`;
    return;
  }
  container.innerHTML = entries
    .map((e, i) => {
      const cat = `<span class="chip chip-cat">${escapeHtml(e.auto_category)}</span>`;
      const tags = e.tags
        .map((t) => `<span class="chip chip-tag">${escapeHtml(t)}</span>`)
        .join("");
      const status = e.is_decryptable
        ? `<span class="chip chip-vault">🔒 已加密</span>`
        : `<span class="chip chip-undec">⚠ 无法解密</span>`;
      const noteLine = e.note
        ? `<div class="vault-note">${escapeHtml(e.note)}</div>`
        : "";
      return `
      <div class="vault-row${e.is_decryptable ? "" : " undecryptable"}${i === selected ? " selected" : ""}" data-id="${e.id}" data-index="${i}">
        <div class="row-main">
          <div class="row-meta">${status}${cat}${tags}</div>
          ${noteLine}
        </div>
        <div class="row-time">${formatTime(e.created_at)}</div>
      </div>`;
    })
    .join("");
}
