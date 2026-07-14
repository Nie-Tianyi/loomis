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

**Rust agent framework** (Rust 2024 edition, Tokio async). Cargo workspace named `agent_oxide`.

**Rust edition**: Uses Rust 2024 with native async fn in traits (RPITIT). Do NOT use `async-trait` crate. Prefer sync traits for dyn-dispatch; keep async work in dedicated components.

### Workspace structure

```
agent_oxide/
├── Cargo.toml              # [workspace] — members = ["libs/*", "bins/*"]
├── libs/
│   ├── provider/           # LLMClient trait + shared types (Message, ToolCall, ToolDef, etc.)
│   ├── deepseek/           # DeepSeekClient — implements LLMClient with SSE streaming
│   ├── tools/              # Tool trait, ToolRegistry, WorkspaceFs sandbox, ProgressStream
│   ├── tools-macros/       # #[tool] proc macro — generates Tool trait impls
│   ├── memory/             # Memory buffer, PendingHints, conversation persistence
│   ├── hooks/              # MicroCompactHook + MacroCompactHook (AgentHook impls)
│   ├── engine/             # Agent (ReAct loop), AgentHook trait, AgentEvent stream, ResponseRouter
│   └── subagent/           # SubagentTool — spawn child agents as tools
├── bins/
│   └── loomis/             # Binary — concrete tools, hooks, sandbox, TUI, assembly
└── docs/
    ├── beginner-developer-guide.md
    ├── senior-developer-guide.md
    └── sandbox-architecture.md
```

### Crate map

| Crate | Role | Key types |
| --- | --- | --- |
| `provider` | Abstraction | `LLMClient` trait, `Message`, `Role`, `ToolCall`, `ToolCallKind`, `ToolDef`, `CompletionRequest`/`Response`, `ProviderError`, `StreamChunk`, `Delta`, `FinishReason`, `Usage` |
| `deepseek` | Provider impl | `DeepSeekClient` (impl `LLMClient`), SSE streaming parser |
| `tools` | Abstraction | `Tool` trait (sync, `Send+Sync`), `ToolRegistry`, `WorkspaceFs`, `SandboxConfig`, `Progress`, `ProgressStream`, `ToolError` |
| `tools-macros` | Proc macro | `#[tool]` attribute — generates `Tool` trait impl |
| `memory` | Abstraction | `Memory`, `SharedMemory`, `PendingHints`, `PersistenceConfig`, `ThreadInfo`, persistence fns |
| `hooks` | Concrete hooks | `MicroCompactHook`, `MacroCompactHook<C>`, compaction constants |
| `engine` | Core loop | `Agent`, `AgentBuilder`, `AgentHook` trait, `AgentEvent`, `AgentError`, `EngineContext`, `ResponseRouter`, `InterventionRequest`/`Response`, `RunOutcome`, `CallOrigin` |
| `subagent` | Concrete | `SubagentTool<C>`, `SubagentConfig`, `filter_tools()` |
| `loomis` | Binary | 11 concrete tools, 4 hooks (Sandbox, Persistence, SystemPrompt, TodoList), sandbox system, TUI, `AgentKit`, `build_coding_agent()` |

### Dependency graph

```text
provider (no internal deps)
    ↑
    ├── deepseek ──────── (impl LLMClient)
    ├── tools ─────────── (uses ToolDef)
    ├── memory ────────── (uses Message)
    ↑
    ├── engine ────────── (uses provider + tools + memory)
    │       ↑
    ├── hooks ─────────── (uses provider + memory + engine)
    │       ↑
    ├── subagent ──────── (uses provider + tools + engine + memory)
    │       ↑
    └── loomis (bin) ──── (uses all libs)
```

## Key patterns

### `LLMClient` trait
Abstraction over LLM providers. Uses Rust 2024 native async fn, NOT `#[async_trait]`. `DeepSeekClient` is the reference implementation.

### `Tool` trait
Sync and object-safe. `execute_stream()` returns `ProgressStream` — short tools emit a single `Progress::Done`, long-running tools (shell) emit `Progress::InProgress` updates then `Progress::Done`. Use `tokio::sync::mpsc` from a spawned thread for async I/O.

### `AgentHook` trait
10 lifecycle callbacks, all with default no-ops. Naming convention:

| Prefix | Meaning |
| --- | --- |
| `on_<event>` | Pure notification — cannot intervene |
| `before_<action>` | Can intervene — return `Err` to block |
| `after_<action>` | Observe result — cannot intervene |

Callbacks: `on_run_start`, `on_run_finish(&RunOutcome)`, `on_step_start`, `on_llm_start(&SharedMemory)`, `on_llm_end(&Message)`, `on_llm_error(&ProviderError, attempt, will_retry)`, `before_tool_call`, `after_tool_call`, `on_tool_failed`.

Hooks run in registration order. For async work inside sync hooks (e.g. LLM summarisation), use `tokio::runtime::Handle::block_on` — the agent loop runs in a dedicated tokio task separate from the TUI thread.

Concrete hooks:
- **`hooks` crate**: `MicroCompactHook` (tool-output clearing), `MacroCompactHook<C>` (LLM summarisation)
- **`loomis` crate**: `SandboxHook` (security), `PersistenceHook` (auto-save), `SystemPromptHook` (seed prompts), `TodoListHook` (sync todo state)

