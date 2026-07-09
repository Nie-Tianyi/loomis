//! Loomis — Modular Agent with TUI.
//!
//! Entry point for the Loomis binary.

use std::path::PathBuf;

const DEFAULT_MODEL: &str = "deepseek-chat";

#[tokio::main]
async fn main() {
    let use_tui = !std::env::args().any(|a| a == "--no-tui");

    // Load environment
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API").unwrap_or_else(|_| {
        eprintln!("ERROR: DEEPSEEK_API not set.");
        eprintln!("Create a .env file with: DEEPSEEK_API=sk-...");
        std::process::exit(1);
    });

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let model = std::env::var("DEFAULT_PRO_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());

    let (agent, memory, tool_names, model) = loomis::build_coding_agent(&api_key, &cwd, &model);

    if use_tui {
        match loomis::tui::run(agent, memory, tool_names, &model, cwd) {
            Ok(()) => {}
            Err(e) => eprintln!("TUI error: {e}"),
        }
    } else {
        eprintln!("--no-tui mode is not yet migrated to the new crate structure.");
        eprintln!("Please use the TUI mode for now.");
        std::process::exit(1);
    }
}
