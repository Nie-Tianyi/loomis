//! [`FilesystemConfig`] — sandbox policy for file-system operations.
//!
//! This type lives in the `tools` crate so that [`WorkspaceFs`](crate::fs::WorkspaceFs)
//! can depend on it without pulling in the full `sandbox` crate.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Safety limits applied to file reads and writes by [`WorkspaceFs`].
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct FilesystemConfig {
    /// Maximum bytes that `read()` will return for a single file.
    pub max_read_bytes: usize,
    /// Maximum bytes that `write()` will accept in a single call.
    pub max_write_bytes: usize,
    /// Reject writes whose content contains a null byte (binary heuristic).
    pub forbid_binary_writes: bool,
    /// Reject writes to dot-files (e.g. `.env`, `.gitignore`).
    pub forbid_hidden_file_writes: bool,
    /// File extensions that cannot be created or modified.
    pub blocked_write_extensions: Vec<String>,
}

impl Default for FilesystemConfig {
    fn default() -> Self {
        Self {
            max_read_bytes: 1_048_576, // 1 MiB
            max_write_bytes: 524_288,  // 512 KiB
            forbid_binary_writes: true,
            forbid_hidden_file_writes: true,
            blocked_write_extensions: vec![
                ".exe".into(),
                ".dll".into(),
                ".so".into(),
                ".dylib".into(),
                ".sys".into(),
                ".bin".into(),
            ],
        }
    }
}

/// Private wrapper — serde extracts only the `[filesystem]` section from
/// the full `.loomis/config.toml` while ignoring `[shell]`, `[quotas]`,
/// and `[audit]`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ConfigFile {
    filesystem: FilesystemConfig,
}

impl FilesystemConfig {
    /// Load filesystem config from the same `.loomis/config.toml` file
    /// used by [`SandboxConfig`](sandbox::SandboxConfig).
    ///
    /// Only the `[filesystem]` section is extracted; other sections are
    /// silently ignored by serde.  Falls back to
    /// [`FilesystemConfig::default`] when the file is missing or the
    /// `[filesystem]` section cannot be parsed.
    ///
    /// Unlike `SandboxConfig::load`, this method does **not** write a
    /// default template — the template is already written by the sandbox
    /// config loader, which runs first.
    pub fn load(config_path: &Path) -> Self {
        match std::fs::read_to_string(config_path) {
            Ok(contents) => match toml::from_str::<ConfigFile>(&contents) {
                Ok(cf) => cf.filesystem,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to parse [filesystem] section, using safe defaults",
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Failed to read config for [filesystem], using safe defaults",
                );
                Self::default()
            }
        }
    }
}
