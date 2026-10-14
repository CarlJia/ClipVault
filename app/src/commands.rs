//! Tauri IPC 命令层。
//!
//! 这里定义前端浮层调用的 `#[tauri::command]` 函数与它们返回的 DTO。
//! 命令保持"薄"实现：把查询委托给 [`Storage`] 的公开 API 与 `search` /
//! `pin` / `pasteback` / `privacy` 等模块，返回 serde 可序列化的结构。
//! 各命令具体落地的单元见其函数上的 "Ux 完成" 说明。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use clipvault_core::Entry;
use rusqlite::Result as SqliteResult;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::storage::Storage;
use crate::tray::{self, TrayState};

/// 进程内共享状态：存储句柄 + 与 watcher 共享的两个原子开关。
///
/// `guest_mode` / `skip_next` 与 [`crate::watcher::Watcher`] 持有的是**同一批**
/// `Arc<AtomicBool>`，因此这里的写入会被后台 watcher 立即看到（U10 完成持久化）。
///
/// `panel_opacity`（U12）只服务 UI 状态，watcher 不消费，但仍作为
/// `get_panel_opacity` / `set_panel_opacity` 命令的快速读写槽，避免每次查库。
#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<Storage>,
    pub guest_mode: Arc<AtomicBool>,
    pub skip_next: Arc<AtomicBool>,
    pub panel_opacity: Arc<AtomicU32>,
    pub keymap: Arc<RwLock<clipvault_core::keymap::HotkeyMap>>,
    /// U2：AppHandle，用于 apply_global_hotkey 和 hotkey::register。
    pub app: tauri::AppHandle,
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
    /// 加密区条目是否可解密（`false` → UI 显示「无法解密」徽标，U9）。
    pub is_decryptable: bool,
    pub created_at: i64,
    /// 多类型 plan U5：`text` / `image` / `file` / `rtf` / `color`。
    pub content_kind: String,
    /// 多类型 plan U5：二进制 MIME（如 `image/png`）。文本条目为 `None`。
    pub mime_type: Option<String>,
    /// 多类型 plan U5：图片缩略图 base64（64×64 PNG），其他 kind 为 `None`。
    pub thumb: Option<String>,
    /// 多类型 plan U5：结构化 metadata JSON（图片维度/文件名/RGBA 等）。
    pub metadata: Option<String>,
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
///
/// 用单条 `IN (...)` 分组查询替代逐 id 查询，避免 N+1；`ORDER BY entry_id, tag`
/// 保证每个 id 的标签仍按 tag 升序（与旧的逐 id `ORDER BY tag` 一致）。
async fn load_tags(state: &AppState, ids: &[i64]) -> SqliteResult<HashMap<i64, Vec<String>>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let ids = ids.to_vec();
    state
        .storage
        .with_conn(move |conn| {
            let placeholders = vec!["?"; ids.len()].join(", ");
            let sql = format!(
                "SELECT entry_id, tag FROM tags WHERE entry_id IN ({placeholders}) \
                 ORDER BY entry_id, tag"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut map: HashMap<i64, Vec<String>> = HashMap::new();
            let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (id, tag) = row?;
                map.entry(id).or_default().push(tag);
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
            // 解析 metadata_json 提取 thumb 字段;thumb 外的 width/height/paths/rgba
            // 字段已嵌入 metadata JSON,前端 parseMeta 自行解析。
            let thumb = e
                .metadata_json
                .as_deref()
                .and_then(extract_thumb_from_metadata);
            let is_vault = e.is_vault;
            EntryDto {
                id: e.id,
                // 加密区条目的 `content` 是密文，一律不下发前端：UI 只按元数据
                // （类别 / 标签 / 备注 / 时间）渲染，解密与回贴全程留在后端
                // （`paste_entry` 解密后写剪贴板）。
                content: if is_vault { String::new() } else { e.content },
                note: e.note,
                auto_category: e.auto_category,
                tags,
                is_pinned: e.is_pinned,
                pinned_at: e.pinned_at,
                is_vault,
                is_decryptable: e.is_decryptable,
                created_at: e.created_at,
                content_kind: e.content_kind,
                mime_type: e.mime_type,
                thumb,
                metadata: e.metadata_json,
            }
        })
        .collect())
}

