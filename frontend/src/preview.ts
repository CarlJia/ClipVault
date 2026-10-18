// frontend/src/preview.ts
//
// 列表旁预览栏（列表旁预览栏 plan U3 / R1 / R2 / R3 / R4）。
//
// 模块边界与 `editor.ts` 同型：DOM 节点 + 模块级状态 + open/close + render + IPC，
// 外部只经 `bindPreviewPane` / `openPreview` / `closePreview` / `togglePreview` /
// `updatePreviewForSelection` / `resetPreviewCache` 驱动；两侧状态（面板尺寸）由本模块
// 独占，主流程不碰。
//
// 为什么预览栏要「长在面板里」而不是另开一个窗口：焦点必须一直在搜索框（用户还在
// 打字 / 按方向键），第二个 WebView 会把焦点、生命周期、失焦收起全部再实现一遍。
// 尺寸切换走一条 IPC（`set_panel_mode`），后端负责把窗口长宽与落位算好。
//
// ## 数据从哪来
//
// - **文本**：列表 DTO 里就有 `content`（`panel.ts` 的 `previewOf` 只截前 120 字符，
//   这里要全文）。**加密区文本例外**：它的 `content` 从不下发（密文不出后端），
//   必须走一次 `get_entry_preview` 让后端解密。
// - **图片**：DTO 只有 64×64 缩略图，原图按需取 —— 后端缩到长边 ≤ 1600px 再回传。
// - **file / rtf / color**：本轮不做 Quick Look，栏内显示一句提示（plan Scope）。

import { invoke } from "@tauri-apps/api/core";
import { type EntryDto, escapeHtml } from "./panel";

/** 后端 `preview::PreviewDto` 的镜像。 */
export interface PreviewDto {
  kind: "image" | "text" | "none";
  mime: string | null;
  /** `data:image/png;base64,…`；仅图片有值。 */
  data_url: string | null;
  /** 完整文本；仅文本有值。 */
  content: string | null;
  width: number | null;
  height: number | null;
  /** 加密区条目解不开：界面显示「无法解密」，此时 `data_url` / `content` 都是 null。 */
  undecryptable: boolean;
}

/** 预览栏的 `<aside>`（`index.html` 的 `#preview`）。由 `main.ts` 在启动时注入。 */
let pane: HTMLElement | null = null;

/** 加密区解不开时栏里那句话（两条路径共用：前端已知解不开 / 后端回 `undecryptable`）。 */
const UNDECRYPTABLE_HINT = "🔒 无法解密";

/** 当前是否展开。 */
let open = false;
/** 栏内正在展示的条目 id；`null` 表示没有（栏关闭或列表空）。 */
let currentId: number | null = null;
/** 正在飞的预览请求的条目 id（单飞：同一时刻最多一个）。 */
let inflightId: number | null = null;
/** 请求在飞期间又换了选中：只留最新一条，前一个落地后再发。 */
let queuedEntry: EntryDto | null = null;

/**
 * 图片预览缓存：`id -> { data URL, 固有宽高 }`。
 *
 * **关闭时不清**（R3：方向键来回切不该反复向 IPC 要同一张大图），只在面板重新被
 * 唤出时清（`resetPreviewCache`）—— 用户离开又回来，看到的不该是上次那份可能已经
 * 过期的图。
 *
 * 缓存宽高（W5 / R21）：命中缓存时也要能把 `width` / `height` 属性写进 `<img>`，
 * 否则命中路径缺了这两个数字、布局仍会抖。
 */
interface CachedImage {
  url: string;
  width: number | null;
  height: number | null;
}
const imageCache = new Map<number, CachedImage>();

/** 缓存条数上限：单条 data URL 可达数 MB，面板长时间开着不能无限攒。 */
const IMAGE_CACHE_LIMIT = 16;

/** 写缓存并按 FIFO 淘汰最旧的一条（`Map` 的迭代顺序就是插入顺序）。 */
function cacheImage(
  id: number,
  url: string,
  width: number | null,
  height: number | null,
): void {
  imageCache.delete(id);
  imageCache.set(id, { url, width, height });
  if (imageCache.size <= IMAGE_CACHE_LIMIT) return;
  const oldest = imageCache.keys().next().value;
  if (oldest !== undefined) imageCache.delete(oldest);
}

/** 绑定 DOM 节点。重复调用是幂等的（HMR 重载时不会留下两个引用）。 */
export function bindPreviewPane(el: HTMLElement): void {
  pane = el;
}

