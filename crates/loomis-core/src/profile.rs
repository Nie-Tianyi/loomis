//! User profile — accumulates behavioral signals across sessions and
//! synthesises them into a compact `[PROFILE]` System message that
//! personalises the agent's responses.
//!
//! # Architecture
//!
//! The profiling system has two layers:
//!
//! **Real-time rule engine** (in [`ProfileHook`](crate::hooks::ProfileHook)):
//! tool-call counters, language detection, session count — updated
//! synchronously in hook callbacks at zero token cost.
//!
//! **Periodic LLM synthesis** (also in `ProfileHook`): every N sessions,
//! the most recent conversation context is sent to a cheap flash model
//! which updates the natural-language fields (`preferences`, `avoidances`,
//! `expertise_signals`, `coding_conventions`).
//!
//! # Persistence
//!
//! The profile lives at `<workspace>/.loomis/profile.json`.  Users can
//! open this file directly to inspect or hand-edit their profile — no
//! TUI command needed.  The file is pretty-printed JSON for readability.
//!
//! # Data model
//!
//! | Field | Updated by | Injected into System prompt? |
//! |---|---|---|
//! | `total_sessions` | Rule engine | Yes (always) |
//! | `tool_stats` | Rule engine | No (stored only) |
//! | `language_preference` | Rule engine + Synthesis | Yes |
//! | `verbosity` | Synthesis | Yes |
//! | `coding_conventions` | Synthesis | Yes |
//! | `preferences` | Synthesis | Yes |
//! | `avoidances` | Synthesis | Yes |
//! | `expertise_signals` | Synthesis | Yes |

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── Marker ──────────────────────────────────────────────────────────────────

/// Prefix that identifies an injected `[PROFILE]` System message.
///
/// Used by [`ProfileHook`](crate::hooks::ProfileHook) for idempotent
/// remove-then-reinsert, following the same convention as
/// `[SKILL:`, `[TODO]`, and `[PLAN_MODE]`.
pub const PROFILE_MARKER: &str = "[PROFILE]";

// ── Verbosity ───────────────────────────────────────────────────────────────

/// How much detail the user prefers in agent responses.
///
/// Deserialized from `profile.json` as lowercase snake_case
/// (`"concise"`, `"normal"`, `"detailed"`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Verbosity {
    /// Short, factual answers — no narration, no justification.
    Concise,
    /// Balanced: explain the why but skip the obvious.
    #[default]
    Normal,
    /// Thorough explanations with reasoning, alternatives, and context.
    Detailed,
}

impl Verbosity {
    /// Human-readable label used in the `[PROFILE]` System message.
    pub fn as_str(self) -> &'static str {
        match self {
            Verbosity::Concise => "concise",
            Verbosity::Normal => "normal",
            Verbosity::Detailed => "detailed",
        }
    }
}

// ── ToolStats ───────────────────────────────────────────────────────────────

/// Per-tool invocation counters aggregated across all sessions.
///
/// `rejected` calls are **not** tracked directly because the
/// `AgentHook` trait has no `on_tool_rejected` callback.  Instead,
/// they are computed from `total_calls - (successes + failures)`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct ToolStats {
    /// Every `before_tool_call` for this tool.
    pub total_calls: u64,
    /// Successful executions (`after_tool_call`).
    pub successes: u64,
    /// Failed executions (`on_tool_failed`).
    pub failures: u64,
}

impl ToolStats {
    /// Calls that were neither successful nor failed — blocked, rejected,
    /// or swallowed by another hook before execution.
    pub fn rejected(&self) -> u64 {
        self.total_calls
            .saturating_sub(self.successes + self.failures)
    }
}

// ── UserProfile ─────────────────────────────────────────────────────────────

/// The persistent user profile — one per workspace.
///
/// All fields have sensible defaults so a missing or corrupt
/// `profile.json` is not an error condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    /// Total number of agent runs (both success and error).
    pub total_sessions: u64,
    /// Per-tool invocation statistics.
    #[serde(default)]
    pub tool_stats: HashMap<String, ToolStats>,
    /// Detected user language: `"zh-CN"` or `"en-US"`.
    #[serde(default = "default_language")]
    pub language_preference: String,
    /// How much detail the user prefers.
    #[serde(default)]
    pub verbosity: Verbosity,
    /// Observed coding conventions (e.g. `"snake_case"`, `"中文注释"`).
    #[serde(default)]
    pub coding_conventions: Vec<String>,
    /// Free-form natural-language preferences synthesised by the flash model.
    #[serde(default)]
    pub preferences: Vec<String>,
    /// Things the user clearly avoids (e.g. `"过度抽象"`, `"unsafe code"`).
    #[serde(default)]
    pub avoidances: Vec<String>,
    /// Demonstrable expertise indicators (e.g. `"Rust 中级"`).
    #[serde(default)]
    pub expertise_signals: Vec<String>,
    /// `total_sessions` value when the LLM synthesis last ran.
    /// Used to decide whether another synthesis is due.
    #[serde(default)]
    pub last_synthesis_session: u64,
    /// ISO 8601 timestamp of the most recent profile update.
    pub updated_at: String,
    /// ISO 8601 timestamp of profile creation.
    pub created_at: String,
}

