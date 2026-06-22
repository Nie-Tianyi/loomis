use std::sync::{Arc, RwLock};

use crate::core::client::{Message, Role};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default character-count threshold that triggers compaction advice.
/// Roughly 2M characters approximates 500k–1M tokens depending on
/// language mix (English ~4 chars/token, Chinese ~1.5–2 chars/token).
/// This is a conservative safety net; the true model limit is higher.
pub const DEFAULT_COMPACT_CHARS: usize = 2_000_000;

/// Number of most recent messages preserved verbatim during compaction.
/// Everything older than this window is fed to the summarizer.
pub const KEEP_LAST_N_MESSAGES: usize = 10;

// ── Types ─────────────────────────────────────────────────────────────────────

/// In-memory conversation history.
///
/// Stores a sequence of [`Message`]s and provides context-length-aware
/// compaction advice. **Not** thread-safe by itself — wrap in
/// [`SharedMemory`] for multi-task access.
///
/// # Compaction strategy
///
/// When the total character count of all message `content` fields exceeds
/// the threshold, the caller is signalled via [`CompactSignal`]. The
/// recommended two-phase approach:
///
/// 1. Call [`split_for_compact`](Self::split_for_compact) to drain old
///    messages (everything before the last N messages).
/// 2. Summarize the drained messages (e.g., via LLM).
/// 3. Call [`apply_compact`](Self::apply_compact) to insert the summary
///    as a new System message at position 0.
#[derive(Clone, Debug)]
pub struct Memory {
    messages: Vec<Message>,
    compact_threshold: usize,
}

/// Thread-safe shared conversation memory for use across tokio tasks.
///
/// # Example
///
/// ```ignore
/// let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
/// mem.write().unwrap().push(Message::new(Role::User, "Hello"));
/// let ctx: Vec<Message> = mem.read().unwrap().get_context().to_vec();
/// ```
pub type SharedMemory = Arc<RwLock<Memory>>;

/// Signal returned by [`Memory::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactSignal {
    /// Context is within budget; no action needed.
    Ok,
    /// Character count has exceeded [`Memory::compact_threshold`];
    /// the caller should consider compacting before the next LLM call.
    NeedsCompact,
}

// ── Construction ──────────────────────────────────────────────────────────────

impl Memory {
    /// Creates an empty memory with the default compaction threshold
    /// ([`DEFAULT_COMPACT_CHARS`]).
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            compact_threshold: DEFAULT_COMPACT_CHARS,
        }
    }

    /// Creates an empty memory with a pre-allocated capacity for `cap`
    /// messages (reduces reallocations during growth).
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            messages: Vec::with_capacity(cap),
            compact_threshold: DEFAULT_COMPACT_CHARS,
        }
    }

    /// Creates an empty memory with a custom compaction threshold
    /// (in characters, not tokens).
    pub fn with_threshold(threshold: usize) -> Self {
        Self {
            messages: Vec::new(),
            compact_threshold: threshold,
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
        }
    }
}

// ── Core Methods ──────────────────────────────────────────────────────────────

impl Memory {
    /// Appends a message to the conversation history.
    ///
    /// Returns [`CompactSignal::NeedsCompact`] when the total character
    /// count of all stored messages exceeds the configured threshold.
    /// The caller does **not** need to compact immediately — the signal
    /// is advisory and can be handled before the next LLM call.
    pub fn push(&mut self, message: Message) -> CompactSignal {
        self.messages.push(message);
        if self.total_chars() > self.compact_threshold {
            CompactSignal::NeedsCompact
        } else {
            CompactSignal::Ok
        }
    }

    /// Returns a reference to all stored messages.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Returns the full conversation history for use in an LLM request.
    ///
    /// Alias for [`Self::messages`]; use whichever reads better at the
    /// call site.
    pub fn get_context(&self) -> &[Message] {
        &self.messages
    }

    /// Clones the messages into an owned `Vec<Message>`, suitable for
    /// building a [`DeepSeekRequest`].
    pub fn to_context_vec(&self) -> Vec<Message> {
        self.messages.clone()
    }
}

