# LOOMIS.md

This file provides guidance to the Loomis agent when working with code
in this repository.

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
See `.env.example` for all supported env vars (`BASE_URL`, `DEFAULT_PRO_MODEL`,
`DEFAULT_FLASH_MODEL`).

Tests are **inline** (`#[cfg(test)] mod tests { ... }`) co-located with source
— no separate `tests/` directories.

## Architecture

**Rust agent framework** (Rust 2024 edition, Tokio async). Cargo workspace
named `agent_oxide`.

**Rust edition**: Uses Rust 2024 with native async fn in traits (RPITIT).
Do NOT use `async-trait` crate. Prefer sync traits for dyn-dispatch; keep
async work in dedicated components.

### Workspace structure

```
agent_oxide/
├── Cargo.toml              # [workspace] — members = ["libs/*", "bins/*"]
├── libs/
│   ├── provider/           # LLMClient trait + shared types
│   ├── deepseek/           # DeepSeekClient — implements LLMClient
│   ├── tools/              # Tool trait, ToolRegistry, WorkspaceFs, ProgressStream
│   ├── tools-macros/       # #[tool] proc macro
│   ├── memory/             # Memory buffer, PendingHints, conversation persistence
│   ├── hooks/              # MicroCompactHook + MacroCompactHook
│   ├── engine/             # Agent (ReAct loop), AgentHook trait, AgentEvent, ResponseRouter
│   ├── subagent/           # SubagentTool — spawn child agents as tools
│   └── observability/      # TraceEvent, TraceStore, RunMetrics — full-chain tracing
├── bins/
│   └── loomis/             # Binary — concrete tools, hooks, sandbox, TUI
└── docs/
    ├── beginner-developer-guide.md
    ├── senior-developer-guide.md
    └── sandbox-architecture.md
```

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
    ├── subagent ──────── (uses provider + tools + engine + memory + observability)
    │       ↑
    ├── observability ─── (uses provider)
    │       ↑
    └── loomis (bin) ──── (uses all libs)
```

## Key patterns

### `LLMClient` trait
Abstraction over LLM providers. Uses Rust 2024 native async fn (NOT
`#[async_trait]`). `DeepSeekClient` is the reference implementation.

### `Tool` trait
Sync and object-safe. `execute_stream()` returns `ProgressStream` — short
tools emit a single `Progress::Done`, long-running tools (shell) emit
`Progress::InProgress` updates then `Progress::Done`. Use
`tokio::sync::mpsc` from a spawned thread for async I/O.

### `AgentHook` trait — 9 lifecycle callbacks
All have default no-ops. Naming convention:

| Prefix | Meaning |
| --- | --- |
| `on_<event>` | Pure notification — cannot intervene |
| `before_<action>` | Can intervene — return `Err` to block |
| `after_<action>` | Observe result — cannot intervene |

Callbacks (all receive `session_id: &str`):
- `on_run_start(&str, user_input: &str, memory: &SharedMemory)`
- `on_run_finish(&str, outcome: &RunOutcome, memory: &SharedMemory)`
- `on_step_start(&str, step: usize, max_steps: usize)`
- `on_llm_start(&str, memory: &SharedMemory)`
- `on_llm_end(&str, response: &Message)`
- `on_llm_error(&str, error: &ProviderError, attempt: usize, will_retry: bool)`
- `before_tool_call(&str, tool_call: &ToolCall) -> Result<(), AgentError>`
- `after_tool_call(&str, tool_call: &ToolCall, observation: &str)`
- `on_tool_failed(&str, tool_call: &ToolCall, error: &str)`

Hooks run in registration order. For async work inside sync hooks (e.g. LLM
summarisation), use `tokio::runtime::Handle::block_on` — the agent loop runs
in a dedicated tokio task separate from the TUI thread.

### `AgentEvent` stream
Single `mpsc::unbounded_channel`. Variants:

| Event | When |
| --- | --- |
| `RunStarted { session_id, user_input }` | New task begins |
| `Token(String)` / `ReasoningToken(String)` | LLM output streaming |
| `ToolCallStart { id, name }` | Tool name known before args |
| `ToolCall { id, name, arguments, origin }` | Before tool execution |
| `ToolSuccessful { id, name, output }` | Tool completed |
| `ToolRejected { id, name, reason }` | Hook blocked tool |
| `ToolFailure { id, name, error }` | Tool execution failed |
| `ToolProgress { id, name, message }` | Real-time progress |
| `InterventionRequired(InterventionRequest)` | Hook needs user decision |
| `RunCompleted { answer }` | Success |
| `RunFailed { error }` | Error |
| `Cancelled` | User cancelled |
| `Done` | Sentinel — always last |