fn default_language() -> String {
    "en-US".to_string()
}

impl Default for UserProfile {
    fn default() -> Self {
        Self::new()
    }
}

impl UserProfile {
    /// A fresh profile with zeroed counters, `en-US`, `Normal` verbosity,
    /// and empty preference/avoidance/convention/expertise vectors.
    pub fn new() -> Self {
        let now = agent_oxide::util::iso8601_now();
        Self {
            total_sessions: 0,
            tool_stats: HashMap::new(),
            language_preference: default_language(),
            verbosity: Verbosity::default(),
            coding_conventions: Vec::new(),
            preferences: Vec::new(),
            avoidances: Vec::new(),
            expertise_signals: Vec::new(),
            last_synthesis_session: 0,
            updated_at: now.clone(),
            created_at: now,
        }
    }
}

// ── ProfileStore ────────────────────────────────────────────────────────────

/// Loads, holds, and persists a [`UserProfile`] to disk.
///
/// The store is wrapped in `Arc<RwLock<ProfileStore>>` inside
/// [`ProfileHook`](crate::hooks::ProfileHook) so it can be read for
/// System-message injection and written for stat updates concurrently.
pub struct ProfileStore {
    /// The current profile — mutate in place, then call [`save`](Self::save).
    pub profile: UserProfile,
    /// Workspace root for resolving `.loomis/profile.json`.
    workspace_root: PathBuf,
}

impl ProfileStore {
    /// Load the profile from `<workspace_root>/.loomis/profile.json`.
    ///
    /// If the file is missing or corrupt, returns a default
    /// [`UserProfile`] — this is not an error because a first-time
    /// user has no profile yet.
    pub fn load(workspace_root: &Path) -> Self {
        let path = profile_path(workspace_root);
        let profile = match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Corrupt profile.json — using defaults",
                );
                UserProfile::new()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // First run — no profile yet. This is the common case.
                UserProfile::new()
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Cannot read profile.json — using defaults",
                );
                UserProfile::new()
            }
        };
        Self {
            profile,
            workspace_root: workspace_root.to_path_buf(),
        }
    }

    /// Persist the current profile as pretty-printed JSON.
    ///
    /// Creates `.loomis/` if it doesn't exist.  Called from
    /// [`ProfileHook::on_run_finish`] after every agent run.
    pub fn save(&self) {
        let path = profile_path(&self.workspace_root);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&self.profile) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, &json) {
                    tracing::error!(
                        path = %path.display(),
                        error = %e,
                        "Failed to write profile.json",
                    );
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to serialise profile");
            }
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Absolute path to the profile file.
fn profile_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".loomis").join("profile.json")
}

/// Build the `[PROFILE]` System message injected by
/// [`ProfileHook::on_llm_start`](crate::hooks::ProfileHook).
///
/// The message is intentionally compact (a few hundred characters)
/// and only includes fields that help the LLM personalise its
/// behaviour.  Raw statistics like `tool_stats` are excluded.
pub fn build_profile_system_message(profile: &UserProfile) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(8);

    lines.push(PROFILE_MARKER.to_string());

    // Always-included fields.
    lines.push(format!("- Sessions: {}", profile.total_sessions));
    lines.push(format!("- Language: {}", profile.language_preference));
    lines.push(format!("- Verbosity: {}", profile.verbosity.as_str()));

    // Optional fields — only included when they carry signal.
    if !profile.coding_conventions.is_empty() {
        lines.push(format!(
            "- Coding conventions: {}",
            profile.coding_conventions.join(", ")
        ));
    }
    if !profile.preferences.is_empty() {
        lines.push(format!("- Preferences: {}", profile.preferences.join("; ")));
    }
    if !profile.avoidances.is_empty() {
        lines.push(format!("- Avoidances: {}", profile.avoidances.join("; ")));
    }
    if !profile.expertise_signals.is_empty() {
        lines.push(format!(
            "- Expertise: {}",
            profile.expertise_signals.join("; ")
        ));
    }

    lines.join("\n")
}