### `AgentEvent` stream
Single `mpsc::unbounded_channel`. Variants:

| Event | When |
| --- | --- |
| `RunStarted { session_id, user_input }` | New task begins |
| `Token(String)` / `ReasoningToken(String)` | LLM output streaming |
| `ToolCall { id, name, arguments, origin }` | Before tool execution |
| `ToolSuccessful { id, name, output }` | Tool completed |
| `ToolRejected { id, name, reason }` | Hook blocked tool |
| `ToolFailure { id, name, error }` | Tool execution failed |
| `ToolProgress { id, name, message }` | Real-time progress from tool |
| `InterventionRequired(InterventionRequest)` | Hook needs user decision |
| `RunCompleted { answer }` | Success |
| `RunFailed { error }` | Error |
| `Cancelled` | User cancelled |
| `Done` | Sentinel — always last |

`CallOrigin::Llm` vs `CallOrigin::User` distinguishes LLM tool calls from user `!command` invocations.

### `AgentBuilder` vs `EngineContextBuilder`
- `Agent::builder(client, model)` — simple API: auto-creates Memory, seeds system prompt, collects tools into ToolRegistry.
- `EngineContext::builder(client, memory, tools, model)` — advanced API: supply Memory and ToolRegistry explicitly, configure hooks, max_steps, max_retries, streaming, pending_hints.

### Two-tier compaction
Both in the `hooks` crate:
1. **MicroCompact** — `on_llm_start()` clears old tool outputs from high-volume tools (read, shell, grep, glob, edit, write, ls) in-place.
2. **MacroCompact** — `on_llm_start()` checks `total_chars()`; when over threshold, drains old non-System messages (keeping last N), calls a compact model for summarisation via `block_on`, inserts summary as System message.

### Sandbox (defense in depth)

| Layer | Component | Role |
| --- | --- | --- |
| 1 | `WorkspaceFs` | Path sandbox — canonicalization, file-size caps, extension blocklist, hidden-file protection, binary detection, TOCTOU re-check |
| 2 | `ShellFilter` | Command classification — auto-approve (prefixes like `git`, `cargo`), deny (patterns like `rm -rf /`, `sudo`), prompt user for rest. Strict allowlist mode via `allowed_commands.binaries`. Runs in both `before_tool_call` and `execute_stream` |
| 3 | `SandboxHook` | Orchestrator — checks quotas, classifies commands, prompts user via `InterventionRequired`, updates resource counters, logs to `AuditLogger`. Uses `ResponseRouter` + rendezvous channel for blocking approval |
| 4 | `EnvSanitizer` | Clears dangerous env vars before spawning child processes |
| 5 | Watchdog | Kills process tree on timeout (`taskkill /F /T` on Windows) |

Config: `.loomis/config.toml` → `SandboxConfig` (safe defaults if missing).

### `ResponseRouter`
Maps `request_id` → `SyncSender<InterventionResponse>`. Multiple components (SandboxHook, AskUserQuestionTool) can need user intervention simultaneously — each registers its own channel, sends an `InterventionRequired` event with its `request_id`, and blocks on its receiver. The TUI routes responses through the router.

### `!command` shell execution
User-typed `!` prefix in TUI runs commands, pushes output to `SharedMemory`, emits unified `ToolCall { origin: User }` / `ToolSuccessful` events.

### Conversation persistence
Auto-saves to `.loomis/threads/{name}.json` + `.md` after each agent turn via `PersistenceHook`. Thread picker via `/resume`. Date suffixes on thread names for uniqueness.

### Subagent
`SubagentTool<C>` wraps a child `Agent` as a `Tool`. Spawned with filtered (typically read-only) tool set and its own Memory. Results streamed as `Progress::InProgress`/`Progress::Done`. Config: `SubagentConfig` (model, max_steps, timeout_secs, inherit_context_messages).

### Loomis concrete tools (11)
`Calculator`, `Read`, `Edit`, `Write`, `Glob`, `Grep`, `Ls`, `Shell`, `Subagent`, `AskUserQuestion`, `Todo`

### Loomis concrete hooks (4)
`SystemPromptHook` (seeds initial system messages), `PersistenceHook` (auto-save), `TodoListHook` (syncs [TODO] System message), `SandboxHook` (security)

### TUI module (`bins/loomis/src/tui/`)

ratatui + crossterm. Channel topology:

```text
TUI thread                          Agent task (tokio::spawn)
─────────                          ────────────────────────
cmd_tx ───────── TuiCommand ──────→ cmd_rx
agent_rx ←────── AgentEvent ─────── agent_tx
```

- **Keybindings**: Enter (submit), Ctrl+C (cancel), Esc (cancel), Ctrl+D (exit), PgUp/PgDown (scroll), Up/Down (history), Left/Right/Home/End (cursor). Intervention prompts: ↑/↓ (navigate), Enter (select), Esc (cancel)
- **Slash commands**: `/exit`, `/new`, `/save <name>`, `/resume [name]`, `/threads`, `/stats`, `/tools`, `/help`
- **Bang prefix**: `!command` — runs shell, output shared with agent

## Future work

- [ ] Publish lib crates to crates.io
- [ ] Add `libs/openai/`, `libs/anthropic/` provider implementations
- [ ] RAG/vector DB support
- [ ] `tracing` observability
