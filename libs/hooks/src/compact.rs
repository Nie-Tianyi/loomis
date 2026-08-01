//! # Compaction
//!
//! Two-tier context compaction:
//!
//! - [`MicroCompactHook`] — an [`AgentHook`] that clears old tool-output
//!   content in-place during `on_llm_start`.
//!
//! - [`MacroCompactConfig`] — configuration for full LLM summarisation.
//!   The agent loop calls out to a cheap model when the token budget
//!   is exceeded, draining old non-System messages and inserting a summary
//!   as a new System message.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use engine::AgentHook;
use memory::SharedMemory;
use provider::{CompletionRequest, LLMClient, Message, Role};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Fallback placeholder used when a compacted tool output's arguments
/// cannot be parsed into a contextual description.
pub const COMPACTED_TOOL_OUTPUT_PLACEHOLDER: &str = "[Old tool result content cleared]";

/// Prefix shared by all contextual compaction placeholders — also used to
/// detect already-compacted tool outputs (see [`format_compact_placeholder`]).
pub const COMPACTED_TOOL_OUTPUT_PREFIX: &str = "[Cleared: ";

/// Maximum characters kept from a tool-call argument (command / pattern)
/// when embedded in a contextual placeholder.
const PLACEHOLDER_ARG_TRUNCATE: usize = 60;

/// Default number of recent tool outputs to preserve during compaction.
pub const DEFAULT_KEEP_RECENT_TOOL_OUTPUTS: usize = 10;

/// Default set of tool names whose outputs are eligible for compaction.
pub const DEFAULT_COMPACT_ELIGIBLE_TOOLS: &[&str] =
    &["read", "shell", "grep", "glob", "edit", "write", "ls"];

/// Default character budget before macro-compaction triggers.
pub const DEFAULT_COMPACT_CHAR_LIMIT: usize = 2_000_000;

/// Default token budget before macro-compaction triggers.
/// 1M tokens — conservative for modern 1M+ context windows,
/// leaving ample headroom for completion tokens.
pub const DEFAULT_COMPACT_TOKEN_LIMIT: usize = 1_000_000;

/// Default number of non-System messages preserved during macro-compaction drain.
pub const DEFAULT_KEEP_LAST_N: usize = 10;

// ── CompactError ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum CompactError {
    /// The summarisation model returned an error.
    SummariserFailed(String),
}

impl fmt::Display for CompactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SummariserFailed(reason) => write!(f, "summariser failed: {reason}"),
        }
    }
}

impl std::error::Error for CompactError {}

// ── MicroCompactHook ──────────────────────────────────────────────────────────

/// Lightweight tool-output compaction hook.
///
/// Implements [`AgentHook`] — in `on_llm_start`, clears old tool-result
/// content in-place, replacing it with a contextual placeholder describing
/// what was cleared (tool, file path, line range — see
/// [`format_compact_placeholder`]).  The most recent `keep_recent` outputs
/// per compactable tool are preserved.
pub struct MicroCompactHook {
    /// How many of the most recent tool outputs to preserve.
    pub keep_recent: usize,
    /// Which tool names are eligible for output compaction.
    pub compact_eligible_tools: HashSet<String>,
}

impl MicroCompactHook {
    pub fn new(keep_recent: usize, compact_eligible_tools: HashSet<String>) -> Self {
        Self {
            keep_recent,
            compact_eligible_tools,
        }
    }
}

impl AgentHook for MicroCompactHook {
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        let mut mem = memory.write().expect("memory lock poisoned");
        let compacted = compact_messages(
            &mut mem.messages,
            self.keep_recent,
            &self.compact_eligible_tools,
        );
        if compacted > 0 {
            tracing::debug!(
                compacted_count = compacted,
                keep_recent = self.keep_recent,
                "micro-compaction cleared old tool outputs",
            );
        }
    }
}

// ── MacroCompactHook ──────────────────────────────────────────────────────────

