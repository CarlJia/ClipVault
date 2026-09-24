//! Tauri IPC 命令层（U4）。
//!
//! 这里定义前端浮层调用的 `#[tauri::command]` 函数与它们返回的 DTO。
//! U4 只需要“薄”实现：把查询委托给 [`Storage`] 的公开 API，
//! 返回 serde 可序列化的结构。真正的排序 / 检索细化留给后续单元：
//!
//! - `search_results` 的 FTS 排序细化 → U5
//! - `toggle_pin` 的置顶排序 → U7
//! - `update_entry` 的备注/标签编辑 → U6
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

/// 打开「系统设置 → 隐私与安全性 → 辅助功能」深链(U8 首启引导主按钮)。
///
/// 用 `open(1)` 打开 `x-apple.systempreferences:` URL scheme;非 macOS 为 no-op。
#[tauri::command]
pub fn open_accessibility_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        const URL: &str =
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
        std::process::Command::new("open")
            .arg(URL)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("仅支持 macOS".to_string())
    }
}

// ----------------------------------------------------------------
// 后续单元的类型化接缝（U6 / U7 / U8 / U10）
// ----------------------------------------------------------------

/// U8 完成：CGEvent 模拟 ⌘V 回贴。
///
/// 返回体 [`PasteResult`]:`status` 区分成功分支(`pasted` / `window_switched`
/// / `clipboard_only`),`message` 为对应固定 toast(成功时为空)。
/// 错误以 `Err(String)` 返回:`missing-accessibility` 让前端展示 Accessibility
/// 横幅(AE7),加密区条目返回 U9 seam 文案。
#[derive(Debug, Clone, Serialize)]
pub struct PasteResult {
    /// `pasted` | `window_switched` | `clipboard_only`。
    pub status: String,
    /// 需要展示的 toast(空串表示无需提示)。
    pub message: String,
}

/// 错误码:未授权。前端据此展示 Accessibility 横幅而非普通 toast(AE7)。
pub const ERR_MISSING_ACCESSIBILITY: &str = "missing-accessibility";

/// U8 完成:Accessibility 门控 + NSPasteboard 写入 + CGEvent ⌘V 回贴。
///
/// 流程:读条目 → [`crate::pasteback::perform_paste`](每调用重校验授权 →
/// vault 接缝 → 写剪贴板 → 隐藏面板 → 模拟 ⌘V → 失焦比对)。真实 CGEvent
/// 注入放在 `spawn_blocking`,避免 ~200ms 的按键 + 焦点等待阻塞 async executor。
#[tauri::command]
pub async fn paste_entry(
    id: i64,
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
) -> Result<PasteResult, String> {
    let entry = state
        .storage
        .get_by_id(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("entry {id} 不存在"))?;

    #[cfg(target_os = "macos")]
    {
        use crate::pasteback::{
            CGEVENT_FAIL_MSG, MacosBackend, PasteError, PasteOutcome, RealAccessibilityChecker,
            VAULT_SEAM_MSG, WINDOW_SWITCHED_MSG, perform_paste,
        };

        let prev = crate::hotkey::last_frontmost();
        let win = window.clone();
        let outcome = tauri::async_runtime::spawn_blocking(move || {
            let checker = RealAccessibilityChecker;
            let backend = MacosBackend::new(win);
            perform_paste(&checker, &backend, &entry, prev.as_deref())
        })
        .await
        .map_err(|e| e.to_string())?;

        match outcome {
            Ok(PasteOutcome::Pasted) => Ok(PasteResult {
                status: "pasted".to_string(),
                message: String::new(),
            }),
            Ok(PasteOutcome::WindowSwitched) => {
                // 面板已被隐藏;重新显示以便用户看到手动 ⌘V 的提示。
                let _ = window.show();
                let _ = window.set_focus();
                Ok(PasteResult {
                    status: "window_switched".to_string(),
                    message: WINDOW_SWITCHED_MSG.to_string(),
                })
            }
            Ok(PasteOutcome::ClipboardOnly) => {
                let _ = window.show();
                let _ = window.set_focus();
                Ok(PasteResult {
                    status: "clipboard_only".to_string(),
                    message: CGEVENT_FAIL_MSG.to_string(),
                })
            }
            Err(PasteError::MissingAccessibility) => Err(ERR_MISSING_ACCESSIBILITY.to_string()),
            Err(PasteError::VaultDecryptUnimplemented) => Err(VAULT_SEAM_MSG.to_string()),
            Err(PasteError::UnsupportedPlatform) => Err("回贴仅支持 macOS".to_string()),
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (&entry, &window);
        Err("回贴仅支持 macOS".to_string())
    }
}

/// U7 完成：切换 `id` 条目的置顶状态，返回刷新后的 [`EntryDto`]。
///
/// - 置顶写 `is_pinned = 1` + `pinned_at = now`；取消写 `is_pinned = 0` +
///   `pinned_at = NULL`（见 [`crate::pin`] 的 `pinned_at` 语义说明）。
/// - 排序（Pin 优先，R17）由 U5 的查询负责；前端拿到 DTO 后重新拉取当前
///   tab，使服务端的置顶优先顺序成为权威（AE8）。
#[tauri::command]
pub async fn toggle_pin(id: i64, state: State<'_, AppState>) -> Result<EntryDto, String> {
    let entry = crate::pin::toggle_pin(&state.storage, id)
        .await
        .map_err(|e| e.to_string())?;

    to_dtos(&state, vec![entry])
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| "无法构建 EntryDto".to_string())
}

/// U6 完成：写入备注 + 标签，返回刷新后的 [`EntryDto`] 供前端重绘该行。
///
/// - 标签为集合语义：先去除首尾空白、丢弃空串、去重（保序），再整组替换
///   该条目的既有标签（见 [`Storage::update_note_and_tags`]）。
/// - 备注 2000 字符上限（R8）在此处与存储层双重校验；超限返回 `Err`，
///   命中前端保存状态机的 `error` 分支。
/// - 备注 / 标签写入后由 U2 的触发器同步进 FTS，搜索立即命中（AE2）。
#[tauri::command]
pub async fn update_entry(
    id: i64,
    note: String,
    tags: Vec<String>,
    state: State<'_, AppState>,
) -> Result<EntryDto, String> {
    if note.chars().count() > crate::storage::MAX_NOTE_CHARS {
        return Err(format!(
            "备注超过 {} 字符上限",
            crate::storage::MAX_NOTE_CHARS
        ));
    }

    // 标签集合归一化：trim → 去空 → 去重（保序）。
    let mut seen = std::collections::HashSet::new();
    let tags: Vec<String> = tags
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .filter(|t| seen.insert(t.clone()))
        .collect();

    state
        .storage
        .update_note_and_tags(id, &note, &tags)
        .await
        .map_err(|e| e.to_string())?;

    let entry = state
        .storage
        .get_by_id(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("entry {id} 不存在"))?;

    to_dtos(&state, vec![entry])
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| "无法构建 EntryDto".to_string())
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
