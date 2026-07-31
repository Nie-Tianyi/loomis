//! [`LsTool`] — Directory listing tool.
//!
//! Lists directory contents showing name, type, and size. Directories are listed first.

use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

#[cfg(test)]
use tools::FilesystemConfig;
use tools::WorkspaceFs;
use tools::{EntryType, FsError, ProgressStream, ToolError, tool};

/// Arguments for the ls tool.
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LsArgs {
    /// Directory to list, relative to workspace root.
    #[schemars(
        description = "Directory to list, relative to workspace root. Omit, pass empty string, or pass '.' to list the workspace root. Must be a directory — passing a file path returns an error."
    )]
    pub path: Option<String>,
}

/// Tool for listing directory contents.
///
/// # Arguments
///
/// ```json
/// {"path": "src/"}
/// ```
///
/// `path` is optional; omitting it lists the workspace root.
#[tool(
    name = "ls",
    description = "List the contents of a directory in the workspace. Entries are shown with \
         type, size, and name. Directories are listed first, then files. Sizes use \
         human-readable format (B, K, M, G).\n\n\
         Output format:\n\
         ```\n\
         d        -  dir_name/\n\
         -    1.2 K  file_name.rs\n\
         -    256 B  another_file.md\n\
         ```\n\
         Column 1: d=directory, -=file, l=symlink. Column 2: size.\n\n\
         When to use: exploring project structure, checking what is in a directory \
         before reading or editing, verifying that a file or directory exists.\n\n\
         When NOT to use: finding files by pattern (use glob — sorted, recursive, \
         cleaner for pattern-based search), searching content (use grep), reading a \
         file (use read).\n\n\
         Omit the path argument to list the workspace root. Returns '(empty \
         directory)' when the directory has no entries.",
    args = LsArgs
)]
pub struct LsTool {
    fs: Arc<WorkspaceFs>,
}

impl LsTool {
    pub fn new(fs: Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }

    fn execute_stream(&self, args: LsArgs) -> Result<ProgressStream, ToolError> {
        let path = args.path.as_deref().filter(|s| !s.is_empty());

        let entries = self.fs.ls(path).map_err(map_fs_err)?;

        let output = if entries.is_empty() {
            "(empty directory)".to_string()
        } else {
            entries
                .iter()
                .map(|e| {
                    let type_char = match e.entry_type {
                        EntryType::Dir => "d",
                        EntryType::Symlink => "l",
                        EntryType::File => "-",
                    };
                    format!("{} {:>8}  {}", type_char, format_size(e.size), e.name)
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(ProgressStream::done(output))
    }
}

/// Human-readable file size.
fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "K", "M", "G"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{}", bytes)
    } else {
        format!("{size:.1} {}", UNITS[unit_idx])
    }
}

fn map_fs_err(e: FsError) -> ToolError {
    match e {
        FsError::NotADirectory(_) | FsError::WorkspaceEscape(_) => {
            ToolError::InvalidArgs(e.to_string())
        }
        _ => ToolError::Execution(e.to_string()),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tools::Tool;

    fn setup() -> (tempfile::TempDir, LsTool) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path(), &FilesystemConfig::default()).unwrap();
        let tool = LsTool::new(Arc::new(fs));
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
        assert_eq!(tool.name(), "ls");
    }

    #[test]
    fn test_parameters_schema() {
        let (_dir, tool) = setup();
        let params = tool.parameter_schema();
        assert_eq!(params["type"], "object");
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn test_ls_root_empty() {
        let (_dir, tool) = setup();
        let result = Tool::execute_stream(&tool, r#"{}"#).unwrap().poll_done();
        assert!(result.contains("(empty directory)"));
    }

    #[test]
    fn test_ls_with_files_and_dirs() {
        let (dir, tool) = setup();
        write_file(&dir, "foo.txt", "hello");
        std::fs::create_dir(dir.path().join("bar")).unwrap();

        let result = Tool::execute_stream(&tool, r#"{}"#).unwrap().poll_done();
        // Directories first
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("d"));
        assert!(lines[0].contains("bar"));
        assert!(lines[1].starts_with("-"));
        assert!(lines[1].contains("foo.txt"));
    }

    #[test]
    fn test_ls_subdirectory() {
        let (dir, tool) = setup();
        write_file(&dir, "sub/a.txt", "");
        write_file(&dir, "sub/b.txt", "");

        let result = Tool::execute_stream(&tool, r#"{"path": "sub"}"#)
            .unwrap()
            .poll_done();
        assert!(result.contains("a.txt"));
        assert!(result.contains("b.txt"));
    }

    #[test]
    fn test_ls_not_a_directory() {
        let (dir, tool) = setup();
        write_file(&dir, "file.txt", "");

        let err = Tool::execute_stream(&tool, r#"{"path": "file.txt"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_ls_without_path_param() {
        let (_dir, tool) = setup();
        // Omitting path should list the root directory
        let result = Tool::execute_stream(&tool, "{}").unwrap().poll_done();
        assert!(result.contains("(empty directory)"));
    }
}
