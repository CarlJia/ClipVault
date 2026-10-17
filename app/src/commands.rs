//! Tauri IPC 命令层。
//!
//! 这里定义前端浮层调用的 `#[tauri::command]` 函数与它们返回的 DTO。
//! 命令保持"薄"实现：把查询委托给 [`Storage`] 的公开 API 与 `search` /
//! `pin` / `pasteback` / `privacy` 等模块，返回 serde 可序列化的结构。
//! 各命令具体落地的单元见其函数上的 "Ux 完成" 说明。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use clipvault_core::Entry;
use rusqlite::Result as SqliteResult;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::preview::PreviewDto;
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
    /// U2：日志级别原子,`set_log_level` / `get_log_level` 路径写入与读取。
    pub log_level: Arc<AtomicU8>,
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
    // 预览栏开着就先收回 560×420 再隐藏（R12）：否则窗口以 960 宽留在隐藏态，
    // 下一次唤出会与按 normal 算出来的落位对不上。
    crate::panel::restore_normal(&window);
    let result = crate::pasteback::hide_panel_and_restore_focus(&window);
    // 面板收起 = 录制态结束（快捷键 plan KTD9）：失焦自动收起那条走 `panel::handle_window_event`，
    // Esc / 前端收起这条走这里 —— 两条都得让全局热键归位，否则「录制中把面板收起来」
    // 会把 ⌥⌘V 留在缺席状态。
    crate::keymap::restore_global_hotkey_after_leaving_panel(window.app_handle());
    result
}

