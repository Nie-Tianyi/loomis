# LOOMIS.md

This file provides guidance to the Loomis agent when working with code
in this repository.

## Build & Test

```bash
cargo build                        # debug build
cargo build --release              # release build
cargo test --all                   # all tests (workspace = loomis-core + loomis)
cargo test -p loomis-core <name>   # core tests (tools/hooks/profile/runtime) by name substring
cargo test -p loomis <name>        # TUI tests by name substring
cargo clippy --all                 # lint
cargo run -p loomis                # launch the TUI
```

For framework tests, cd into the sibling `agent_oxide/` repo and run
`cargo test` — see `AGENT_OXIDE.md` there.

Set `DEEPSEEK_API` in `.env` before running — `dotenvy` loads it at startup.
See `.env.example` for all supported env vars (`BASE_URL`, `DEFAULT_PRO_MODEL`,
`DEFAULT_FLASH_MODEL`).

Tests are **inline** (`#[cfg(test)] mod tests { ... }`) co-located with source
— no separate `tests/` directories.

Commit messages follow the existing conventional style. Do **not** append
`Co-Authored-By` trailers or other attribution lines — the user owns these
commits.

## Architecture

**Rust agent application** (Rust 2024 edition, Tokio async). Cargo workspace
with `members = ["crates/*", "bins/*"]`, two layers:

- **`crates/loomis-core`** — the UI-agnostic agent core: concrete tools,
  hooks, sandbox wiring, persistence, and the `Runtime` driver. Frontends
  (TUI today, WebUI/GUI later) interact with it only through the `Runtime`
  façade — `RuntimeCommand`s into a driver task, `AgentEvent`s out, and sync
  façade methods (`save_thread`, `load_skill`, `approve_plan`, …). The
  framework is the **`agent_oxide`** crate from crates.io — a single
  umbrella crate (`agent_oxide = "0.5.1"` in `crates/loomis-core/Cargo.toml`)
  whose source lives in the sibling `agent_oxide/` repo.
- **`bins/loomis`** — the TUI, a pure binary. Depends only on `loomis-core`;
  its agent_oxide dependency was deleted so the compiler enforces that the
  TUI never touches the framework directly.

**Rust edition**: Uses Rust 2024 with native async fn in traits (RPITIT).
Do NOT use `async-trait` crate. Prefer sync traits for dyn-dispatch; keep
async work in dedicated components.

### Workspace structure

```text
loomis/                        # this repo — the application
├── Cargo.toml                 # [workspace] — members = ["crates/*", "bins/*"]
├── crates/loomis-core/        # Agent core — UI-agnostic, depends on agent_oxide
│   ├── Cargo.toml             # agent_oxide = "0.5.1" (crates.io) + tokio/serde/schemars/…
│   ├── prompts/               # system.md, plan_mode.md, init.md (include_str!-ed)
│   ├── skills/                # skill-generator.md (seeded on first run)
│   └── src/
│       ├── lib.rs             # Public façade: Runtime + UI-agnostic re-exports
│       ├── runtime.rs         # Runtime/RuntimeCommand/UiState + driver task
│       ├── config.rs          # CoreConfig — build options for Runtime::build
│       ├── app.rs             # Private assembly (tools + hooks + sandbox wiring)
│       ├── hooks/             # plan_mode, profile, skill, system_prompt, todo hooks
│       ├── tools/             # read/write/edit/shell/glob/grep/ls/… 14 tools
│       ├── shell_util.rs      # Shared shell spawn/collect (ShellTool + !command)
│       └── user_shell.rs      # !command execution (30s watchdog)
├── bins/loomis/               # Pure binary — TUI only, no agent_oxide
│   ├── Cargo.toml             # loomis-core + TUI deps (ratatui/crossterm/…)
│   └── src/
│       ├── main.rs            # dotenv/tracing/panic-hook → Runtime::build → tui::run
│       └── tui/               # 10 modules — event loop, keyboard, rendering
└── docs/                      # App docs (API payload examples; framework guides live in agent_oxide/docs/)
agent_oxide/                   # sibling repo — the framework (open source)
├── Cargo.toml                 # [package] agent_oxide + [workspace] agent_oxide-macros
├── src/                       # Single crate — core/ + extensions/ modules
│   ├── core/                  # provider, deepseek, memory, tools, engine, util
│   └── extensions/            # agent_kit, hooks, observability, persistence,
│                              # sandbox, skills, subagent
├── agent_oxide-macros/        # Proc macros — #[derive(Agent)], #[agent_impl], #[tool]
└── docs/                      # Framework guides (beginner, senior, sandbox-architecture, agent-kit)
```

