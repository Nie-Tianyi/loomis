//! [`ReadTool`] — 文件读取工具。
//!
//! 读取文件内容并以 `cat -n` 风格的行号格式返回。
//! 支持可选的行偏移和行数限制。

use serde_json::Value;

use super::fs::WorkspaceFs;
use super::tool::{Tool, extract_string_arg};
use super::{FsError, ToolError};

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
    fs: std::sync::Arc<WorkspaceFs>,
}

impl ReadTool {
    pub fn new(fs: std::sync::Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }
}

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a file from the workspace. \
         Returns file contents with line numbers (like cat -n). \
         Use optional 'offset' and 'limit' to read a specific range of lines."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to read, relative to workspace root"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-indexed, optional)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (optional)"
                }
            },
            "required": ["file_path"],
            "additionalProperties": false
        })
    }

    fn execute(&self, args: &str) -> Result<String, ToolError> {
        let file_path = extract_string_arg(args, "file_path")?;

        let v: Value = serde_json::from_str(args)
            .map_err(|e| ToolError::InvalidArgs(format!("invalid JSON: {e}")))?;

        let offset = v.get("offset").and_then(|v| v.as_u64()).map(|n| n as usize);

        let limit = v.get("limit").and_then(|v| v.as_u64()).map(|n| n as usize);

        self.fs.read(&file_path, offset, limit).map_err(map_fs_err)
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
    use std::sync::Arc;

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
