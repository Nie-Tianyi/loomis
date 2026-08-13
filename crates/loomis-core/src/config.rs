//! Core configuration — the single entry for frontends to configure the
//! agent runtime.

use std::path::PathBuf;

use agent_oxide::persistence::PersistenceConfig;
use agent_oxide::sandbox::SandboxConfig;

/// Default main agent model.
pub const DEFAULT_MODEL: &str = "deepseek-v4-pro";
/// Default cheap model (compaction, profile synthesis, subagents).
pub const DEFAULT_FLASH_MODEL: &str = "deepseek-v4-flash";

/// Everything [`Runtime::build`](crate::Runtime::build) needs to assemble
/// the agent. Builder-style: `CoreConfig::new(api_key, workspace_root)
/// .model(..).flash_model(..)`.
pub struct CoreConfig {
    pub api_key: String,
    pub workspace_root: PathBuf,
    pub model: String,
    pub flash_model: String,
    /// Explicit sandbox policy. `None` → load `<ws>/.loomis/config.toml`
    /// with safe defaults if the file is missing.
    pub sandbox: Option<SandboxConfig>,
    /// Persistence layout (defaults to the Loomis `.loomis/threads` /
    /// `.loomis/current` convention).
    pub persistence: PersistenceConfig,
}

impl CoreConfig {
    pub fn new(api_key: impl Into<String>, workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            api_key: api_key.into(),
            workspace_root: workspace_root.into(),
            model: DEFAULT_MODEL.into(),
            flash_model: DEFAULT_FLASH_MODEL.into(),
            sandbox: None,
            persistence: PersistenceConfig {
                threads_dir: ".loomis/threads".into(),
                current_thread_file: ".loomis/current".into(),
                markdown_title: "Loomis Conversation".into(),
                ..Default::default()
            },
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn flash_model(mut self, model: impl Into<String>) -> Self {
        self.flash_model = model.into();
        self
    }

    pub fn sandbox(mut self, config: SandboxConfig) -> Self {
        self.sandbox = Some(config);
        self
    }

    pub fn persistence(mut self, config: PersistenceConfig) -> Self {
        self.persistence = config;
        self
    }

    /// Resolve the effective sandbox config.
    ///
    /// When no explicit config is given, loads `<ws>/.loomis/config.toml`
    /// (safe defaults on failure) and rewrites the generic library audit
    /// path (`.agent/audit.jsonl`) to the Loomis convention
    /// (`.loomis/audit.jsonl`). An explicitly provided config is used
    /// verbatim — a library consumer owns its audit path.
    pub(crate) fn resolve_sandbox(&self) -> SandboxConfig {
        let mut config = match &self.sandbox {
            Some(config) => return config.clone(),
            None => {
                let config_path = self.workspace_root.join(".loomis").join("config.toml");
                match SandboxConfig::load(&config_path) {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "Failed to load sandbox config, using safe defaults"
                        );
                        SandboxConfig::default()
                    }
                }
            }
        };
        if config.audit.log_file == agent_oxide::sandbox::config::AuditConfig::default().log_file {
            config.audit.log_file = ".loomis/audit.jsonl".into();
        }
        config
    }
}
