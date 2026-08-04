#![deny(unsafe_code)]
//! # Memory — conversation memory management
//!
//! In-memory conversation buffer with compaction and disk persistence.

pub mod memory;
pub mod persistence;

pub use memory::{Memory, PendingHints, SharedMemory};
pub use persistence::{
    PersistenceConfig, ThreadInfo, default_thread_name, iso8601_now, list_threads,
    load_conversation, read_current_thread_name, sanitize_filename, save_conversation,
    thread_name_from_message, write_current_thread_name,
};
