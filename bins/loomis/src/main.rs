//! Loomis — Modular Agent with TUI.
//!
//! Entry point for the Loomis binary.

use std::path::{Path, PathBuf};

use agent_oxide::sandbox::SandboxConfig;
use tracing_appender::non_blocking::WorkerGuard;

const DEFAULT_MODEL: &str = "deepseek-v4-pro";
const DEFAULT_FLASH_MODEL: &str = "deepseek-v4-flash";

/// Install a process-wide panic hook that writes the panic to the tracing
/// log **and** restores the terminal before the default hook prints it.
///
/// Must be called after `init_tracing` so the subscriber (and its
/// `WorkerGuard`) is live. A single hook covers the whole process — raw-mode
/// restoration is a no-op when the terminal is not in raw mode, so there is
/// no need for a separate TUI-specific hook.
fn install_panic_hook() {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 1. Write to the log first — the tracing worker is still alive here.
        let location = info.location().map(|l| format!("{l}")).unwrap_or_default();
        let msg = agent_oxide::engine::panic_message(info.payload());
        tracing::error!(
            panic.location = %location,
            panic.message = %msg,
            "FATAL: process panicked",
        );

        // 2. Restore the terminal if it was in raw/alternate-screen mode.
        //    No-ops outside the TUI.
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture,
            crossterm::event::DisableBracketedPaste,
        );

        // 3. Let the default hook print the panic to stderr.
        prev_hook(info);
    }));
}

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
    // Load environment
    dotenvy::dotenv().ok();

    // Check the required API key before touching the filesystem (tracing
    // would create `.loomis/logs`) — a missing `.env` must fail loudly
    // without leaving any artifacts behind.
    let api_key = std::env::var("DEEPSEEK_API").unwrap_or_else(|_| {
        eprintln!("error: DEEPSEEK_API not set. Create a .env file with: DEEPSEEK_API=sk-...");
        std::process::exit(1);
    });

    // Determine workspace root early — needed for log directory path.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Initialize structured logging before any business logic.
    // The guard must stay alive until process exit.
    let _guard = init_tracing(&cwd);

    // Install the process-wide panic hook: panic messages are written to the
    // log file and the terminal is restored before the default hook prints.
    install_panic_hook();

    let model = std::env::var("DEFAULT_PRO_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let flash_model =
        std::env::var("FLASH_MODEL").unwrap_or_else(|_| DEFAULT_FLASH_MODEL.to_string());

    // Load sandbox config (includes [filesystem], [shell], [quotas], [audit]).
    let config_path = cwd.join(".loomis").join("config.toml");
    let mut sandbox_config = match SandboxConfig::load(&config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load sandbox config, using safe defaults");
            SandboxConfig::default()
        }
    };

    // The generic library default (`.agent/audit.jsonl`) is not the loomis
    // convention — all app artifacts live under `.loomis/`.
    if sandbox_config.audit.log_file
        == agent_oxide::sandbox::config::AuditConfig::default().log_file
    {
        sandbox_config.audit.log_file = ".loomis/audit.jsonl".into();
    }

    let kit = loomis::build_coding_agent(&api_key, &cwd, &model, &flash_model, &sandbox_config);

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        %model,
        %flash_model,
        workspace = %cwd.display(),
        "Loomis initialized",
    );

    let model = kit.model.clone();
    match loomis::tui::run(kit, cwd, &model) {
        Ok(()) => {}
        Err(e) => {
            tracing::error!(error = %e, "TUI error");
            eprintln!("error: TUI failed: {e}");
        }
    }
}
