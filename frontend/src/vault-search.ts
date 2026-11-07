// frontend/src/vault-search.ts
//
// 加密区面板的搜索（纯函数层）。
//
// 为什么单独一个模块：`main.ts` 顶部就执行 `document.querySelector`，前端测试跑在
// **没有 DOM** 的 vitest 环境里，根本导入不了它 —— 所以过滤与选中收敛的状态转移
// 必须住在一个能被直接 import 的纯函数模块里，测试才有东西可钉（仓库既有做法见
// `panel-show.test.ts` / `tray-entries.test.ts`：用 `?raw` 读源码做接线断言）。
//
// 一条重要的不变式：**视图是派生值，不是第二份状态**。全量条目数组 + 输入框里的
// 查询 → 视图，现算现用；不存在「上一次过滤结果」那种需要各处手动刷新的缓存。
// 少了它，改备注 / 删除条目之后就会出现「列表还留着旧行」的幽灵状态。

/**
 * 参与匹配的条目字段（结构化子集）。
 *
 * 与 `panel.ts` 的 `EntryDto` 结构兼容，故 `EntryDto` 可以直接传进来；这里只声明
 * 用得到的字段，是为了本模块不必 import `panel.ts`（那会多出一条依赖边）。
 */
export interface VaultSearchable {
  id: number;
  /** 备注。 */
  note: string;
  /** 自动分类。 */
  auto_category: string;
  /** 标签。 */
  tags: string[];
  /** 创建时间（秒级 Unix 时间戳）。 */
  created_at: number;
}

/**
 * 字段分隔符：NUL。
 *
 * 查询词按空白拆分，故任何查询词都不可能含有 NUL —— 「备注尾 + 标签头」这种
 * 跨字段误命中因此不会发生（用空格或换行分隔就会：备注 "ab" + 标签 "c" 会被
 * "abc" 命中）。
 */
const FIELD_SEP = "\u0000";

/**
 * 本地时区的 `YYYY-MM-DD`。
 *
 * 用**本地**取值（`getFullYear` / `getMonth` / `getDate`），不是 `toISOString()` ——
 * 后者是 UTC，跨时区时会差一天，用户按自己看到的日期搜反而搜不到。
 */