/// Crude CJK detection — returns `true` if `text` contains any
/// character in the CJK Unified Ideographs or CJK Compatibility
/// Ideographs blocks.
///
/// Used as a low-cost heuristic for detecting Chinese-language users
/// before the first LLM synthesis runs.  Once set to `"zh-CN"`, the
/// preference is sticky — the user can manually change it in
/// `profile.json`.
pub fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            c,
            '\u{4E00}'..='\u{9FFF}' // CJK Unified Ideographs
                | '\u{3400}'..='\u{4DBF}' // CJK Unified Ideographs Extension A
                | '\u{F900}'..='\u{FAFF}' // CJK Compatibility Ideographs
        )
    })
}

/// Truncate `s` to at most `max` bytes, breaking on a UTF-8 character
/// boundary so the result is always valid.
///
/// Appends a `"… [truncated N bytes]"` suffix when truncation occurs.
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Walk back from `max` to the nearest char boundary.
    let boundary = (0..=max)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    let truncated_bytes = s.len() - boundary;
    format!("{}… [truncated {} bytes]", &s[..boundary], truncated_bytes)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── UserProfile ──────────────────────────────────────────────

    #[test]
    fn test_new_profile_defaults() {
        let p = UserProfile::new();
        assert_eq!(p.total_sessions, 0);
        assert_eq!(p.language_preference, "en-US");
        assert_eq!(p.verbosity, Verbosity::Normal);
        assert!(p.coding_conventions.is_empty());
        assert!(p.preferences.is_empty());
        assert!(p.avoidances.is_empty());
        assert!(p.expertise_signals.is_empty());
        assert_eq!(p.last_synthesis_session, 0);
        assert!(!p.updated_at.is_empty(), "updated_at should be set");
        assert!(!p.created_at.is_empty(), "created_at should be set");
        assert!(p.tool_stats.is_empty());
    }

    // ── ToolStats ────────────────────────────────────────────────

    #[test]
    fn test_tool_stats_default_zeroed() {
        let s = ToolStats::default();
        assert_eq!(s.total_calls, 0);
        assert_eq!(s.successes, 0);
        assert_eq!(s.failures, 0);
    }

    #[test]
    fn test_tool_stats_rejected_computed() {
        let s = ToolStats {
            total_calls: 10,
            successes: 7,
            failures: 1,
        };
        // 10 - (7 + 1) = 2 rejected
        assert_eq!(s.rejected(), 2);
    }

    #[test]
    fn test_tool_stats_rejected_saturates_at_zero() {
        let s = ToolStats {
            total_calls: 5,
            successes: 10, // more successes than total (shouldn't happen)
            failures: 0,
        };
        assert_eq!(s.rejected(), 0, "rejected should saturate at 0");
    }

    #[test]
    fn test_tool_stats_all_succeeded() {
        let s = ToolStats {
            total_calls: 3,
            successes: 3,
            failures: 0,
        };
        assert_eq!(s.rejected(), 0);
    }

    // ── Verbosity ────────────────────────────────────────────────

    #[test]
    fn test_verbosity_as_str() {
        assert_eq!(Verbosity::Concise.as_str(), "concise");
        assert_eq!(Verbosity::Normal.as_str(), "normal");
        assert_eq!(Verbosity::Detailed.as_str(), "detailed");
    }

    #[test]
    fn test_verbosity_default_is_normal() {
        assert_eq!(Verbosity::default(), Verbosity::Normal);
    }

    // ── ProfileStore (round-trip) ────────────────────────────────

    #[test]
    fn test_profile_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        // Create a populated profile.
        let mut store = ProfileStore::load(&root);
        store.profile.total_sessions = 42;
        store.profile.language_preference = "zh-CN".to_string();
        store.profile.verbosity = Verbosity::Detailed;
        store.profile.coding_conventions = vec!["snake_case".into()];
        store.profile.preferences = vec!["prefers async".into()];
        store.profile.avoidances = vec!["avoids unsafe".into()];
        store.profile.expertise_signals = vec!["Rust expert".into()];
        store.profile.tool_stats.insert(
            "read".into(),
            ToolStats {
                total_calls: 5,
                successes: 4,
                failures: 1,
            },
        );
        store.save();

        // Reload and verify.
        let loaded = ProfileStore::load(&root);
        assert_eq!(loaded.profile.total_sessions, 42);
        assert_eq!(loaded.profile.language_preference, "zh-CN");
        assert_eq!(loaded.profile.verbosity, Verbosity::Detailed);
        assert_eq!(loaded.profile.coding_conventions, vec!["snake_case"]);
        assert_eq!(loaded.profile.preferences, vec!["prefers async"]);
        assert_eq!(loaded.profile.avoidances, vec!["avoids unsafe"]);
        assert_eq!(loaded.profile.expertise_signals, vec!["Rust expert"]);
        assert_eq!(loaded.profile.tool_stats.len(), 1);
        let read_stats = loaded.profile.tool_stats.get("read").unwrap();
        assert_eq!(read_stats.total_calls, 5);
        assert_eq!(read_stats.successes, 4);
        assert_eq!(read_stats.failures, 1);
    }

    #[test]
    fn test_profile_load_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let store = ProfileStore::load(&root);
        // Should be a fresh default profile.
        assert_eq!(store.profile.total_sessions, 0);
        assert_eq!(store.profile.language_preference, "en-US");
    }

    #[test]
    fn test_profile_load_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let looms = root.join(".loomis");
        std::fs::create_dir_all(&looms).unwrap();
        std::fs::write(looms.join("profile.json"), "not valid json {{").unwrap();

        let store = ProfileStore::load(&root);
        // Should fall back to defaults without panicking.
        assert_eq!(store.profile.total_sessions, 0);
    }

    // ── build_profile_system_message ─────────────────────────────

    #[test]
    fn test_build_message_minimal() {
        let p = UserProfile::new();
        let msg = build_profile_system_message(&p);
        assert!(
            msg.starts_with(PROFILE_MARKER),
            "should start with [PROFILE] marker, got: {msg}"
        );
        assert!(msg.contains("Sessions: 0"), "should show session count");
        assert!(msg.contains("Language: en-US"), "should show language");
        assert!(msg.contains("Verbosity: normal"), "should show verbosity");
        // Empty optional fields should be absent.
        assert!(!msg.contains("Coding conventions"), "no conventions yet");
        assert!(!msg.contains("Preferences"), "no preferences yet");
        assert!(!msg.contains("Avoidances"), "no avoidances yet");
        assert!(!msg.contains("Expertise"), "no expertise yet");
    }

    #[test]
    fn test_build_message_full() {
        let mut p = UserProfile::new();
        p.total_sessions = 7;
        p.language_preference = "zh-CN".to_string();
        p.verbosity = Verbosity::Detailed;
        p.coding_conventions = vec!["snake_case".into(), "中文注释".into()];
        p.preferences = vec!["先解释再写代码".into()];
        p.avoidances = vec!["过度抽象".into()];
        p.expertise_signals = vec!["Rust 中级".into()];

        let msg = build_profile_system_message(&p);
        assert!(msg.contains("Sessions: 7"));
        assert!(msg.contains("Language: zh-CN"));
        assert!(msg.contains("Verbosity: detailed"));
        assert!(msg.contains("snake_case, 中文注释"));
        assert!(msg.contains("先解释再写代码"));
        assert!(msg.contains("过度抽象"));
        assert!(msg.contains("Rust 中级"));
    }

    // ── has_cjk ──────────────────────────────────────────────────

    #[test]
    fn test_has_cjk_chinese() {
        assert!(has_cjk("你好世界"), "Chinese text should trigger CJK");
        assert!(has_cjk("hello 世界"), "mixed text should trigger CJK");
    }

    #[test]
    fn test_has_cjk_ascii_only() {
        assert!(!has_cjk("hello world"), "ASCII should not trigger CJK");
        assert!(!has_cjk(""), "empty string should not trigger CJK");
    }

    #[test]
    fn test_has_cjk_japanese() {
        // Hiragana/Katakana are not in the CJK block, so this is
        // expected behaviour — Japanese detection would need
        // additional ranges.
        assert!(
            !has_cjk("こんにちは"),
            "hiragana alone should not trigger CJK"
        );
    }

    // ── truncate ─────────────────────────────────────────────────

    #[test]
    fn test_truncate_short_string_unchanged() {
        let s = "hello";
        assert_eq!(truncate(s, 100), s);
    }

    #[test]
    fn test_truncate_exact_boundary() {
        let s = "abcdef";
        assert_eq!(truncate(s, 3), "abc… [truncated 3 bytes]");
    }

    #[test]
    fn test_truncate_multibyte_safe() {
        // "你好世界" = 12 bytes (3 bytes per char)
        let s = "你好世界";
        let result = truncate(s, 6);
        // 6 bytes = first 2 chars = "你好"
        assert!(
            result.starts_with("你好"),
            "should truncate at char boundary, got: {result}"
        );
        assert!(
            result.contains("… [truncated"),
            "should have truncation suffix"
        );
    }

    #[test]
    fn test_truncate_mid_char_boundary() {
        // "你好" = 6 bytes, max=4 should fall back to 3-byte boundary → "你"
        let s = "你好";
        let result = truncate(s, 4);
        assert!(result.starts_with('你'), "should land on char boundary");
        assert!(!result.starts_with("你好"), "should not keep both chars");
    }
}
