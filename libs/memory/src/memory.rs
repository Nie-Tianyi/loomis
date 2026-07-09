//! # Memory — Conversation History with Compaction
//!
//! This module stores the agent's conversation history and provides
//! a **two-phase compaction** mechanism for keeping context within
//! token-budget limits.
//!
//! ## Two-phase compaction
//!
//! 1. [`Memory::drain_for_compact`] — removes old non-System messages.
//! 2. [`Memory::apply_compact`] — inserts a summary as a new System message.
//!
//! Provider-specific compaction (e.g. `compact_with_deepseek`) lives in
//! downstream crates (e.g. `loomis`).

use std::fmt;
use std::sync::{Arc, RwLock};

use provider::{Message, Role};

// ── Constants ─────────────────────────────────────────────────────────────────

pub const DEFAULT_COMPACT_CHARS: usize = 2_000_000;
pub const DEFAULT_KEEP_LAST_N: usize = 10;

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum MemoryError {
    SummariserFailed(String),
    NothingToCompact,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SummariserFailed(reason) => write!(f, "summariser failed: {reason}"),
            Self::NothingToCompact => write!(f, "nothing to compact — conversation is within budget"),
        }
    }
}

impl std::error::Error for MemoryError {}

// ── CompactSignal ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactSignal {
    WithinBudget,
    NeedsCompact,
}

// ── Memory ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Memory {
    messages: Vec<Message>,
    compact_threshold: usize,
    keep_last_n: usize,
}

pub type SharedMemory = Arc<RwLock<Memory>>;

// ── Builder ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MemoryBuilder {
    messages: Vec<Message>,
    threshold: usize,
    keep_last: usize,
}

impl MemoryBuilder {
    pub fn threshold(mut self, chars: usize) -> Self {
        self.threshold = chars;
        self
    }

    pub fn keep_last(mut self, n: usize) -> Self {
        self.keep_last = n;
        self
    }

    pub fn with_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    pub fn build(self) -> Memory {
        Memory {
            messages: self.messages,
            compact_threshold: self.threshold,
            keep_last_n: self.keep_last,
        }
    }
}

// ── Construction ──────────────────────────────────────────────────────────────

