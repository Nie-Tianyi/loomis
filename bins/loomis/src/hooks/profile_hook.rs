//! Hook that accumulates a user profile from agent-run lifecycle events
//! and injects a `[PROFILE]` System message each LLM call.
//!
//! # Design
//!
//! The hook collects signals at two tiers:
//!
//! **Tier 1 — real-time rules** (zero token cost, synchronous):
//! - Language detection from user input (`on_run_start`)
//! - Per-tool invocation counters (`before_tool_call`, `after_tool_call`,
//!   `on_tool_failed`)
//! - Session count + timestamp (`on_run_finish`)
//!
//! **Tier 2 — LLM synthesis** (cheap flash model, every N sessions):
//! - `coding_conventions`, `preferences`, `avoidances`,
//!   `expertise_signals`, `verbosity`
//!
//! The `[PROFILE]` System message is injected in [`on_llm_start`] using
//! the same remove-then-reinsert pattern as [`SkillHook`] and
//! [`TodoListHook`](crate::hooks::TodoListHook).  This ensures exactly
//! one `[PROFILE]` message exists at all times, placed at index 0 so
//! the model sees it before any other instruction.
//!
//! # Synthesis
//!
//! Every [`SYNTHESIS_INTERVAL`] sessions, the hook collects the most
//! recent user/assistant messages from conversation memory, sends them
//! to the flash model with a structured prompt, and merges the parsed
//! results into the profile.  The LLM call uses
//! [`engine::block_on`] — this blocks the agent loop
//! (a dedicated tokio task) but not the TUI (the main thread).

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use deepseek::DeepSeekClient;
use engine::{AgentHook, RunOutcome};
use memory::SharedMemory;
use provider::{CompletionRequest, LLMClient, Message, Role, ToolCall};

use crate::profile::{
    PROFILE_MARKER, ProfileStore, Verbosity, build_profile_system_message, has_cjk, truncate,
};

// ── Constants ────────────────────────────────────────────────────────────────

/// How many agent runs between LLM-driven profile synthesis passes.
///
/// Set conservatively — synthesis blocks the agent loop briefly and
/// consumes a few hundred tokens.  Five sessions is frequent enough
/// to pick up on preferences quickly without wasting API calls.
const SYNTHESIS_INTERVAL: u64 = 5;

/// How many recent user + assistant messages to include as context
/// for the synthesis prompt.
const SYNTHESIS_CONTEXT_SIZE: usize = 10;

/// Maximum bytes per message included in the synthesis prompt.
/// Longer messages are truncated to keep the prompt compact.
const SYNTHESIS_MSG_MAX_BYTES: usize = 1000;

// ── ProfileHook ──────────────────────────────────────────────────────────────

/// Agent hook that collects user behaviour signals, periodically
/// synthesises them via LLM, and injects a `[PROFILE]` System message
/// so the agent personalises its responses.
pub struct ProfileHook {
    /// Shared profile store — loaded at construction, saved after each run.
    store: Arc<RwLock<ProfileStore>>,
    /// The cheap model used for synthesis (e.g. `"deepseek-chat"`).
    flash_model: String,
    /// Stateless HTTP client for the synthesis LLM call.
    client: DeepSeekClient,
}

impl ProfileHook {
    /// Create a new profile hook.
    ///
    /// Loads (or creates) the profile from
    /// `<workspace_root>/.loomis/profile.json`.
    pub fn new(workspace_root: PathBuf, flash_model: String, client: DeepSeekClient) -> Self {
        let store = Arc::new(RwLock::new(ProfileStore::load(&workspace_root)));
        Self {
            store,
            flash_model,
            client,
        }
    }
}

impl AgentHook for ProfileHook {
    // ── Run lifecycle ────────────────────────────────────────────