/// Full LLM summarisation hook.
///
/// Implements [`AgentHook`] — in `on_llm_start`, checks whether the
/// conversation's `prompt_tokens` (from the previous LLM response, stored
/// on [`Memory::last_usage`]) exceeds `threshold` tokens.  If it does,
/// drains old non-System messages (keeping the most recent `keep_last_n`),
/// calls the compact model for a summary, and inserts it as a System message.
///
/// The LLM call blocks the agent loop via [`engine::block_on`], which
/// performs a legal `block_in_place` + `Handle::block_on` on the
/// multi-threaded runtime.  The agent loop runs in a dedicated tokio task,
/// separate from the TUI main thread — blocking here does not affect the UI.
pub struct MacroCompactHook<C: LLMClient> {
    /// Model name for summarisation (cheap model).
    pub compact_model: String,
    /// Token budget before compaction triggers (compared against
    /// `prompt_tokens` from the previous LLM response).
    pub threshold: usize,
    /// Number of non-System messages to preserve during drain.
    pub keep_last_n: usize,
    /// LLM client (same provider, different model).
    pub client: C,
    /// Set when a summarisation attempt fails — prevents retrying on every
    /// subsequent LLM call until memory grows further or compaction succeeds.
    pub compaction_failed: AtomicBool,
}

impl<C: LLMClient> MacroCompactHook<C> {
    pub fn new(compact_model: String, threshold: usize, keep_last_n: usize, client: C) -> Self {
        Self {
            compact_model,
            threshold,
            keep_last_n,
            client,
            compaction_failed: AtomicBool::new(false),
        }
    }
}

impl<C: LLMClient> AgentHook for MacroCompactHook<C> {
    fn on_llm_start(&self, _session_id: &str, memory: &SharedMemory) {
        let needs = {
            let mem = memory.read().expect("memory lock poisoned");
            match &mem.last_usage {
                Some(usage) => usage.prompt_tokens as usize > self.threshold,
                None => {
                    // No usage data yet (first LLM call of the session).
                    // Skip compaction — after this call completes,
                    // `last_usage` will be populated for the next check.
                    false
                }
            }
        };
        if !needs {
            return;
        }

        let old = {
            let mut mem = memory.write().expect("memory lock poisoned");
            drain_for_compact(&mut mem.messages, self.keep_last_n)
        };
        if old.is_empty() {
            return;
        }

        tracing::info!(
            drained_count = old.len(),
            keep_last_n = self.keep_last_n,
            "macro-compaction triggered, summarising drained messages",
        );

        // Build summarisation transcript
        let transcript: String = old
            .iter()
            .map(|m| format!("[{}]: {}", m.role.label(), m.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let prompt = format!(
            "Summarise the following conversation history concisely into a single paragraph. \
             You MUST preserve:\n\
             - Every file path the agent read or modified, with line ranges when known\n\
             - Every shell command the agent ran and its outcome (success/failure)\n\
             - All unfinished tasks and pending user requests\n\
             - All decisions the agent made and the reasoning behind them\n\
             - User preferences, constraints, and feedback\n\
             Output only the summary, no preamble or meta-commentary:\n\n{transcript}"
        );

        let request =
            CompletionRequest::new(&self.compact_model, vec![Message::new(Role::User, prompt)]);

        // Block the agent loop (not the UI — different thread).  Uses
        // `engine::block_on` — a bare `Handle::block_on` would panic here
        // because hooks run on a runtime worker thread.
        let result = engine::block_on(self.client.generate(request));

        let summary = match result {
            Ok(resp) => {
                // Summarisation succeeded — clear the failure flag.
                self.compaction_failed.store(false, Ordering::Relaxed);
                resp.choices
                    .into_iter()
                    .next()
                    .and_then(|c| c.message.content)
                    .unwrap_or_default()
            }
            Err(e) => {
                // Summarisation failed — log the error and set a flag to
                // avoid retrying on every subsequent LLM call (which would
                // burn API calls in a tight loop).
                if !self.compaction_failed.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        error = %e,
                        "Macro-compaction summarisation failed; will not retry until it succeeds once",
                    );
                }
                String::new()
            }
        };

        if !summary.is_empty() {
            tracing::info!(
                summary_len = summary.len(),
                model = %self.compact_model,
                "macro-compaction summary inserted as System message",
            );
            let mut mem = memory.write().expect("memory lock poisoned");
            mem.messages.insert(0, Message::new(Role::System, summary));
        } else {
            tracing::warn!("macro-compaction produced empty summary");
        }
    }
}