/** 清空图片缓存。面板每次被唤出时调用（见模块顶部注释）。 */
export function resetPreviewCache(): void {
  imageCache.clear();
}

/**
 * 打开预览栏并展示 `entry`（R1 / R2）。
 *
 * 已经开着时只换内容、**不**再切一次尺寸档位：`set_panel_mode("expanded")` 会在后端
 * 重新落位一次，连按方向键时会看到窗口抖动。
 */
export function openPreview(entry: EntryDto): void {
  if (!pane) return;
  const wasOpen = open;
  open = true;
  currentId = entry.id;
  pane.classList.remove("hidden");
  renderEntry(entry);
  if (!wasOpen) void setPanelMode("expanded");
}

/**
 * 关闭预览栏并把窗口收回 normal（R4 / R10 / R12）。
 *
 * 没开着时直接返回：关面板的路径（失焦 / 回贴 / 托盘）都会顺手调一次，不该为此
 * 每次多打一条 IPC。
 *
 * 返回 Promise 是为了让回贴路径能**等**尺寸收回去之后再去贴（R12：先把窗口收回
 * normal 再走原流程）—— 面板马上要被后端藏起来，漏了这一下，窗口会以 960 宽留在
 * 下一次唤出时的重定位里（那是按 560 算的）。
 */
export async function closePreview(): Promise<void> {
  if (!open) return;
  open = false;
  currentId = null;
  queuedEntry = null;
  paint(hintHtml("无预览"));
  pane?.classList.add("hidden");
  await setPanelMode("normal");
}

/** 开关式切换（R10 + R11：Space 与点击 `.row-thumb` 共用这一个入口）。 */
export function togglePreview(entry: EntryDto | undefined): void {
  if (open) {
    void closePreview();
    return;
  }
  if (entry) openPreview(entry);
}

/**
 * 选中条目变了（R3）：栏开着时同步换内容，栏没开就什么都不做。
 *
 * 选中的条目消失（列表被清空 / 当前 tab 换了）时顺手收起，避免栏里留着一条
 * 已经不存在的条目。
 */
