//! [`EditTool`] — 行级文件编辑工具。
//!
//! 替换文件中指定行范围的内容。支持删除行（传入空字符串）。

use serde_json::Value;

use super::fs::WorkspaceFs;
use super::tool::{Tool, extract_string_arg};
use super::{FsError, ToolError};

/// 用行号替换文件内容的工具。
///
/// # 参数
///
/// ```json
/// {
///     "file_path": "src/main.rs",
///     "start_line": 5,
///     "end_line": 7,
///     "new_content": "    let x = 42;\n    println!(\"{x}\");"
/// }
/// ```
///
/// 行号是 1-indexed，`start_line` 和 `end_line` 都是包含的。
pub struct EditTool {
    fs: std::sync::Arc<WorkspaceFs>,
}

impl EditTool {
    pub fn new(fs: std::sync::Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }
}

impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace lines in a file by line number. \
         Provide start_line (1-indexed), end_line (inclusive), and new_content. \
         Passing empty new_content deletes the lines."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to edit, relative to workspace root"
                },
                "start_line": {
                    "type": "integer",
                    "description": "First line to replace (1-indexed, inclusive)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "Last line to replace (1-indexed, inclusive)"
                },
                "new_content": {
                    "type": "string",
                    "description": "New text to insert in place of the replaced lines"
                }
            },
            "required": ["file_path", "start_line", "end_line", "new_content"],
            "additionalProperties": false
        })
    }

    fn execute(&self, args: &str) -> Result<String, ToolError> {
        let file_path = extract_string_arg(args, "file_path")?;
        let new_content = extract_string_arg(args, "new_content")?;

        let v: Value = serde_json::from_str(args)
            .map_err(|e| ToolError::InvalidArgs(format!("invalid JSON: {e}")))?;

        let start_line = v
            .get("start_line")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ToolError::InvalidArgs("missing 'start_line' field".into()))?
            as usize;

        let end_line = v
            .get("end_line")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| ToolError::InvalidArgs("missing 'end_line' field".into()))?
            as usize;

        self.fs
            .edit_lines(&file_path, start_line, end_line, &new_content)
            .map_err(map_fs_err)
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

    fn setup() -> (tempfile::TempDir, EditTool) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path()).unwrap();
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

    #[test]
    fn test_name() {
        let (_dir, tool) = setup();
        assert_eq!(tool.name(), "edit");
    }

    #[test]
    fn test_replace_single_line() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "line1\nline2\nline3\n");

        let result = tool
            .execute(r#"{"file_path": "f.txt", "start_line": 2, "end_line": 2, "new_content": "REPLACED"}"#)
            .unwrap();
        assert!(result.contains("Replaced"));
        assert_eq!(read_file(&dir, "f.txt"), "line1\nREPLACED\nline3\n");
    }

    #[test]
    fn test_replace_range() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "a\nb\nc\nd\ne\n");

        tool.execute(
            r#"{"file_path": "f.txt", "start_line": 2, "end_line": 4, "new_content": "X\nY"}"#,
        )
        .unwrap();
        assert_eq!(read_file(&dir, "f.txt"), "a\nX\nY\ne\n");
    }

    #[test]
    fn test_delete_lines() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "a\nb\nc\n");

        tool.execute(
            r#"{"file_path": "f.txt", "start_line": 2, "end_line": 2, "new_content": ""}"#,
        )
        .unwrap();
        assert_eq!(read_file(&dir, "f.txt"), "a\nc\n");
    }

    #[test]
    fn test_insert_at_end() {
        let (dir, tool) = setup();
        write_file(&dir, "f.txt", "a\nb\n");

        // 替换超出行范围的"append"行为（替换不存在的行就变成 append）
        tool.execute(
            r#"{"file_path": "f.txt", "start_line": 3, "end_line": 3, "new_content": "c"}"#,
        )
        .unwrap();
        assert_eq!(read_file(&dir, "f.txt"), "a\nb\nc\n");
    }

    #[test]
    fn test_missing_start_line() {
        let (_dir, tool) = setup();
        let err = tool
            .execute(r#"{"file_path": "f.txt", "end_line": 1, "new_content": "x"}"#)
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_nonexistent_file() {
        let (_dir, tool) = setup();
        let err = tool
            .execute(
                r#"{"file_path": "nope.txt", "start_line": 1, "end_line": 1, "new_content": "x"}"#,
            )
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
