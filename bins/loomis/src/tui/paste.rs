//! Pasted content model — Claude Code style paste placeholders.
//!
//! When the user pastes multi-line text into the input area, the raw text is
//! **not** dumped into the editable input buffer (that would flood the input
//! line and — without bracketed paste — fire one submit per line). Instead:
//!
//! 1. The text is stored aside in a numbered [`PastedBlock`] inside
//!    [`PasteStore`].
//! 2. A compact placeholder like `[Pasted text #1 +11 lines]` is inserted
//!    into the input buffer at the cursor — the user can keep typing around
//!    it, and Backspace removes it atomically.
//! 3. On submit, [`PasteStore::expand_all`] swaps every placeholder back to
//!    its real content, producing **one** user message.
//!
//! ## Extending to images / PDFs / files
//!
//! [`PastedContent`] is the single extension point. Adding
//! `Image { media_type, data }` or `File { path, media_type }` variants only
//! requires extending [`PastedBlock::placeholder`] (the label shown inline)
//! and [`PastedBlock::expand`] (what is sent to the model); every call site
//! — input rendering, Backspace, submit expansion — works unchanged.

// ── PastedContent ────────────────────────────────────────────────────────────────

/// The payload of one paste operation.
///
/// Only text is supported today (terminals deliver clipboard text via
/// bracketed paste). Binary payloads (screenshots, dropped PDFs) will arrive
/// through different channels — clipboard image reads, drag-and-drop file
/// paths — and become new variants here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PastedContent {
    /// Plain text pasted from the clipboard.
    Text {
        /// The pasted text, newlines already normalized to `\n`.
        text: String,
        /// Number of logical lines (`text.lines().count()`), shown in the
        /// placeholder as `+N lines`.
        line_count: usize,
    },
}

// ── PastedBlock ──────────────────────────────────────────────────────────────────

/// One numbered paste block. The `id` is what the placeholder references:
/// `[Pasted text #<id> +N lines]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastedBlock {
    /// 1-based sequence number, unique within a [`PasteStore`] session.
    pub id: usize,
    /// The pasted payload.
    pub content: PastedContent,
}

impl PastedBlock {
    /// Returns the compact placeholder that lives inline in the input
    /// buffer, e.g. `[Pasted text #1 +11 lines]`.
    ///
    /// The `+N lines` suffix is omitted for single-line pastes so the label
    /// stays short: `[Pasted text #2]`.
    pub fn placeholder(&self) -> String {
        match &self.content {
            PastedContent::Text { line_count, .. } => {
                if *line_count > 1 {
                    format!("[Pasted text #{} +{} lines]", self.id, line_count)
                } else {
                    format!("[Pasted text #{}]", self.id)
                }
            }
        }
    }

    /// Returns the real content this placeholder expands to on submit.
    pub fn expand(&self) -> &str {
        match &self.content {
            PastedContent::Text { text, .. } => text,
        }
    }
}

// ── PasteStore ───────────────────────────────────────────────────────────────────

/// All paste blocks registered since the last submit.
///
/// The store is cleared after every submission so ids in a fresh input
/// always restart from 1, matching what the user sees in the placeholders.
#[derive(Debug, Default)]
pub struct PasteStore {
    /// Next id to hand out. Reset to 1 by [`PasteStore::clear`].
    next_id: usize,
    /// Registered blocks, in paste order.
    blocks: Vec<PastedBlock>,
}

impl PasteStore {
    /// Registers a multi-line paste and returns the placeholder to insert
    /// into the input buffer.
    ///
    /// `text` must already be newline-normalized (see
    /// [`normalize_newlines`]); the line count is derived from it.
    pub fn add_text(&mut self, text: String) -> String {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        let block = PastedBlock {
            id,
            content: PastedContent::Text {
                line_count: text.lines().count(),
                text,
            },
        };
        let placeholder = block.placeholder();
        self.blocks.push(block);
        placeholder
    }

    /// Replaces every registered placeholder found in `text` with its real
    /// content.
    ///
    /// Placeholders with no matching block — e.g. the user literally typed
    /// `[Pasted text #1]` by hand — are left untouched, so collisions with
    /// user-typed text are harmless.
    pub fn expand_all(&self, text: &str) -> String {
        let mut expanded = text.to_string();
        for block in &self.blocks {
            expanded = expanded.replace(&block.placeholder(), block.expand());
        }
        expanded
    }

    /// Returns the registered placeholder that `text` ends with, if any.
    ///
    /// Used by Backspace handling to delete a placeholder atomically
    /// instead of character-by-character.
    pub fn placeholder_suffix(&self, text: &str) -> Option<String> {
        self.blocks
            .iter()
            .map(PastedBlock::placeholder)
            .find(|placeholder| text.ends_with(placeholder.as_str()))
    }

    /// Removes the block whose placeholder matches, returning `true` when
    /// a block was actually removed.
    pub fn remove_by_placeholder(&mut self, placeholder: &str) -> bool {
        let before = self.blocks.len();
        self.blocks
            .retain(|block| block.placeholder() != placeholder);
        self.blocks.len() != before
    }

