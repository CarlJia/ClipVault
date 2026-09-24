//! ClipVault app library.
//!
//! U1 scaffold: placeholder.
//! U2 added storage.
//! U3 added the pasteboard watcher + auto-category classification.
//! U4 adds the frontend shell wiring: IPC commands, the menu-bar tray,
//! and the Carbon global hotkey.

pub mod classify;
pub mod commands;
pub mod hotkey;
pub mod search;
pub mod storage;
pub mod tray;
pub mod watcher;

#[cfg(test)]
mod search_test;
#[cfg(test)]
mod watcher_test;
