//! # Agent Oxide — Interactive CLI
//!
//! A command-line chat interface with real-time streaming token display.
//! Tokens appear as the model generates them; tool calls are highlighted
//! inline so you can see what the agent is doing.
//!
//! ## Usage
//!
//! ```text
//! $ cargo run
//! ╔══════════════════════════════════════════════╗
//! ║       Agent Oxide — Interactive CLI          ║
//! ╚══════════════════════════════════════════════╝
//!
//! > Write a short poem about Rust
//! 🤖 A language born of fire and care...
//!
//! > What files are in the current directory?
//! 🤖
//!   🔧 ls
//!   ✓ ls → src/ Cargo.toml ...
//! The current directory contains: ...
//! ```
//!
//! ## Commands
//!
//! | Command  | Action |
//! |----------|--------|
//! | `/exit`  | Quit |
//! | `/clear` | Reset conversation (system prompt preserved) |
//! | `/stats` | Memory statistics |
//! | `/tools` | List registered tools |
//!
//! Set `DEEPSEEK_API` in `.env` before running.

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use agent_oxide::core::agent::{Agent, AgentEvent};
use agent_oxide::core::client::{DeepSeekClient, Message, Role};
use agent_oxide::memory::Memory;
use agent_oxide::tools::{
    CalculatorTool, GlobTool, GrepTool, LsTool, ReadTool, ToolRegistry, WorkspaceFs, WriteTool,
};

// ── Constants ──────────────────────────────────────────────────────────────────

const DEFAULT_MODEL: &str = "deepseek-chat";
const MAX_STEPS: usize = 15;

const SYSTEM_PROMPT: &str = "\
You are a helpful AI assistant. You have access to tools for:
- File operations: read, write, list directories, search with glob/grep
- Calculations: evaluate mathematical expressions

Use tools when they help answer the user's request. \
Respond concisely in the same language the user uses. \
If you use a tool, briefly explain what you're doing.\
";

// ── main ───────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // ── Load environment ─────────────────────────────────────────
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API").unwrap_or_else(|_| {
        eprintln!("ERROR: DEEPSEEK_API not set.");
        eprintln!("Create a .env file with: DEEPSEEK_API=sk-...");
        std::process::exit(1);
    });

    // ── Workspace filesystem ─────────────────────────────────────
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let workspace = match WorkspaceFs::new(&cwd) {
        Ok(ws) => Arc::new(ws),
        Err(e) => {
            eprintln!("ERROR: Cannot create workspace at {}: {e}", cwd.display());
            std::process::exit(1);
        }
    };

    // ── Tool registry ────────────────────────────────────────────
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CalculatorTool));
    registry.register(Arc::new(ReadTool::new(workspace.clone())));
    registry.register(Arc::new(WriteTool::new(workspace.clone())));
    registry.register(Arc::new(GlobTool::new(workspace.clone())));
    registry.register(Arc::new(GrepTool::new(workspace.clone())));
    registry.register(Arc::new(LsTool::new(workspace.clone())));

    // ── Agent ────────────────────────────────────────────────────
    let model = std::env::var("AGENT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let client = DeepSeekClient::new(&api_key);
    let memory = Arc::new(std::sync::RwLock::new(Memory::new()));
    let agent = Agent::new(client, memory.clone(), Arc::new(registry))
        .with_model(&model)
        .with_max_steps(MAX_STEPS);

    // ── Seed system prompt ───────────────────────────────────────
    {
        let mut mem = memory.write().unwrap();
        mem.push(Message::new(Role::System, SYSTEM_PROMPT));
    }

    // ── Welcome ──────────────────────────────────────────────────
    print_welcome(&model, &cwd, &agent);

    // ── Interactive loop ─────────────────────────────────────────
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        // Prompt
        print!("\n> ");
        stdout.flush().unwrap();

        let mut input = String::new();
        match stdin.lock().read_line(&mut input) {
            Ok(0) => break, // EOF (Ctrl+D / Ctrl+Z)
            Ok(_) => {}
            Err(e) => {
                eprintln!("Read error: {e}");
                break;
            }
        }

        let input = input.trim().to_owned();
        if input.is_empty() {
            continue;
        }

        // ── Handle special commands ───────────────────────────────
        if handle_command(&input, &memory).await {
            continue;
        }

        // ── Push user message ─────────────────────────────────────
        {
            let mut mem = memory.write().unwrap();
            mem.push(Message::new(Role::User, &input));
        }

        // ── Run agent with real-time event display ────────────────
        print!("🤖 ");
        stdout.flush().unwrap();

        // Create a channel for streaming events from the agent.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        // Spawn a render task that consumes events as they arrive.
        // This runs concurrently with the agent loop — tokens appear
        // on screen while the model is still generating.
        let printer = tokio::spawn(async move {
            render_events(&mut rx).await;
        });

        // `tx` is moved into run_with_events and dropped when it
        // returns, which causes rx.recv().await to return None,
        // signalling the printer task to exit.
        match agent.run_with_events(tx).await {
            Ok(_response) => {
                // Tokens were already printed by render_events.
            }
            Err(e) => {
                println!("\n  ✗ {e}");
            }
        }

        // Wait for the printer task to finish consuming events.
        printer.await.unwrap();
    }

    println!("\nGoodbye!");
}

