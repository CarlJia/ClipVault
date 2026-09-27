//! 搜索 SQL 构建与排序逻辑（U5）。
//!
//! 本模块是「纯」的：只负责把用户输入 + tab 维度翻译成一条 SQL
//! 语句和一组绑定参数（[`SearchPlan`]），真正的执行交给
//! [`crate::storage::Storage::search_entries`]。这样搜索规则可被
//! 单元测试直接断言，存储层只保留连接与执行。
//!
//! 排序遵守 R17「Pin 优先」：任何 tab、任何筛选、任何查询下,
//! 置顶条目都排在非置顶条目之前，先按 `pinned_at DESC`（nulls-last,
//! SQLite 无 `NULLS LAST`，用 `CASE WHEN ... IS NULL` 手工兜底,
//! 沿用 U2 的写法），再按 `created_at DESC`。
//!
//! 匹配策略（R5）：
//! - 空查询 → 不加匹配条件，退化为按 tab 的纯列表。
//! - 查询 < 3 字符，或含 FTS5 敏感字符（冒号/引号等）→ 用
//!   `LIKE '%q%'` 跨 content/note/tags 兜底，避免 FTS5 query
//!   parser 崩溃（如 `tag:acme` 会被当成列过滤符）。
//! - 其余 → FTS5 `MATCH`，命中 content/note/tags_text 三列。
//!
//! 加密区（`is_vault = 1`）条目在所有分支都被 `WHERE e.is_vault = 0`
//! 排除，永不出现在普通搜索里（其专属视图由 U9 负责）。

/// 一条可执行的搜索计划：SQL 文本 + 顺序绑定的字符串参数。
///
/// `LIMIT` 已作为经校验的整数内联进 `sql`，因此所有 `params`
/// 都是字符串（FTS query / LIKE pattern / tag / category），
/// 可用 `rusqlite::params_from_iter` 一次性绑定。
#[derive(Debug, Clone)]
pub struct SearchPlan {
    pub sql: String,
    pub params: Vec<String>,
}

/// tab 维度。字符串形态见 [`parse_tab`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchTab {
    /// 「最近」：Pin 优先，固定上限 30 条。
    Recent,
    /// 「全部」：Pin 优先全量排序，尊重查询。
    All,
    /// 「标签」筛选到某个标签。
    Tag(String),
    /// 「类别」筛选到某个 auto_category。
    Category(String),
}

const SELECT_COLS: &str = "e.id, e.content_hash, e.content, e.note, e.auto_category, \
     e.is_pinned, e.pinned_at, e.is_vault, e.is_decryptable, e.created_at, \
     e.content_kind, e.mime_type, e.payload, e.payload_nonce, e.metadata_json";

/// Pin 优先 + 时间倒序（R17）。`pinned_at` nulls-last 手工兜底。
pub(crate) const PIN_ORDER: &str = "e.is_pinned DESC, \
     CASE WHEN e.pinned_at IS NULL THEN 1 ELSE 0 END ASC, \
     e.pinned_at DESC, e.created_at DESC";

/// 「最近」tab 的固定条数上限（R6：默认 30 条）。
pub const RECENT_LIMIT: u32 = 30;

/// 把前端传来的 `tab` 字符串解析成 [`SearchTab`]。
///
/// 约定：
/// - `recent` → [`SearchTab::Recent`]
/// - `tag:<name>` → [`SearchTab::Tag`]（`<name>` 可含冒号）
/// - `category:<name>` → [`SearchTab::Category`]
/// - 其余（`all` / 裸 `tag` / 裸 `category` / 未知）→ [`SearchTab::All`]
///
/// 裸 `tag` / `category` 归到 `All` 是有意的降级：这两个 tab 的
/// 「列出标签/类别 + 计数」由 `get_all_tags` / `get_categories` 命令
/// 负责，`search_results` 只在「已选中某标签/类别」时被调用。
pub fn parse_tab(tab: &str) -> SearchTab {
    if let Some(name) = tab.strip_prefix("tag:") {
        SearchTab::Tag(name.to_string())
    } else if let Some(name) = tab.strip_prefix("category:") {
        SearchTab::Category(name.to_string())
    } else if tab == "recent" {
        SearchTab::Recent
    } else {
        SearchTab::All
    }
}

/// 清洗查询：剥离控制字符，去掉首尾空白。
pub fn sanitize_query(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// 是否含 FTS5 query parser 敏感字符。含则降级到 LIKE 兜底。
///
/// 这些字符在 FTS5 MATCH 语法里有特殊含义（列过滤 `:`、短语 `"`、
/// NOT `-`、前缀 `*`、分组 `()` 等），未加转义直接送入会触发
/// parse error。降级到 LIKE 既不崩溃，又能对 `client-acme` 之类
/// 带连字符的标签做子串命中。
fn is_fts_hostile(q: &str) -> bool {
    q.chars().any(|c| {
        matches!(
            c,
            '"' | '\''
                | ':'
                | '('
                | ')'
                | '*'
                | '^'
                | '-'
                | '+'
                | '{'
                | '}'
                | '['
                | ']'
                | '/'
                | '\\'
                | '~'
                | '<'
                | '>'
                | '='
        )
    })
}

/// 把已清洗、非敏感的查询包装成 FTS5 前缀短语匹配串。
fn fts_match_string(q: &str) -> String {
    format!("\"{q}\"*")
}

/// 构建搜索计划：拼装 `WHERE` / `ORDER BY` / `LIMIT` 与绑定参数。
pub fn build_search_query(raw_query: &str, tab: &SearchTab, limit: u32) -> SearchPlan {
    let query = sanitize_query(raw_query);
    let mut params: Vec<String> = Vec::new();
    // 加密区条目永不进入普通搜索（R + U9）。
    let mut filters: Vec<String> = vec!["e.is_vault = 0".to_string()];

    // tab 维度筛选。
    match tab {
        SearchTab::Tag(name) => {
            filters.push("e.id IN (SELECT entry_id FROM tags WHERE tag = ?)".to_string());
            params.push(name.clone());
        }
        SearchTab::Category(name) => {
            filters.push("e.auto_category = ?".to_string());
            params.push(name.clone());
        }
        SearchTab::Recent | SearchTab::All => {}
    }

    // 查询匹配（content / note / tags）。
    if !query.is_empty() {
        let use_like = query.chars().count() < 3 || is_fts_hostile(&query);
        if use_like {
            let pattern = format!("%{query}%");
            filters.push(
                "(e.content LIKE ? OR e.note LIKE ? \
                 OR e.id IN (SELECT entry_id FROM tags WHERE tag LIKE ?))"
                    .to_string(),
            );
            params.push(pattern.clone());
            params.push(pattern.clone());
            params.push(pattern);
        } else {
            filters.push(
                "e.id IN (SELECT rowid FROM entries_fts WHERE entries_fts MATCH ?)".to_string(),
            );
            params.push(fts_match_string(&query));
        }
    }

    let effective_limit = match tab {
        SearchTab::Recent => RECENT_LIMIT,
        _ => limit,
    };

    let sql = format!(
        "SELECT {SELECT_COLS} FROM entries e WHERE {filters} ORDER BY {PIN_ORDER} LIMIT {effective_limit}",
        filters = filters.join(" AND "),
    );

    SearchPlan { sql, params }
}
