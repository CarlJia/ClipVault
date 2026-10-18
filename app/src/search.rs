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
     e.content_kind, e.mime_type, NULL AS payload, NULL AS payload_nonce, e.metadata_json";

/// 带标签的查询列清单（性能优化 W4 / R16）。
///
/// 在第 16 列（索引 15）追加 `GROUP_CONCAT(t.tag, CHAR(31))` —— 用 ASCII
/// Unit Separator（0x1F）作分隔符，而不是逗号：标签本身可能含逗号，
/// 用逗号会让「一个含逗号的标签」与「两个标签」无法区分。
///
/// 第一段 15 列与 [`SELECT_COLS`] 逐字相同，`Entry::from_sqlite_row` 的
/// 索引 0..=14 不受影响；调用方（`Storage::search_entries_with_tags`）从
/// 索引 15 取标签串。
pub(crate) const SELECT_COLS_WITH_TAGS: &str = "e.id, e.content_hash, e.content, e.note, e.auto_category, \
     e.is_pinned, e.pinned_at, e.is_vault, e.is_decryptable, e.created_at, \
     e.content_kind, e.mime_type, NULL AS payload, NULL AS payload_nonce, e.metadata_json, \
     GROUP_CONCAT(t.tag, CHAR(31))";

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
///
/// 性能优化 W1 / KTD1:把 `-` / `+` / `=` / `.` / `/` / `\` 从降级列表里
/// 拿掉。它们不是 FTS5 的语法字符,但**仍是 token 的一部分**:unicode61
/// tokenizer 在它们两侧会照常用作词边界,搜 `k=v` / `client-acme` /
/// `https://example.com/path` 时走 FTS5 比走 LIKE 三表全表扫快几个数量级。
/// 真正会让 FTS5 报错的字符(`'` / `"` / `*` / `^` / `:` / `(` / `)` /
/// `[` / `]` / `{` / `}` / `~` / `<` / `>`)保留降级。
fn is_fts_hostile(q: &str) -> bool {
    q.chars().any(|c| {
        matches!(
            c,
            '\'' | '"' | ':' | '(' | ')' | '*' | '^' | '{' | '}' | '[' | ']' | '~' | '<' | '>'
        )
    })
}

/// 把已清洗、非敏感的查询包装成 FTS5 前缀短语匹配串。
fn fts_match_string(q: &str) -> String {
    format!("\"{q}\"*")
}

/// 构建搜索计划：拼装 `WHERE` / `ORDER BY` / `LIMIT` 与绑定参数。
pub fn build_search_query(raw_query: &str, tab: &SearchTab, limit: u32) -> SearchPlan {
    build_search_query_impl(raw_query, tab, limit, false)
}

/// 带标签的搜索计划（性能优化 W4 / R16）。
///
/// 与 [`build_search_query`] 的 `WHERE` / `ORDER BY` / `LIMIT` 完全相同，只是在
/// `SELECT` 后追加 `GROUP_CONCAT(t.tag, CHAR(31))` 并 `LEFT JOIN tags`：
/// 一次 SQLite round-trip 就把每条条目的标签带回，命令层不再为同一批 id
/// 二次 `with_conn`（旧 `load_tags` 的 N+1 形状）。
pub fn build_search_query_with_tags(raw_query: &str, tab: &SearchTab, limit: u32) -> SearchPlan {
    build_search_query_impl(raw_query, tab, limit, true)
}

