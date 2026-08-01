//! [`EditTool`] — Content-based file editing (replace, delete, insert).
//!
//! Replaces the unique occurrence of `old_content` with `new_content`.
//! Pass an empty string as `new_content` to delete the matched fragment.
//!
//! Matching is literal and must be **unique** — a stale `old_content`
//! (from memory or a prior read) fails loudly with 0-or-multiple matches
//! instead of corrupting the file.
//!
//! Streams the replacement content to the TUI via
//! [`Progress::InProgress`] events so the user sees what's being
//! edited while the tool executes.

use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use sandbox::{FsError, WorkspaceFs};
use tools::{Progress, ProgressStream, ToolError, tool};

#[cfg(test)]
use sandbox::FilesystemConfig;

/// Arguments for the edit tool.
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditArgs {
    /// Path to the file to edit, relative to workspace root.
    #[schemars(
        description = "Path to the file to edit, relative to workspace root. Must be an existing file. Always use forward slashes."
    )]
    pub file_path: String,

    /// Exact fragment of the file's current content to replace.
    #[schemars(
        description = "The exact text to match in the file's CURRENT content. Must appear exactly once. Literal match — no regex. Line-ending differences are ignored. If the match fails (zero or multiple matches), the file changed or your content is stale: re-read the file and retry with fresh content. Include neighboring lines when the fragment is not unique."
    )]
    pub old_content: String,

    /// Replacement text to insert in place of the matched fragment.
    #[schemars(
        description = "Replacement text to insert in place of the matched fragment. Pass empty string to delete the matched fragment. Use \\n for multiple lines."
    )]
    pub new_content: String,
}

/// Tool for replacing file content by exact fragment match.
///
/// # Arguments
///
/// ```json
/// {
///     "file_path": "src/main.rs",
///     "old_content": "    let x = 41;",
///     "new_content": "    let x = 42;\n    println!(\"{x}\");"
/// }
/// ```
#[tool(
    name = "edit",
    description = "Replace an exact text fragment in a file. old_content is matched against the \
         file's CURRENT content and must appear EXACTLY ONCE. The match is literal (no regex, no \
         globs); line-ending differences (\\r\\n vs \\n) are ignored.\n\n\
         IMPORTANT — READ FIRST: Read the file before editing. old_content must come from that \
         read, not from memory or an earlier version of the file. If the edit fails (zero \
         matches, or multiple matches), the file changed or your content is stale: re-read the \
         file and retry with fresh content. The tool NEVER edits on a failed match — no \
         corruption, only a retry.\n\n\
         AMBIGUOUS MATCHES: if old_content appears more than once, extend it with neighboring \
         lines to make the match unique.\n\n\
         Deletion: pass empty new_content to delete the matched fragment.\n\
         Insertion: match a small anchor fragment and include the new lines in new_content.\n\n\
         When to use: modifying part of an existing file, deleting a fragment, inserting lines.\n\n\
         When NOT to use: creating a new file or rewriting it wholesale (use write — simpler and \
         less error-prone), bulk mechanical renames (use write of the whole file).\n\n\
         Return format: 'Edited {file_path}: replaced match at lines {start}-{end} ({old} → {new} \
         lines)'.",
    args = EditArgs
)]
pub struct EditTool {
    fs: Arc<WorkspaceFs>,
}

impl EditTool {
    pub fn new(fs: Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }

    fn execute_stream(&self, args: EditArgs) -> Result<ProgressStream, ToolError> {
        tracing::debug!(
            path = %args.file_path,
            old_len = args.old_content.len(),
            new_len = args.new_content.len(),
            "Editing file"
        );
        // Validate and edit synchronously first (errors surface immediately).
        let span = self
            .fs
            .edit_content(&args.file_path, &args.old_content, &args.new_content)
            .map_err(|e| {
                tracing::error!(
                    path = %args.file_path,
                    error = %e,
                    "Failed to edit file"
                );
                map_fs_err(e)
            })?;
        tracing::info!(
            path = %args.file_path,
            old_len = args.old_content.len(),
            new_len = args.new_content.len(),
            span = ?span,
            "File edited"
        );

        let file_path = args.file_path.clone();
        let span_label = if span.start_line == span.end_line {
            format!("line {}", span.start_line)
        } else {
            format!("lines {}-{}", span.start_line, span.end_line)
        };
        let old_lines = args.old_content.lines().count();
        let new_lines = args.new_content.lines().count();
        let output = format!(
            "Edited {file_path}: replaced match at {span_label} ({old_lines} → {new_lines} lines)"
        );
        let preview = super::content_preview(&args.new_content, "Replace with");

        // Stream progress events with small delays so the TUI can render
        // intermediate states before Done transitions to Complete.
        let (tx, rx) = mpsc::unbounded_channel::<Progress>();

        tokio::spawn(async move {
            tx.send(Progress::InProgress(format!(
                "Editing {}: {}...",
                file_path, span_label
            )))
            .ok();
            tokio::time::sleep(Duration::from_millis(80)).await;

            if !preview.is_empty() {
                tx.send(Progress::InProgress(preview)).ok();
                tokio::time::sleep(Duration::from_millis(80)).await;
            }

            tx.send(Progress::Done(output)).ok();
        });

        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(ProgressStream::new(Box::pin(stream)))
    }
}

