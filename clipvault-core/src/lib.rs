//! ClipVault core library.
//!
//! Shared types and storage primitives used by the app binary and
//! future test binaries.

pub mod category;
pub mod crypto;
pub mod entry;
pub mod schema;

pub use category::{AutoCategory, classify};
pub use entry::Entry;