    /// Detect the user's language from their first few messages.
    ///
    /// Uses a crude CJK heuristic (Unicode block range check) that
    /// is cheap, synchronous, and correct for Chinese users.
    /// Once set to `"zh-CN"`, the preference is sticky — subsequent
    /// English-only messages won't flip it back.  The LLM synthesis
    /// may refine this later.
    fn on_run_start(&self, _session_id: &str, user_input: &str, _memory: &SharedMemory) {
        let mut store = self.store.write().expect("profile store lock poisoned");

        // Sticky: once we've detected Chinese, don't regress.
        if store.profile.language_preference != "zh-CN" && has_cjk(user_input) {
            store.profile.language_preference = "zh-CN".to_string();
        }
    }

    /// Persist the updated profile and optionally trigger synthesis.
    ///
    /// Synthesis fires when `total_sessions - last_synthesis_session`
    /// reaches [`SYNTHESIS_INTERVAL`].
    fn on_run_finish(&self, _session_id: &str, _outcome: &RunOutcome, memory: &SharedMemory) {
        let needs_synthesis = {
            let mut store = self.store.write().expect("profile store lock poisoned");

            store.profile.total_sessions += 1;
            store.profile.updated_at = memory::iso8601_now();
            store.save();

            store.profile.total_sessions - store.profile.last_synthesis_session
                >= SYNTHESIS_INTERVAL
        };

        if needs_synthesis {
            self.run_synthesis(memory);
        }
    }

    // ── Tool tracking ────────────────────────────────────────────

    /// Count every tool invocation *before* execution.
    ///
    /// Always returns `Ok(())` — we never block, only observe.
    fn before_tool_call(
        &self,
        _session_id: &str,
        tool_call: &ToolCall,
    ) -> Result<(), engine::AgentError> {
        let mut store = self.store.write().expect("profile store lock poisoned");

        let stats = store
            .profile
            .tool_stats
            .entry(tool_call.function.name.clone())
            .or_default();
        stats.total_calls += 1;

        Ok(())
    }

    /// Count successful tool executions.
    fn after_tool_call(&self, _session_id: &str, tool_call: &ToolCall, _observation: &str) {
        let mut store = self.store.write().expect("profile store lock poisoned");

        let stats = store
            .profile
            .tool_stats
            .entry(tool_call.function.name.clone())
            .or_default();
        stats.successes += 1;
    }

    /// Count failed tool executions.
    fn on_tool_failed(&self, _session_id: &str, tool_call: &ToolCall, _error: &str) {
        let mut store = self.store.write().expect("profile store lock poisoned");

        let stats = store
            .profile
            .tool_stats
            .entry(tool_call.function.name.clone())
            .or_default();
        stats.failures += 1;
    }

    // ── System message injection ─────────────────────────────────

    /// Refresh the `[PROFILE]` System message.
    ///
    /// Fires before every LLM call — removes any stale `[PROFILE]`
    /// message and inserts a fresh one at index 0.  This runs after
    /// all tool results from the previous step are committed to
    /// memory, so inserting a System message here does not violate
    /// the API ordering constraint (assistant tool_calls message must
    /// be followed by tool result messages).
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        let profile_msg = {
            let store = self.store.read().expect("profile store lock poisoned");
            build_profile_system_message(&store.profile)
        };

        let mut mem = memory.write().expect("memory lock poisoned");

        // Remove any existing [PROFILE] message — idempotent.
        mem.messages
            .retain(|m| !(m.role == Role::System && m.content.starts_with(PROFILE_MARKER)));

        // Insert the fresh [PROFILE] at index 0.
        mem.messages
            .insert(0, Message::new(Role::System, profile_msg));
    }
}

// ── Synthesis (private helpers) ──────────────────────────────────────────────

