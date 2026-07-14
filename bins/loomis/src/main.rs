//! Loomis — Modular Agent with TUI.
//!
//! Entry point for the Loomis binary.

use std::path::{Path, PathBuf};

use tools::SandboxConfig;
use tracing_appender::non_blocking::WorkerGuard;

const DEFAULT_MODEL: &str = "deepseek-v4-pro";
const DEFAULT_FLASH_MODEL: &str = "deepseek-v4-flash";

/// Initialize the tracing subscriber for file-based logging.
///
/// Logs go to `.loomis/logs/loomis.log` (rolling daily).
/// Level is controlled by `LOOMIS_LOG` env var (default: `info`).
///
/// Returns a [`WorkerGuard`] that must be kept alive for the lifetime of the
/// process — when dropped, remaining events are flushed and the worker exits.
fn init_tracing(workspace_root: &Path) -> WorkerGuard {
    let log_dir = workspace_root.join(".loomis").join("logs");
    std::fs::create_dir_all(&log_dir).expect("Failed to create .loomis/logs directory");

    let file_appender = tracing_appender::rolling::daily(&log_dir, "loomis.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = std::env::var("LOOMIS_LOG").unwrap_or_else(|_| "info".into());

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(non_blocking)
        .with_ansi(false) // no ANSI escape codes in file output
        .with_target(true) // include module path
        .with_line_number(true)
        .init();

    guard
}

#[tokio::main]
async fn main() {
    let use_tui = !std::env::args().any(|a| a == "--no-tui");

    // Load environment
    dotenvy::dotenv().ok();

    // Determine workspace root early — needed for log directory path.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Initialize structured logging before any business logic.
    // The guard must stay alive until process exit.
    let _guard = init_tracing(&cwd);

    let api_key = std::env::var("DEEPSEEK_API").unwrap_or_else(|_| {
        tracing::error!("DEEPSEEK_API not set. Create a .env file with: DEEPSEEK_API=sk-...");
        std::process::exit(1);
    });

    let model = std::env::var("DEFAULT_PRO_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let flash_model =
        std::env::var("FLASH_MODEL").unwrap_or_else(|_| DEFAULT_FLASH_MODEL.to_string());

    // Load sandbox config from .loomis/config.toml (falls back to safe defaults).
    let sandbox_config = match SandboxConfig::load(&cwd) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load sandbox config, using safe defaults");
            SandboxConfig::default()
        }
    };

    let mut kit = loomis::build_coding_agent(&api_key, &cwd, &model, &flash_model, &sandbox_config);

    // Override generic defaults with loomis-specific paths.
    kit.persistence_config = memory::PersistenceConfig {
        threads_dir: ".loomis/threads".into(),
        current_thread_file: ".loomis/current".into(),
        markdown_title: "Loomis Conversation".into(),
        ..Default::default()
    };

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        %model,
        %flash_model,
        workspace = %cwd.display(),
        "Loomis initialized",
    );

    if use_tui {
        let model = kit.model.clone();
        match loomis::tui::run(kit, cwd, &model) {
            Ok(()) => {}
            Err(e) => tracing::error!(error = %e, "TUI error"),
        }
    } else {
        tracing::warn!("--no-tui mode is not yet migrated to the new crate structure.");
        std::process::exit(1);
    }
}