    /// Drops all blocks and restarts id numbering from 1.
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.next_id = 1;
    }

    /// Number of registered blocks — used by tests and diagnostics.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────────

/// Normalizes clipboard line endings to `\n`.
///
/// Bracketed paste delivers the clipboard verbatim; the three common
/// line-ending conventions must all be collapsed so the placeholder
/// logic (`contains('\n')`) recognizes the paste as multi-line:
///
/// | Source        | Line ending | Example        |
/// |---------------|-------------|----------------|
/// | Windows       | `\r\n`      | CRLF           |
/// | Old Mac OS    | `\r`        | CR             |
/// | Unix / macOS  | `\n`        | LF             |
///
/// The order matters: `\r\n` → `\n` first, then any remaining `\r` → `\n`.
pub fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

// ── Tests ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_includes_line_count_for_multi_line_paste() {
        let mut store = PasteStore::default();
        let placeholder = store.add_text("alpha\nbeta\ngamma".to_string());
        assert_eq!(placeholder, "[Pasted text #1 +3 lines]");
    }

    #[test]
    fn placeholder_omits_line_count_for_single_line_paste() {
        let mut store = PasteStore::default();
        // add_text is normally only called for multi-line pastes, but the
        // label must stay correct if a caller passes single-line content.
        let placeholder = store.add_text("just one line".to_string());
        assert_eq!(placeholder, "[Pasted text #1]");
    }

    #[test]
    fn block_ids_increment_across_pastes() {
        let mut store = PasteStore::default();
        let first = store.add_text("a\nb".to_string());
        let second = store.add_text("c\nd".to_string());
        assert_eq!(first, "[Pasted text #1 +2 lines]");
        assert_eq!(second, "[Pasted text #2 +2 lines]");
    }

    #[test]
    fn expand_all_replaces_registered_placeholders() {
        let mut store = PasteStore::default();
        let placeholder = store.add_text("line one\nline two".to_string());
        let input = format!("please review:\n{placeholder}\nthanks");
        assert_eq!(
            store.expand_all(&input),
            "please review:\nline one\nline two\nthanks"
        );
    }

    #[test]
    fn expand_all_handles_multiple_blocks() {
        let mut store = PasteStore::default();
        let first = store.add_text("1\n2".to_string());
        let second = store.add_text("a\nb\nc".to_string());
        let input = format!("{first} then {second}");
        assert_eq!(store.expand_all(&input), "1\n2 then a\nb\nc");
    }

    #[test]
    fn expand_all_leaves_unregistered_literal_placeholders_untouched() {
        let store = PasteStore::default();
        // The user typed the placeholder text by hand; no block is
        // registered, so the text must pass through unchanged.
        let typed = "literal [Pasted text #1 +9 lines] typed by hand";
        assert_eq!(store.expand_all(typed), typed);
    }

    #[test]
    fn placeholder_suffix_matches_only_at_the_end() {
        let mut store = PasteStore::default();
        let placeholder = store.add_text("x\ny".to_string());

        let cursor_at_end = format!("prefix {placeholder}");
        assert_eq!(
            store.placeholder_suffix(&cursor_at_end),
            Some(placeholder.clone())
        );

        // Text after the placeholder means the cursor is not right behind
        // it — Backspace should delete a plain character instead.
        let cursor_not_at_end = format!("{placeholder} trailing");
        assert_eq!(store.placeholder_suffix(&cursor_not_at_end), None);
    }

    #[test]
    fn placeholder_suffix_ignores_partial_lookalikes() {
        let mut store = PasteStore::default();
        store.add_text("x\ny".to_string());
        // Same prefix but not the exact placeholder string.
        assert_eq!(store.placeholder_suffix("text [Pasted text #1"), None);
        assert_eq!(store.placeholder_suffix("[Pasted text #9 +2 lines]"), None);
    }

    #[test]
    fn remove_by_placeholder_removes_exactly_one_block() {
        let mut store = PasteStore::default();
        let first = store.add_text("a\nb".to_string());
        let second = store.add_text("c\nd".to_string());

        assert!(store.remove_by_placeholder(&first));
        assert_eq!(store.len(), 1);
        // Second removal of the same placeholder is a no-op.
        assert!(!store.remove_by_placeholder(&first));
        assert!(store.remove_by_placeholder(&second));
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn clear_restarts_id_numbering() {
        let mut store = PasteStore::default();
        store.add_text("a\nb".to_string());
        store.clear();
        assert_eq!(store.len(), 0);
        let placeholder = store.add_text("c\nd".to_string());
        assert_eq!(placeholder, "[Pasted text #1 +2 lines]");
    }

    #[test]
    fn normalize_newlines_converts_crlf_and_cr() {
        // Windows CRLF → LF
        assert_eq!(normalize_newlines("a\r\nb\r\nc"), "a\nb\nc");
        // Old Mac CR → LF
        assert_eq!(normalize_newlines("a\rb\rc"), "a\nb\nc");
        // Mixed CRLF + bare CR → all LF
        assert_eq!(normalize_newlines("a\r\nb\rc"), "a\nb\nc");
        // Unix LF passes through unchanged.
        assert_eq!(normalize_newlines("a\nb\nc"), "a\nb\nc");
    }
}
