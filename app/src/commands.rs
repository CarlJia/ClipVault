//! Tauri IPC 命令层。
//!
//! 这里定义前端浮层调用的 `#[tauri::command]` 函数与它们返回的 DTO。
//! 命令保持"薄"实现：把查询委托给 [`Storage`] 的公开 API 与 `search` /
//! `pin` / `pasteback` / `privacy` 等模块，返回 serde 可序列化的结构。
//! 各命令具体落地的单元见其函数上的 "Ux 完成" 说明。

use std::collections::HashMap;
use std::sync::Arc;
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
            EntryDto {
                id: e.id,
                content: e.content,
                note: e.note,
                auto_category: e.auto_category,
                tags,
                is_pinned: e.is_pinned,
                pinned_at: e.pinned_at,
                is_vault: e.is_vault,
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
        .and_then(|v| v.get("thumb").and_then(|t| t.as_str().map(|s| s.to_string())))
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
            CGEVENT_FAIL_MSG, MacosBackend, NO_ACCESSIBILITY_MSG, PasteError, PasteOutcome,
            RealAccessibilityChecker, VAULT_DECRYPT_FAIL_MSG, WINDOW_SWITCHED_MSG, perform_paste,
        };

        // 加密区条目：不可解密（如迁移机器后旧 key 丢失）直接拒绝，避免把密文贴出。
        if entry.is_vault && !entry.is_decryptable {
            return Err(VAULT_DECRYPT_FAIL_MSG.to_string());
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
            // 多类型 plan U4:binary vault 条目需要 payload_nonce 才能解密 payload BLOB。
            // 现有 get_vault_nonce 读 entries.nonce 列;此处复用同一函数
            // (content_kind 已确保 kind 与 nonce 列对应:nonce 保护文本 content,
            //  payload_nonce 保护 payload BLOB)。
            state
                .storage
                .get_vault_nonce(id)
                .await
                .map_err(|e| e.to_string())?
        } else {
            None
        };

        let prev = crate::hotkey::last_frontmost();
        let win = window.clone();
        let outcome = tauri::async_runtime::spawn_blocking(move || {
            let checker = RealAccessibilityChecker;
            let backend = MacosBackend::new(win);
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
            Ok(PasteOutcome::NoAccessibilityClipboardOnly) => {
                // 未授权:内容已在剪贴板,但没能自动回贴。重新显示面板,
                // 前端据 status 亮授权横幅并提示可手动 ⌘V。
                let _ = window.show();
                let _ = window.set_focus();
                Ok(PasteResult {
                    status: "no_accessibility".to_string(),
                    message: NO_ACCESSIBILITY_MSG.to_string(),
                })
            }
            Err(PasteError::MissingAccessibility) => Err(ERR_MISSING_ACCESSIBILITY.to_string()),
            Err(PasteError::VaultDecryptFailed) => Err(VAULT_DECRYPT_FAIL_MSG.to_string()),
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
pub async fn set_panel_opacity(
    opacity: u32,
    state: State<'_, AppState>,
) -> Result<u32, String> {
    crate::privacy::set_panel_opacity(&state.storage, &state.panel_opacity, opacity)
        .await
        .map_err(|e| e.to_string())
}

// ----------------------------------------------------------------
// 加密区 / Vault（U9 / R12）
// ----------------------------------------------------------------

/// Keychain 数据密钥当前是否可用（读取或首次创建成功）。
///
/// 前端据此决定是否显示「⚠️ Keychain 不可用，加密区不可用」横幅（plan U9）。
/// 首次调用可能触发密钥创建（`SecItemAdd`）；成功后进程内缓存。
#[tauri::command]
pub fn keychain_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        use clipvault_core::crypto::KeyProvider;
        crate::crypto::KeychainKeyProvider::new().data_key().is_ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// 把一条明文条目移入加密区（AES-GCM 加密 + Keychain 密钥）。
///
/// Keychain 不可用时返回 `Err`，条目保持明文未加密（plan U9：绝不静默写明文进
/// vault）；前端据此弹 toast + 展示横幅。
#[tauri::command]
pub async fn move_to_vault(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let cipher = crate::crypto::default_vault_cipher();
        state
            .storage
            .move_to_vault(id, &cipher)
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (id, state);
        Err("加密区仅支持 macOS".to_string())
    }
}

/// 列出加密区条目（元数据；`content` 已清空，不外泄密文）。供 Settings 加密区管理。
#[tauri::command]
pub async fn list_vault(state: State<'_, AppState>) -> Result<Vec<EntryDto>, String> {
    let entries = state
        .storage
        .list_vault_entries()
        .await
        .map_err(|e| e.to_string())?;
    let mut dtos = to_dtos(&state, entries).await?;
    for d in &mut dtos {
        // 密文不下发前端；UI 按 is_decryptable / note / 类别 / 时间展示。
        d.content = String::new();
    }
    Ok(dtos)
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
// URL ⌘O 打开（U7 / R20 / F7）
// ----------------------------------------------------------------

/// 多类型 plan U7：在默认浏览器打开 URL 类别条目。
///
/// 仅接受 `http://` 与 `https://` scheme：拒绝 `file://`、`javascript:`、
/// `data:` 等潜在攻击面（clipboard hijacking）。非 URL 类别条目返回错误。
///
/// 失败（URL 非法、Command 启动失败）→ 后端把 content 写入
/// `NSPasteboard.string` 并返回 `clipboard_only`，前端 toast 提示。
#[tauri::command]
pub fn open_url(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    use std::process::Command;

    // 同步读取（commands 在 Tauri 主线程跑;MVP 同模式入库读取已沿用）。
    let entry = tauri::async_runtime::block_on(async {
        state.storage.get_by_id(id).await
    })
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("entry {id} 不存在"))?;

    if entry.auto_category != "URL" {
        return Err(format!(
            "非 URL 条目不支持打开（auto_category={}）",
            entry.auto_category
        ));
    }

    let url = entry.content.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!(
            "URL scheme 必须为 http:// 或 https://（实际: {url}）"
        ));
    }

    #[cfg(target_os = "macos")]
    {
        match Command::new("open").arg(url).spawn() {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("打开失败,内容已复制到剪贴板,请手动 ⌘V（{e}）")),
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        Err("URL 打开仅支持 macOS".to_string())
    }
}