`CallOrigin::Llm` vs `CallOrigin::User` distinguishes LLM tool calls from
user `!command` invocations.

### `AgentBuilder` vs `EngineContextBuilder`
- `Agent::builder(client, model)` — simple API: auto-creates Memory, seeds
  system prompt, collects tools into ToolRegistry.
- `EngineContext::builder(client, memory, tools, model)` — advanced API:
  supply Memory and ToolRegistry explicitly, configure hooks, max_steps,
  max_retries, streaming, pending_hints.

### Two-tier compaction (hooks crate)
1. **MicroCompact** — `on_llm_start()` clears old tool outputs from
   high-volume tools (read, shell, grep, glob, edit, write, ls) in-place.
2. **MacroCompact** — `on_llm_start()` checks `total_chars()`; when over
   threshold, drains old non-System messages (keeping last N), calls a
   compact model for summarisation via `block_on`, inserts summary as
   System message.

### Sandbox (defense in depth)

| Layer | Component | Role |
| --- | --- | --- |
| 1 | `WorkspaceFs` | Path sandbox — canonicalization, file-size caps, extension blocklist, hidden-file protection, binary detection, TOCTOU re-check |
| 2 | `ShellFilter` | Command classification — auto-approve (prefixes: `git`, `cargo`), deny (patterns: `rm -rf /`, `sudo`), prompt user for rest |
| 3 | `SandboxHook` | Orchestrator — checks quotas, classifies commands, prompts user via `InterventionRequired`, logs to `AuditLogger`. Uses `ResponseRouter` + rendezvous channel for blocking approval |
| 4 | `EnvSanitizer` | Clears dangerous env vars before spawning child processes |
| 5 | Watchdog | Kills process tree on timeout (`taskkill /F /T` on Windows) |

Config: `.loomis/config.toml` → `SandboxConfig` (safe defaults if missing).
Shell output is capped at **100 KB**.

### Observability (full-chain tracing)
`ObservabilityHook` captures lifecycle events with timing data and token
counts via a side channel (`Arc<TraceStore>`) shared between agent task and
TUI. `TraceStore` is a thread-safe ring buffer (4096 entries) with lock-free
`RunMetrics` atomics. TUI drains at 20fps. Toggle debug overlay with `Ctrl+O`
or `/debug`. Export traces with `/trace-save` → `.loomis/traces/`.

### Plan Mode (read-only research & planning)
Toggled via `/plan`. `PlanModeHook` runs at position 1 — `before_tool_call`
blocks write/edit/shell (except `.loomis/plan.md`). Allowed tools: `read`,
`ls`, `glob`, `grep`, `calculator`, `ask_user_question`, `todo`, `task`/
`subagent`, `write` (only to `.loomis/plan.md`). `/approve` exits plan mode.

### Concrete tools (13)
`Calculator`, `Read`, `Edit`, `Write`, `Glob`, `Grep`, `Ls`, `Shell`,
`Subagent`, `AskUserQuestion`, `Todo`, `EnterPlanMode`, `ExitPlanMode`

Note: `EchoTool` exists in source but is not registered in `build_coding_agent()`.

### Concrete hooks (6 loomis + 2 from hooks crate)
`SystemPromptHook` (seed prompts), `PlanModeHook` (tool restriction),
`ObservabilityHook` (trace collection), `PersistenceHook` (auto-save),
`TodoListHook` (sync todo state), `SandboxHook` (security) +
`MicroCompactHook` + `MacroCompactHook<C>` from the hooks crate.

### TUI module (`bins/loomis/src/tui/`)
ratatui + crossterm. Channel topology:

```text
TUI thread                          Agent task (tokio::spawn)
─────────                          ────────────────────────
cmd_tx ───────── TuiCommand ──────→ cmd_rx
agent_rx ←────── AgentEvent ─────── agent_tx
```

**Slash commands**: `/exit`, `/new`, `/plan`, `/approve`, `/save <name>`,
`/resume [name]`, `/threads`, `/stats`, `/tools`, `/debug`, `/trace-save`,
`/help`

**Bang prefix**: `!command` — runs shell, output shared with agent.

### `ResponseRouter`
Maps `request_id` → `SyncSender<InterventionResponse>`. Multiple components
can need user intervention simultaneously — each registers its own channel.
TUI routes responses through the router.