export function localDateString(unixSecs: number): string {
  const d = new Date(unixSecs * 1000);
  const y = String(d.getFullYear()).padStart(4, "0");
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/**
 * 条目的可搜索文本：备注 / 类别 / 每个标签 / 本地日期，小写、以 NUL 分隔。
 *
 * 只取这四项 —— 加密区条目的**正文是密文**，不解密就搜不到；`metadata_json`
 * 里的字段（文件路径、图片尺寸）面板里并不显示，搜到了也是「命中了看不见的东西」。
 */
export function vaultHaystack(e: VaultSearchable): string {
  return [e.note, e.auto_category, ...e.tags, localDateString(e.created_at)]
    .join(FIELD_SEP)
    .toLowerCase();
}

/** 查询拆词：按空白拆分。全空白 → 空数组（= 不过滤）。 */
function queryTerms(query: string): string[] {
  return query
    .trim()
    .split(/\s+/)
    .filter((t) => t.length > 0);
}

/**
 * 这条是否命中查询：词间「与」，每个词做大小写不敏感的子串匹配。
 *
 * 空查询命中一切（没有查询就没有过滤）。
 */
export function matchesVaultQuery(e: VaultSearchable, query: string): boolean {
  const terms = queryTerms(query);
  if (terms.length === 0) return true;
  const hay = vaultHaystack(e);
  return terms.every((t) => hay.includes(t.toLowerCase()));
}

/**
 * 过滤出命中项，顺序与输入一致。
 *
 * 空查询直接返回全量的一份拷贝（视图是独立的一份数组，不与全量共享引用）。
 */
export function filterVaultEntries<T extends VaultSearchable>(
  entries: T[],
  query: string,
): T[] {
  if (queryTerms(query).length === 0) return entries.slice();
  return entries.filter((e) => matchesVaultQuery(e, query));
}

/** 一次状态转移的结果：视图 + 选中索引。 */
export interface VaultViewState<T> {
  view: T[];
  selected: number;
}

/**
 * 选中态的快照。
 *
 * `id` 是**跨视图稳定**的标识（索引会随过滤结果变化，id 不会），
 * `index` 是位置（条目不在结果里了时，用它夹紧）。
 */
export interface VaultSelection {
  id: number | null;
  index: number;
}

/**
 * 「查询变化」这条路径：视图按查询过滤，选中回到第一条。
 *
 * 用户刚敲下的查询与之前的选中无关（命中的集合整个变了），归零是最可预期的行为。
 */
export function applyVaultQuery<T extends VaultSearchable>(
  entries: T[],
  query: string,
): VaultViewState<T> {
  return { view: filterVaultEntries(entries, query), selected: 0 };
}

/**
 * 「数据变化」这条路径（改完备注 / 删掉条目之后）：视图重算，选中**按 id 收敛**。
 *
 * - 该条目仍在结果里 → 保持选中它（改完备注还命中的那条不该跳走）；
 * - 已经不在结果里 → 按旧索引夹紧（删掉的那一行被后一行顶上，与主列表删除后的
 *   观感一致），而不是跳回第一条。
 */
export function reapplyVaultFilter<T extends VaultSearchable>(
  entries: T[],
  query: string,
  prev: VaultSelection,
): VaultViewState<T> {
  const view = filterVaultEntries(entries, query);
  if (view.length === 0) return { view, selected: 0 };
  if (prev.id !== null) {
    const idx = view.findIndex((e) => e.id === prev.id);
    if (idx >= 0) return { view, selected: idx };
  }
  const idx = Math.min(Math.max(prev.index, 0), view.length - 1);
  return { view, selected: idx };
}

/** 按 id 摘行（删除条目）。返回新数组 —— 全量数组是唯一的状态，不就地改。 */
export function removeVaultEntry<T extends VaultSearchable>(entries: T[], id: number): T[] {
  return entries.filter((e) => e.id !== id);
}

/** 按 id 换行（改完备注 / 标签后）。返回新数组。 */
export function replaceVaultEntry<T extends VaultSearchable>(entries: T[], updated: T): T[] {
  return entries.map((e) => (e.id === updated.id ? updated : e));
}

/** 加密区空态文案（无条目）。 */
export const VAULT_EMPTY_TEXT = "加密区暂无条目";

/** 加密区空态文案（有查询但没命中）。 */
export const VAULT_NO_MATCH_TEXT = "没有匹配的条目";

/**
 * 加密区空态文案：有查询没命中 → 「没有匹配的条目」；无查询 → 「加密区暂无条目」。
 *
 * 两者必须区分：把「搜不到」说成「没有条目」，用户会以为自己的条目丢了。
 */
export function vaultEmptyStateText(hasQuery: boolean): string {
  return hasQuery ? VAULT_NO_MATCH_TEXT : VAULT_EMPTY_TEXT;
}

/**
 * 加密区头部的计数行：「共 12 条 · 3 条无法解密 · 匹配 2 条」。
 *
 * - 「无法解密」的条数与「匹配」数口径不同：前者按**全量**算（那是要被「清除不可
 *   解密条目」清掉的东西），后者按当前查询命中的算。
 * - 总数为 0 → 返回空串：那时的引导语含平台相关的键位（`⌘L` / `Ctrl+L`），
 *   由 `main.ts` 用 `platform.ts` 的 `keys()` 拼，纯函数产不出平台文案。
 */
export function vaultHeaderText(
  total: number,
  undecryptable: number,
  matched: number,
  hasQuery: boolean,
): string {
  if (total === 0) return "";
  const parts = [`共 ${total} 条`];
  if (undecryptable > 0) parts.push(`${undecryptable} 条无法解密`);
  if (hasQuery) parts.push(`匹配 ${matched} 条`);
  return parts.join(" · ");
}
