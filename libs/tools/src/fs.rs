//! [`WorkspaceFs`] �?sandboxed file-system operations.
//!
//! All path operations go through [`WorkspaceFs::resolve`], which ensures
//! paths cannot escape the `workspace_root`.

use std::fs;
use std::path::{Path, PathBuf};

use super::error::FsError;
use super::sandbox::SandboxConfig;

/// Sandboxed file-system handle. All operations are confined to `workspace_root`.
///
/// Policy knobs (file-size caps, extension blocklist, hidden-file protection)
/// come from [`SandboxConfig`] and are baked into the handle at construction.
#[derive(Debug)]
pub struct WorkspaceFs {
    workspace_root: PathBuf,
    max_read_bytes: usize,
    max_write_bytes: usize,
    forbid_binary_writes: bool,
    forbid_hidden_file_writes: bool,
    blocked_write_extensions: Vec<String>,
}

impl WorkspaceFs {
    /// Create a new workspace file-system handle.
    ///
    /// Validates that `root` exists and is a directory, then canonicalizes
    /// it. Sandbox policies are taken from `config.filesystem`.
    pub fn new(root: impl Into<PathBuf>, config: &SandboxConfig) -> Result<Self, FsError> {
        let root: PathBuf = root.into();

        if !root.try_exists().map_err(FsError::Io)? {
            return Err(FsError::NotFound(root.display().to_string()));
        }
        if !root.is_dir() {
            return Err(FsError::NotADirectory(root.display().to_string()));
        }

        let workspace_root = root.canonicalize().map_err(FsError::Io)?;

        Ok(Self {
            workspace_root,
            max_read_bytes: config.filesystem.max_read_bytes,
            max_write_bytes: config.filesystem.max_write_bytes,
            forbid_binary_writes: config.filesystem.forbid_binary_writes,
            forbid_hidden_file_writes: config.filesystem.forbid_hidden_file_writes,
            blocked_write_extensions: config.filesystem.blocked_write_extensions.clone(),
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Check whether a file's extension is in the blocked list (e.g. `.exe`, `.dll`).
    fn is_extension_blocked(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|ext| {
                let dot_ext = format!(".{}", ext);
                self.blocked_write_extensions
                    .iter()
                    .any(|blocked| blocked.eq_ignore_ascii_case(&dot_ext))
            })
            .unwrap_or(false)
    }

    /// Heuristic: check whether raw bytes look like binary content.
    ///
    /// Scans the first 8 KiB for null bytes �?a reliable indicator of binary
    /// formats (executables, images, archives, etc.).
    fn is_likely_binary(bytes: &[u8]) -> bool {
        let check_len = bytes.len().min(8192);
        bytes[..check_len].contains(&0)
    }

    /// Resolve a relative path to an absolute path within the workspace.
    ///
    /// On success the returned path is guaranteed to start with
    /// `workspace_root`.  When the resolved path already exists on disk
    /// we also perform a **TOCTOU re-check** (see below).
    ///
    /// ## Known limitations
    ///
    /// 1. **Non-existing paths** bypass the TOCTOU re-check entirely �?    ///    if a file is created by an attacker between resolution and
    ///    the subsequent I/O operation, it will not be detected.
    /// 2. **File identity** is verified via `(len, modified)` heuristic
    ///    rather than platform-specific inode/file-index APIs. This is
    ///    not cryptographically strong �?a determined attacker with
    ///    write access can craft a file with matching size and mtime.
    ///
    /// A truly race-free design would require handle-based I/O (open
    /// file, then `fstat` the handle).
    fn resolve(&self, path: &str) -> Result<PathBuf, FsError> {
        let joined = if path.is_empty() {
            self.workspace_root.clone()
        } else {
            self.workspace_root.join(path)
        };

        let normalized = match joined.canonicalize() {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => normalize_partial(&joined)?,
            Err(e) => return Err(FsError::Io(e)),
        };

        if !normalized.starts_with(&self.workspace_root) {
            return Err(FsError::WorkspaceEscape(format!(
                "'{}' resolves outside workspace",
                path
            )));
        }

        // ── TOCTOU re-check for existing paths ──────────────────────────
        // Re-canonicalize and verify the file identity hasn't changed.
        // We compare file length + modification time as a heuristic for
        // "same file" �?this is NOT an inode/file-index comparison, and
        // can be defeated by a determined attacker with write access.
        // If the path didn't exist at the first canonicalize (normalize_partial
        // path), this re-check is skipped �?new files are not covered.
        if let Ok(meta) = normalized.metadata() {
            let re_canon = normalized.canonicalize().map_err(FsError::Io)?;
            if !re_canon.starts_with(&self.workspace_root) {
                return Err(FsError::WorkspaceEscape(format!(
                    "'{}' escapes workspace (TOCTOU re-check)",
                    path
                )));
            }
            // Compare file identity: same length + same modification time
            // is a decent heuristic for "same file" without platform-specific
            // inode APIs.
            if let Ok(re_meta) = re_canon.metadata()
                && (meta.len() != re_meta.len() || meta.modified().ok() != re_meta.modified().ok())
            {
                return Err(FsError::WorkspaceEscape(format!(
                    "'{}' file identity changed between checks �?possible symlink swap",
                    path
                )));
            }
        }

        Ok(normalized)
    }

    /// Read file content with optional `offset` (1-indexed line) and `limit`.
    ///
    /// Files larger than `max_read_bytes` are rejected before reading to
    /// avoid accidental OOM on huge files.
    pub fn read(
        &self,
        path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<String, FsError> {
        let resolved = self.resolve(path)?;

        if !resolved.exists() {
            return Err(FsError::NotFound(path.to_string()));
        }
        if !resolved.is_file() {
            return Err(FsError::NotAFile(path.to_string()));
        }

        // ── Size limit check ────────────────────────────────────────────
        let metadata = resolved.metadata().map_err(FsError::Io)?;
        let file_size = metadata.len();
        if file_size > self.max_read_bytes as u64 {
            return Err(FsError::FileTooLarge {
                path: path.to_string(),
                size: file_size,
                max: self.max_read_bytes as u64,
            });
        }

        let content = fs::read_to_string(&resolved).map_err(FsError::Io)?;
        let all_lines: Vec<&str> = content.lines().collect();

        let start = offset.map(|o| o.saturating_sub(1)).unwrap_or(0);
        let end = limit
            .map(|l| (start + l).min(all_lines.len()))
            .unwrap_or(all_lines.len());

        if start >= all_lines.len() {
            return Ok(String::new());
        }

        let selected = &all_lines[start..end];
        let numbered: String = selected
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6} {}", start + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(numbered)
    }

    /// Create or overwrite a file. Creates parent directories as needed.
    ///
    /// Enforces content size limits, extension blocklist, hidden-file
    /// protection, and binary-content detection (null-byte heuristic).
    ///
    /// **TOCTOU note**: There is a window between [`resolve`](Self::resolve)
    /// and the actual `fs::write` call. A symlink-swap in that window can
    /// bypass the path sandbox. See [`resolve`](Self::resolve) for details.
    pub fn write(&self, path: &str, content: &str) -> Result<(), FsError> {
        let resolved = self.resolve(path)?;

        // ── Content size limit ──────────────────────────────────────────
        if content.len() > self.max_write_bytes {
            return Err(FsError::FileTooLarge {
                path: path.to_string(),
                size: content.len() as u64,
                max: self.max_write_bytes as u64,
            });
        }

        // ── Extension blocklist ─────────────────────────────────────────
        if let Some(ext) = resolved.extension().and_then(|e| e.to_str()) {
            let dot_ext = format!(".{}", ext);
            if self
                .blocked_write_extensions
                .iter()
                .any(|blocked| blocked.eq_ignore_ascii_case(&dot_ext))
            {
                return Err(FsError::ExtensionBlocked(path.to_string()));
            }
        }

        // ── Binary content detection ────────────────────────────────────
        if self.forbid_binary_writes && content.contains('\0') {
            return Err(FsError::BinaryContentDetected(path.to_string()));
        }

        // ── Hidden file protection ──────────────────────────────────────
        if self.forbid_hidden_file_writes
            && let Some(name) = resolved.file_name().and_then(|n| n.to_str())
            && name.starts_with('.')
        {
            return Err(FsError::HiddenFileBlocked(path.to_string()));
        }

        if resolved.exists() && resolved.is_dir() {
            return Err(FsError::NotAFile(path.to_string()));
        }

        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent).map_err(FsError::Io)?;
        }

        fs::write(&resolved, content).map_err(FsError::Io)?;
        Ok(())
    }