impl ProfileHook {
    /// Run the LLM-driven profile synthesis.
    ///
    /// Collects recent conversation context, sends a structured
    /// prompt to the flash model, and merges the returned signals
    /// into the profile.  Uses [`engine::block_on`] — a bare
    /// `Handle::block_on` would panic because the agent loop (and
    /// therefore this hook) runs on a tokio worker thread.
    fn run_synthesis(&self, memory: &SharedMemory) {
        tracing::info!(
            model = %self.flash_model,
            "Profile synthesis started",
        );
        let synthesis_started = std::time::Instant::now();

        // ── Gather input under locks (released before the LLM call) ──
        let (profile_json, context_text) = {
            let mem = memory.read().expect("memory lock poisoned");
            let store = self.store.read().expect("profile store lock poisoned");

            let profile_json = serde_json::to_string_pretty(&store.profile).unwrap_or_default();

            let recent: Vec<String> = mem
                .messages
                .iter()
                .rev()
                .filter(|m| matches!(m.role, Role::User | Role::Assistant))
                .take(SYNTHESIS_CONTEXT_SIZE)
                .map(|m| {
                    format!(
                        "[{}]: {}",
                        m.role.label(),
                        truncate(&m.content, SYNTHESIS_MSG_MAX_BYTES)
                    )
                })
                .collect();

            (profile_json, recent.join("\n\n"))
        };

        // ── Call the flash model ──
        let prompt = build_synthesis_prompt(&profile_json, &context_text);
        let request =
            CompletionRequest::new(&self.flash_model, vec![Message::new(Role::User, prompt)]);

        // Block the agent loop, not the UI — same pattern as MacroCompactHook.
        // A bare `Handle::block_on` would panic here: hooks run on a tokio
        // worker thread, and blocking a worker is only legal after
        // `block_in_place` (handled inside `engine::block_on`).
        let result = engine::block_on(self.client.generate(request));

        match result {
            Ok(resp) => {
                let text = resp
                    .choices
                    .into_iter()
                    .next()
                    .and_then(|c| c.message.content)
                    .unwrap_or_default();

                if let Some(update) = parse_synthesis_response(&text) {
                    // Snapshot the extracted entry counts before the merge
                    // consumes the update struct.
                    let (preferences, avoidances, expertise, conventions) = (
                        update.preferences.len(),
                        update.avoidances.len(),
                        update.expertise_signals.len(),
                        update.coding_conventions.len(),
                    );

                    let mut store = self.store.write().expect("profile store lock poisoned");

                    // Merge only non-empty fields — empty arrays mean
                    // "no new evidence" and should not overwrite.
                    if !update.preferences.is_empty() {
                        store.profile.preferences = update.preferences;
                    }
                    if !update.avoidances.is_empty() {
                        store.profile.avoidances = update.avoidances;
                    }
                    if !update.expertise_signals.is_empty() {
                        store.profile.expertise_signals = update.expertise_signals;
                    }
                    if !update.coding_conventions.is_empty() {
                        store.profile.coding_conventions = update.coding_conventions;
                    }

                    store.profile.verbosity = update.verbosity;

                    if !update.language_preference.is_empty()
                        && update.language_preference != store.profile.language_preference
                    {
                        store.profile.language_preference = update.language_preference;
                    }

                    store.profile.last_synthesis_session = store.profile.total_sessions;
                    store.profile.updated_at = memory::iso8601_now();
                    store.save();

                    tracing::info!(
                        duration_ms = synthesis_started.elapsed().as_millis() as u64,
                        preferences = preferences,
                        avoidances = avoidances,
                        expertise = expertise,
                        conventions = conventions,
                        "Profile synthesis completed and merged",
                    );
                }
            }
            Err(e) => {
                // Don't advance last_synthesis_session — we'll retry
                // on the next run instead of skipping an interval.
                tracing::warn!(
                    error = %e,
                    "Profile synthesis failed; will retry on next agent run",
                );
            }
        }
    }
}

// ── Synthesis prompt ─────────────────────────────────────────────────────────