fn build_search_query_impl(
    raw_query: &str,
    tab: &SearchTab,
    limit: u32,
    with_tags: bool,
) -> SearchPlan {
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

    let filters_sql = filters.join(" AND ");
    let sql = if with_tags {
        // `GROUP BY e.id`:e.id 是主键,每组只有一行 entries;SQLite 的 bare-column
        // 扩展允许 SELECT 里出现非聚合列(取该组任意一行,这里只有一行)。
        format!(
            "SELECT {SELECT_COLS_WITH_TAGS} FROM entries e \
             LEFT JOIN tags t ON t.entry_id = e.id \
             WHERE {filters_sql} GROUP BY e.id ORDER BY {PIN_ORDER} LIMIT {effective_limit}"
        )
    } else {
        format!(
            "SELECT {SELECT_COLS} FROM entries e WHERE {filters_sql} ORDER BY {PIN_ORDER} LIMIT {effective_limit}"
        )
    };

    SearchPlan { sql, params }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 性能优化 W1 / KTD1:`is_fts_hostile` 必须把**真正会让 FTS5 parse error** 的字符
    /// 留下,把可作为 token 一部分的字符(`- + = . / \`)放走,让 URL / 路径 / `k=v` /
    /// `client-acme` 走 FTS5 而不是 LIKE。
    ///
    /// 这条单测纯粹钉字符集——`build_search_query` 的端到端验证由 `app/src/storage.rs`
    /// 的 `like_fallback_short_query` / `fts5_hit_on_content` 等用例覆盖。
    #[test]
    fn is_fts_hostile_allows_slash_dot_dash_plus_equal_and_backslash() {
        // 放开:`- + = . / \`
        for c in ['-', '+', '=', '.', '/', '\\'] {
            let q = format!("client{c}acme");
            assert!(
                !is_fts_hostile(&q),
                "字符 {c:?} 不应触发 LIKE 降级（走 FTS5 倒排更快）"
            );
        }

        // 真 FTS5 敏感字符保留降级（任挑几个代表）
        for c in ['\'', '"', '*', '^', ':', '(', ')', '[', ']', '~', '<', '>'] {
            let q = format!("query{c}suffix");
            assert!(
                is_fts_hostile(&q),
                "字符 {c:?} 必须触发 LIKE 降级（送进 FTS5 会 parse error）"
            );
        }
    }

    /// 5s 内的同内容重复只入一次 + `content_hash` UNIQUE 兜底去掉后到的同指纹——这条
    /// 路径只由 storage 层覆盖(`process_dedups_within_window` /
    /// `process_swallows_duplicate_fingerprint_already_in_db`)。search.rs 自身只断言
    /// `parse_tab` 把 `recent` / `tag:x` / `category:x` 翻译正确,以及
    /// `sanitize_query` 把控制字符剥掉、首尾空白 trim。这两条之前从未有人单测——这两
    /// 条是从 search.rs 抽出来后的纯函数,远比底层 SQL 更易回归。
    #[test]
    fn parse_tab_dispatches_to_recent_tag_and_category() {
        assert_eq!(parse_tab("recent"), SearchTab::Recent);
        assert_eq!(parse_tab("tag:work"), SearchTab::Tag("work".to_string()));
        assert_eq!(
            parse_tab("category:URL"),
            SearchTab::Category("URL".to_string())
        );
        // 未带前缀 / 未知 → All。这条降级是有意的(`search_results` 只在「已选中
        // 标签/类别」时被调用,「全部」由 `tag` 或 `category` 返回的列表项触发)。
        assert_eq!(parse_tab("all"), SearchTab::All);
        assert_eq!(parse_tab(""), SearchTab::All);
    }

    #[test]
    fn sanitize_query_strips_control_chars_and_trims() {
        // 控制字符被过滤、首尾空白 trim。
        assert_eq!(sanitize_query("  \thello\n  "), "hello");
        // NUL 等控制字符滤掉,普通字符保留。
        let raw = "foo bar";
        let cleaned = sanitize_query(raw);
        assert!(!cleaned.contains('\0'));
        assert!(cleaned.contains("foobar"));
        assert_eq!(
            sanitize_query("foo bar"),
            "foo bar",
            "中间空格不是控制字符，必须保留"
        );
    }

    /// W4 / R16:带标签版本必须 `LEFT JOIN tags` 加 `GROUP_CONCAT(t.tag, CHAR(31))`
    /// 再加 `GROUP BY e.id`。无标签的原版本必须保持不变——不能让改动顺手扩大
    /// 到 `list_vault` 之类还在用原路径的调用点。
    #[test]
    fn build_search_query_with_tags_adds_join_concat_and_group_by() {
        let with_tags = build_search_query_with_tags("q", &SearchTab::All, 10);
        assert!(
            with_tags
                .sql
                .contains("LEFT JOIN tags t ON t.entry_id = e.id"),
            "带标签版本必须 LEFT JOIN tags:{}",
            with_tags.sql
        );
        assert!(
            with_tags.sql.contains("GROUP_CONCAT(t.tag, CHAR(31))"),
            "带标签版本必须用 CHAR(31) 作分隔符(不是逗号):{}",
            with_tags.sql
        );
        assert!(
            with_tags.sql.contains("GROUP BY e.id"),
            "带标签版本必须 GROUP BY e.id:{}",
            with_tags.sql
        );
        // WHERE / ORDER BY 与无标签版本一致（只差 SELECT 段）。
        assert!(with_tags.sql.contains("e.is_vault = 0"));
        assert!(with_tags.sql.contains(PIN_ORDER));

        let plain = build_search_query("q", &SearchTab::All, 10);
        assert!(
            !plain.sql.contains("GROUP_CONCAT"),
            "无标签版本不得被顺带改成带 JOIN 的形状:{}",
            plain.sql
        );
        assert!(
            !plain.sql.contains("JOIN"),
            "无标签版本不该有 JOIN:{}",
            plain.sql
        );
    }

    /// 两种版本的绑定参数必须逐字一致(同一查询、同一 tab → 同样的 params);
    /// 否则命令层切到 with_tags 之后会立刻因参数错位而静默查错。
    #[test]
    fn with_tags_variant_binds_the_same_params() {
        for tab in [
            SearchTab::All,
            SearchTab::Recent,
            SearchTab::Tag("work".to_string()),
            SearchTab::Category("URL".to_string()),
        ] {
            let plain = build_search_query("client-acme", &tab, 25);
            let tagged = build_search_query_with_tags("client-acme", &tab, 25);
            assert_eq!(
                plain.params, tagged.params,
                "同一输入下两种版本的绑定参数必须一致（tab = {tab:?}）"
            );
        }
    }
}