    /// Replace lines `start..=end` (1-indexed) with `new_content`.
    ///
    /// **TOCTOU note**: There is a window between [`resolve`](Self::resolve)
    /// and the actual `fs::write` call. See [`resolve`](Self::resolve) for
    /// the limitations of our TOCTOU protection.
    pub fn edit_lines(
        &self,
        path: &str,
        start: usize,
        end: usize,
        new_content: &str,
    ) -> Result<String, FsError> {
        if start == 0 || end == 0 {
            return Err(FsError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "line numbers are 1-indexed; 0 is invalid",
            )));
        }
        if start > end {
            return Err(FsError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("start ({start}) > end ({end})"),
            )));
        }

        let resolved = self.resolve(path)?;
        if !resolved.is_file() {
            return Err(FsError::NotAFile(path.to_string()));
        }

        let content = fs::read_to_string(&resolved).map_err(FsError::Io)?;
        let line_end = detect_line_end(&content);
        let lines: Vec<&str> = content.lines().collect();

        let start_idx = start - 1;
        let end_idx = end - 1;
        let mut new_lines: Vec<String> = Vec::new();

        for line in lines.iter().take(start_idx.min(lines.len())) {
            new_lines.push(line.to_string());
        }

        if !new_content.is_empty() {
            for line in new_content.lines() {
                new_lines.push(line.to_string());
            }
        }

        for line in lines.iter().skip(end_idx + 1) {
            new_lines.push(line.to_string());
        }

        let new_file = new_lines.join(&line_end);
        let new_file = if content.ends_with('\n') && !new_file.ends_with('\n') {
            new_file + &line_end
        } else {
            new_file
        };

        fs::write(&resolved, &new_file).map_err(FsError::Io)?;

        Ok(format!(
            "Replaced lines {start}-{end} in {path} ({} lines removed, {} lines inserted)",
            (end_idx - start_idx + 1).min(lines.len().saturating_sub(start_idx)),
            new_content.lines().count(),
        ))
    }

    /// Glob files matching a pattern relative to workspace root.
    pub fn glob(&self, pattern: &str) -> Result<Vec<String>, FsError> {
        let full_pattern = self.workspace_root.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        let entries = glob::glob(&pattern_str)
            .map_err(FsError::from)?
            .filter_map(|entry| entry.ok())
            .filter(|p| p.is_file())
            .filter_map(|p| {
                p.strip_prefix(&self.workspace_root)
                    .ok()
                    .map(|rel| rel.to_string_lossy().to_string())
            })
            .collect::<Vec<String>>();

        let mut entries = entries;
        entries.sort();
        Ok(entries)
    }

    /// Search files with a regex pattern.
    pub fn grep(&self, pattern: &str, path_glob: Option<&str>) -> Result<Vec<GrepMatch>, FsError> {
        let re = regex::Regex::new(pattern).map_err(FsError::from)?;
        let glob_pattern = path_glob.unwrap_or("**/*");
        let files = self.glob(glob_pattern)?;

        let mut matches = Vec::new();
        for file_path in &files {
            let resolved = self.resolve(file_path)?;

            // Skip files with blocked extensions (binary formats like .exe, .dll, .bin).
            if self.is_extension_blocked(&resolved) {
                continue;
            }

            // Skip files too large to read (consistent with `read()` behavior).
            let metadata = resolved.metadata().map_err(FsError::Io)?;
            if metadata.len() > self.max_read_bytes as u64 {
                continue;
            }

            // Read as raw bytes and convert to UTF-8 losslessly. Binary files
            // (null bytes in first 8 KiB) are skipped �?text search is only
            // meaningful in text files.
            let bytes = fs::read(&resolved).map_err(FsError::Io)?;
            if Self::is_likely_binary(&bytes) {
                continue;
            }
            let content = String::from_utf8_lossy(&bytes);

            for (line_num, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    matches.push(GrepMatch {
                        file_path: file_path.clone(),
                        line_number: line_num + 1,
                        line_content: line.to_string(),
                    });
                }
            }
        }
        Ok(matches)
    }

    /// List directory contents. `None` or `""` = root.
    pub fn ls(&self, path: Option<&str>) -> Result<Vec<DirEntry>, FsError> {
        let resolved = self.resolve(path.unwrap_or(""))?;
        if !resolved.is_dir() {
            return Err(FsError::NotADirectory(path.unwrap_or("").to_string()));
        }

        let mut entries = Vec::new();
        let dir = fs::read_dir(&resolved).map_err(FsError::Io)?;
        for entry in dir {
            let entry = entry.map_err(FsError::Io)?;
            let name = entry.file_name().to_string_lossy().to_string();
            let metadata = entry.metadata().map_err(FsError::Io)?;
            let entry_type = if metadata.is_dir() {
                EntryType::Dir
            } else if metadata.is_symlink() {
                EntryType::Symlink
            } else {
                EntryType::File
            };
            entries.push(DirEntry {
                name,
                entry_type,
                size: metadata.len(),
            });
        }

        entries.sort_by(|a, b| {
            use std::cmp::Ordering;
            match (a.entry_type, b.entry_type) {
                (EntryType::Dir, EntryType::Dir)
                | (EntryType::File, EntryType::File)
                | (EntryType::Symlink, EntryType::Symlink) => a.name.cmp(&b.name),
                (EntryType::Dir, _) => Ordering::Less,
                (_, EntryType::Dir) => Ordering::Greater,
                _ => a.name.cmp(&b.name),
            }
        });

        Ok(entries)
    }
}