// ── Context Length ────────────────────────────────────────────────────────────

impl Memory {
    /// Returns the total number of characters across **all** `content`
    /// fields in every stored message.
    ///
    /// This is a rough proxy for token count. Use it to decide when to
    /// compact.
    pub fn total_chars(&self) -> usize {
        self.messages.iter().map(|m| m.content.len()).sum()
    }

    /// Returns the number of messages currently stored.
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Checks whether the total character count exceeds the configured
    /// threshold.
    pub fn needs_compact(&self) -> bool {
        self.total_chars() > self.compact_threshold
    }

    /// Returns the current compaction threshold in characters.
    pub fn compact_threshold(&self) -> usize {
        self.compact_threshold
    }

    /// Sets a new compaction threshold.
    pub fn set_compact_threshold(&mut self, threshold: usize) {
        self.compact_threshold = threshold;
    }
}

// ── Compaction ────────────────────────────────────────────────────────────────

impl Memory {
    /// Drains the "old" messages (everything before the last
    /// [`KEEP_LAST_N_MESSAGES`]) and returns them for summarization.
    ///
    /// After this call, `self` contains only the most recent
    /// [`KEEP_LAST_N_MESSAGES`] messages. The caller should:
    ///
    /// 1. Send the returned messages to a (small) LLM for summarization.
    /// 2. Call [`apply_compact`](Self::apply_compact) with the result.
    ///
    /// Returns an empty `Vec` if there are fewer messages than the
    /// keep-window (i.e., nothing to compact).
    pub fn split_for_compact(&mut self) -> Vec<Message> {
        let total = self.messages.len();
        let keep = std::cmp::min(KEEP_LAST_N_MESSAGES, total);
        let pivot = total.saturating_sub(keep);

        if pivot == 0 {
            Vec::new()
        } else {
            self.messages.drain(..pivot).collect()
        }
    }

    /// Inserts a compressed summary at the beginning of memory as a
    /// System message.
    ///
    /// Call this **after** [`split_for_compact`](Self::split_for_compact)
    /// once the summary text has been obtained (e.g., from an LLM).
    ///
    /// If `summary` is empty, this is a no-op (no message is inserted).
    pub fn apply_compact(&mut self, summary: String) {
        if !summary.is_empty() {
            self.messages
                .insert(0, Message::new(Role::System, summary));
        }
    }

    /// Convenience method: runs the full compaction cycle synchronously.
    ///
    /// Calls [`split_for_compact`](Self::split_for_compact), passes the
    /// old messages to `summarize`, then calls
    /// [`apply_compact`](Self::apply_compact) with the result.
    ///
    /// For async summarization (the common case), use the two-phase API
    /// directly instead.
    pub fn compact(&mut self, summarize: impl FnOnce(&[Message]) -> String) {
        let old = self.split_for_compact();
        if old.is_empty() {
            return;
        }
        let summary = summarize(&old);
        self.apply_compact(summary);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ──────────────────────────────────────────────────────────

    fn make_msg(role: Role, content: &str) -> Message {
        Message::new(role, content)
    }

    fn user_msg(content: &str) -> Message {
        Message::new(Role::User, content)
    }

    fn assistant_msg(content: &str) -> Message {
        Message::new(Role::Assistant, content)
    }

    // ── Construction ─────────────────────────────────────────────────────

    #[test]
    fn test_new_creates_empty() {
        let mem = Memory::new();
        assert!(mem.messages().is_empty());
        assert_eq!(mem.message_count(), 0);
        assert_eq!(mem.compact_threshold(), DEFAULT_COMPACT_CHARS);
    }

    #[test]
    fn test_default_equals_new() {
        let m1 = Memory::new();
        let m2 = Memory::default();
        assert_eq!(m1.message_count(), m2.message_count());
        assert_eq!(m1.compact_threshold(), m2.compact_threshold());
    }

    #[test]
    fn test_with_capacity_pre_allocates() {
        let mem = Memory::with_capacity(100);
        assert!(mem.messages().is_empty());
        assert!(mem.messages.capacity() >= 100);
    }

    #[test]
    fn test_with_threshold_custom() {
        let mem = Memory::with_threshold(500);
        assert_eq!(mem.compact_threshold(), 500);
        assert!(mem.messages().is_empty());
    }

    #[test]
    fn test_from_vec() {
        let msgs = vec![user_msg("a"), assistant_msg("b")];
        let mem = Memory::from(msgs.clone());
        assert_eq!(mem.messages().len(), 2);
        assert_eq!(mem.message_count(), 2);
    }

    #[test]
    fn test_clone_independent() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        let mut cloned = mem.clone();
        cloned.push(assistant_msg("world"));
        // Original unchanged
        assert_eq!(mem.message_count(), 1);
        assert_eq!(cloned.message_count(), 2);
    }

