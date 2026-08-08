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
├── Cargo.toml              # [workspace] — members = ["core/*", "extensions/*", "bins/*"]
├── core/
│   ├── provider/           # LLMClient trait + shared types
│   ├── deepseek/           # DeepSeekClient — implements LLMClient
│   ├── tools/              # Tool trait, ToolRegistry, WorkspaceFs, ProgressStream
│   ├── tools-macros/       # #[tool] proc macro
│   ├── memory/             # Memory buffer, PendingHints
│   ├── util/               # Shared workspace utilities (iso8601_now)
│   └── engine/             # Agent (ReAct loop), AgentHook trait, AgentEvent, ResponseRouter
├── extensions/
│   ├── skills/             # SkillDef, SkillRegistry, ActiveSkills — skill discovery & loading
│   ├── compact/            # MicroCompactHook + MacroCompactHook
│   ├── persistence/        # Conversation persistence — save/load threads, PersistenceHook
│   ├── subagent/           # SubagentTool — spawn child agents as tools
│   ├── observability/      # TraceEvent, TraceStore, RunMetrics — full-chain tracing
│   └── sandbox/            # Sandbox runtime — WorkspaceFs, ShellFilter, SandboxHook, etc.
├── bins/
│   └── loomis/             # Binary — concrete tools, hooks, sandbox, TUI
└── docs/
    ├── beginner-developer-guide.md
    ├── senior-developer-guide.md
    └── sandbox-architecture.md
```

### Dependency graph

```text
core/
    provider (no internal deps)
        ↑
        ├── deepseek ──── (impl LLMClient)
        ├── tools ─────── (uses provider + tools-macros)
        ├── memory ────── (uses provider)
        ↑
        └── engine ────── (uses provider + tools + memory)
                ↑
extensions/
    skills ────────────── (no internal deps)
    hooks ─────────────── (uses provider + memory + engine)
    persistence ───────── (uses provider + engine + memory)
    observability ─────── (uses provider + engine + memory)
    sandbox ───────────── (uses engine + memory + provider)
    subagent ──────────── (uses provider + tools + engine + memory + observability)
                ↑
bins/
    loomis ────────────── (uses all crates from core/ and extensions/)
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

### `#[tool]` proc macro
Annotate a struct with `#[tool(name = "...", description = "...", args = ArgsType)]`.
Generates `Tool` trait impl — the struct must define an inherent
`execute_stream(&self, args: ArgsType) -> Result<ProgressStream, ToolError>`.
JSON Schema is lazily generated from `ArgsType` via `schemars`.

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
   high-volume tools (`read`, `shell`, `grep`, `glob`, `edit`, `write`, `ls`)
   in-place, keeping the most recent N intact (default 10).
2. **MacroCompact** — `on_llm_start()` checks `prompt_tokens` from the last
   `Usage` against a token threshold (default 1,000,000 tokens); when over,
   drains old non-System messages (keeping last N), calls a compact model
   for summarisation via `block_on`, inserts summary as System message.

### Sandbox (defense in depth)

| Layer | Component | Role |
| --- | --- | --- |
| 1 | `WorkspaceFs` | Path sandbox — canonicalization, file-size caps, extension blocklist, hidden-file protection, binary detection, TOCTOU re-check; read-only roots (`filesystem.read_only_paths`, defaults to the cargo registry cache) are readable via `read`/`ls`/`glob`/`grep` but never writable |
| 2 | `ShellFilter` | Command classification — auto-approve (prefixes: `git`, `cargo`, `npm`, `node`, `python`, etc.), deny (patterns: `rm -rf /`, `sudo`), prompt user for rest |
| 3 | `SandboxHook` | Orchestrator — checks quotas, classifies commands, prompts user via `InterventionRequired`, logs to `AuditLogger`. Uses `ResponseRouter` + rendezvous channel for blocking approval |
| 4 | `EnvSanitizer` | Clears dangerous env vars before spawning child processes |
| 5 | Watchdog | Kills process tree on timeout (`taskkill /F /T` on Windows) |

