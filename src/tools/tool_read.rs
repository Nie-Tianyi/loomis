//! [`ReadTool`] — 文件读取工具。
//!
//! 读取文件内容并以 `cat -n` 风格的行号格式返回。
//! 支持可选的行偏移和行数限制。

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

use super::fs::WorkspaceFs;
use super::schema::generate_schema;
use super::tool::Tool;
use super::{FsError, ToolError};

/// Read 工具的参数。
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadArgs {
    /// Path to the file, relative to workspace root.
    #[schemars(
        description = "Path to the file, relative to workspace root. Must be a file (not a directory). Always use forward slashes."
    )]
    pub file_path: String,

    /// Line to start reading from (1-indexed).
    #[schemars(description = "Line to start reading from (1-indexed). Omit to start at line 1.")]
    pub offset: Option<u64>,

    /// Maximum lines to return.
    #[schemars(
        description = "Maximum lines to return. Omit for the full file. For large files, set to 100-200 to avoid flooding context."
    )]
    pub limit: Option<u64>,
}

/// 读取文件内容的工具。
///
/// # 参数
///
/// ```json
/// {"file_path": "src/main.rs", "offset": 10, "limit": 50}
/// ```
///
/// `offset` 和 `limit` 为可选的整数。
pub struct ReadTool {
    fs: Arc<WorkspaceFs>,
    schema: Value,
}

impl ReadTool {
    pub fn new(fs: Arc<WorkspaceFs>) -> Self {
        Self {
            fs,
            schema: generate_schema::<ReadArgs>(),
        }
    }
}

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a file from the workspace and return its contents with line numbers \
         (like `cat -n`). Each output line is prefixed with a 6-digit right-aligned \
         number followed by a tab.\n\n\
         IMPORTANT: Read a file BEFORE editing or writing to it — accurate edits \
         depend on seeing the file's actual current contents. Do NOT re-read a file \
         you just wrote or edited; the write/edit tool would have errored if the \
         change failed, and re-reading wastes context.\n\n\
         When to use: inspecting file contents, reading source code, checking \
         configuration, verifying code before making edits.\n\n\
         When NOT to use: finding files by name (use glob), searching text across \
         files (use grep), listing directory contents (use ls), verifying a completed \
         write/edit (trust the tool result).\n\n\
         For large files, start with limit=100 and increase if needed to avoid \
         flooding the conversation with irrelevant content."
    }

    fn parameters(&self) -> Value {
        self.schema.clone()
    }

    fn execute(&self, args: &str) -> Result<String, ToolError> {
        let args: ReadArgs = serde_json::from_str(args)
            .map_err(|e| ToolError::InvalidArgs(format!("invalid args: {e}")))?;

        self.fs
            .read(
                &args.file_path,
                args.offset.map(|n| n as usize),
                args.limit.map(|n| n as usize),
            )
            .map_err(map_fs_err)
    }
}

/// 将 FsError 映射为 ToolError。
fn map_fs_err(e: FsError) -> ToolError {
    match e {
        FsError::NotFound(_) | FsError::NotAFile(_) | FsError::PathEscapesWorkspace(_) => {
            ToolError::InvalidArgs(e.to_string())
        }
        _ => ToolError::Execution(e.to_string()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, ReadTool) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path()).unwrap();
        let tool = ReadTool::new(Arc::new(fs));
        (dir, tool)
    }

    fn write_file(dir: &tempfile::TempDir, path: &str, content: &str) {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }

    #[test]
    fn test_name() {
        let (_dir, tool) = setup();
        assert_eq!(tool.name(), "read");
    }

    #[test]
    fn test_description() {
        let (_dir, tool) = setup();
        assert!(tool.description().contains("workspace"));
    }

    #[test]
    fn test_parameters_schema() {
        let (_dir, tool) = setup();
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], false);
        assert!(
            params["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("file_path"))
        );
    }

    #[test]
    fn test_read_success() {
        let (dir, tool) = setup();
        write_file(&dir, "test.txt", "hello\nworld\n");

        let result = tool.execute(r#"{"file_path": "test.txt"}"#).unwrap();
        assert!(result.contains("hello"));
        assert!(result.contains("world"));
    }

    #[test]
    fn test_read_with_offset_and_limit() {
        let (dir, tool) = setup();
        write_file(&dir, "test.txt", "1\n2\n3\n4\n5\n");

        let result = tool
            .execute(r#"{"file_path": "test.txt", "offset": 2, "limit": 2}"#)
            .unwrap();
        assert!(!result.contains("     1\t"));
        assert!(result.contains("     2\t"));
        assert!(result.contains("     3\t"));
        assert!(!result.contains("     4\t"));
    }

    #[test]
    fn test_read_not_found() {
        let (_dir, tool) = setup();
        let err = tool.execute(r#"{"file_path": "nope.txt"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_read_missing_field() {
        let (_dir, tool) = setup();
        let err = tool.execute(r#"{}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_read_bad_json() {
        let (_dir, tool) = setup();
        let err = tool.execute("garbage").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