/// 从 metadata_json 提取 thumb 字段(U3 写入的 base64 PNG)。
fn extract_thumb_from_metadata(s: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(s)
        .ok()
        .and_then(|v| {
            v.get("thumb")
                .and_then(|t| t.as_str().map(|s| s.to_string()))
        })
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

/// 平台标识（KTD16）。前端据此渲染修饰键提示（`⌘X` / `Ctrl+X`）与平台相关文案。
///
/// 刻意由后端给：WebView2 的 UA 串会随版本漂移，而 `std::env::consts::OS` 是事实。
/// 前端只在启动时取一次并缓存（`frontend/src/platform.ts`）。
#[derive(Debug, Clone, Serialize)]
pub struct PlatformInfo {
    /// `"macos"` | `"windows"` | `"linux"` | …（取值即 `std::env::consts::OS`）。
    pub os: String,
}

#[tauri::command]
pub fn platform_info() -> PlatformInfo {
    PlatformInfo {
        os: std::env::consts::OS.to_string(),
    }
}

/// 关闭（隐藏）主面板。前端 `Esc` 调用。
///
/// Esc 是「我不贴了」：隐藏面板的同时必须把键盘焦点交还唤出面板前的前台 App，
/// 否则用户回到编辑器 / 浏览器时输入框已失去光标，得再点一次才能继续打字或
/// 手动 ⌘V（`pasteback::hide_panel_and_restore_focus`）。
#[tauri::command]
pub fn hide_panel(window: tauri::WebviewWindow) -> Result<(), String> {
    crate::pasteback::hide_panel_and_restore_focus(&window)
}

/// 打开「系统设置 → 隐私与安全性 → 辅助功能」深链(U8 首启引导主按钮)。
///
/// 用 `open(1)` 打开 `x-apple.systempreferences:` URL scheme。
///
/// Windows 上不存在这个授权概念（`hotkey::check_accessibility` 恒为已授权），因此
/// 前端也不会显示引导横幅、不会调到这里；万一调到了，返回一句说清原因的错误，
/// 而不是一句在 Windows 上毫无意义的"仅支持 macOS"。
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
    #[cfg(target_os = "windows")]
    {
        Err("Windows 无需「辅助功能」授权，回贴无需任何系统授权".to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("当前平台不支持".to_string())
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
/// vault 接缝 → 写剪贴板 → 隐藏面板并**把激活权交还唤出面板前的前台 App**、
/// 有界等待它真的回到前台 → 模拟 ⌘V → 失焦比对)。真实 CGEvent 注入放在
/// `spawn_blocking`,避免按键注入 + 焦点归还的等待阻塞 async executor。
///
/// 失焦比对的目标取自 [`crate::pasteback::target_bundle_id`]——热键 / 托盘在
/// 显示面板**之前**记录的那个前台应用(见 `pasteback::capture_target_app`)。
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

    // macOS 与 Windows 的编排完全一致，只有后端类型不同（`RealPasteBackend` 是按平台
    // 选定的别名），所以这里是**一份**实现而不是两份。其余平台走下面的降级分支。
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        use crate::pasteback::{
            IS_MACOS, PasteError, PasteOutcome, RealAccessibilityChecker, RealPasteBackend,
            cgevent_fail_msg, no_accessibility_msg, perform_paste, vault_decrypt_fail_msg,
            window_switched_msg,
        };

        // 加密区条目：不可解密（如迁移机器后旧 key 丢失）直接拒绝，避免把密文贴出。
        if entry.is_vault && !entry.is_decryptable {
            return Err(vault_decrypt_fail_msg(IS_MACOS));
        }
        // vault 条目需要 nonce 才能解密（在锁外、spawn_blocking 前异步取出）。
        let nonce = if entry.is_vault {
            state
                .storage
                .get_vault_nonce(id)
                .await
                .map_err(|e| e.to_string())?
        } else {
            None
        };
        let payload_nonce = if entry.is_vault && entry.content_kind != "text" {
            // 多类型 plan U4/U6：binary vault 条目的 payload BLOB 由独立的
            // `payload_nonce` 列保护（与保护文本 content 的 `nonce` 列不是同一个值，
            // 见 `Storage::encrypt_into_vault`）。该值已随 `get_by_id` 的
            // `ENTRY_COLS` 读出，直接用 `entry.payload_nonce`，不要读 `nonce` 列。
            entry.payload_nonce.clone()
        } else {
            None
        };

        let prev = crate::pasteback::target_bundle_id();
        let win = window.clone();
        let outcome = tauri::async_runtime::spawn_blocking(move || {
            let checker = RealAccessibilityChecker;
            let backend = RealPasteBackend::new(win);
            // Keychain 数据密钥解密器；非 vault 条目不会触达它（不会命中 Keychain）。
            let decryptor = crate::crypto::default_vault_cipher();
            perform_paste(
                &checker,
                &backend,
                &entry,
                prev.as_deref(),
                nonce.as_deref(),
                payload_nonce.as_deref(),
                &decryptor,
            )
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
                    message: window_switched_msg(IS_MACOS),
                })
            }
            Ok(PasteOutcome::ClipboardOnly) => {
                let _ = window.show();
                let _ = window.set_focus();
                Ok(PasteResult {
                    status: "clipboard_only".to_string(),
                    message: cgevent_fail_msg(IS_MACOS),
                })
            }
            Ok(PasteOutcome::NoAccessibilityClipboardOnly) => {
                // 未授权:内容已在剪贴板,但没能自动回贴。重新显示面板,
                // 前端据 status 亮授权横幅并提示可手动 ⌘V。
                let _ = window.show();
                let _ = window.set_focus();
                Ok(PasteResult {
                    status: "no_accessibility".to_string(),
                    message: no_accessibility_msg(IS_MACOS),
                })
            }
            Err(PasteError::MissingAccessibility) => Err(ERR_MISSING_ACCESSIBILITY.to_string()),
            Err(PasteError::VaultDecryptFailed) => Err(vault_decrypt_fail_msg(IS_MACOS)),
            // `perform_paste` 从不构造这个变体（它是给"没有回贴能力的平台"准备的），
            // 走到这里说明平台 gating 出了错。文案不再写死 macOS —— 这句话在 Windows 上
            // 会是纯粹的误导。
            Err(PasteError::UnsupportedPlatform) => Err("当前平台不支持自动回贴".to_string()),
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (&entry, &window);
        Err("当前平台不支持自动回贴".to_string())
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

/// 设置访客模式开关（U10 / R10）。
///
/// 写路径经 [`crate::privacy::set_guest_mode`]：先更新与 watcher 共享的原子
/// （即时生效），再持久化到 `settings` 表（跨重启保留）。随后同步托盘图标状态
/// 与菜单栏 CheckMenuItem 的勾选态，使原子 / 库 / 托盘 / 前端四者一致。
#[tauri::command]
pub async fn set_guest_mode(
    enabled: bool,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    crate::privacy::set_guest_mode(&state.storage, &state.guest_mode, enabled)
        .await
        .map_err(|e| e.to_string())?;

    let next = if enabled {
        TrayState::Guest
    } else {
        TrayState::Monitoring
    };
    tray::set_state(&app, next);
    // 前端触发的切换也反映到托盘 CheckMenuItem 勾选态（若托盘已构建）。
    if let Some(item) = app.try_state::<tray::GuestMenuItem>() {
        let _ = item.0.set_checked(enabled);
    }
    Ok(())
}

/// 读取当前访客模式状态。以运行时共享原子为准（启动时已与 `settings` 表对齐）。
#[tauri::command]
pub fn get_guest_mode(state: State<'_, AppState>) -> bool {
    state.guest_mode.load(Ordering::SeqCst)
}

/// 「本次不记录」：置位一次性 skip flag，watcher 消费后复位（R11）。纯运行时，不持久化。
#[tauri::command]
pub fn set_skip_next(state: State<'_, AppState>) {
    crate::privacy::set_skip_next(&state.skip_next);
}

// ----------------------------------------------------------------
// 面板透明度（U12）
// ----------------------------------------------------------------

/// 读取当前面板透明度（0-100 整数百分比）。以运行时共享原子为准
/// （启动时已与 `settings` 表对齐）。
#[tauri::command]
pub fn get_panel_opacity(state: State<'_, AppState>) -> u32 {
    state.panel_opacity.load(Ordering::SeqCst)
}

/// 设置面板透明度（0-100 整数百分比）。
///
/// 写路径经 [`crate::privacy::set_panel_opacity`]：clamp 到
/// [`crate::privacy::MIN_PANEL_OPACITY`]..=[`crate::privacy::MAX_PANEL_OPACITY`]
/// 区间 → 更新共享原子（即时生效）→ 持久化到 `settings` 表（跨重启保留）。
/// 返回 clamp 后的最终值，便于前端回填 slider 防漂移。
#[tauri::command]
pub async fn set_panel_opacity(opacity: u32, state: State<'_, AppState>) -> Result<u32, String> {
    crate::privacy::set_panel_opacity(&state.storage, &state.panel_opacity, opacity)
        .await
        .map_err(|e| e.to_string())
}

// ----------------------------------------------------------------
// HotkeyMap（U1）
// ----------------------------------------------------------------

/// 读取当前快捷键映射（JSON 字符串）。以运行时共享 slot 为准。
#[tauri::command]
pub fn get_hotkeys(state: State<'_, AppState>) -> String {
    let map = state.keymap.read().unwrap();
    clipvault_core::keymap::serialize(&map)
}

/// 设置某个 action 的快捷键。
///
/// 写路径：
/// 1. 把 `serde_json::Value` 解析为 `KeyCombo`
/// 2. 验证：非空 combo（modifier-only 拒绝）、key 存在
/// 3. 更新 `HotkeyMap` 中对应 action
/// 4. `persist_hotkeys` 持久化到 `settings` 表
/// 5. `apply_hotkeys` 更新 in-memory slot
///
/// U2 会在这里加入 `apply_global_hotkey` 调用以完成 live re-register。
#[tauri::command]
pub async fn set_hotkey(
    action: String,
    combo: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<(), String> {
    use clipvault_core::keymap::{CharKind, HotkeyAction, KeyCombo, Mods};

    // 1. 把 action string → enum
    let action =
        HotkeyAction::from_json_str(&action).ok_or_else(|| format!("unknown action '{action}'"))?;

    // 2. 从 JSON Value 提取 mods + key
    let combo_obj = combo.as_object().ok_or("combo must be a JSON object")?;

    // mods
    let mods_array = combo_obj
        .get("mods")
        .and_then(|v| v.as_array())
        .ok_or("combo.mods must be an array")?;
    let mut mods = Mods::empty();
    for m in mods_array {
        let s = m.as_str().ok_or("mods elements must be strings")?;
        match s {
            "meta" => mods |= Mods::META,
            "shift" => mods |= Mods::SHIFT,
            "alt" => mods |= Mods::ALT,
            "ctrl" => mods |= Mods::CTRL,
            _ => return Err(format!("unknown modifier '{s}'")),
        }
    }

    // key — 拒绝 null（modifier-only）
    let key_value = combo_obj.get("key").ok_or("combo.key is required")?;
    if key_value.is_null() {
        return Err("modifier-only combos are not allowed".to_string());
    }
    let key_str = key_value.as_str().ok_or("combo.key must be a string")?;
    let key =
        CharKind::from_json_str(key_str).ok_or_else(|| format!("unrecognised key '{key_str}'"))?;

    let new_combo = KeyCombo { mods, key };

    // 3. 更新 map
    let map = {
        let mut map = state.keymap.read().unwrap().clone();
        map.actions.insert(action, new_combo);
        map
    };

    // 4. 持久化 + 更新 slot
    crate::keymap::set_hotkey(&state.storage, &state.keymap, &map)
        .await
        .map_err(|e| e.to_string())?;

    // 5. U2：live re-register 全局热键（若改的是 OpenPanel）
    if action == HotkeyAction::OpenPanel {
        #[allow(clippy::collapsible_if)]
        if let Err(e) = crate::keymap::apply_global_hotkey(&state.app, &map) {
            // 注册失败不阻止命令返回（已在内存中更新）；冲突由前端 toast 提示。
            // settings 表已在 step 4 持久化，故本次绑定在库中可见。
            // 用户需再改一次才能重试，或重启后回落。
            eprintln!("[keymap] apply_global_hotkey 失败：{e}");
        }
    }

    Ok(())
}

// ----------------------------------------------------------------
// 加密区 / Vault（U9 / R12）
// ----------------------------------------------------------------

/// 系统凭据库中的数据密钥当前是否可用（读取或首次创建成功）。
///
/// 前端据此决定是否显示「⚠️ Keychain 不可用，加密区不可用」横幅（plan U9）。
/// 首次调用可能触发密钥创建；成功后进程内缓存。
///
/// 命令名保留 `keychain_available`（前端已在用），但在 Windows 上它的语义是
/// **Windows 凭据管理器是否可用** —— 二者是同一角色的两个平台实现
/// （见 `crypto` 模块的 `CredentialKeyProvider`）。
#[tauri::command]
pub fn keychain_available() -> bool {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        crate::crypto::system_key_available()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

/// 把一条明文条目移入加密区（AES-GCM 加密 + 系统凭据库托管的密钥）。
///
/// 密钥不可用时返回 `Err`，条目保持明文未加密（plan U9：绝不静默写明文进
/// vault）；前端据此弹 toast + 展示横幅。
#[tauri::command]
pub async fn move_to_vault(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let cipher = crate::crypto::default_vault_cipher();
        state
            .storage
            .move_to_vault(id, &cipher)
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (id, state);
        Err("加密区仅支持 macOS".to_string())
    }
}

/// 列出加密区条目（元数据；`content` 已由 [`to_dtos`] 清空，不外泄密文）。
/// 供「加密区管理」模态渲染：条目同样可 ↩ 回贴（走 `paste_entry` 解密）、⌘E 备注。
#[tauri::command]
pub async fn list_vault(state: State<'_, AppState>) -> Result<Vec<EntryDto>, String> {
    let entries = state
        .storage
        .list_vault_entries()
        .await
        .map_err(|e| e.to_string())?;
    to_dtos(&state, entries).await
}

/// 批量删除不可解密的加密区条目（`is_decryptable=0`）。返回删除条数（plan U9）。
#[tauri::command]
pub async fn clear_undecryptable(state: State<'_, AppState>) -> Result<usize, String> {
    state
        .storage
        .clear_undecryptable()
        .await
        .map_err(|e| e.to_string())
}

// ----------------------------------------------------------------
// URL ⌘O 打开（U7 / R20 / F7；Windows 侧 U5）
// ----------------------------------------------------------------

/// 校验一条条目内容能否作为 URL 打开，返回去掉首尾空白后的 URL。
///
/// 只接受 `http://` 与 `https://`：拒绝 `file://`、`javascript:`、`data:` 等潜在攻击面
/// （clipboard hijacking）。抽成纯函数，两端共用同一条判据，也可单测。
pub(crate) fn openable_url(raw: &str) -> Result<&str, String> {
    let url = raw.trim();
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(url)
    } else {
        Err(format!(
            "URL scheme 必须为 http:// 或 https://（实际: {url}）"
        ))
    }
}

/// 多类型 plan U7：在系统默认浏览器打开 URL 类别条目。
///
/// 仅接受 `http://` 与 `https://` scheme；非 URL 类别条目直接返回错误。
///
/// 打开失败只返回错误（前端 toast 提示），**不**代客户写剪贴板：用户想复制这条内容的话，
/// 在面板上对它按回车即可 —— 回贴链路本来就会把内容写进剪贴板，这里再写一次是多余的。
/// （本函数的文档此前声称失败时会"把 content 写入 NSPasteboard 并返回 clipboard_only"，
/// 那从未实现过，已改正。）
#[tauri::command]
pub fn open_url(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    // 同步读取（commands 在 Tauri 主线程跑;MVP 同模式入库读取已沿用）。
    let entry = tauri::async_runtime::block_on(async { state.storage.get_by_id(id).await })
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("entry {id} 不存在"))?;

    if entry.auto_category != "URL" {
        return Err(format!(
            "非 URL 条目不支持打开（auto_category={}）",
            entry.auto_category
        ));
    }

    let url = openable_url(&entry.content)?;

    #[cfg(target_os = "macos")]
    {
        // `use` 放在分支内部：放在函数顶部的话，Windows 构建会因"未使用的导入"
        // 在 `-D warnings` 下直接编译失败。
        use std::process::Command;
        Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("打开失败：{e}"))
    }

    #[cfg(target_os = "windows")]
    {
        open_in_default_browser(url)
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = url;
        Err("当前平台不支持打开 URL".to_string())
    }
}

/// Windows：`ShellExecuteW` 把 URL 交给系统默认处理器（浏览器 / 关联程序）。
///
/// **不用 `cmd /C start`**：那要把 URL 拼进命令行，`&`、`^`、`|`、`%` 都得转义，
/// 漏一个就是命令注入（而这里的 URL 来自剪贴板，正是不可信输入）。`ShellExecuteW`
/// 直接把 URL 当参数交给系统，没有这层转义面。
///
/// 用返回值判断成败：这套 API 的 `HINSTANCE` 实际被当作整型用，**> 32 才是成功**，
/// ≤ 32 是错误码（0 表示内存不足、2 表示文件不存在等）。只看"没 panic"会把打开失败
/// 静默当成成功。
#[cfg(target_os = "windows")]
fn open_in_default_browser(url: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;

    let operation = crate::winclip::utf16_nul("open");
    let target = crate::winclip::utf16_nul(url);
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = result.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!("打开失败（ShellExecute 返回 {code}）"))
    }
}