Config: `.loomis/config.toml` → `SandboxConfig` (safe defaults if missing).
Shell output is capped at **100 KB**.

### Observability (full-chain tracing)
`ObservabilityHook` captures lifecycle events with timing data and token
counts via a side channel (`Arc<TraceStore>`) shared between agent task and
TUI. `TraceStore` is a thread-safe ring buffer (4096 entries) with lock-free
`RunMetrics` atomics. All trace events are automatically written to
`.loomis/logs/loomis.log` (daily rolling).

### Plan Mode (read-only research & planning)
Toggled via `/plan`. `PlanModeHook` runs at position 5 (after `SkillHook`) —
`before_tool_call` blocks write/edit/shell (except `.loomis/plan.md`).
Allowed tools: `read`,
`ls`, `glob`, `grep`, `calculator`, `ask_user_question`, `todo`, `task`/
`subagent`, `enter_plan_mode`, `exit_plan_mode`, `write` (only to
`.loomis/plan.md`). On `/approve` or `exit_plan_mode` approval, the plan is
archived to `.loomis/plan/<summary>.md` so past plans are never overwritten.
`/approve` exits plan mode.

### Skills system
Skills provide reusable domain knowledge and specialized instructions as
System messages. Three components work together:

| Component | Crate | Role |
| --- | --- | --- |
| `SkillRegistry` | `extensions/skills` | Discover + parse `.md` skill files (YAML frontmatter + body) from skill directories |
| `SkillTool` | `bins/loomis` | Tool (`name = "skill"`) — loaded as System message, gives the agent the skill's instructions |
| `SkillHook` | `bins/loomis` | Maintains `[SKILL: name]` System messages in memory, synced with `ActiveSkills` |

Skill files live in `bins/loomis/skills/` (discovered at startup, listed in
the system prompt via `{skill_list}`). The `/skill <name>` slash command loads
a skill manually. `ActiveSkills` (`Arc<RwLock<HashMap<String, String>>`) is
shared between the TUI, SkillTool, and SkillHook.

### Profile system
`ProfileHook` builds a user profile across sessions and injects a `[PROFILE]`
System message at the tail of the System block (via `insert_before_history`,
never at index 0 — a front-of-request insert would invalidate the
prompt-cache prefix) into every LLM call. Two-tier design:

1. **Real-time** (zero-token): language detection from user input (CJK heuristic),
   per-tool invocation counters, session count + timestamp.
2. **LLM synthesis** (every 5 sessions): uses a cheap flash model to analyze
   recent conversation context and extract preferences, avoidances, expertise
   signals, coding conventions, verbosity, and language preference.

Profile is persisted to `.loomis/profile.json` in the workspace. Merging is
conservative — only non-empty synthesis fields overwrite existing values.

### Concrete tools (14)
`Calculator`, `Read`, `Edit`, `Write`, `Glob`, `Grep`, `Ls`, `Shell`,
`Subagent`/`task`, `AskUserQuestion`, `Todo`, `EnterPlanMode`, `ExitPlanMode`,
`Skill`

### Concrete hooks (8 loomis + 2 from hooks crate)
`SystemPromptHook` (seed prompts with tool list + skill list + env context + project rules),
`PlanModeHook` (tool restriction),
`ObservabilityHook` (trace collection),
`PersistenceHook` (auto-save),
`TodoListHook` (sync todo state),
`SkillHook` (sync active skills to `[SKILL: ...]` System messages),
`ProfileHook` (build + inject `[PROFILE]` System message),
`SandboxHook` (security) +
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
`/resume [name]`, `/threads`, `/stats`, `/tools`, `/help`, `/skill <name>`,
`/init`

**Bang prefix**: `!command` — runs shell, output shared with agent.

### `ResponseRouter`
Maps `request_id` → `SyncSender<InterventionResponse>`. Multiple components
can need user intervention simultaneously — each registers its own channel.
TUI routes responses through the router.