// ── Tool Output Compaction (core algorithm) ───────────────────────────────────

fn compact_messages(
    messages: &mut [Message],
    keep_recent: usize,
    compactable: &HashSet<String>,
) -> usize {
    // Pass 1: map each tool-call id to its (tool name, raw arguments JSON).
    // The arguments let the placeholder tell the agent *what* was cleared.
    let mut id_to_call: HashMap<String, (String, String)> = HashMap::new();
    for msg in messages.iter() {
        if msg.role == Role::Assistant
            && let Some(ref tool_calls) = msg.tool_calls
        {
            for tc in tool_calls {
                id_to_call.insert(
                    tc.id.clone(),
                    (tc.function.name.clone(), tc.function.arguments.clone()),
                );
            }
        }
    }

    if id_to_call.is_empty() {
        return 0;
    }

    let mut compactable_count_from_end = 0usize;
    let mut should_keep = vec![false; messages.len()];

    for (i, msg) in messages.iter().enumerate().rev() {
        if msg.role != Role::Tool {
            continue;
        }
        if is_compacted(msg) {
            continue;
        }
        if let Some(ref tool_call_id) = msg.tool_call_id
            && let Some((tool_name, _)) = id_to_call.get(tool_call_id)
            && compactable.contains(tool_name)
            && compactable_count_from_end < keep_recent
        {
            should_keep[i] = true;
            compactable_count_from_end += 1;
        }
    }

    let mut compacted = 0usize;
    for (i, msg) in messages.iter_mut().enumerate() {
        if msg.role != Role::Tool || should_keep[i] {
            continue;
        }
        if is_compacted(msg) {
            continue;
        }
        if let Some(ref tool_call_id) = msg.tool_call_id
            && let Some((tool_name, arguments)) = id_to_call.get(tool_call_id)
            && compactable.contains(tool_name)
        {
            msg.content = format_compact_placeholder(tool_name, arguments);
            compacted += 1;
        }
    }

    compacted
}

/// Whether a Tool message has already been compacted (either by a
/// contextual placeholder or the legacy fallback).
fn is_compacted(msg: &Message) -> bool {
    msg.content == COMPACTED_TOOL_OUTPUT_PLACEHOLDER
        || msg.content.starts_with(COMPACTED_TOOL_OUTPUT_PREFIX)
}

/// Build a contextual placeholder for a compacted tool output.
///
/// Extracts the file path / command / pattern from the tool-call arguments
/// (a JSON string) so the agent still knows *what* was cleared and can
/// re-fetch it cheaply (e.g. a targeted `read` with `offset`/`limit`).
/// Falls back to [`COMPACTED_TOOL_OUTPUT_PLACEHOLDER`] when the arguments
/// cannot be parsed or lack the expected fields.
fn format_compact_placeholder(tool_name: &str, arguments_json: &str) -> String {
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments_json) else {
        return COMPACTED_TOOL_OUTPUT_PLACEHOLDER.to_string();
    };

    let get = |key: &str| args.get(key).and_then(|v| v.as_str()).map(str::to_string);

    let path = match tool_name {
        "read" | "edit" | "write" => get("file_path"),
        "ls" => get("path"),
        _ => None,
    };

    let description = match tool_name {
        "read" => match (path, line_range(&args)) {
            (Some(p), Some(r)) => format!("read {p}{r}"),
            (Some(p), None) => format!("read {p}"),
            _ => return COMPACTED_TOOL_OUTPUT_PLACEHOLDER.to_string(),
        },
        "shell" => match get("command") {
            Some(cmd) => format!("shell \"{}\"", truncate(&cmd)),
            None => return COMPACTED_TOOL_OUTPUT_PLACEHOLDER.to_string(),
        },
        "grep" => match get("pattern") {
            Some(pattern) => match get("path_glob") {
                Some(p) => format!("grep \"{}\" in {p}", truncate(&pattern)),
                None => format!("grep \"{}\"", truncate(&pattern)),
            },
            None => return COMPACTED_TOOL_OUTPUT_PLACEHOLDER.to_string(),
        },
        "glob" => match get("pattern") {
            Some(pattern) => format!("glob \"{}\"", truncate(&pattern)),
            None => return COMPACTED_TOOL_OUTPUT_PLACEHOLDER.to_string(),
        },
        "edit" => match path {
            Some(p) => match get("old_content") {
                // The matched fragment is what was changed — a truncated
                // snippet helps the agent remember which part of the file.
                Some(old) => format!("edit {p}: \"{}\"", truncate(&old)),
                None => format!("edit {p}"),
            },
            None => return COMPACTED_TOOL_OUTPUT_PLACEHOLDER.to_string(),
        },
        // `write` content is deliberately omitted — it can be as large as
        // the output itself and adds no navigation value.
        "write" => match path {
            Some(p) => format!("write {p}"),
            None => return COMPACTED_TOOL_OUTPUT_PLACEHOLDER.to_string(),
        },
        "ls" => match path {
            Some(p) => format!("ls {p}"),
            // `ls` with no path lists the workspace root — still useful.
            None => "ls (workspace root)".to_string(),
        },
        _ => return COMPACTED_TOOL_OUTPUT_PLACEHOLDER.to_string(),
    };

    format!("{COMPACTED_TOOL_OUTPUT_PREFIX}{description} — re-fetch with the tool if needed]")
}