/// 切换面板尺寸档位（列表旁预览栏 plan U2 / R1 / R4）。
///
/// `mode` 取 `"normal"`（560×420，列表独占）或 `"expanded"`（960×420，右侧长出
/// 预览栏）。落在 `panel` 模块：尺寸 / 落位 / 收回时的归位都归它，命令层只做字符串
/// 到枚举的解析（非法值直接 `Err`，不静默当 normal —— 那样窗口会莫名缩一下）。
#[tauri::command]
pub fn set_panel_mode(window: tauri::WebviewWindow, mode: String) -> Result<(), String> {
    let mode = crate::panel::PanelMode::parse(&mode)
        .ok_or_else(|| format!("未知面板模式：{mode}（应为 normal / expanded）"))?;
    crate::panel::set_mode(&window, mode);
    Ok(())
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
                //
                // **这里刻意不重新定位面板（KTD7 / R5）**：本分支不是「唤出」——回贴后
                // 目标 App 已回到前台，「焦点屏」会变成目标 App 所在屏，若在此重定位
                // 面板会在用户眼前跨屏跳动。面板留在唤出时的位置。
                // （唤出时的定位与 `tauri.conf.json` 的 `center:true` 的关系：`center`
                // 只在窗口创建时生效一次，每次都定位到焦点屏的重定位会覆盖它，故
                // `center:true` 原样保留、不删。）
                let _ = window.show();
                let _ = window.set_focus();
                Ok(PasteResult {
                    status: "window_switched".to_string(),
                    message: window_switched_msg(IS_MACOS),
                })
            }
            Ok(PasteOutcome::ClipboardOnly) => {
                // 同 WindowSwitched：降级提示不是唤出，不重定位（KTD7 / R5）。
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
                // 同 WindowSwitched：降级提示不是唤出，不重定位（KTD7 / R5）。
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

/// 删除单条记录（主列表与加密区管理共用）。
///
/// 只按 `id` 删，不区分是否加密区条目 —— 加密区条目的密文行同样直接删掉，
/// 不需要先解密（密文与 nonce 随行一起消失）。
///
/// 返回实际删掉的行数：`0` 表示这个 id 已经不存在（例如同一条被删了两次）。
/// 前端据此区分「已删除」与「早就不在了」，不要把它当成恒真的成功信号。
#[tauri::command]
pub async fn delete_entry(id: i64, state: State<'_, AppState>) -> Result<usize, String> {
    state
        .storage
        .delete_entry(id)
        .await
        .map_err(|e| e.to_string())
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
// 诊断日志（U2 / U4）
// ----------------------------------------------------------------

/// 当前日志级别字符串,以运行时共享原子为准。
#[tauri::command]
pub fn get_log_level(state: State<'_, AppState>) -> String {
    crate::log::Level::from_u8(state.log_level.load(Ordering::SeqCst))
        .as_str()
        .to_string()
}

/// 设置日志级别。接受 `\"info\"` / `\"verbose\"`,其它值回落 `info`。
/// 写路径经 [`crate::log::apply_log_level`]:clamp + 原子更新 + 持久化。
#[tauri::command]
pub async fn set_log_level(level: String, state: State<'_, AppState>) -> Result<String, String> {
    let parsed = crate::log::parse_level(Some(&level));
    crate::log::apply_log_level(&state.storage, &state.log_level, parsed)
        .await
        .map_err(|e| e.to_string())?;
    Ok(parsed.as_str().to_string())
}

/// 日志视图元信息 + 尾部内容。`lines` 字段已按 R13 cap(64 KiB / 400 行)
/// 截断,前端按 R14 再 cap 一道「最多渲染 200 行」。
#[tauri::command]
pub async fn read_log(state: State<'_, AppState>) -> Result<crate::log::LogInfo, String> {
    let path = crate::log::default_log_path();
    let dir = path.parent().unwrap_or(std::path::Path::new(""));
    let lines = crate::log::read_tail(dir, 64 * 1024, 400).map_err(|e| e.to_string())?;
    let mut info = crate::log::read_info(&path, &state.log_level);
    info.lines = lines;
    Ok(info)
}

/// 清空日志(R12):截断 `clipvault.log` 到 0 + 删除 `clipvault.log.1`。
/// 实际工作由 `log::clear` 走 `LOG_STATE` 锁内路径完成,与 `write_line` 互斥。
#[tauri::command]
pub async fn clear_log() -> Result<(), String> {
    let path = crate::log::default_log_path();
    let dir = path.parent().unwrap_or(std::path::Path::new(""));
    crate::log::clear(dir).map_err(|e| e.to_string())?;
    ::log::info!(target: "main", "用户清空诊断日志");
    Ok(())
}

/// 把日志文件路径写进系统剪贴板(U4 补充:面板里的路径被 `max-width` 截断,只能靠点击拿全)。
///
/// 复用**回贴写剪贴板**的同一条平台路径([`crate::pasteback::copy_text_to_clipboard`]),
/// 不走 WebView 的 `navigator.clipboard`:仓库未装 `tauri-plugin-clipboard-manager`
/// (grep `Cargo.toml` / `app/Cargo.toml` / `tauri.conf.json` 均无),裸 async
/// clipboard API 在 WKWebView 下的可用性不可依赖。
///
/// 两条**已知边界**,改这里的人需要知道:
/// - 平台侧写入失败(Windows 剪贴板被别的进程占用、macOS `setStringForType` 返回 false)
///   只在日志里留痕,`Err` 不会回传 —— 前端 toast 可能报「已复制」而剪贴板没变。
///   要真正区分,得让 `PasteBackend::write_clipboard` 可失败,属独立改动。
/// - 写完后 watcher 会把这条路径当成一次新复制入库(剪贴板历史多一条),与用户自己
///   复制任何内容无异;目前没有自写抑制。
#[tauri::command]
pub async fn copy_log_path(window: tauri::WebviewWindow) -> Result<(), String> {
    let path = crate::log::default_log_path()
        .to_string_lossy()
        .into_owned();

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        use crate::pasteback::RealPasteBackend;

        // 平台调用放 `spawn_blocking`(与 `paste_entry` 同一理由):Windows 侧的写剪贴板
        // 会在剪贴板被别的进程占用时重试,不该堵住 async 线程。
        tauri::async_runtime::spawn_blocking(move || {
            let backend = RealPasteBackend::new(window);
            crate::pasteback::copy_text_to_clipboard(&backend, &path);
        })
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (&window, &path);
        Err("当前平台不支持复制到剪贴板".to_string())
    }
}

// ----------------------------------------------------------------
// 「查看日志…」(U5 / R10)
// ----------------------------------------------------------------

/// 在系统文件管理器中显示日志文件(macOS Finder / Windows 资源管理器)。
///
/// 走 KTD6 的纯函数 `reveal_argv` 拿参数数组,`std::process::Command::new(prog).args(args).spawn()`
/// 启动对应命令。错误以 `Err(String)` 返回,UI 端只发 log 警告。
pub fn reveal_log(_app: &tauri::AppHandle) -> Result<(), String> {
    use std::process::Command;
    let path = crate::log::default_log_path();
    let path_str = path.to_string_lossy().into_owned();
    let (prog, args) = crate::log::reveal_argv(cfg!(target_os = "macos"), &path_str);
    // `Command` 在 Windows 上需要 std 句柄,这里允许 inherit。
    Command::new(&prog)
        .args(&args)
        .spawn()
        .map(|_child| ())
        .map_err(|e| format!("reveal_log: spawn `{prog}` 失败: {e}"))
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
/// 写路径（**注册先于落库**，U2 code review P0/P1 修复）：
/// 1. `parse_set_hotkey_args`：action 字符串 + JSON combo → `(action, combo)`；
///    任一非法值在此直接返回 `Err`，不触碰状态。
/// 2. 构造新 map（仅内存副本，未生效）：
///    - `force = false`：该 combo 已被另一条 action 占用 → `Err`，不触碰状态。
///    - `force = true`（前端「强行换绑」确认后的路径）：`HotkeyMap::claim` 把 combo
///      归给本 action，并把原来占着它的 action 解绑（写成「未绑定」）。
/// 3. 若这次改动动了全局热键（`OpenPanel` 的组合变了，或它被解绑）：先
///    `apply_global_hotkey` 落到 OS。注册失败（冲突 / 不支持的键）→ 直接返回 `Err`，
///    **库与内存 slot 均未改动**，用户的旧热键继续可用（这正是 R4 要求的「保持原绑定」）。
/// 4. 注册成功（或改的是面板内快捷键）→ 持久化 + 更新内存 slot。
///
/// `force` 是用户意图的传声筒，不是安全边界：前端已经用弹框让用户确认过「另一条会
/// 变成未绑定」。它只影响「被占用时要拒绝还是抢」，不改变其余校验。
#[tauri::command]
pub async fn set_hotkey(
    action: String,
    combo: serde_json::Value,
    force: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // 1. 解析入参（纯函数，可单测）。
    let (action, new_combo) = crate::keymap::parse_set_hotkey_args(&action, &combo)?;

    // 2. 构造新 map（内存副本）。
    let old_map = state.keymap.read().unwrap().clone();
    let new_map = {
        let mut map = old_map.clone();
        if force {
            // 强行换绑：占用者变成未绑定，组合键在表里仍然唯一。
            map.claim(action, new_combo);
        } else {
            // 表内冲突：这个组合已经被另一条 action 占着 → 直接返回，不动任何状态。
            //
            // 两条 action 共用同一组合时，前端 `matches()` 只会派发给排序靠前的那条，另一条
            // 永远不触发 —— 而设置里那一行仍然显示着这个组合。前端录制路径已先拦一次（能说出
            // 中文标签），这里是写入口的兜底。放在 OS 注册之前：被拒的请求连注册都不该碰。
            if let Some(msg) = crate::keymap::combo_conflict_error(&map, action, new_combo) {
                return Err(msg);
            }
            map.actions.insert(action, Some(new_combo));
        }
        map
    };

    // 3. 全局热键：先注册（或被解绑时摘掉），失败即返回，不改任何状态。
    let touches_global = crate::keymap::global_hotkey_changed(&old_map, &new_map);
    if touches_global {
        crate::keymap::apply_global_hotkey(&state.app, &new_map).map_err(|e| e.to_string())?;
    }

    // 4. 持久化 + 更新内存 slot。
    //
    // 若持久化失败而全局热键已在 step 3 落过 OS，则尽力把 OS 侧退回旧 map，
    // 避免「热键生效了但库里还是旧值」的不一致。
    if let Err(e) = crate::keymap::set_hotkey(&state.storage, &state.keymap, &new_map).await {
        if touches_global {
            let _ = crate::keymap::apply_global_hotkey(&state.app, &old_map);
        }
        return Err(e.to_string());
    }

    Ok(())
}

/// 录制态开关（快捷键 plan KTD9）：设置面板等用户按键期间，让本进程**让出**全局热键。
///
/// 为什么必须让（本次 bug 的根因）：`open_panel` 的 ⌥⌘V 是用 `RegisterEventHotKey`
/// 注册到 OS 的**系统级**热键，HIToolbox 在按键进入前台 App 的派发链之前就把它截走 ——
/// 「点重绑 → 按 ⌥⌘V」时 WebView 里的录制监听器收不到任何 keydown，那一行永远停在
/// 「请按新键…」，什么也不会被写（Windows 同理：`RegisterHotKey` 只投 `WM_HOTKEY`，
/// 按键同样进不了 WebView）。
///
/// 让出是**有界**的：本命令 `active=false`、面板隐藏（`panel::handle_window_event` 与
/// 本文件的 `hide_panel`）都会把热键装回来。装不回去时不静默 —— 返回 `Err` 让前端
/// toast 出来，用户至少知道 ⌥⌘V 为什么失灵。
///
/// 只做「这一次切换真正该做的事」（判断在 `keymap::recording_transition` 里）：启动路径
/// 那次 `active=false`、以及面板收起时本命令没被调过的情况，都落到 `Nothing` ——
/// 既是空转，也免得把「组合被别的 App 占着」的注册失败报成「恢复失败」。
///
/// 与 `set_hotkey` 的先后：前端在调 `active=false` 之前先 `await setHotkey(...)`，所以
/// 这里读到的 `state.keymap` 已经是新绑定，装回去的就是用户刚录的那个组合。
#[tauri::command]
pub async fn set_hotkey_recording(active: bool, state: State<'_, AppState>) -> Result<(), String> {
    use crate::keymap::HotkeySync;

    match crate::keymap::set_recording(active) {
        HotkeySync::Nothing => Ok(()),
        HotkeySync::Release => {
            crate::keymap::release_global_hotkey();
            Ok(())
        }
        HotkeySync::Restore => {
            let map = state.keymap.read().unwrap().clone();
            crate::keymap::apply_global_hotkey(&state.app, &map).map_err(|e| e.to_string())
        }
    }
}

/// 把全部 14 个热键重置为出厂默认(R4 之外的「恢复原状」入口)。
///
/// 流程与 `set_hotkey` 同形:
/// 1. 内存构造一份 default_map()（同 `parse_persist_hotkeys` 的回落路径）。
/// 2. 先在 OS 侧重新注册全局热键(失败即返回,不动持久化)。
/// 3. 持久化到 `settings` 表(失败则把 OS 侧退回旧 combo,与 `set_hotkey` 的回退一致)。
/// 4. 写成功后再更新 in-memory slot。
#[tauri::command]
pub async fn reset_hotkeys(state: State<'_, AppState>) -> Result<(), String> {
    use clipvault_core::keymap::HotkeyMap;

    let default_map = HotkeyMap::default_map();
    let old_map = state.keymap.read().unwrap().clone();

    // 1. 先在 OS 侧注册新的全局热键;失败立即返回,不动任何状态。
    crate::keymap::apply_global_hotkey(&state.app, &default_map)
        .map_err(|e| format!("reset_global_hotkey 失败: {e}"))?;

    // 2. 持久化;若失败把 OS 侧退回旧 combo,避免「OS 已切换但库未变」的不一致。
    if let Err(e) = crate::keymap::set_hotkey(&state.storage, &state.keymap, &default_map).await {
        let _ = crate::keymap::apply_global_hotkey(&state.app, &old_map);
        return Err(e.to_string());
    }
    Ok(())
}

// ----------------------------------------------------------------
// 列表旁预览栏（U1 / R5 / R6 / R7）
// ----------------------------------------------------------------

/// 取一条条目的预览数据（图片按预览尺寸缩放 / 文本给全文 / 其它给 `none`）。
///
/// 错误语义（R5）：id 不存在、图片解不开 → `Err(String)`；「条目在、但这个类型
/// 没有预览」→ `Ok(kind = "none")`。两种情形前端表现不同（前者 toast，后者显示提示）。
#[tauri::command]
pub async fn get_entry_preview(id: i64, state: State<'_, AppState>) -> Result<PreviewDto, String> {
    // 解密器按平台给：macOS 是 Keychain、Windows 是凭据管理器（`crypto` 模块的两个
    // `default_vault_cipher`），其余平台没有凭据库。
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let decryptor = crate::crypto::default_vault_cipher();
        crate::preview::get_entry_preview_inner(&state.storage, id, &decryptor).await
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        crate::preview::get_entry_preview_inner(&state.storage, id, &crate::preview::NoDecryptor)
            .await
    }
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
