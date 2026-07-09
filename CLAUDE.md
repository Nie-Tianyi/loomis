# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```bash
cargo build                        # debug build (all crates)
cargo build --release              # release build
cargo build -p loomis              # build just the binary
cargo test --all                   # run all tests
cargo test -p provider             # run provider crate tests
cargo test -p loomis               # run loomis (binary) tests
cargo clippy --all                 # lint all crates
cargo run -p loomis                # launch the TUI
```

Set `DEEPSEEK_API` in `.env` before running — `dotenvy` loads it at startup.

## Architecture

This is a **Rust agent framework** (Rust 2024 edition, Tokio async). The project is organized as a Cargo workspace (`agent_oxide`) with six crates.

### Workspace structure

```
agent_oxide/
├── Cargo.toml              # [workspace] — members = ["libs/*", "bins/*"]
├── libs/
│   ├── provider/           # LLMClient trait + shared types (Message, ToolCall, ToolDef, etc.)
│   ├── deepseek/           # DeepSeekClient — implements LLMClient with SSE streaming
│   ├── tools/              # Tool trait, ToolRegistry, WorkspaceFs sandbox, generate_schema
│   ├── memory/             # Memory (in-memory buffer), compaction, persistence
│   └── engine/             # Agent (ReAct loop), AgentHook trait, AgentEvent stream
├── bins/
│   └── loomis/             # Binary — concrete tools, hooks, TUI, assembly, main.rs
└── docs/
    └── architecture.md
```

### Crate map

| Crate | Location | Role | Key types |
|-------|----------|------|-----------|
| `provider` | `libs/` | Abstraction | `LLMClient` trait, `Message`, `Role`, `ToolCall`, `ToolDef`, `CompletionRequest`, `CompletionResponse`, `ProviderError`, `StreamChunk`, `Delta` |
| `deepseek` | `libs/` | Concrete | `DeepSeekClient` (impl `LLMClient`), `DeepSeekStream` (SSE parser), `DeepSeekRequest`, `DeepSeekError` |
| `tools` | `libs/` | Abstraction | `Tool` trait (sync, `Send+Sync`), `ToolRegistry`, `WorkspaceFs`, `ToolError`, `FsError`, `generate_schema` |
| `memory` | `libs/` | Abstraction | `Memory` (in-memory buffer), `SharedMemory` (`Arc<RwLock<Memory>>`), `MemoryBuilder`, two-phase compaction, `save_conversation`/`load_conversation`/`list_threads` |
| `engine` | `libs/` | Abstraction | `Agent`, `AgentEvent` (Token, ToolCallStart, ToolResult, etc.), `AgentError`, `AgentHook` trait (on_run_start, before_tool_call, etc.), `EngineContext` |
| `loomis` | `bins/` | Binary + Lib | Concrete tools (CalculatorTool, ReadTool, ShellTool, ...), concrete hooks (CliLoggerHook, DangerousCommandApprovalHook), TUI (ratatui), `build_coding_agent()`, `compact_with_deepseek()`, `main.rs` |

### Dependency graph

```
provider (no internal deps)
    ↑
    ├── deepseek ──────── (implements provider::LLMClient)
    ├── tools ─────────── (uses provider::ToolDef)
    ├── memory ────────── (uses provider::Message)
    ↑
    └── engine ────────── (uses provider + tools + memory)
            ↑
        loomis (bin) ──── (uses all five libs)
```

### Key patterns

- **`LLMClient` trait** — abstraction over LLM providers. Uses `#[async_trait]` for dyn-compatibility. `DeepSeekClient` in `libs/deepseek/` is the reference implementation.
- **`Tool` trait** — sync, object-safe (no `async_trait`). CPU-bound tools run inline; I/O-heavy ones use `spawn_blocking` internally.
- **Two-phase compaction** — `drain_for_compact()` + `apply_compact()`. System messages are never drained. `compact_with_deepseek()` is in the `loomis` crate.
- **`AgentHook` trait** — lifecycle callbacks (on_run_start, on_llm_start, on_llm_end, before_tool_call, after_tool_call). `before_tool_call` can return `Err` to block tool execution. Concrete hooks in `loomis` crate.
- **`AgentEvent` stream** — real-time tokens, tool call starts/args/results via `mpsc::unbounded_channel`. The TUI consumes these for rendering.
- **`WorkspaceFs` sandbox** — all file paths canonicalized and checked against `workspace_root`. `FsError` mapped to `ToolError` by each tool.
- **SSE streaming pipeline** — `DeepSeekStream` in `libs/deepseek/`: HTTP chunk → buffer → `find_event_end` (\n\n) → `extract_sse_data` (strip "data: ") → parse JSON → `StreamChunk`.
- **Shell confirmation** — handled by `DangerousCommandApprovalHook` (stdin-based approval), not the TUI. Commands with `rm -rf` / `drop table` / `format C:` trigger a prompt.
- **`!command` shell execution** — user-typed `!` prefix in the TUI runs commands via `execute_shell_command()`, pushes output to `SharedMemory`, displays via `ShellOutput` events.
- **Conversation persistence** — saves to `.loomis/threads/{name}.json` + `.md`. Auto-save after each agent turn. `/resume` with picker overlay.

### TUI module (`bins/loomis/src/tui/`)

ratatui + crossterm chat interface. Channel topology:

```text
TUI thread                          Agent task (tokio::spawn)
─────────                          ────────────────────────
cmd_tx ───────── TuiCommand ──────→ cmd_rx
agent_rx ←────── AgentEvent ─────── agent_tx
```

- **Keybindings**: Enter (submit), Ctrl+C (cancel), Esc (cancel), Ctrl+D (exit), PgUp/PgDown (scroll), Up/Down (history), Left/Right/Home/End (cursor)
- **Slash commands**: `/exit`, `/new`, `/save <name>`, `/resume [name]`, `/threads`, `/stats`, `/tools`, `/help`
- **Bang prefix**: `!command` runs shell, output shared with agent via SharedMemory

### Roadmap

- [x] `libs/provider` — LLMClient trait + shared types
- [x] `libs/deepseek` — DeepSeekClient with SSE streaming
- [x] `libs/tools` — Tool trait + ToolRegistry + WorkspaceFs
- [x] `libs/memory` — Memory + compaction + persistence
- [x] `libs/engine` — Agent loop + AgentHook + AgentEvent
- [x] `bins/loomis` — Concrete tools, hooks, TUI, assembly
- [ ] Publish lib crates to crates.io
- [ ] Add `libs/openai/`, `libs/anthropic/` provider implementations
- [ ] RAG/vector DB support (Phase 2)
- [ ] `tracing` observability (Phase 3)