impl Memory {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            compact_threshold: DEFAULT_COMPACT_CHARS,
            keep_last_n: DEFAULT_KEEP_LAST_N,
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            messages: Vec::with_capacity(cap),
            compact_threshold: DEFAULT_COMPACT_CHARS,
            keep_last_n: DEFAULT_KEEP_LAST_N,
        }
    }

    pub fn with_threshold(threshold: usize) -> Self {
        Self {
            messages: Vec::new(),
            compact_threshold: threshold,
            keep_last_n: DEFAULT_KEEP_LAST_N,
        }
    }

    pub fn builder() -> MemoryBuilder {
        MemoryBuilder {
            messages: Vec::new(),
            threshold: DEFAULT_COMPACT_CHARS,
            keep_last: DEFAULT_KEEP_LAST_N,
        }
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Vec<Message>> for Memory {
    fn from(messages: Vec<Message>) -> Self {
        Self {
            messages,
            compact_threshold: DEFAULT_COMPACT_CHARS,
            keep_last_n: DEFAULT_KEEP_LAST_N,
        }
    }
}

// ── Core Methods ──────────────────────────────────────────────────────────────

impl Memory {
    pub fn push(&mut self, message: Message) -> CompactSignal {
        self.messages.push(message);
        if self.total_chars() > self.compact_threshold {
            CompactSignal::NeedsCompact
        } else {
            CompactSignal::WithinBudget
        }
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn to_context_vec(&self) -> Vec<Message> {
        self.messages.clone()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

// ── Context Length ────────────────────────────────────────────────────────────

impl Memory {
    pub fn total_chars(&self) -> usize {
        self.messages.iter().map(|m| m.content.len()).sum()
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn needs_compact(&self) -> bool {
        self.total_chars() > self.compact_threshold
    }

    pub fn compact_threshold(&self) -> usize {
        self.compact_threshold
    }

    pub fn set_compact_threshold(&mut self, threshold: usize) {
        self.compact_threshold = threshold;
    }

    pub fn keep_last_n(&self) -> usize {
        self.keep_last_n
    }
}

// ── Compaction ────────────────────────────────────────────────────────────────

impl Memory {
    /// Drains old non-System messages, keeping the most recent `keep_last_n`.
    /// System messages are never drained.
    pub fn drain_for_compact(&mut self) -> Vec<Message> {
        let non_system_count = self
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .count();
        let keep = std::cmp::min(self.keep_last_n, non_system_count);
        let to_drain = non_system_count.saturating_sub(keep);

        if to_drain == 0 {
            return Vec::new();
        }

        let mut drained = Vec::with_capacity(to_drain);
        let mut kept = Vec::with_capacity(self.messages.len() - to_drain);
        let mut drained_so_far = 0;

        for msg in self.messages.drain(..) {
            if msg.role != Role::System && drained_so_far < to_drain {
                drained.push(msg);
                drained_so_far += 1;
            } else {
                kept.push(msg);
            }
        }

        self.messages = kept;
        drained
    }

    /// Inserts a summary string as a new System message at position 0.
    pub fn apply_compact(&mut self, summary: String) {
        if summary.is_empty() {
            return;
        }
        self.messages
            .insert(0, Message::new(Role::System, summary));
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[allow(dead_code)]
const fn role_label(role: Role) -> &'static str {
    match role {
        Role::System => "System",
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::Tool => "Tool",
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: Role, content: &str) -> Message {
        Message::new(role, content)
    }

    fn user_msg(content: &str) -> Message {
        Message::new(Role::User, content)
    }

    fn assistant_msg(content: &str) -> Message {
        Message::new(Role::Assistant, content)
    }

    fn sys_msg(content: &str) -> Message {
        Message::new(Role::System, content)
    }

    #[test]
    fn test_new_creates_empty() {
        let mem = Memory::new();
        assert!(mem.messages().is_empty());
        assert_eq!(mem.compact_threshold(), DEFAULT_COMPACT_CHARS);
    }

    #[test]
    fn test_push_appends_message() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        assert_eq!(mem.message_count(), 1);
    }

    #[test]
    fn test_push_returns_within_budget_below_threshold() {
        let mut mem = Memory::with_threshold(1000);
        let signal = mem.push(user_msg("short"));
        assert_eq!(signal, CompactSignal::WithinBudget);
    }

    #[test]
    fn test_push_returns_needs_compact_when_over_threshold() {
        let mut mem = Memory::with_threshold(5);
        let signal = mem.push(user_msg("hello world"));
        assert_eq!(signal, CompactSignal::NeedsCompact);
    }

    #[test]
    fn test_total_chars_sums_content() {
        let mut mem = Memory::new();
        mem.push(user_msg("abc"));
        mem.push(assistant_msg("defg"));
        assert_eq!(mem.total_chars(), 7);
    }

    #[test]
    fn test_drain_preserves_last_n_messages() {
        let mut mem = Memory::new();
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.drain_for_compact();
        assert_eq!(old.len(), 5);
        assert_eq!(mem.message_count(), 10);
        assert_eq!(mem.messages()[0].content, "msg_5");
    }

    #[test]
    fn test_drain_noop_when_fewer_than_keep() {
        let mut mem = Memory::new();
        mem.push(user_msg("a"));
        mem.push(user_msg("b"));
        let old = mem.drain_for_compact();
        assert!(old.is_empty());
        assert_eq!(mem.message_count(), 2);
    }

    #[test]
    fn test_apply_compact_inserts_at_front() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        mem.apply_compact("Summary.".into());
        assert_eq!(mem.message_count(), 2);
        assert_eq!(mem.messages()[0].role, Role::System);
    }

    #[test]
    fn test_apply_compact_void_on_empty() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        mem.apply_compact(String::new());
        assert_eq!(mem.message_count(), 1);
    }

    #[test]
    fn test_drain_preserves_system_messages() {
        let mut mem = Memory::new();
        mem.push(sys_msg("System instructions"));
        for i in 0..12 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.drain_for_compact();
        assert_eq!(old.len(), 2);
        assert_eq!(mem.messages()[0].role, Role::System);
    }

    #[test]
    fn test_shared_memory_write_read() {
        let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
        {
            let mut w = mem.write().unwrap();
            w.push(user_msg("hello"));
        }
        {
            let r = mem.read().unwrap();
            assert_eq!(r.message_count(), 1);
        }
    }

    #[test]
    fn test_builder_custom() {
        let mem = Memory::builder()
            .threshold(500)
            .keep_last(7)
            .with_messages(vec![user_msg("preloaded")])
            .build();
        assert_eq!(mem.compact_threshold(), 500);
        assert_eq!(mem.keep_last_n(), 7);
        assert_eq!(mem.message_count(), 1);
    }

    #[test]
    fn test_from_vec() {
        let msgs = vec![user_msg("a"), assistant_msg("b")];
        let mem = Memory::from(msgs);
        assert_eq!(mem.message_count(), 2);
    }

    #[test]
    fn test_memory_error_display() {
        let e = MemoryError::NothingToCompact;
        assert!(e.to_string().contains("nothing to compact"));
    }

    #[test]
    fn test_role_label_all_variants() {
        assert_eq!(role_label(Role::System), "System");
        assert_eq!(role_label(Role::User), "User");
        assert_eq!(role_label(Role::Assistant), "Assistant");
        assert_eq!(role_label(Role::Tool), "Tool");
    }
}
