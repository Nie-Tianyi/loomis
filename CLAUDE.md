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

This is a **Rust agent framework** (Rust 2024 edition, Tokio async). The project is organized as a Cargo workspace (`agent_oxide`).

**Rust edition**: The codebase uses Rust 2024 which has **native async fn in traits** (RPITIT). Do NOT bring in the `async-trait` crate — use `async fn` directly in trait definitions. `Box<dyn Trait>` with async methods requires dyn-compatibility; prefer sync traits for dyn dispatch and keep async work in dedicated components.

### Workspace structure

```
agent_oxide/
├── Cargo.toml              # [workspace] — members = ["libs/*", "bins/*"]
├── libs/
│   ├── provider/           # LLMClient trait + shared types (Message, ToolCall, ToolDef, etc.)
│   ├── deepseek/           # DeepSeekClient — implements LLMClient with SSE streaming
│   ├── tools/              # Tool trait, ToolRegistry, WorkspaceFs sandbox, ProgressStream
│   ├── tools-macros/       # #[tool] proc macro — generates Tool trait impls
│   ├── memory/             # Memory (in-memory buffer), persistence
│   ├── hooks/              # Ready-to-use AgentHook impls (MicroCompactHook, MacroCompactHook)
│   ├── engine/             # Agent (ReAct loop), AgentHook trait, AgentEvent stream
│   └── subagent/           # SubagentTool — spawn child agents as tools
├── bins/
│   └── loomis/             # Binary — concrete tools, sandbox, hooks, TUI, assembly, main.rs
└── docs/
    ├── developer-guide.md
    └── sandbox-architecture.md
```

### Crate map

| Crate | Location | Role | Key types |
|-------|----------|------|-----------|
| `provider` | `libs/` | Abstraction | `LLMClient` trait, `Message`, `Role`, `ToolCall`, `ToolDef`, `CompletionRequest`, `CompletionResponse`, `ProviderError`, `StreamChunk`, `Delta` |
| `deepseek` | `libs/` | Concrete | `DeepSeekClient` (impl `LLMClient`), `DeepSeekStream` (SSE parser), `DeepSeekRequest`, `DeepSeekError` |
| `tools` | `libs/` | Abstraction | `Tool` trait (sync, `Send+Sync`), `ToolRegistry`, `WorkspaceFs`, `SandboxConfig`, `Progress`, `ProgressStream`, `ToolError`, `FsError`, `generate_schema` |
| `tools-macros` | `libs/` | Proc macro | `#[tool]` attribute — generates `Tool` trait impl from struct + attribute args |
| `memory` | `libs/` | Abstraction | `Memory` (in-memory buffer, `pub messages: Vec<Message>`), `SharedMemory`, `save_conversation`/`load_conversation`/`list_threads` |
| `hooks` | `libs/` | Concrete | `MicroCompactHook` (tool-output clearing in `on_llm_start`), `MacroCompactHook<C>` (LLM summarisation in `on_llm_start`), `DEFAULT_COMPACT_CHARS`, `DEFAULT_KEEP_LAST_N` |
| `engine` | `libs/` | Abstraction | `Agent`, `AgentBuilder`, `AgentEvent`, `AgentError`, `AgentHook` trait, `EngineContext`, `EngineContextBuilder`, `CallOrigin`, `InterveneRequest`, `InterveneResponse`, `RunOutcome` |
| `subagent` | `libs/` | Concrete | `SubagentTool<C>` (Tool impl that spawns child Agent), `SubagentConfig`, `filter_tools()` |
| `loomis` | `bins/` | Binary + Lib | Concrete tools (CalculatorTool, ReadTool, ShellTool, ...), sandbox system (SandboxHook, ShellFilter, AuditLogger, ResourceTracker, EnvSanitizer), TUI (ratatui), `build_coding_agent()`, `main.rs` |

### Dependency graph

```
provider (no internal deps)
    ↑
    ├── deepseek ──────── (implements provider::LLMClient)
    ├── tools ─────────── (uses provider::ToolDef)
    ├── memory ────────── (uses provider::Message)
    ↑
    ├── engine ────────── (uses provider + tools + memory)
    │       ↑
    ├── hooks ─────────── (uses provider + memory + engine)
    │       ↑
    ├── subagent ──────── (uses provider + tools + engine + memory)
    │       ↑
    └── loomis (bin) ──── (uses all libs)
```