/// Render a line range from `read` arguments (`offset`/`limit`).  Returns
/// `None` when no range info is present. (`edit` is content-based and has
/// no line-range arguments.)
fn line_range(args: &serde_json::Value) -> Option<String> {
    match (args.get("offset"), args.get("limit")) {
        // `offset` is 1-indexed, `limit` is a count — render as inclusive.
        (Some(offset), Some(limit)) => {
            let (Some(offset), Some(limit)) = (offset.as_u64(), limit.as_u64()) else {
                return None;
            };
            Some(format!(
                ":{}-{}",
                offset,
                offset.saturating_add(limit).saturating_sub(1)
            ))
        }
        (Some(offset), None) => Some(format!(":{}", offset.as_u64()?)),
        _ => None,
    }
}

/// Truncate a string to [`PLACEHOLDER_ARG_TRUNCATE`] chars, appending an
/// ellipsis when cut.  Preserves char boundaries.
fn truncate(s: &str) -> String {
    if s.chars().count() <= PLACEHOLDER_ARG_TRUNCATE {
        return s.to_string();
    }
    let cut = s.floor_char_boundary(PLACEHOLDER_ARG_TRUNCATE);
    format!("{}…", &s[..cut])
}

// ── Private helpers (also used by tests) ──────────────────────────────────────