export function updatePreviewForSelection(entry: EntryDto | undefined): void {
  if (!open) return;
  if (!entry) {
    // 选中的条目没了（列表变空 / 切了 tab）：收起来，别在栏里留一条已经不存在的内容。
    void closePreview();
    return;
  }
  if (entry.id === currentId) return;
  currentId = entry.id;
  renderEntry(entry);
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

function paint(html: string): void {
  if (!pane) return;
  pane.innerHTML = html;
}

/**
 * 按 `content_kind` 分派渲染（R2）。
 *
 * 需要 IPC 的两条（图片 / 加密区文本）先画占位、再异步补内容：占位一帧比让栏留空
 * 好认 —— 用户能看到「点了有反应」。
 */
function renderEntry(entry: EntryDto): void {
  const kind = entry.content_kind ?? "text";
  // 加密区条目已知解不开：不必再跑一次 IPC（后端也会拒），直接说结果。
  if (entry.is_vault && !entry.is_decryptable) {
    paint(hintHtml(UNDECRYPTABLE_HINT));
    return;
  }
  switch (kind) {
    case "image": {
      const cached = imageCache.get(entry.id);
      if (cached) {
        paint(imageHtml(cached.url, cached.width, cached.height));
        return;
      }
      paint(hintHtml("正在加载图片…"));
      requestPreview(entry);
      return;
    }
    case "text": {
      if (!entry.is_vault) {
        paint(textHtml(entry.content));
        return;
      }
      // 加密区文本：`content` 是空的（密文不下发），全文只能由后端解出来。
      paint(hintHtml("正在解密…"));
      requestPreview(entry);
      return;
    }
    case "file":
    case "rtf":
    case "color":
      paint(hintHtml(`无预览（${kind}）`));
      return;
    default:
      paint(textHtml(entry.content));
  }
}

/**
 * 排一次预览请求（图片 / 加密区文本）。
 *
 * 同一时刻只让**一个** `get_entry_preview` 在飞：方向键按住扫过十几条时，之前的写法
 * 是每条各发一次全量解码（20MB 原图 × N 并发），既拖慢当前这条、又把 CPU 打满。
 * 在飞期间的新选中只记最新的一条，等前一个落地再发 —— 用户停在哪条，最后画出来的
 * 就是哪条（中间的选中本来就没人看得到）。
 */
function requestPreview(entry: EntryDto): void {
  if (inflightId !== null) {
    queuedEntry = entry;
    return;
  }
  void loadPreview(entry);
}

/**
 * 取一次预览数据并渲染（图片 / 加密区文本）。
 *
 * 返回时先核对「还是不是同一条、栏还开着」：关栏 / 收起后迟到的响应不能往栏里画，
 * 切换到别条时旧结果也不能覆盖新内容（AE1 的「结果不混」）。
 */
async function loadPreview(entry: EntryDto): Promise<void> {
  const id = entry.id;
  inflightId = id;
  try {
    const dto = await invoke<PreviewDto>("get_entry_preview", { id });
    const image = dto.kind === "image" ? dto.data_url : null;
    // 先入缓存再判「还是不是这一条」：迟到的响应也得留下，否则再转回来还要重解码一次。
    // 加密区条目例外：解密结果不留在模块级缓存里，只在栏内显示一次。
    if (image && !entry.is_vault) cacheImage(id, image, dto.width, dto.height);
    if (isCurrent(id)) {
      if (image) {
        paint(imageHtml(image, dto.width, dto.height));
      } else if (dto.kind === "text" && dto.content !== null) {
        paint(textHtml(dto.content));
      } else if (dto.undecryptable) {
        paint(hintHtml(UNDECRYPTABLE_HINT));
      } else {
        paint(hintHtml("无预览"));
      }
    }
  } catch (err) {
    if (isCurrent(id)) paint(hintHtml(`预览失败：${String(err)}`));
  } finally {
    inflightId = null;
    const next = queuedEntry;
    queuedEntry = null;
    // 排队的这条可能已经被跳过（又按了方向键 / 栏关了）：只发当前真正在展示的那条。
    if (next && next.id !== id && isCurrent(next.id)) void loadPreview(next);
  }
}

/** 栏还开着、且展示的还是 `id` 这一条。 */
function isCurrent(id: number): boolean {
  return open && currentId === id;
}

/**
 * 预览图的 `<img>`（W5 / R21 / R22）。
 *
 * - **宽高属性**：后端 `PreviewDto` 给的是解码后的真实像素尺寸。写进 `width` /
 *   `height` 属性后，浏览器在图片字节解码完成前就能按正确比例预留空间，
 *   消除「正在加载图片…」提示 → `<img>` 切换时栏内的布局抖动（CLS）。
 *   与 CSS 的 `object-fit: contain` + `max-width/height: 100%` 不冲突：
 *   那两个属性只决定**渲染框**，固有尺寸决定的是**比例**。
 * - **不做 `escapeHtml`**：`src` 是后端拼的 `data:image/png;base64,…`，前缀是固定
 *   字面量、载荷只有 base64 字母表（`A-Za-z0-9+/=`），**不含任何 HTML 元字符**。
 *   对一条可达数百 KB 的字符串跑 5 段正则替换纯属白扫。前提是这里只接后端 data URL；
 *   若将来有别的 src 来源，必须重新评估。
 */
function imageHtml(src: string, width: number | null, height: number | null): string {
  const dims =
    width !== null && height !== null && width > 0 && height > 0
      ? ` width="${width}" height="${height}"`
      : "";
  return `<img class="preview-image" src="${src}"${dims} alt="图片预览">`;
}

function textHtml(content: string): string {
  // `<pre>` + `white-space: pre-wrap`：保留换行与缩进（代码片段、日志），同时自动折行。
  return `<pre class="preview-text">${escapeHtml(content)}</pre>`;
}

/** 提示文案：唯一一处把纯文本放进 HTML 的地方，转义收在这里。 */
function hintHtml(text: string): string {
  return `<div class="preview-hint">${escapeHtml(text)}</div>`;
}

// ---------------------------------------------------------------------------
// 面板尺寸档位（U2 的 IPC）
// ---------------------------------------------------------------------------

/**
 * 切换面板尺寸档位。失败只留一条 console 警告：尺寸没变不会让面板不可用，
 * 而把错误抛到 UI 上只会多一条没人看得懂的提示。
 */
async function setPanelMode(mode: "normal" | "expanded"): Promise<void> {
  try {
    await invoke("set_panel_mode", { mode });
  } catch (err) {
    console.warn(`[preview] set_panel_mode(${mode}) 失败：`, err);
  }
}