### Key patterns

- **`LLMClient` trait** — abstraction over LLM providers. Uses Rust 2024 native async fn (`impl Future<Output = ...> + Send`), NOT `#[async_trait]`. `DeepSeekClient` in `libs/deepseek/` is the reference implementation.
- **`Tool` trait** — sync, object-safe (no `async_trait`). Returns `ProgressStream` from `execute_stream()` — short-lived tools emit a single `Progress::Done`, long-running tools (shell) emit `Progress::InProgress` updates followed by `Progress::Done`. Use `tokio::sync::mpsc` from a spawned thread for async I/O tools.
- **Two-tier compaction** — both tiers live in the `hooks` crate as `AgentHook` impls: (1) **MicroCompact** (`MicroCompactHook`): `on_llm_start()` clears old tool outputs from high-volume tools (read, shell, grep, glob, edit, write, ls) in-place, replacing content with `[Old tool result content cleared]`. (2) **MacroCompact** (`MacroCompactHook<C>`): `on_llm_start()` checks `memory.total_chars()` against a threshold; when exceeded, drains old non-System messages (keeping the most recent N), calls the compact model for LLM summarisation via `tokio::runtime::Handle::block_on`, and inserts the summary as a System message. Register both hooks via `Agent::builder().hook(...)` or `EngineContext::builder().hook(...)`.
- **`AgentHook` trait** — 10 lifecycle callbacks, all with default no-ops: `on_run_start`, `on_run_finish(&RunOutcome)`, `on_step_start`, `on_llm_start(&SharedMemory)`, `on_llm_end(&Message)`, `on_llm_error(&ProviderError, attempt, will_retry)`, `before_tool_call` (can return `Err` to block), `after_tool_call`, `on_tool_failed`. Hooks are called in registration order. Concrete hooks: `MicroCompactHook`, `MacroCompactHook` (in `hooks` crate), `SandboxHook` (in `loomis` crate).
- **`AgentEvent` stream** — single `mpsc::unbounded_channel` for all events. `RunStarted`/`Token`/`ReasoningToken` for LLM output, `ToolCall { origin, .. }`/`ToolSuccessful`/`ToolRejected`/`ToolFailure`/`ToolProgress` for tools (with `CallOrigin::Llm` vs `CallOrigin::User`), `NeedUserIntervene(InterveneRequest)` for interactive user prompts, `RunCompleted`/`RunFailed`/`Cancelled` for run outcomes, `Done` sentinel. The TUI consumes this single channel for rendering — no separate hook-event channel.
- **`Agent::builder()` vs `EngineContext::builder()`** — `Agent::builder(client, model)` is the simple API (auto-creates Memory, auto-seeds system prompt, collects tools into ToolRegistry). `EngineContext::builder(client, memory, tools, model)` is the advanced API (you supply Memory and ToolRegistry explicitly).
- **`WorkspaceFs` sandbox** — all file paths canonicalized and checked against `workspace_root`. Enforces file-size caps, extension blocklist, hidden-file protection, binary-content detection, and TOCTOU re-checks. Policy driven by [`SandboxConfig`](libs/tools/src/sandbox/config.rs) (loaded from `.loomis/config.toml`).
- **Multi-layered sandbox** — defense in depth: (1) `WorkspaceFs` path sandbox for file tools; (2) `ShellFilter` command classification (auto-approve / deny / prompt); (3) `SandboxHook` orchestrates permission checks, resource quotas, and audit logging; (4) `EnvSanitizer` clears dangerous env vars before spawning child processes; (5) Watchdog kills process tree on timeout (`taskkill /F /T` on Windows).
- **Shell sandbox** — `ShellFilter` classifies commands: `auto_approve.prefixes` (e.g. `git`, `cargo`) run immediately; `deny_patterns` (e.g. `rm -rf /`, `sudo`) are blocked outright; everything else prompts the user via `SandboxHook` using `InterveneRequest`/`InterveneResponse` (navigable options: Approve/Deny/Other…). Strict allowlist mode available via `allowed_commands.binaries`. Environment variables are sanitized (cleared then restored from a safe allowlist) before child processes are spawned. ShellFilter runs in both `before_tool_call` (Hook layer) and `execute_stream` (Tool layer) as dual defense.
- **SandboxHook** — unified `AgentHook` for sandbox enforcement. In `before_tool_call`: checks `ResourceTracker` quotas, classifies commands via `ShellFilter`, auto-approves/denies/prompts accordingly via `AgentEvent::NeedUserIntervene`. In `after_tool_call`: updates `ResourceTracker` counters, logs to `AuditLogger`. In `on_run_finish`: logs final outcome. Uses `SyncSender<InterveneResponse>` rendez-vous channel for blocking user approval.
- **SSE streaming pipeline** — `DeepSeekStream` in `libs/deepseek/`: HTTP chunk → buffer → `find_event_end` (\n\n) → `extract_sse_data` (strip "data: ") → parse JSON → `StreamChunk`.
- **Sandbox configuration** — `.loomis/config.toml` controls all sandbox policies (filesystem limits, shell filtering, resource quotas, audit). Missing file → safe defaults. See [`SandboxConfig`](libs/tools/src/sandbox/config.rs) for the full schema.
- **`!command` shell execution** — user-typed `!` prefix in the TUI runs commands via `execute_shell_command()`, pushes output to `SharedMemory`, displays via unified `ToolCall { origin: User }` / `ToolSuccessful` events (same channel as LLM tool calls).
- **Conversation persistence** — saves to `.loomis/threads/{name}.json` + `.md`. Functions take `workspace_root: &Path`. Auto-save after each agent turn. `/resume` with picker overlay.
- **Subagent** — `SubagentTool<C>` in `libs/subagent/` wraps a child `Agent` as a `Tool`. When the parent LLM calls `task`, a child agent is spawned with a filtered (typically read-only) tool set and its own Memory. Results are streamed back as `Progress::InProgress`/`Progress::Done`. Configured via `SubagentConfig` (model, max_steps, timeout_secs, inherit_context_messages). `filter_tools()` creates a filtered `ToolRegistry` from the parent's.