fn map_fs_err(e: FsError) -> ToolError {
    match e {
        FsError::NotAFile(_)
        | FsError::WorkspaceEscape(_)
        | FsError::NoMatch { .. }
        | FsError::AmbiguousMatch { .. } => ToolError::InvalidArgs(e.to_string()),
        _ => ToolError::Execution(e.to_string()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use tools::Tool;

    /// Drive a progress stream to completion, collecting all messages.
    /// Returns the final `Done` payload.
    async fn stream_done(mut stream: ProgressStream) -> String {
        let mut in_progress = vec![];
        while let Some(progress) = stream.next().await {
            match progress {
                Progress::InProgress(msg) => in_progress.push(msg),
                Progress::Done(output) => {
                    assert!(
                        !in_progress.is_empty(),
                        "expected at least one InProgress before Done"
                    );
                    return output;
                }
            }
        }
        panic!("stream ended without Progress::Done");
    }

    fn setup() -> (tempfile::TempDir, EditTool) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path(), &FilesystemConfig::default()).unwrap();
        let tool = EditTool::new(Arc::new(fs));
        (dir, tool)
    }

    fn write_file(dir: &tempfile::TempDir, path: &str, content: &str) {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }

    fn read_file(dir: &tempfile::TempDir, path: &str) -> String {
        std::fs::read_to_string(dir.path().join(path)).unwrap()
    }

    #[tokio::test]
    async fn test_name() {
        let (_dir, tool) = setup();
        assert_eq!(tool.name(), "edit");
    }

    #[tokio::test]
    async fn test_parameters_schema() {
        let (_dir, tool) = setup();
        let params = tool.parameter_schema();
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], false);
    }

    #[tokio::test]
    async fn test_replace_single_fragment() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "line1\nline2\nline3\n");

        let stream = Tool::execute_stream(
            &tool,
            r#"{"file_path": "f.txt", "old_content": "line2", "new_content": "REPLACED"}"#,
        )
        .unwrap();
        let output = stream_done(stream).await;
        assert!(output.contains("Edited f.txt"));
        assert!(output.contains("line 2"), "got: {output}");
        assert_eq!(read_file(&dir, "f.txt"), "line1\nREPLACED\nline3\n");
    }

    #[tokio::test]
    async fn test_replace_multi_line_fragment() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "a\nb\nc\nd\ne\n");

        stream_done(
            Tool::execute_stream(
                &tool,
                r#"{"file_path": "f.txt", "old_content": "b\nc\nd", "new_content": "X\nY"}"#,
            )
            .unwrap(),
        )
        .await;
        assert_eq!(read_file(&dir, "f.txt"), "a\nX\nY\ne\n");
    }

    #[tokio::test]
    async fn test_delete_fragment() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "a\nb\nc\n");

        stream_done(
            Tool::execute_stream(
                &tool,
                r#"{"file_path": "f.txt", "old_content": "b\n", "new_content": ""}"#,
            )
            .unwrap(),
        )
        .await;
        assert_eq!(read_file(&dir, "f.txt"), "a\nc\n");
    }

    #[tokio::test]
    async fn test_insert_via_anchor() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "a\nb\n");

        // Insert after line 2 by matching the anchor and appending.
        stream_done(
            Tool::execute_stream(
                &tool,
                r#"{"file_path": "f.txt", "old_content": "b", "new_content": "b\nc"}"#,
            )
            .unwrap(),
        )
        .await;
        assert_eq!(read_file(&dir, "f.txt"), "a\nb\nc\n");
    }

    #[tokio::test]
    async fn test_no_match_reports_helpful_error() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "line1\nline2\nline3\n");

        let err = Tool::execute_stream(
            &tool,
            r#"{"file_path": "f.txt", "old_content": "STALE_CONTENT", "new_content": "x"}"#,
        )
        .unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(_)),
            "got: {err:?}"
        );
        // File untouched — the failure is loud, not corrupting.
        assert_eq!(read_file(&dir, "f.txt"), "line1\nline2\nline3\n");
    }

    #[tokio::test]
    async fn test_ambiguous_match_reports_helpful_error() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "x = 1;\ny = 2;\nx = 1;\n");

        let err = Tool::execute_stream(
            &tool,
            r#"{"file_path": "f.txt", "old_content": "x = 1;", "new_content": "x = 0;"}"#,
        )
        .unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidArgs(_)),
            "got: {err:?}"
        );
        assert_eq!(read_file(&dir, "f.txt"), "x = 1;\ny = 2;\nx = 1;\n");
    }

    #[tokio::test]
    async fn test_missing_old_content() {
        let (_dir, tool) = setup();
        let err = Tool::execute_stream(
            &tool,
            r#"{"file_path": "f.txt", "new_content": "x"}"#,
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn test_nonexistent_file() {
        let (_dir, tool) = setup();
        let err = Tool::execute_stream(
            &tool,
            r#"{"file_path": "nope.txt", "old_content": "x", "new_content": "y"}"#,
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn test_content_preview_delete() {
        // Empty content (delete) should return empty preview.
        assert!(crate::tools::content_preview("", "Replace with").is_empty());
    }

    #[tokio::test]
    async fn test_content_preview_single_line() {
        let preview = crate::tools::content_preview("hello world", "Replace with");
        assert_eq!(preview, "Replace with: hello world");
    }

    #[tokio::test]
    async fn test_content_preview_multi_line() {
        let preview = crate::tools::content_preview("line1\nline2\nline3", "Replace with");
        assert!(preview.contains("line1"));
        assert!(preview.contains("+2 more lines"));
    }
}