fn drain_for_compact(messages: &mut Vec<Message>, keep_last_n: usize) -> Vec<Message> {
    let non_system_count = messages.iter().filter(|m| m.role != Role::System).count();
    let keep = std::cmp::min(keep_last_n, non_system_count);
    let to_drain = non_system_count.saturating_sub(keep);
    if to_drain == 0 {
        return Vec::new();
    }
    let mut drained = Vec::with_capacity(to_drain);
    let mut kept = Vec::with_capacity(messages.len() - to_drain);
    let mut drained_so_far = 0;
    for msg in messages.drain(..) {
        if msg.role != Role::System && drained_so_far < to_drain {
            drained.push(msg);
            drained_so_far += 1;
        } else {
            kept.push(msg);
        }
    }
    *messages = kept;
    drained
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use provider::{ToolCall, ToolCallFunction, ToolCallKind};

    fn user_msg(content: &str) -> Message {
        Message::new(Role::User, content)
    }

    fn assistant_msg(content: &str) -> Message {
        Message::new(Role::Assistant, content)
    }

    fn sys_msg(content: &str) -> Message {
        Message::new(Role::System, content)
    }

    fn assistant_with_tool_call(id: &str, tool_name: &str) -> Message {
        assistant_with_tool_call_args(id, tool_name, "{}")
    }

    fn assistant_with_tool_call_args(id: &str, tool_name: &str, arguments: &str) -> Message {
        Message::assistant_with_tools(
            "",
            vec![ToolCall {
                index: 0,
                id: id.to_string(),
                kind: ToolCallKind::Function,
                function: ToolCallFunction {
                    name: tool_name.to_string(),
                    arguments: arguments.to_string(),
                },
            }],
        )
    }

    fn tool_msg(id: &str, content: &str) -> Message {
        Message::tool_result(id, content)
    }

    fn compactable_set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // ── compact_messages tests ─────────────────────────────────────────────

    #[test]
    fn test_compact_tool_output_noop_when_no_tools() {
        let mut messages = vec![user_msg("hello"), assistant_msg("hi there")];
        let compacted = compact_messages(&mut messages, 5, &compactable_set(&["read"]));
        assert_eq!(compacted, 0);
    }

    #[test]
    fn test_compact_tool_output_preserves_recent() {
        let mut messages = vec![
            sys_msg("system prompt"),
            assistant_with_tool_call("call_1", "read"),
            tool_msg("call_1", "file contents one"),
            assistant_msg("processed file one"),
            assistant_with_tool_call("call_2", "read"),
            tool_msg("call_2", "file contents two"),
            assistant_msg("processed file two"),
            assistant_with_tool_call("call_3", "read"),
            tool_msg("call_3", "file contents three"),
            assistant_msg("processed file three"),
        ];
        let compacted = compact_messages(&mut messages, 2, &compactable_set(&["read"]));
        assert_eq!(compacted, 1);
        assert_eq!(messages[2].content, COMPACTED_TOOL_OUTPUT_PLACEHOLDER);
        assert_eq!(messages[5].content, "file contents two");
        assert_eq!(messages[8].content, "file contents three");
    }

    #[test]
    fn test_compact_tool_output_keep_zero_compacts_all() {
        let mut messages = vec![
            assistant_with_tool_call("call_1", "shell"),
            tool_msg("call_1", "command output 1"),
            assistant_with_tool_call("call_2", "shell"),
            tool_msg("call_2", "command output 2"),
        ];
        let compacted = compact_messages(&mut messages, 0, &compactable_set(&["shell"]));
        assert_eq!(compacted, 2);
    }

    #[test]
    fn test_compact_tool_output_respects_filter() {
        let mut messages = vec![
            assistant_with_tool_call("call_1", "read"),
            tool_msg("call_1", "read output"),
            assistant_with_tool_call("call_2", "calculator"),
            tool_msg("call_2", "42"),
        ];
        let compacted = compact_messages(&mut messages, 0, &compactable_set(&["read"]));
        assert_eq!(compacted, 1);
        assert_eq!(messages[3].content, "42");
    }

    #[test]
    fn test_compact_tool_output_skips_already_compacted() {
        let mut messages = vec![
            assistant_with_tool_call("call_1", "read"),
            tool_msg("call_1", "read output 1"),
            assistant_with_tool_call("call_2", "read"),
            tool_msg("call_2", "read output 2"),
            assistant_with_tool_call("call_3", "read"),
            tool_msg("call_3", "read output 3"),
        ];
        let c1 = compact_messages(&mut messages, 1, &compactable_set(&["read"]));
        assert_eq!(c1, 2);
        let c2 = compact_messages(&mut messages, 2, &compactable_set(&["read"]));
        assert_eq!(c2, 0);
    }

    #[test]
    fn test_compact_tool_output_empty_compactable_set() {
        let mut messages = vec![
            assistant_with_tool_call("call_1", "read"),
            tool_msg("call_1", "output"),
        ];
        let compacted = compact_messages(&mut messages, 0, &HashSet::new());
        assert_eq!(compacted, 0);
    }

    #[test]
    fn test_default_compact_eligible_tools_is_non_empty() {
        assert!(!DEFAULT_COMPACT_ELIGIBLE_TOOLS.is_empty());
    }

    #[test]
    fn test_placeholder_is_non_empty() {
        assert!(!COMPACTED_TOOL_OUTPUT_PLACEHOLDER.is_empty());
    }

    // ── format_compact_placeholder tests ────────────────────────────────────

    #[test]
    fn test_placeholder_read_includes_path_and_range() {
        let p = format_compact_placeholder(
            "read",
            r#"{"file_path": "src/main.rs", "offset": 10, "limit": 50}"#,
        );
        assert_eq!(
            p,
            "[Cleared: read src/main.rs:10-59 — re-fetch with the tool if needed]"
        );
    }

    #[test]
    fn test_placeholder_read_without_range() {
        let p = format_compact_placeholder("read", r#"{"file_path": "src/main.rs"}"#);
        assert_eq!(
            p,
            "[Cleared: read src/main.rs — re-fetch with the tool if needed]"
        );
    }

    #[test]
    fn test_placeholder_read_offset_only() {
        let p = format_compact_placeholder("read", r#"{"file_path": "f.rs", "offset": 42}"#);
        assert!(p.contains("read f.rs:42"), "got: {p}");
    }

    #[test]
    fn test_placeholder_shell_includes_truncated_command() {
        let long_cmd = "cargo ".repeat(20); // 120 chars — over the 60-char cap
        let p = format_compact_placeholder("shell", &format!(r#"{{"command": "{long_cmd}"}}"#));
        assert!(p.starts_with("[Cleared: shell \""), "got: {p}");
        assert!(
            p.ends_with("…\" — re-fetch with the tool if needed]"),
            "got: {p}"
        );
        assert!(p.len() < 120, "placeholder not truncated: {p}");
    }

    #[test]
    fn test_placeholder_grep_glob_edit_ls() {
        let grep = format_compact_placeholder(
            "grep",
            r#"{"pattern": "fn\\s+main", "path_glob": "src/**/*.rs"}"#,
        );
        assert_eq!(
            grep,
            "[Cleared: grep \"fn\\s+main\" in src/**/*.rs — re-fetch with the tool if needed]"
        );

        let glob = format_compact_placeholder("glob", r#"{"pattern": "**/*.rs"}"#);
        assert_eq!(
            glob,
            "[Cleared: glob \"**/*.rs\" — re-fetch with the tool if needed]"
        );

        let edit = format_compact_placeholder(
            "edit",
            r#"{"file_path": "src/fs.rs", "old_content": "fn foo() {\n    let x = 1;\n}"}"#,
        );
        assert_eq!(
            edit,
            "[Cleared: edit src/fs.rs: \"fn foo() {\n    let x = 1;\n}\" — re-fetch with the tool if needed]"
        );

        let ls = format_compact_placeholder("ls", r#"{"path": "src/"}"#);
        assert_eq!(ls, "[Cleared: ls src/ — re-fetch with the tool if needed]");
    }

    #[test]
    fn test_placeholder_ls_without_path() {
        let p = format_compact_placeholder("ls", "{}");
        assert_eq!(
            p,
            "[Cleared: ls (workspace root) — re-fetch with the tool if needed]"
        );
    }

    #[test]
    fn test_placeholder_write_omits_content() {
        let p = format_compact_placeholder(
            "write",
            r#"{"file_path": "output/result.md", "content": "HUGE_CONTENT_SHOULD_NOT_APPEAR"}"#,
        );
        assert_eq!(
            p,
            "[Cleared: write output/result.md — re-fetch with the tool if needed]"
        );
        assert!(!p.contains("HUGE_CONTENT_SHOULD_NOT_APPEAR"));
    }

    #[test]
    fn test_placeholder_fallback_on_invalid_json() {
        assert_eq!(
            format_compact_placeholder("read", "not json at all"),
            COMPACTED_TOOL_OUTPUT_PLACEHOLDER
        );
    }

    #[test]
    fn test_placeholder_fallback_on_missing_fields() {
        // Args parse but lack file_path — not enough to be useful.
        assert_eq!(
            format_compact_placeholder("read", "{}"),
            COMPACTED_TOOL_OUTPUT_PLACEHOLDER
        );
        // Unknown tool names are not summarised.
        assert_eq!(
            format_compact_placeholder("calculator", r#"{"expr": "1+1"}"#),
            COMPACTED_TOOL_OUTPUT_PLACEHOLDER
        );
    }

    // ── compact_messages integration with contextual placeholders ─────────

    #[test]
    fn test_compact_uses_contextual_placeholder() {
        let mut messages = vec![
            assistant_with_tool_call_args(
                "call_1",
                "read",
                r#"{"file_path": "src/fs.rs", "offset": 1, "limit": 320}"#,
            ),
            tool_msg("call_1", "full file contents..."),
            assistant_msg("processed"),
        ];
        let compacted = compact_messages(&mut messages, 0, &compactable_set(&["read"]));
        assert_eq!(compacted, 1);
        assert!(
            messages[1]
                .content
                .starts_with("[Cleared: read src/fs.rs:1-320")
        );
    }

    #[test]
    fn test_compact_skips_contextual_placeholders() {
        let mut messages = vec![
            assistant_with_tool_call_args("call_1", "read", r#"{"file_path": "src/fs.rs"}"#),
            tool_msg("call_1", "file contents"),
        ];
        let c1 = compact_messages(&mut messages, 0, &compactable_set(&["read"]));
        assert_eq!(c1, 1);
        // Second pass must recognise the contextual placeholder and skip it.
        let c2 = compact_messages(&mut messages, 0, &compactable_set(&["read"]));
        assert_eq!(c2, 0);
        assert!(messages[1].content.starts_with("[Cleared: read src/fs.rs"));
    }

    // ── drain_for_compact tests ────────────────────────────────────────────

    #[test]
    fn test_drain_preserves_last_n_messages() {
        let mut messages: Vec<Message> = (0..15).map(|i| user_msg(&format!("msg_{i}"))).collect();
        let initial_len = messages.len();
        let old = drain_for_compact(&mut messages, 10);
        assert_eq!(old.len(), initial_len - 10);
        assert_eq!(messages.len(), 10);
    }

    #[test]
    fn test_drain_noop_when_fewer_than_keep() {
        let mut messages = vec![user_msg("a"), user_msg("b")];
        let old = drain_for_compact(&mut messages, 10);
        assert!(old.is_empty());
    }

    #[test]
    fn test_drain_preserves_system_messages() {
        let mut messages = vec![sys_msg("System instructions")];
        for i in 0..12 {
            messages.push(user_msg(&format!("msg_{i}")));
        }
        let old = drain_for_compact(&mut messages, 10);
        assert_eq!(old.len(), 2);
        assert_eq!(messages[0].role, Role::System);
    }

    #[test]
    fn test_role_label_all_variants() {
        assert_eq!(Role::System.label(), "System");
        assert_eq!(Role::User.label(), "User");
        assert_eq!(Role::Assistant.label(), "Assistant");
        assert_eq!(Role::Tool.label(), "Tool");
    }

    #[test]
    fn test_compact_error_display() {
        assert!(
            CompactError::SummariserFailed("test".into())
                .to_string()
                .contains("summariser failed")
        );
    }

    // ── MacroCompactHook token-based threshold tests ──────────────────────

    use memory::Memory;
    use provider::{CompletionRequest, CompletionResponse, LLMClient, ProviderError};
    use std::sync::Arc;

    /// A mock LLM client that panics if called — used to verify that
    /// `MacroCompactHook::on_llm_start` returns early when `last_usage` is
    /// `None`, without ever invoking the LLM client.
    struct PanicClient;

    impl LLMClient for PanicClient {
        async fn generate(
            &self,
            _req: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            panic!("MacroCompactHook should not call generate when last_usage is None");
        }

        async fn stream(
            &self,
            _req: CompletionRequest,
        ) -> Result<
            futures_util::stream::BoxStream<'static, Result<provider::StreamChunk, ProviderError>>,
            ProviderError,
        > {
            panic!("MacroCompactHook should not call stream when last_usage is None");
        }
    }

    #[test]
    fn test_macro_compact_skips_when_no_usage() {
        // When `last_usage` is `None` (first LLM call), compaction should
        // be skipped entirely — no LLM call, no messages modified.
        let hook: MacroCompactHook<PanicClient> = MacroCompactHook::new(
            "test-model".into(),
            10, // very low threshold — would trigger if checked
            5,
            PanicClient,
        );
        let mem: SharedMemory = Arc::new(std::sync::RwLock::new(Memory::new()));
        // mem.last_usage is None by default
        hook.on_llm_start("test-session", &mem);
        // Should have returned early — memory is still empty.
        assert!(mem.read().unwrap().messages.is_empty());
    }
}