### TUI module (`bins/loomis/src/tui/`)

ratatui + crossterm chat interface. Channel topology:

```text
TUI thread                          Agent task (tokio::spawn)
─────────                          ────────────────────────
cmd_tx ───────── TuiCommand ──────→ cmd_rx
agent_rx ←────── AgentEvent ─────── agent_tx
```

- **Keybindings**: Enter (submit), Ctrl+C (cancel), Esc (cancel), Ctrl+D (exit), PgUp/PgDown (scroll), Up/Down (history), Left/Right/Home/End (cursor). During intervention prompts: ↑/↓ (navigate options), Enter (select), Esc (cancel)
- **Slash commands**: `/exit`, `/new`, `/save <name>`, `/resume [name]`, `/threads`, `/stats`, `/tools`, `/help`
- **Bang prefix**: `!command` runs shell, output shared with agent via SharedMemory. Displayed with `$` prefix (unified `ToolCall` with `origin: User`)

### Roadmap

- [x] `libs/provider` — LLMClient trait + shared types
- [x] `libs/deepseek` — DeepSeekClient with SSE streaming
- [x] `libs/tools` — Tool trait + ToolRegistry + WorkspaceFs + SandboxConfig
- [x] `libs/tools-macros` — `#[tool]` proc macro
- [x] `libs/memory` — Memory + persistence
- [x] `libs/engine` — Agent loop + AgentHook + AgentEvent
- [x] `libs/hooks` — MicroCompactHook + MacroCompactHook
- [x] `libs/subagent` — SubagentTool + SubagentConfig + filter_tools
- [x] `bins/loomis` — Concrete tools, hooks, sandbox system, TUI, assembly
- [x] Multi-layered sandbox — WorkspaceFs hardening + ShellFilter + SandboxHook + ResourceTracker + AuditLogger
- [x] Two-tier compaction — MicroCompact (tool output clearing) + MacroCompact (LLM summarisation via hooks)
- [ ] Publish lib crates to crates.io
- [ ] Add `libs/openai/`, `libs/anthropic/` provider implementations
- [ ] RAG/vector DB support (Phase 2)
- [ ] `tracing` observability (Phase 3)