    // ── Push ─────────────────────────────────────────────────────────────

    #[test]
    fn test_push_appends_message() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        assert_eq!(mem.message_count(), 1);
        assert_eq!(mem.messages()[0].content, "hello");
        assert_eq!(mem.messages()[0].role, Role::User);
    }

    #[test]
    fn test_push_returns_ok_below_threshold() {
        let mut mem = Memory::with_threshold(1000);
        let signal = mem.push(user_msg("short"));
        assert_eq!(signal, CompactSignal::Ok);
    }

    #[test]
    fn test_push_returns_needs_compact_when_over_threshold() {
        let mut mem = Memory::with_threshold(5);
        let signal = mem.push(user_msg("hello world")); // 11 chars > 5
        assert_eq!(signal, CompactSignal::NeedsCompact);
    }

    #[test]
    fn test_push_returns_ok_at_exact_threshold() {
        let mut mem = Memory::with_threshold(5);
        // "hello" is exactly 5 chars — NOT over threshold
        let signal = mem.push(user_msg("hello"));
        assert_eq!(signal, CompactSignal::Ok);
    }

    // ── Context ──────────────────────────────────────────────────────────

    #[test]
    fn test_get_context_returns_all() {
        let mut mem = Memory::new();
        mem.push(user_msg("q1"));
        mem.push(assistant_msg("a1"));
        mem.push(user_msg("q2"));
        assert_eq!(mem.get_context().len(), 3);
    }

    #[test]
    fn test_to_context_vec_returns_owned() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        let v = mem.to_context_vec();
        assert_eq!(v.len(), 1);
        // Modifying the returned vec doesn't affect memory
        drop(v);
        assert_eq!(mem.message_count(), 1);
    }

    #[test]
    fn test_messages_and_get_context_are_same() {
        let mut mem = Memory::new();
        mem.push(user_msg("test"));
        assert_eq!(
            mem.messages().as_ptr(),
            mem.get_context().as_ptr()
        );
    }

    // ── Context Length ───────────────────────────────────────────────────

    #[test]
    fn test_total_chars_empty() {
        let mem = Memory::new();
        assert_eq!(mem.total_chars(), 0);
    }

    #[test]
    fn test_total_chars_sums_content() {
        let mut mem = Memory::new();
        mem.push(user_msg("abc")); // 3
        mem.push(assistant_msg("defg")); // 4
        assert_eq!(mem.total_chars(), 7);
    }

    #[test]
    fn test_total_chars_with_tool_messages() {
        let mut mem = Memory::new();
        let mut tc_msg = make_msg(Role::Assistant, "calling tool");
        tc_msg.tool_call_id = Some("call_1".into());
        mem.push(tc_msg);
        // Only "calling tool" (12 chars) counts — tool_call_id is ignored
        assert_eq!(mem.total_chars(), 12);
    }

    #[test]
    fn test_message_count() {
        let mut mem = Memory::new();
        assert_eq!(mem.message_count(), 0);
        mem.push(user_msg("a"));
        mem.push(user_msg("b"));
        assert_eq!(mem.message_count(), 2);
    }

    #[test]
    fn test_needs_compact_false_when_empty() {
        let mem = Memory::new();
        assert!(!mem.needs_compact());
    }

    #[test]
    fn test_needs_compact_false_under_threshold() {
        let mut mem = Memory::with_threshold(100);
        mem.push(user_msg("short"));
        assert!(!mem.needs_compact());
    }

    #[test]
    fn test_needs_compact_true_when_over_threshold() {
        let mut mem = Memory::with_threshold(2);
        mem.push(user_msg("abc")); // 3 chars > 2
        assert!(mem.needs_compact());
    }

    #[test]
    fn test_set_compact_threshold() {
        let mut mem = Memory::new();
        assert_eq!(mem.compact_threshold(), DEFAULT_COMPACT_CHARS);
        mem.set_compact_threshold(42);
        assert_eq!(mem.compact_threshold(), 42);
    }

    // ── Compaction: split_for_compact ────────────────────────────────────

    #[test]
    fn test_split_for_compact_preserves_last_n_messages() {
        let mut mem = Memory::new();
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.split_for_compact();
        assert_eq!(old.len(), 5); // 15 - 10 = 5 drained
        assert_eq!(mem.message_count(), 10);
        // First remaining message should be msg_5 (0-indexed)
        assert_eq!(mem.messages()[0].content, "msg_5");
        // Last remaining should be msg_14
        assert_eq!(mem.messages()[9].content, "msg_14");
    }

    #[test]
    fn test_split_for_compact_returns_old_in_order() {
        let mut mem = Memory::new();
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.split_for_compact();
        assert_eq!(old.len(), 5);
        for (i, m) in old.iter().enumerate() {
            assert_eq!(m.content, format!("msg_{i}"));
        }
    }

    #[test]
    fn test_split_for_compact_noop_when_fewer_than_keep() {
        let mut mem = Memory::new();
        mem.push(user_msg("a"));
        mem.push(user_msg("b"));
        mem.push(user_msg("c"));
        let old = mem.split_for_compact();
        assert!(old.is_empty());
        assert_eq!(mem.message_count(), 3); // unchanged
    }

    #[test]
    fn test_split_for_compact_noop_when_exactly_keep() {
        let mut mem = Memory::new();
        for i in 0..KEEP_LAST_N_MESSAGES {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.split_for_compact();
        assert!(old.is_empty());
        assert_eq!(mem.message_count(), KEEP_LAST_N_MESSAGES);
    }

    #[test]
    fn test_split_for_compact_one_extra() {
        let mut mem = Memory::new();
        for i in 0..=KEEP_LAST_N_MESSAGES {
            // 11 messages total
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.split_for_compact();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].content, "msg_0");
        assert_eq!(mem.message_count(), KEEP_LAST_N_MESSAGES); // 10 kept
    }

    #[test]
    fn test_split_for_compact_empty_memory() {
        let mut mem = Memory::new();
        let old = mem.split_for_compact();
        assert!(old.is_empty());
        assert_eq!(mem.message_count(), 0);
    }

    // ── Compaction: apply_compact ────────────────────────────────────────

    #[test]
    fn test_apply_compact_inserts_system_summary() {
        let mut mem = Memory::new();
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        mem.split_for_compact(); // drain first 5
        mem.apply_compact("Summary of old messages".into());
        assert_eq!(mem.message_count(), 11); // 1 summary + 10 kept
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[0].content, "Summary of old messages");
    }

    #[test]
    fn test_apply_compact_empty_is_noop() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        mem.apply_compact(String::new());
        assert_eq!(mem.message_count(), 1); // unchanged
    }

    #[test]
    fn test_apply_compact_whitespace_only_is_inserted() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        mem.apply_compact("  ".into());
        // Whitespace is not empty — it's a string, so it gets inserted
        assert_eq!(mem.message_count(), 2);
    }

    // ── Compaction: compact (sync convenience) ───────────────────────────

    #[test]
    fn test_compact_full_cycle() {
        let mut mem = Memory::new();
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        mem.compact(|old| {
            assert_eq!(old.len(), 5);
            format!("{} messages summarized", old.len())
        });
        assert_eq!(mem.message_count(), 11); // 1 summary + 10 kept
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[0].content, "5 messages summarized");
    }

    #[test]
    fn test_compact_noop_when_few_messages() {
        let mut mem = Memory::new();
        mem.push(user_msg("only one"));
        let called = std::cell::Cell::new(false);
        mem.compact(|_| {
            called.set(true);
            "should not be called".into()
        });
        // Summarize closure was never invoked
        assert!(!called.get());
        assert_eq!(mem.message_count(), 1);
    }

    // ── SharedMemory ─────────────────────────────────────────────────────

    #[test]
    fn test_shared_memory_write_read() {
        let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
        {
            let mut w = mem.write().unwrap();
            w.push(user_msg("hello"));
            w.push(assistant_msg("world"));
        }
        {
            let r = mem.read().unwrap();
            assert_eq!(r.message_count(), 2);
            assert_eq!(r.messages()[0].content, "hello");
            assert_eq!(r.messages()[1].content, "world");
        }
    }

    #[test]
    fn test_shared_memory_concurrent_reads() {
        let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
        {
            let mut w = mem.write().unwrap();
            w.push(user_msg("data"));
        }
        // Two read locks can coexist
        let r1 = mem.read().unwrap();
        let r2 = mem.read().unwrap();
        assert_eq!(r1.message_count(), 1);
        assert_eq!(r2.message_count(), 1);
        drop(r1);
        drop(r2);
    }

    #[test]
    fn test_shared_memory_clone_increases_ref_count() {
        let mem: SharedMemory = Arc::new(RwLock::new(Memory::new()));
        let mem2 = Arc::clone(&mem);
        {
            let mut w = mem2.write().unwrap();
            w.push(user_msg("via clone"));
        }
        let r = mem.read().unwrap();
        assert_eq!(r.message_count(), 1);
    }

    // ── Edge Cases ───────────────────────────────────────────────────────

    #[test]
    fn test_large_single_message_triggers_compact() {
        let mut mem = Memory::with_threshold(10);
        let signal = mem.push(user_msg("this is a long message"));
        assert_eq!(signal, CompactSignal::NeedsCompact);
    }

    #[test]
    fn test_multiple_pushes_accumulate() {
        let mut mem = Memory::with_threshold(10);
        assert_eq!(mem.push(user_msg("abc")), CompactSignal::Ok); // 3
        assert_eq!(mem.push(user_msg("def")), CompactSignal::Ok); // 6
        assert_eq!(mem.push(user_msg("ghij")), CompactSignal::Ok); // 10
        // Next push crosses threshold
        assert_eq!(mem.push(user_msg("k")), CompactSignal::NeedsCompact); // 11
    }

    #[test]
    fn test_system_message_content_counts() {
        let mut mem = Memory::with_threshold(10);
        mem.push(make_msg(Role::System, "You are a helpful assistant"));
        // "You are a helpful assistant" = 28 chars > 10
        assert!(mem.needs_compact());
    }

    #[test]
    fn test_split_preserves_tool_messages_in_keep_window() {
        let mut mem = Memory::new();
        // Build: user → assistant(tool_calls) → tool → assistant → ... (12 messages)
        for i in 0..4 {
            mem.push(user_msg(&format!("question_{i}")));
            let mut tc_msg = assistant_msg(&format!("calling tool_{i}"));
            tc_msg.tool_call_id = Some(format!("call_{i}"));
            mem.push(tc_msg);
            mem.push(make_msg(Role::Tool, &format!("tool_result_{i}")));
        }
        // 12 messages total, KEEP_LAST_N_MESSAGES = 10
        let old = mem.split_for_compact();
        assert_eq!(old.len(), 2); // first 2 drained
        assert_eq!(mem.message_count(), 10);
    }
}