### Dependency graph

```text
bins/loomis ──────→ loomis-core ──────→ agent_oxide = "0.5.1" (crates.io)
                                       (single umbrella crate)
                                           ↑
agent_oxide/                        (framework internals — see AGENT_OXIDE.md
                                    for the core/ → extensions/ module layout)
```

`bins/loomis` never imports `agent_oxide` directly — all framework types
reach the TUI via `loomis_core` re-exports and the `Runtime` façade.

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
summarisation), use `engine::block_on` — a bare `Handle::block_on` panics on
tokio worker threads. The agent loop runs in a dedicated tokio task (the
`loomis_core` driver), separate from the TUI thread.

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
| `SkillRegistry` | agent_oxide `extensions/skills` | Discover + parse `.md` skill files (YAML frontmatter + body) from skill directories |
| `SkillTool` | loomis-core | Tool (`name = "skill"`) — loaded as System message, gives the agent the skill's instructions |
| `SkillHook` | loomis-core | Maintains `[SKILL: name]` System messages in memory, synced with `ActiveSkills` |

Skill files live in `<workspace>/.loomis/skills/` and `~/.loomis/skills/`
(discovered at startup, listed in the system prompt via `{skill_list}`). The
`/skill <name>` slash command loads a skill manually (via
`Runtime::load_skill`). `ActiveSkills`
(`Arc<RwLock<HashMap<String, String>>>`) is shared between the frontend,
SkillTool, and SkillHook.

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

### Concrete hooks (8 loomis-core + 2 from the agent_oxide hooks crate)
`SystemPromptHook` (seed prompts with tool list + skill list + env context + project rules),
`PlanModeHook` (tool restriction),
`ObservabilityHook` (trace collection),
`PersistenceHook` (auto-save),
`TodoListHook` (sync todo state),
`SkillHook` (sync active skills to `[SKILL: ...]` System messages),
`ProfileHook` (build + inject `[PROFILE]` System message),
`SandboxHook` (security) +
`MicroCompactHook` + `MacroCompactHook<C>` from the hooks crate.
Registration order in loomis-core's assembly: system_prompt → observability →
persistence → skill → plan_mode → profile → micro_compact → macro_compact →
todo_list → sandbox.

### TUI module (`bins/loomis/src/tui/`)
ratatui + crossterm, pure presentation — no `agent_oxide` imports (the
dependency was deleted from `bins/loomis/Cargo.toml`). Channel topology
(both channel halves live in `loomis_core::Runtime`; the TUI's `App` holds a
`Runtime` clone for slash commands and `!command` classification):

```text
TUI thread                          Driver task (loomis_core::Runtime, tokio::spawn)
─────────                          ──────────────────────────────────────────────
cmd_tx ──────── RuntimeCommand ───→ cmd_rx
agent_rx ←────── AgentEvent ─────── agent_tx
```

**Slash commands**: `/exit`, `/new`, `/plan`, `/approve`, `/save <name>`,
`/resume [name]`, `/threads`, `/stats`, `/tools`, `/help`, `/skill <name>`,
`/init` — each delegates to the `Runtime` façade (`save_thread`,
`resume_thread`, `load_skill`, `approve_plan`, …).

**Bang prefix**: `!command` — sends `RuntimeCommand::RunShell`; the driver
runs the shell and shares output with the agent via
`ToolCall { origin: User }`.

### `ResponseRouter`
Maps `request_id` → `SyncSender<InterventionResponse>`. Multiple components
can need user intervention simultaneously — each registers its own channel.
The frontend routes responses back through `Runtime::respond_intervention`.
