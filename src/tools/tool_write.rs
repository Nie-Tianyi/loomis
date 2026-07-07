//! [`WriteTool`] — 文件写入工具。
//!
//! 创建或覆写文件内容。自动创建缺失的父目录。

use serde_json::Value;

use super::fs::WorkspaceFs;
use super::tool::{Tool, extract_string_arg};
use super::{FsError, ToolError};

/// 写入文件内容的工具。
///
/// # 参数
///
/// ```json
/// {"file_path": "output/result.md", "content": "# Hello\n\nWorld"}
/// ```
pub struct WriteTool {
    fs: std::sync::Arc<WorkspaceFs>,
}

impl WriteTool {
    pub fn new(fs: std::sync::Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }
}

impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write content to a file in the workspace. Creates the file if it does not \
         exist; silently overwrites if it does. Parent directories are created \
         automatically.\n\n\
         IMPORTANT: Read the file first before overwriting, so you understand the \
         current state and don't accidentally destroy work.\n\n\
         When to use: creating a new file, replacing an entire file's contents, \
         writing a file that does not yet exist.\n\n\
         When NOT to use: modifying part of a file (use edit), appending (use shell \
         with >>), checking if a file exists (use ls or glob).\n\n\
         Return format: 'Wrote {N} bytes to {file_path}'."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to write to, relative to workspace root. Parent directories are created automatically. Always use forward slashes."
                },
                "content": {
                    "type": "string",
                    "description": "The full content to write. Multi-line text is supported via \\n newlines. CAUTION: existing content at this path is silently overwritten — read the file first."
                }
            },
            "required": ["file_path", "content"],
            "additionalProperties": false
        })
    }

    fn execute(&self, args: &str) -> Result<String, ToolError> {
        let file_path = extract_string_arg(args, "file_path")?;
        let content = extract_string_arg(args, "content")?;

        self.fs.write(&file_path, &content).map_err(map_fs_err)?;

        Ok(format!("Wrote {} bytes to {}", content.len(), file_path))
    }
}

fn map_fs_err(e: FsError) -> ToolError {
    match e {
        FsError::NotAFile(_) | FsError::PathEscapesWorkspace(_) => {
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

    fn setup() -> (tempfile::TempDir, WriteTool) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path()).unwrap();
        let tool = WriteTool::new(Arc::new(fs));
        (dir, tool)
    }

    fn read_file(dir: &tempfile::TempDir, path: &str) -> String {
        std::fs::read_to_string(dir.path().join(path)).unwrap()
    }

    #[test]
    fn test_name() {
        let (_dir, tool) = setup();
        assert_eq!(tool.name(), "write");
    }

    #[test]
    fn test_description() {
        let (_dir, tool) = setup();
        assert!(tool.description().contains("workspace"));
    }

    #[test]
    fn test_write_new_file() {
        let (dir, tool) = setup();
        let result = tool
            .execute(r#"{"file_path": "hello.txt", "content": "hello world"}"#)
            .unwrap();
        assert!(result.contains("hello.txt"));
        assert!(result.contains("11 bytes"));
        assert_eq!(read_file(&dir, "hello.txt"), "hello world");
    }

    #[test]
    fn test_write_overwrite() {
        let (dir, tool) = setup();
        tool.execute(r#"{"file_path": "f.txt", "content": "old"}"#)
            .unwrap();
        tool.execute(r#"{"file_path": "f.txt", "content": "new"}"#)
            .unwrap();
        assert_eq!(read_file(&dir, "f.txt"), "new");
    }

    #[test]
    fn test_write_nested_path() {
        let (dir, tool) = setup();
        tool.execute(r#"{"file_path": "a/b/c/file.txt", "content": "deep"}"#)
            .unwrap();
        assert_eq!(read_file(&dir, "a/b/c/file.txt"), "deep");
    }

    #[test]
    fn test_write_empty_content() {
        let (dir, tool) = setup();
        tool.execute(r#"{"file_path": "empty.txt", "content": ""}"#)
            .unwrap();
        assert_eq!(read_file(&dir, "empty.txt"), "");
    }

    #[test]
    fn test_missing_file_path() {
        let (_dir, tool) = setup();
        let err = tool.execute(r#"{"content": "stuff"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
