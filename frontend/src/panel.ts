// ClipVault 面板渲染层（U4）。
//
// 负责把后端返回的 DTO 渲染成 Raycast 风格的平铺列表 / 标签列表 / 类别列表。
// 纯渲染，无 IPC；交互与状态由 main.ts 驱动。

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
  created_at: number;
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
function previewOf(content: string): string {
  const oneLine = content.replace(/\s+/g, " ").trim();
  return oneLine.length > 120 ? `${oneLine.slice(0, 120)}…` : oneLine;
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
      const pin = e.is_pinned ? `<span class="pin" title="已置顶">􀎧</span>` : "";
      const cat = `<span class="chip chip-cat">${escapeHtml(e.auto_category)}</span>`;
      const tags = e.tags
        .map((t) => `<span class="chip chip-tag">${escapeHtml(t)}</span>`)
        .join("");
      const note = e.note
        ? `<span class="note-dot" title="有备注">􀈬</span>`
        : "";
      return `
      <div class="row${i === selected ? " selected" : ""}" data-id="${e.id}" data-index="${i}">
        <div class="row-main">
          <div class="row-content">${pin}${escapeHtml(previewOf(e.content))}</div>
          <div class="row-meta">${cat}${tags}${note}</div>
        </div>
        <div class="row-time">${formatTime(e.created_at)}</div>
      </div>`;
    })
    .join("");
}

/** 渲染「标签」聚合列表。可点击筛选；`active` 为当前选中的标签名。 */
export function renderTags(
  container: HTMLElement,
  tags: TagInfo[],
  active: string | null = null,
): void {
  if (tags.length === 0) {
    container.innerHTML = `<div class="empty">还没有标签</div>`;
    return;
  }
  container.innerHTML = tags
    .map(
      (t, i) => `
      <div class="agg-row${t.tag === active ? " selected" : ""}" data-index="${i}" data-kind="tag">
        <span class="chip chip-tag">${escapeHtml(t.tag)}</span>
        <span class="agg-count">${t.count}</span>
      </div>`,
    )
    .join("");
}

/** 渲染「类别」聚合列表。可点击筛选；`active` 为当前选中的类别名。 */
export function renderCategories(
  container: HTMLElement,
  cats: CategoryInfo[],
  active: string | null = null,
): void {
  if (cats.length === 0) {
    container.innerHTML = `<div class="empty">还没有记录</div>`;
    return;
  }
  container.innerHTML = cats
    .map(
      (c, i) => `
      <div class="agg-row${c.category === active ? " selected" : ""}" data-index="${i}" data-kind="category">
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
