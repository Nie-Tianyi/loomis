//! [`GlobTool`] — 文件模式匹配工具。
//!
//! 使用 glob 模式查找匹配的文件，返回排序后的相对路径列表。

use serde_json::Value;

use super::fs::WorkspaceFs;
use super::tool::{Tool, extract_string_arg};
use super::{FsError, ToolError};

/// 使用 glob 模式查找文件的工具。
///
/// # 参数
///
/// ```json
/// {"pattern": "**/*.rs"}
/// ```
pub struct GlobTool {
    fs: std::sync::Arc<WorkspaceFs>,
}

impl GlobTool {
    pub fn new(fs: std::sync::Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }
}

impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern. \
         Returns a sorted list of relative file paths. \
         Supports ** for recursive matching (e.g., '**/*.rs', 'src/**/*.rs')."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files, e.g. '**/*.rs' or 'src/*.md'"
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn execute(&self, args: &str) -> Result<String, ToolError> {
        let pattern = extract_string_arg(args, "pattern")?;

        let files = self.fs.glob(&pattern).map_err(map_fs_err)?;

        if files.is_empty() {
            Ok("No files matched.".to_string())
        } else {
            Ok(files.join("\n"))
        }
    }
}

fn map_fs_err(e: FsError) -> ToolError {
    match e {
        FsError::Glob(_) => ToolError::InvalidArgs(e.to_string()),
        _ => ToolError::Execution(e.to_string()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn setup() -> (tempfile::TempDir, GlobTool) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path()).unwrap();
        let tool = GlobTool::new(Arc::new(fs));
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
        assert_eq!(tool.name(), "glob");
    }

    #[test]
    fn test_glob_rs_files() {
        let (dir, tool) = setup();
        write_file(&dir, "main.rs", "");
        write_file(&dir, "lib.rs", "");
        write_file(&dir, "Cargo.toml", "");

        let result = tool.execute(r#"{"pattern": "*.rs"}"#).unwrap();
        assert!(result.contains("lib.rs"));
        assert!(result.contains("main.rs"));
        assert!(!result.contains("Cargo.toml"));
    }

    #[test]
    fn test_glob_recursive() {
        let (dir, tool) = setup();
        write_file(&dir, "src/a.rs", "");
        write_file(&dir, "src/deep/b.rs", "");
        write_file(&dir, "tests/c.rs", "");

        let result = tool.execute(r#"{"pattern": "**/*.rs"}"#).unwrap();
        assert!(result.contains("src/a.rs"));
        assert!(result.contains("src/deep/b.rs"));
        assert!(result.contains("tests/c.rs"));
    }

    #[test]
    fn test_glob_no_match() {
        let (_dir, tool) = setup();
        let result = tool.execute(r#"{"pattern": "*.nonexistent"}"#).unwrap();
        assert_eq!(result, "No files matched.");
    }

    #[test]
    fn test_missing_pattern() {
        let (_dir, tool) = setup();
        let err = tool.execute(r#"{}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }
}