// ── Event Rendering ───────────────────────────────────────────────────────────

/// Consumes [`AgentEvent`]s from the channel and renders them to stdout.
///
/// Runs in a dedicated tokio task so token display is concurrent with
/// network I/O — the user sees output as it arrives.
async fn render_events(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) {
    let mut stdout = io::stdout();
    let mut tool_mode = false; // true while we're between tool calls

    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::Token(text) => {
                if tool_mode {
                    // First text token after tool results — add spacing
                    println!();
                    tool_mode = false;
                }
                print!("{text}");
            }

            AgentEvent::ReasoningToken(_text) => {
                // Chain-of-thought is verbose. Uncomment the next line
                // to see the model's internal reasoning:
                // print!("\x1b[2m{_text}\x1b[0m");
            }

            AgentEvent::ToolCallStart { name, .. } => {
                print!("\n  🔧 {name} ");
                tool_mode = true;
            }

            AgentEvent::ToolCallArgsDelta { .. } => {
                // JSON arguments are not user-friendly to display.
                // The accumulator reassembles them for execution.
            }

            AgentEvent::ToolResult { name, output, .. } => {
                let preview = truncate_for_display(&output, 150);
                println!("\n  ✓ {name} → {preview}");
            }

            AgentEvent::Done => {
                println!();
            }
        }

        // Critical: flush after every event so tokens appear immediately.
        stdout.flush().unwrap();
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Prints the welcome banner.
fn print_welcome(model: &str, cwd: &std::path::Path, agent: &Agent) {
    let streaming_label = if agent.streaming() {
        "streaming"
    } else {
        "batch"
    };
    println!();
    println!("╔══════════════════════════════════════════════╗");
    println!("║       Agent Oxide — Interactive CLI          ║");
    println!("╠══════════════════════════════════════════════╣");
    println!("║  Model  : {model:<33}║");
    println!("║  Mode   : {streaming_label:<33}║");
    println!("║  Root   : {:<33}║", truncate_path(cwd, 33));
    println!("╠══════════════════════════════════════════════╣");
    println!("║  /exit   — quit                             ║");
    println!("║  /clear  — reset conversation               ║");
    println!("║  /stats  — memory statistics                ║");
    println!("║  /tools  — list registered tools            ║");
    println!("╚══════════════════════════════════════════════╝");
}

/// Handles slash commands. Returns `true` if the input was a command.
async fn handle_command(
    input: &str,
    memory: &std::sync::Arc<std::sync::RwLock<Memory>>,
) -> bool {
    match input {
        "/exit" => {
            println!("Goodbye!");
            std::process::exit(0);
        }
        "/clear" => {
            let mut mem = memory.write().unwrap();
            // Preserve only System messages
            let system_msgs: Vec<Message> = mem
                .to_context_vec()
                .into_iter()
                .filter(|m| m.role == Role::System)
                .collect();
            let count = mem.message_count();
            *mem = Memory::new();
            for msg in system_msgs {
                mem.push(msg);
            }
            println!("  ✓ Cleared {count} messages (system prompt preserved)");
            return true;
        }
        "/stats" => {
            let mem = memory.read().unwrap();
            println!("  Messages : {}", mem.message_count());
            println!("  Chars    : {}", mem.total_chars());
            println!("  Threshold: {} chars", mem.compact_threshold());
            println!("  Keep last: {}", mem.keep_last_n());
            return true;
        }
        "/tools" => {
            println!("  calculator  — evaluate math expressions");
            println!("  read        — read file contents");
            println!("  write       — create or overwrite a file");
            println!("  glob        — find files by pattern");
            println!("  grep        — search file contents");
            println!("  ls          — list directory contents");
            return true;
        }
        _ => {}
    }
    false
}

/// Truncates `text` to `max_len` characters for compact display.
/// Newlines are replaced with spaces to keep output on one line.
fn truncate_for_display(text: &str, max_len: usize) -> String {
    let text = text.replace('\n', " ");
    if text.len() <= max_len {
        return text;
    }
    format!("{}...", &text[..max_len])
}

/// Truncates a path to fit within `max_len` characters, keeping the tail.
fn truncate_path(path: &std::path::Path, max_len: usize) -> String {
    let s = path.display().to_string();
    if s.len() <= max_len {
        return s;
    }
    format!("...{}", &s[s.len().saturating_sub(max_len - 3)..])
}
