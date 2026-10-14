//! U3 分类 re-export。
//!
//! 让 app 内部（watcher 等）通过 `crate::classify` 使用 clipvault-core 的
//! 自动分类启发式，无需散落地依赖 `clipvault_core::category` 路径。

pub use clipvault_core::category::{AutoCategory, classify};
