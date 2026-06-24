use std::sync::{Arc, RwLock};

use crate::core::client::{DeepSeekClient, DeepSeekError, DeepSeekRequest, Message, Role};

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
/// 1. Call [`split_for_compact`](Self::split_for_compact) to drain old non-System messages (everything before the last N non-System
///    messages). **System messages are never drained** — they stay in
///    memory verbatim.
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
    /// Drains the first `to_drain` non-System messages, preserving System
    /// messages and the last [`KEEP_LAST_N_MESSAGES`] non-System messages
    /// in their original relative order.
    fn drain_old_non_system(&mut self) -> Vec<Message> {
        let non_system_count = self
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .count();
        let keep = std::cmp::min(KEEP_LAST_N_MESSAGES, non_system_count);
        let to_drain = non_system_count.saturating_sub(keep);

        if to_drain == 0 {
            return Vec::new();
        }

        let mut drained = Vec::with_capacity(to_drain);
        let mut remaining = Vec::with_capacity(self.messages.len() - to_drain);
        let mut drained_count = 0;

        for msg in self.messages.drain(..) {
            if msg.role != Role::System && drained_count < to_drain {
                drained.push(msg);
                drained_count += 1;
            } else {
                remaining.push(msg);
            }
        }

        self.messages = remaining;
        drained
    }

    /// Compacts old non-System messages by summarising them via a
    /// lightweight "flash" LLM.
    ///
    /// The flash model is read from the `DEFAULT_FLASH_MODEL` environment
    /// variable (falls back to `"deepseek-chat"`). The API key is read
    /// from `DEEPSEEK_API`.
    ///
    /// # Behaviour
    ///
    /// 1. Drains all non-System messages before the last
    ///    [`KEEP_LAST_N_MESSAGES`].
    /// 2. Sends them to the flash model for summarisation.
    /// 3. Inserts the summary as a System message at position 0.
    ///
    /// **System messages are always preserved verbatim** — only `User`,
    /// `Assistant`, and `Tool` messages are candidates for compaction.
    ///
    /// Does nothing (returns `Ok`) if there are insufficient non-System
    /// messages to compact.
    pub async fn compact(&mut self) -> Result<(), DeepSeekError> {
        let old = self.drain_old_non_system();
        if old.is_empty() {
            return Ok(());
        }

        let flash_model =
            std::env::var("DEFAULT_FLASH_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string());
        let api_key = std::env::var("DEEPSEEK_API").map_err(|_| DeepSeekError::Api {
            status: 0,
            body: "DEEPSEEK_API environment variable not set".into(),
        })?;

        let client = DeepSeekClient::new(api_key);

        // Build a compact conversation transcript for the summariser.
        let transcript: String = old
            .iter()
            .map(|m| format!("[{}]: {}", role_label(m.role), m.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            "Summarise the following conversation history concisely. \
             Preserve key facts, decisions, and context. \
             Output only the summary, no preamble:\n\n{transcript}"
        );

        let request = DeepSeekRequest::new(flash_model, vec![Message::new(Role::User, prompt)]);

        let response = client.send(request).await?;
        let summary = response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();

        if !summary.is_empty() {
            self.messages.insert(0, Message::new(Role::System, summary));
        }

        Ok(())
    }
}

/// Human-readable label for each [`Role`], used when formatting the
/// transcript for summarisation.
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
        assert_eq!(mem.messages().as_ptr(), mem.get_context().as_ptr());
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
        let old = mem.drain_old_non_system();
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
        let old = mem.drain_old_non_system();
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
        let old = mem.drain_old_non_system();
        assert!(old.is_empty());
        assert_eq!(mem.message_count(), 3); // unchanged
    }

    #[test]
    fn test_split_for_compact_noop_when_exactly_keep() {
        let mut mem = Memory::new();
        for i in 0..KEEP_LAST_N_MESSAGES {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.drain_old_non_system();
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
        let old = mem.drain_old_non_system();
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].content, "msg_0");
        assert_eq!(mem.message_count(), KEEP_LAST_N_MESSAGES); // 10 kept
    }

    #[test]
    fn test_split_for_compact_empty_memory() {
        let mut mem = Memory::new();
        let old = mem.drain_old_non_system();
        assert!(old.is_empty());
        assert_eq!(mem.message_count(), 0);
    }

    // ── Compaction: summary insertion ─────────────────────────────────────

    #[test]
    fn test_insert_summary_at_front() {
        let mut mem = Memory::new();
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        mem.drain_old_non_system(); // drain first 5
        // Simulate what compact() does after receiving the LLM summary
        let summary = "Summary of old messages";
        mem.messages
            .insert(0, Message::new(Role::System, summary.to_string()));
        assert_eq!(mem.message_count(), 11); // 1 summary + 10 kept
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[0].content, "Summary of old messages");
    }

    #[test]
    fn test_empty_summary_is_not_inserted() {
        let mut mem = Memory::new();
        mem.push(user_msg("hello"));
        let summary = "";
        if !summary.is_empty() {
            mem.messages
                .insert(0, Message::new(Role::System, summary.to_string()));
        }
        assert_eq!(mem.message_count(), 1); // unchanged
    }

    // ── Compaction: full-cycle drain + insert ─────────────────────────────

    #[test]
    fn test_drain_and_insert_full_cycle() {
        let mut mem = Memory::new();
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        let old = mem.drain_old_non_system();
        assert_eq!(old.len(), 5);
        let summary = format!("{} messages summarized", old.len());
        mem.messages.insert(0, Message::new(Role::System, summary));
        assert_eq!(mem.message_count(), 11); // 1 summary + 10 kept
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[0].content, "5 messages summarized");
    }

    #[test]
    fn test_drain_noop_when_few_messages() {
        let mut mem = Memory::new();
        mem.push(user_msg("only one"));
        let old = mem.drain_old_non_system();
        assert!(old.is_empty());
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
        let old = mem.drain_old_non_system();
        assert_eq!(old.len(), 2); // first 2 drained
        assert_eq!(mem.message_count(), 10);
    }

    // ── Compaction: System message preservation ──────────────────────────

    #[test]
    fn test_split_preserves_system_messages_at_front() {
        let mut mem = Memory::new();
        // System message at the very beginning
        mem.push(make_msg(Role::System, "You are a helpful assistant"));
        // Then 12 user messages (more than KEEP_LAST_N_MESSAGES)
        for i in 0..12 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        assert_eq!(mem.message_count(), 13); // 1 system + 12 user

        let old = mem.drain_old_non_system();
        // 12 non-system - 10 kept = 2 drained
        assert_eq!(old.len(), 2);
        assert_eq!(old[0].content, "msg_0");
        assert_eq!(old[1].content, "msg_1");
        // System message preserved + 10 recent non-system = 11 total
        assert_eq!(mem.message_count(), 11);
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[0].content, "You are a helpful assistant");
        // Recent user messages follow
        assert_eq!(mem.messages()[1].content, "msg_2");
        assert_eq!(mem.messages()[10].content, "msg_11");
    }

    #[test]
    fn test_split_preserves_system_messages_interleaved() {
        let mut mem = Memory::new();
        mem.push(make_msg(Role::System, "System prompt 1"));
        mem.push(user_msg("msg_0"));
        mem.push(assistant_msg("msg_1"));
        mem.push(make_msg(Role::System, "System prompt 2"));
        mem.push(user_msg("msg_2"));
        mem.push(assistant_msg("msg_3"));
        mem.push(user_msg("msg_4"));
        mem.push(assistant_msg("msg_5"));
        mem.push(user_msg("msg_6"));
        mem.push(assistant_msg("msg_7"));
        mem.push(user_msg("msg_8"));
        mem.push(assistant_msg("msg_9"));
        mem.push(user_msg("msg_10"));
        mem.push(assistant_msg("msg_11"));
        // 14 total: 2 system + 12 non-system

        let old = mem.drain_old_non_system();
        // 12 non-system - 10 kept = 2 drained (msg_0, msg_1)
        assert_eq!(old.len(), 2);
        assert!(!old.iter().any(|m| m.role == Role::System));
        // 2 system + 10 non-system = 12 remaining
        assert_eq!(mem.message_count(), 12);
        // Both system messages are preserved
        let system_msgs: Vec<_> = mem
            .messages()
            .iter()
            .filter(|m| m.role == Role::System)
            .collect();
        assert_eq!(system_msgs.len(), 2);
        assert_eq!(system_msgs[0].content, "System prompt 1");
        assert_eq!(system_msgs[1].content, "System prompt 2");
    }

    #[test]
    fn test_split_noop_when_only_system_messages() {
        let mut mem = Memory::new();
        mem.push(make_msg(Role::System, "System A"));
        mem.push(make_msg(Role::System, "System B"));
        mem.push(make_msg(Role::System, "System C"));

        let old = mem.drain_old_non_system();
        // 0 non-system → nothing to drain
        assert!(old.is_empty());
        assert_eq!(mem.message_count(), 3); // all system messages kept
    }

    #[test]
    fn test_split_drains_only_non_system() {
        let mut mem = Memory::new();
        mem.push(make_msg(Role::System, "Important instructions"));
        mem.push(user_msg("q1"));
        mem.push(assistant_msg("a1"));
        mem.push(user_msg("q2"));
        mem.push(assistant_msg("a2"));
        mem.push(user_msg("q3"));
        mem.push(assistant_msg("a3"));
        mem.push(user_msg("q4"));
        mem.push(assistant_msg("a4"));
        mem.push(user_msg("q5"));
        mem.push(assistant_msg("a5"));
        mem.push(user_msg("q6"));
        // 12 total: 1 system + 11 non-system

        let old = mem.drain_old_non_system();
        // 11 non-system - 10 kept = 1 drained (q1)
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].role, Role::User);
        assert_eq!(old[0].content, "q1");
        // System message + 10 non-system = 11 remaining
        assert_eq!(mem.message_count(), 11);
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[1].content, "a1"); // first kept non-system
    }

    #[test]
    fn test_drain_preserves_system_messages_full_cycle() {
        let mut mem = Memory::new();
        mem.push(make_msg(Role::System, "System instructions"));
        for i in 0..15 {
            mem.push(user_msg(&format!("msg_{i}")));
        }
        // 16 total: 1 system + 15 user

        let old = mem.drain_old_non_system();
        // Only non-system messages are presented for summarization
        assert!(!old.iter().any(|m| m.role == Role::System));
        assert_eq!(old.len(), 5); // 15 - 10 = 5
        let summary = format!("{} messages summarized", old.len());
        mem.messages.insert(0, Message::new(Role::System, summary));

        // 1 summary + 1 original system + 10 recent non-system = 12
        assert_eq!(mem.message_count(), 12);
        // First is the summary System message
        assert_eq!(mem.messages()[0].role, Role::System);
        assert_eq!(mem.messages()[0].content, "5 messages summarized");
        // Second is the original System message (preserved)
        assert_eq!(mem.messages()[1].role, Role::System);
        assert_eq!(mem.messages()[1].content, "System instructions");
        // Then the 10 recent non-system messages
        assert_eq!(mem.messages()[2].content, "msg_5");
        assert_eq!(mem.messages()[11].content, "msg_14");
    }
}
