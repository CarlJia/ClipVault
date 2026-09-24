//! Tauri IPC 命令层（U4）。
//!
//! 这里定义前端浮层调用的 `#[tauri::command]` 函数与它们返回的 DTO。
//! U4 只需要“薄”实现：把查询委托给 [`Storage`] 的公开 API，
//! 返回 serde 可序列化的结构。真正的排序 / 检索细化留给后续单元：
//!
//! - `search_results` 的 FTS 排序细化 → U5
//! - `toggle_pin` 的置顶排序 → U7
//! - `open_note_editor` 的备注/标签编辑 → U6
//! - `paste_entry` 的 CGEvent 回贴注入 → U8
//! - `set_guest_mode` / `set_skip_next` 的持久化 → U10
//!
//! U4 只保证类型化的 IPC “接缝”存在并能编译、调用。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clipvault_core::Entry;
use rusqlite::Result as SqliteResult;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::storage::Storage;
use crate::tray::{self, TrayState};

/// 进程内共享状态：存储句柄 + 与 watcher 共享的两个原子开关。
///
/// `guest_mode` / `skip_next` 与 [`crate::watcher::Watcher`] 持有的是**同一批**
/// `Arc<AtomicBool>`，因此这里的写入会被后台 watcher 立即看到（U10 完成持久化）。
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<Storage>,
    pub guest_mode: Arc<AtomicBool>,
    pub skip_next: Arc<AtomicBool>,
}

/// 单条剪贴板记录的前端 DTO。
#[derive(Debug, Clone, Serialize)]
pub struct EntryDto {
    pub id: i64,
    pub content: String,
    pub note: String,
    pub auto_category: String,
    pub tags: Vec<String>,
    pub is_pinned: bool,
    pub pinned_at: Option<i64>,
    pub is_vault: bool,
    pub created_at: i64,
}

/// 「标签」tab 的聚合项：标签名 + 条目数（R6）。
#[derive(Debug, Clone, Serialize)]
pub struct TagInfo {
    pub tag: String,
    pub count: i64,
}

/// 「类别」tab 的聚合项：auto_category + 条目数（R6）。
#[derive(Debug, Clone, Serialize)]
pub struct CategoryInfo {
    pub category: String,
    pub count: i64,
}

// ----------------------------------------------------------------
// 内部 helper
// ----------------------------------------------------------------

/// 为一批 entry id 批量拉取标签。返回 `id -> tags` 映射（无标签的 id 不入表）。
async fn load_tags(state: &AppState, ids: &[i64]) -> SqliteResult<HashMap<i64, Vec<String>>> {
    let ids = ids.to_vec();
    state
        .storage
        .with_conn(move |conn| {
            let mut map: HashMap<i64, Vec<String>> = HashMap::new();
            let mut stmt = conn.prepare("SELECT tag FROM tags WHERE entry_id = ?1 ORDER BY tag")?;
            for id in ids {
                let rows = stmt.query_map([id], |r| r.get::<_, String>(0))?;
                let tags: Vec<String> = rows.collect::<SqliteResult<_>>()?;
                if !tags.is_empty() {
                    map.insert(id, tags);
                }
            }
            Ok(map)
        })
        .await
}

/// 把 [`Entry`] 列表转换为带标签的 [`EntryDto`] 列表。
async fn to_dtos(state: &AppState, entries: Vec<Entry>) -> Result<Vec<EntryDto>, String> {
    let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
    let mut tag_map = load_tags(state, &ids).await.map_err(|e| e.to_string())?;
    Ok(entries
        .into_iter()
        .map(|e| {
            let tags = tag_map.remove(&e.id).unwrap_or_default();
            EntryDto {
                id: e.id,
                content: e.content,
                note: e.note,
                auto_category: e.auto_category,
                tags,
                is_pinned: e.is_pinned,
                pinned_at: e.pinned_at,
                is_vault: e.is_vault,
                created_at: e.created_at,
            }
        })
        .collect())
}

// ----------------------------------------------------------------
// 查询类命令（委托存储层；U5 细化 FTS 排序）
// ----------------------------------------------------------------

