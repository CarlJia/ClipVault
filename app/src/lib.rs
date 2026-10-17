//! ClipVault app library：Tauri 主 binary 的模块根。
//!
//! 模块按职责分组：
//! - 采集 / 分类：`capture`、`watcher`、`winclip`、`classify`
//! - 存储 / 检索：`storage`、`pin`、`search`、`thumbnail`
//! - 加密 / 隐私：`crypto`、`privacy`
//! - 交互 / 回贴：`pasteback`、`hotkey`、`keymap`
//! - 前端桥接：`commands`、`tray`、`panel`、`preview`
//! - 诊断 / Windows 平台：`log`、`single_instance`

pub mod capture;
pub mod classify;
pub mod commands;
pub mod crypto;
pub mod hotkey;
pub mod keymap;
pub mod log;
pub mod panel;
pub mod pasteback;
pub mod pin;
pub mod preview;
pub mod privacy;
pub mod search;
pub mod single_instance;
pub mod storage;
pub mod thumbnail;
pub mod tray;
pub mod watcher;
pub mod winclip;

#[cfg(test)]
mod commands_test;
#[cfg(test)]
mod crypto_test;
#[cfg(test)]
mod log_test;
#[cfg(test)]
mod notes_test;
#[cfg(test)]
mod pasteback_test;
#[cfg(test)]
mod pin_test;
#[cfg(test)]
mod preview_test;
#[cfg(test)]
mod privacy_test;
#[cfg(test)]
mod search_test;
#[cfg(test)]
mod watcher_test;
#[cfg(test)]
mod winclip_test;