// ── Supporting types ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub entry_type: EntryType,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone)]
pub struct GrepMatch {
    pub file_path: String,
    pub line_number: usize,
    pub line_content: String,
}

// ── Internal helpers ────────────────────────────────────────────────────────

fn detect_line_end(text: &str) -> String {
    if text.contains("\r\n") {
        "\r\n".to_string()
    } else {
        "\n".to_string()
    }
}

fn normalize_partial(path: &Path) -> Result<PathBuf, FsError> {
    let mut existing = path.to_path_buf();
    let mut tail_components: Vec<PathBuf> = Vec::new();

    loop {
        if existing.exists() {
            let canon = existing.canonicalize().map_err(FsError::Io)?;
            let mut result = canon;
            for comp in tail_components.iter().rev() {
                if comp == Path::new("..") {
                    if result.parent().is_some() {
                        result.pop();
                    }
                } else if comp != Path::new(".") {
                    result.push(comp);
                }
            }
            return Ok(result);
        }
        if let (Some(parent), Some(file_name)) = (existing.parent(), existing.file_name()) {
            tail_components.push(PathBuf::from(file_name));
            existing = parent.to_path_buf();
        } else {
            return Ok(path.to_path_buf());
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_config() -> SandboxConfig {
        let mut cfg = SandboxConfig::default();
        // Use generous limits for tests �?we're testing sandbox logic,
        // not the specific limit values.
        cfg.filesystem.max_read_bytes = 10_000_000;
        cfg.filesystem.max_write_bytes = 1_000_000;
        cfg.filesystem.forbid_binary_writes = true;
        cfg.filesystem.forbid_hidden_file_writes = false; // allow .files in tests
        cfg
    }

    fn setup_fs() -> (tempfile::TempDir, WorkspaceFs) {
        let dir = tempfile::tempdir().unwrap();
        let fs = WorkspaceFs::new(dir.path(), &test_config()).unwrap();
        (dir, fs)
    }

    #[test]
    fn test_new_valid_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(WorkspaceFs::new(dir.path(), &test_config()).is_ok());
    }

    #[test]
    fn test_new_nonexistent() {
        let cfg = test_config();
        let result = WorkspaceFs::new("/tmp/__nonexistent_dir__", &cfg);
        assert!(matches!(result, Err(FsError::NotFound(_))));
    }

    #[test]
    fn test_path_escapes_workspace() {
        let (_dir, fs) = setup_fs();
        let result = fs.read("../outside_file.txt", None, None);
        assert!(matches!(result, Err(FsError::WorkspaceEscape(_))));
    }

    #[test]
    fn test_read_simple() {
        let (_dir, fs) = setup_fs();
        fs.write("test.txt", "hello\nworld\n").unwrap();
        let result = fs.read("test.txt", None, None).unwrap();
        assert!(result.contains("hello"));
        assert!(result.contains("world"));
    }

    #[test]
    fn test_read_with_offset_and_limit() {
        let (_dir, fs) = setup_fs();
        fs.write("test.txt", "line1\nline2\nline3\nline4\n")
            .unwrap();
        let result = fs.read("test.txt", Some(2), Some(2)).unwrap();
        assert!(!result.contains("line1"));
        assert!(result.contains("line2"));
        assert!(result.contains("line3"));
        assert!(!result.contains("line4"));
    }

    #[test]
    fn test_write_new_file() {
        let (_dir, fs) = setup_fs();
        fs.write("new.txt", "hello").unwrap();
        let content = fs::read_to_string(_dir.path().join("new.txt")).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn test_write_creates_parent_dirs() {
        let (_dir, fs) = setup_fs();
        fs.write("a/b/c/file.txt", "nested").unwrap();
        assert!(_dir.path().join("a/b/c/file.txt").exists());
    }

    #[test]
    fn test_edit_single_line() {
        let (_dir, fs) = setup_fs();
        fs.write("f.txt", "line1\nline2\nline3\n").unwrap();
        fs.edit_lines("f.txt", 2, 2, "replaced").unwrap();
        let content = fs::read_to_string(_dir.path().join("f.txt")).unwrap();
        assert_eq!(content, "line1\nreplaced\nline3\n");
    }

    #[test]
    fn test_glob_basic() {
        let (_dir, fs) = setup_fs();
        fs.write("a.rs", "").unwrap();
        fs.write("b.rs", "").unwrap();
        fs.write("c.txt", "").unwrap();
        let results = fs.glob("*.rs").unwrap();
        assert_eq!(results, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn test_grep_basic() {
        let (_dir, fs) = setup_fs();
        fs.write("a.rs", "fn main() {}\n").unwrap();
        let results = fs.grep("fn", None).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_ls_root() {
        let (_dir, fs) = setup_fs();
        fs.write("a.txt", "").unwrap();
        fs::create_dir(_dir.path().join("sub")).unwrap();
        let entries = fs.ls(None).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "sub"); // directories first
        assert_eq!(entries[1].name, "a.txt");
    }

    // ── New sandbox enforcement tests ───────────────────────────────────

    #[test]
    fn test_read_file_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config();
        cfg.filesystem.max_read_bytes = 10; // tiny limit
        let fs = WorkspaceFs::new(dir.path(), &cfg).unwrap();
        fs.write("big.txt", "this is more than ten bytes of content")
            .unwrap();
        let result = fs.read("big.txt", None, None);
        assert!(
            matches!(result, Err(FsError::FileTooLarge { .. })),
            "expected FileTooLarge, got {result:?}"
        );
    }

    #[test]
    fn test_write_binary_blocked() {
        let (_dir, fs) = setup_fs();
        // Use .txt so the extension check doesn't intercept first.
        let result = fs.write("evil.txt", "MZ\u{0}binary");
        assert!(
            matches!(result, Err(FsError::BinaryContentDetected(_))),
            "expected BinaryContentDetected, got {result:?}"
        );
    }

    #[test]
    fn test_write_extension_blocked() {
        let (_dir, fs) = setup_fs();
        let result = fs.write("malware.exe", "harmless text");
        assert!(
            matches!(result, Err(FsError::ExtensionBlocked(_))),
            "expected ExtensionBlocked, got {result:?}"
        );
    }

    #[test]
    fn test_write_hidden_file_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config();
        cfg.filesystem.forbid_hidden_file_writes = true;
        let fs = WorkspaceFs::new(dir.path(), &cfg).unwrap();
        let result = fs.write(".env", "SECRET=123");
        assert!(
            matches!(result, Err(FsError::HiddenFileBlocked(_))),
            "expected HiddenFileBlocked, got {result:?}"
        );
    }

    #[test]
    fn test_write_content_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config();
        cfg.filesystem.max_write_bytes = 5;
        let fs = WorkspaceFs::new(dir.path(), &cfg).unwrap();
        let result = fs.write("small.txt", "this is way too long");
        assert!(
            matches!(result, Err(FsError::FileTooLarge { .. })),
            "expected FileTooLarge, got {result:?}"
        );
    }
}