/// 主检索命令。`tab` 取值：
/// - `recent` / `all`：条目列表（Pin 优先 + 时间倒序，尊重 `query`）。
/// - `tag:<name>` / `category:<name>`：筛选到某标签 / 类别的条目列表。
///
/// SQL 构建委托给 [`crate::search::build_search_query`]，执行委托给
/// [`Storage::search_entries`]；加密区条目在所有分支被排除，`recent`
/// 固定 30 条上限（R6/R17）。裸 `tag` / `category` 的「列表 + 计数」
/// 由 [`get_all_tags`] / [`get_categories`] 负责，不走此命令。
#[tauri::command]
pub async fn search_results(
    query: String,
    tab: String,
    limit: u32,
    state: State<'_, AppState>,
) -> Result<Vec<EntryDto>, String> {
    let plan = crate::search::build_search_query(&query, &crate::search::parse_tab(&tab), limit);
    let entries = state
        .storage
        .search_entries(plan)
        .await
        .map_err(|e| e.to_string())?;
    to_dtos(&state, entries).await
}

/// 「最近」tab：按 pin 优先 + 时间倒序返回最近 `limit` 条。
#[tauri::command]
pub async fn list_recent(limit: u32, state: State<'_, AppState>) -> Result<Vec<EntryDto>, String> {
    let entries = state
        .storage
        .list_recent(limit)
        .await
        .map_err(|e| e.to_string())?;
    to_dtos(&state, entries).await
}

/// 「标签」tab：所有标签 + 条目数（排除加密区条目，R6）。
#[tauri::command]
pub async fn get_all_tags(state: State<'_, AppState>) -> Result<Vec<TagInfo>, String> {
    let rows = state
        .storage
        .tag_counts()
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(tag, count)| TagInfo { tag, count })
        .collect())
}

/// 「类别」tab：所有 auto_category + 条目数（仅统计非加密条目，R6）。
#[tauri::command]
pub async fn get_categories(state: State<'_, AppState>) -> Result<Vec<CategoryInfo>, String> {
    let rows = state
        .storage
        .category_counts()
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(category, count)| CategoryInfo { category, count })
        .collect())
}

// ----------------------------------------------------------------
// 权限 / 窗口
// ----------------------------------------------------------------

/// AE7：返回 `AXIsProcessTrusted()`。未授权时前端展示 Accessibility 横幅。
/// 真正的 CGEvent 回贴注入在 U8 完成，此处只提供检测接缝。
#[tauri::command]
pub fn check_accessibility() -> bool {
    crate::hotkey::check_accessibility()
}

/// 关闭（隐藏）主面板。前端 `Esc` 调用。
#[tauri::command]
pub fn hide_panel(window: tauri::WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
}

// ----------------------------------------------------------------
// 后续单元的类型化接缝（U6 / U7 / U8 / U10）
// ----------------------------------------------------------------

/// U8 完成：CGEvent 模拟 ⌘V 回贴。U4 仅占位。
#[tauri::command]
pub fn paste_entry(id: i64) -> Result<String, String> {
    let _ = id;
    Ok("unimplemented:U8".to_string())
}

/// U7 完成：切换置顶并按 `pinned_at DESC` 重排。U4 仅占位。
#[tauri::command]
pub fn toggle_pin(id: i64) -> Result<String, String> {
    let _ = id;
    Ok("unimplemented:U7".to_string())
}

/// U6 完成：打开备注/标签编辑器。U4 仅占位。
#[tauri::command]
pub fn open_note_editor(id: i64) -> Result<String, String> {
    let _ = id;
    Ok("unimplemented:U6".to_string())
}

/// 设置访客模式开关。写入与 watcher 共享的原子布尔，并刷新托盘状态。
/// U10 负责把该状态持久化到 `settings` 表。
#[tauri::command]
pub fn set_guest_mode(enabled: bool, app: AppHandle, state: State<'_, AppState>) {
    state.guest_mode.store(enabled, Ordering::SeqCst);
    let next = if enabled {
        TrayState::Guest
    } else {
        TrayState::Monitoring
    };
    tray::set_state(&app, next);
}

/// 读取当前访客模式状态。
#[tauri::command]
pub fn get_guest_mode(state: State<'_, AppState>) -> bool {
    state.guest_mode.load(Ordering::SeqCst)
}

/// 「本次不记录」：置位一次性 skip flag，watcher 消费后复位（R11）。
#[tauri::command]
pub fn set_skip_next(state: State<'_, AppState>) {
    state.skip_next.store(true, Ordering::SeqCst);
}
