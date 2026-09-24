//! ClipVault app library.
//!
//! U1 scaffold: placeholder.
//! U2 added storage.
//! U3 adds the pasteboard watcher + auto-category classification.
//! U4 will add tray + global hotkey registration.

pub mod classify;
pub mod storage;
pub mod watcher;

#[cfg(test)]
mod watcher_test;
