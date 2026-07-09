//! [`SandboxConfig`] — user-facing security policy.
//!
//! All fields are optional in the TOML file; missing keys use the
//! baked-in safe defaults (equivalent to the `"strict"` profile).

use serde::Deserialize;

/// Root configuration for the sandbox system.
///
/// Loaded from `.loomis/config.toml`. If the file is missing or any key
/// is absent, [`SandboxConfig::default`] provides safe fallback values.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    pub filesystem: FilesystemConfig,
    pub shell: ShellConfig,
    pub quotas: QuotaConfig,
    pub audit: AuditConfig,
}

impl SandboxConfig {
    /// Load config from the standard path `.loomis/config.toml` inside
    /// `workspace_root`. Returns `Ok(Self::default())` if the file does
    /// not exist.
    pub fn load(workspace_root: &std::path::Path) -> Result<Self, ConfigError> {
        let config_path = workspace_root.join(".loomis").join("config.toml");
        match std::fs::read_to_string(&config_path) {
            Ok(contents) => toml::from_str(&contents).map_err(ConfigError::Parse),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Io(e)),
        }
    }
}

// ── Filesystem ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
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

// ── Shell ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    /// Default timeout in seconds when the model omits `timeout_secs`.
    pub default_timeout_secs: u64,
    /// Hard cap on timeout (model cannot request more).
    pub max_timeout_secs: u64,
    /// Maximum bytes returned to the model from a single command.
    pub max_output_bytes: usize,
    /// When true, clear all environment variables and only pass a safe
    /// allowlist before spawning child processes.
    pub sanitize_environment: bool,
    pub auto_approve: AutoApproveConfig,
    pub deny_patterns: DenyPatternsConfig,
    pub allowed_commands: AllowedCommandsConfig,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            default_timeout_secs: 30,
            max_timeout_secs: 120,
            max_output_bytes: 100_000,
            sanitize_environment: true,
            auto_approve: AutoApproveConfig::default(),
            deny_patterns: DenyPatternsConfig::default(),
            allowed_commands: AllowedCommandsConfig::default(),
        }
    }
}

/// Commands whose first word matches one of these prefixes are allowed
/// to run without user confirmation.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AutoApproveConfig {
    pub prefixes: Vec<String>,
}

impl Default for AutoApproveConfig {
    fn default() -> Self {
        Self {
            prefixes: vec![
                "cargo".into(),
                "git".into(),
                "npm".into(),
                "node".into(),
                "python".into(),
                "python3".into(),
                "dir".into(),
                "echo".into(),
                "type".into(),
                "ls".into(),
                "cat".into(),
                "head".into(),
                "tail".into(),
                "wc".into(),
                "pwd".into(),
                "date".into(),
                "which".into(),
                "where".into(),
                "printenv".into(),
            ],
        }
    }
}

/// Regex patterns that, when matched against the full command string,
/// cause immediate rejection (no user prompt).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DenyPatternsConfig {
    pub patterns: Vec<String>,
}

impl Default for DenyPatternsConfig {
    fn default() -> Self {
        Self {
            patterns: vec![
                r"rm\s+-rf\s+(/|~)".into(),
                r"sudo\s+".into(),
                r"chmod\s+[0-7]{3,4}\s+/".into(),
                r"dd\s+if=".into(),
                r"mkfs\.".into(),
                "shutdown".into(),
                "reboot".into(),
                r">\s*/dev/".into(),
                r"\|\s*sudo".into(),
            ],
        }
    }
}

/// When non-empty, ONLY these exact binary names are allowed.
/// Empty vec = permissive mode (deny_patterns + auto_approve apply).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AllowedCommandsConfig {
    pub binaries: Vec<String>,
}

// ── Quotas ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct QuotaConfig {
    /// Maximum tool-calling steps per session (already enforced by the
    /// engine, mirrored here for completeness).
    pub max_steps_per_session: usize,
    /// Maximum number of shell commands running concurrently.
    pub max_concurrent_shells: usize,
    /// Hard cap on total tool operations per session.
    pub max_total_operations: usize,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            max_steps_per_session: 50,
            max_concurrent_shells: 2,
            max_total_operations: 10_000,
        }
    }
}

// ── Audit ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AuditConfig {
    /// Master switch for audit logging.
    pub enabled: bool,
    /// Path relative to workspace root for the JSONL audit file.
    pub log_file: String,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            log_file: ".loomis/audit.jsonl".into(),
        }
    }
}

// ── Config Error ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error reading config: {e}"),
            Self::Parse(e) => write!(f, "TOML parse error in config: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}