/// Build the structured prompt sent to the flash model for profile synthesis.
///
/// The prompt asks the model to extract user behaviour signals from
/// recent conversation context and return a JSON object.  We
/// explicitly instruct the model to be conservative — only signals
/// clearly supported by evidence should be added.
fn build_synthesis_prompt(profile_json: &str, context_text: &str) -> String {
    format!(
        "\
You are analysing a user's interaction patterns with an AI coding assistant.

## Current profile

```json
{profile_json}
```

## Recent conversation context

{context_text}

## Task

Based EXCLUSIVELY on the conversation context above, update the
following profile fields.  Be conservative — only add signals clearly
supported by concrete evidence in the conversation.  Merge new
insights with existing values — do NOT discard valid previous
observations unless the conversation explicitly contradicts them.

Return ONLY valid JSON (no markdown, no preamble) with these fields:

- \"preferences\": array of strings — what the user clearly prefers
  (e.g. \"先解释再写代码\", \"functional patterns\")
- \"avoidances\": array of strings — things the user clearly avoids
  or dislikes (e.g. \"过度抽象\", \"unsafe code\")
- \"expertise_signals\": array of strings — demonstrable skill level
  indicators (e.g. \"Rust 中级\", \"TypeScript 新手\")
- \"coding_conventions\": array of strings — observed coding style
  (e.g. \"snake_case\", \"中文注释\", \"2-space indent\")
- \"verbosity\": one of \"concise\", \"normal\", \"detailed\" —
  inferred from whether the user asks for more or less detail
- \"language_preference\": \"zh-CN\" or \"en-US\"

If there is no new evidence for a field, return an empty array (or
the current verbosity / language_preference value unchanged).  Do NOT
invent signals."
    )
}

// ── Response parsing ─────────────────────────────────────────────────────────

/// Deserialized output from the synthesis LLM.
///
/// All fields default to empty / the [`Verbosity`] default so a
/// partial response is safely mergeable.
#[derive(Debug, serde::Deserialize)]
struct SynthesisUpdate {
    #[serde(default)]
    preferences: Vec<String>,
    #[serde(default)]
    avoidances: Vec<String>,
    #[serde(default)]
    expertise_signals: Vec<String>,
    #[serde(default)]
    coding_conventions: Vec<String>,
    #[serde(default)]
    verbosity: Verbosity,
    #[serde(default)]
    language_preference: String,
}

/// Extract a [`SynthesisUpdate`] from the raw LLM response text.
///
/// Handles three common LLM output patterns:
/// 1. Pure JSON (`{"preferences": [...]}`)
/// 2. JSON inside a markdown code fence (` ```json ... ``` `)
/// 3. JSON with trailing text (finds the outermost `{…}`)
///
/// Returns `None` when no valid JSON object can be located.
fn parse_synthesis_response(text: &str) -> Option<SynthesisUpdate> {
    let json_str = extract_json_object(text)?;

    match serde_json::from_str::<SynthesisUpdate>(json_str) {
        Ok(update) => Some(update),
        Err(e) => {
            tracing::debug!(
                error = %e,
                raw = %text.chars().take(200).collect::<String>(),
                "Failed to parse synthesis response as JSON"
            );
            None
        }
    }
}

/// Locate a JSON object (`{…}`) in arbitrary text, preferring the
/// content inside a ` ```json ` code fence when present.
fn extract_json_object(text: &str) -> Option<&str> {
    // Pattern 1: ```json … ``` block
    if let Some(start) = text.find("```json") {
        let after_fence = &text[start + 7..];
        if let Some(end) = after_fence.find("```") {
            return Some(after_fence[..end].trim());
        }
    }

    // Pattern 2: bare { … }
    let open = text.find('{')?;
    let close = text.rfind('}')?;
    if close > open {
        Some(&text[open..=close])
    } else {
        None
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use memory::Memory;

    use super::*;
    use crate::profile::Verbosity;

    // ── Helpers ──────────────────────────────────────────────────

    /// Build a minimal `ProfileHook` for testing lifecycle callbacks.
    ///
    /// Uses a temp directory so profile.json is never written to the
    /// real project during tests.
    fn make_hook(tmp: &tempfile::TempDir) -> ProfileHook {
        let client = DeepSeekClient::new("test-key");
        ProfileHook::new(
            tmp.path().to_path_buf(),
            "deepseek-chat".to_string(),
            client,
        )
    }

    fn make_memory() -> SharedMemory {
        Arc::new(RwLock::new(Memory::new()))
    }

    /// A dummy ToolCall for tests that only need the name.
    fn dummy_tool_call(name: &str) -> ToolCall {
        ToolCall {
            index: 0,
            id: format!("call_{name}"),
            kind: provider::ToolCallKind::Function,
            function: provider::ToolCallFunction {
                name: name.to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    // ── [PROFILE] injection ──────────────────────────────────────

    #[test]
    fn test_injects_profile_message() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_hook(&tmp);
        let memory = make_memory();

        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        let profile_msg = mem
            .messages
            .iter()
            .find(|m| m.role == Role::System && m.content.starts_with(PROFILE_MARKER));

        assert!(
            profile_msg.is_some(),
            "expected [PROFILE] System message after on_llm_start"
        );
        let content = &profile_msg.unwrap().content;
        assert!(
            content.contains("Sessions:"),
            "should contain session count"
        );
        assert!(content.contains("Language:"), "should contain language");
    }

    #[test]
    fn test_removes_old_profile_message_on_refresh() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_hook(&tmp);
        let memory = make_memory();

        // Pre-seed with a stale [PROFILE] message.
        {
            let mut mem = memory.write().unwrap();
            mem.messages.insert(
                0,
                Message::new(Role::System, "[PROFILE]\n- Sessions: 1\n- Language: en-US"),
            );
        }

        // Run the hook — should replace, not duplicate.
        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        let count = mem
            .messages
            .iter()
            .filter(|m| m.role == Role::System && m.content.starts_with(PROFILE_MARKER))
            .count();
        assert_eq!(
            count, 1,
            "[PROFILE] message should be unique (not duplicated)"
        );
    }

    #[test]
    fn test_preserves_non_profile_system_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_hook(&tmp);
        let memory = make_memory();

        // Pre-seed with a regular System message.
        {
            let mut mem = memory.write().unwrap();
            mem.messages
                .insert(0, Message::new(Role::System, "Regular system prompt"));
        }

        hook.on_llm_start("test", &memory);

        let mem = memory.read().unwrap();
        assert!(
            mem.messages
                .iter()
                .any(|m| m.role == Role::System && m.content == "Regular system prompt"),
            "non-profile System messages should survive"
        );
    }

    // ── Tool tracking ────────────────────────────────────────────

    #[test]
    fn test_tracks_tool_call_stats() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_hook(&tmp);
        let _memory = make_memory();

        let tc = dummy_tool_call("read");

        // Simulate: call -> success, call -> failure, call -> success.
        hook.before_tool_call("test", &tc).unwrap();
        hook.after_tool_call("test", &tc, "output");
        // total=1, success=1, fail=0

        hook.before_tool_call("test", &tc).unwrap();
        hook.on_tool_failed("test", &tc, "permission denied");
        // total=2, success=1, fail=1

        hook.before_tool_call("test", &tc).unwrap();
        hook.after_tool_call("test", &tc, "output");
        // total=3, success=2, fail=1

        let store = hook.store.read().unwrap();
        let stats = store.profile.tool_stats.get("read").unwrap();
        assert_eq!(stats.total_calls, 3);
        assert_eq!(stats.successes, 2);
        assert_eq!(stats.failures, 1);
    }

    #[test]
    fn test_tracks_multiple_tools_independently() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_hook(&tmp);
        let _memory = make_memory();

        let read_tc = dummy_tool_call("read");
        let edit_tc = dummy_tool_call("edit");

        hook.before_tool_call("test", &read_tc).unwrap();
        hook.after_tool_call("test", &read_tc, "");

        hook.before_tool_call("test", &edit_tc).unwrap();
        hook.after_tool_call("test", &edit_tc, "");

        hook.before_tool_call("test", &edit_tc).unwrap();
        hook.on_tool_failed("test", &edit_tc, "");

        let store = hook.store.read().unwrap();
        assert_eq!(store.profile.tool_stats.get("read").unwrap().total_calls, 1);
        assert_eq!(store.profile.tool_stats.get("edit").unwrap().total_calls, 2);
    }

    #[test]
    fn test_rejected_computed_from_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_hook(&tmp);

        let tc = dummy_tool_call("shell");
        // call but never succeed or fail (e.g. rejected by SandboxHook)
        hook.before_tool_call("test", &tc).unwrap();

        let store = hook.store.read().unwrap();
        let stats = store.profile.tool_stats.get("shell").unwrap();
        assert_eq!(stats.total_calls, 1);
        assert_eq!(stats.successes, 0);
        assert_eq!(stats.failures, 0);
        assert_eq!(stats.rejected(), 1);
    }

    // ── Language detection ───────────────────────────────────────

    #[test]
    fn test_language_detection_chinese() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_hook(&tmp);
        let memory = make_memory();

        hook.on_run_start("test", "帮我写一个函数", &memory);

        let store = hook.store.read().unwrap();
        assert_eq!(store.profile.language_preference, "zh-CN");
    }

    #[test]
    fn test_language_sticks_after_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_hook(&tmp);
        let memory = make_memory();

        // First message: Chinese.
        hook.on_run_start("s1", "你好", &memory);
        // Second message: English.
        hook.on_run_start("s2", "hello world", &memory);

        let store = hook.store.read().unwrap();
        assert_eq!(
            store.profile.language_preference, "zh-CN",
            "should remain zh-CN after English-only message"
        );
    }

    #[test]
    fn test_language_stays_en_for_ascii() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_hook(&tmp);
        let memory = make_memory();

        hook.on_run_start("test", "write a function", &memory);

        let store = hook.store.read().unwrap();
        assert_eq!(store.profile.language_preference, "en-US");
    }

    // ── Session counter ──────────────────────────────────────────

    #[test]
    fn test_total_sessions_incremented() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = make_hook(&tmp);
        let memory = make_memory();

        // Run fewer than SYNTHESIS_INTERVAL iterations so the
        // test stays in the rule-engine code path (no LLM call).
        // The increment logic is independent of synthesis triggering.
        for _ in 0..4 {
            hook.on_run_finish("test", &RunOutcome::Cancelled, &memory);
        }

        let store = hook.store.read().unwrap();
        assert_eq!(store.profile.total_sessions, 4);
    }

    // ── Response parsing ─────────────────────────────────────────

    #[test]
    fn test_parse_clean_json() {
        let json = r#"{"preferences":["likes rust"],"avoidances":[],"expertise_signals":[],"coding_conventions":[],"verbosity":"concise","language_preference":"en-US"}"#;
        let update = parse_synthesis_response(json).unwrap();
        assert_eq!(update.preferences, vec!["likes rust"]);
        assert!(update.avoidances.is_empty());
        assert_eq!(update.verbosity, Verbosity::Concise);
        assert_eq!(update.language_preference, "en-US");
    }

    #[test]
    fn test_parse_json_in_code_block() {
        let text = "Some preamble text\n```json\n{\"preferences\":[\"prefers async\"],\"avoidances\":[],\"expertise_signals\":[],\"coding_conventions\":[],\"verbosity\":\"normal\",\"language_preference\":\"\"}\n```\nMore text";
        let update = parse_synthesis_response(text).unwrap();
        assert_eq!(update.preferences, vec!["prefers async"]);
    }

    #[test]
    fn test_parse_bare_json_with_surrounding_text() {
        let text = "Here's the profile: {\"preferences\":[\"test\"],\"avoidances\":[],\"expertise_signals\":[],\"coding_conventions\":[],\"verbosity\":\"detailed\",\"language_preference\":\"\"} done.";
        let update = parse_synthesis_response(text).unwrap();
        assert_eq!(update.preferences, vec!["test"]);
        assert_eq!(update.verbosity, Verbosity::Detailed);
    }

    #[test]
    fn test_parse_malformed_returns_none() {
        assert!(parse_synthesis_response("not json at all").is_none());
        assert!(parse_synthesis_response("").is_none());
    }

    #[test]
    fn test_parse_partial_json_uses_defaults() {
        // Missing fields should get defaults.
        let json = r#"{"preferences":["one"]}"#;
        let update = parse_synthesis_response(json).unwrap();
        assert_eq!(update.preferences, vec!["one"]);
        assert!(update.avoidances.is_empty());
        assert!(update.expertise_signals.is_empty());
        assert_eq!(update.verbosity, Verbosity::Normal); // default
    }
}
