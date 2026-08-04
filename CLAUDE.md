# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> `LOOMIS.md` contains overlapping agent-facing guidance; `docs/` holds long-form
> developer guides (`beginner-developer-guide.md`, `senior-developer-guide.md`,
> `sandbox-architecture.md`). Keep this file and `LOOMIS.md` in sync when
> architecture changes.

## Build & Test

```bash
cargo build                        # debug build (all crates)
cargo build --release              # release build
cargo test --all                   # all tests
cargo test -p engine               # one crate's tests
cargo test -p engine <name>        # single test by name substring
cargo clippy --all                 # lint
cargo run -p loomis                # launch TUI (debug)
cargo run -p loomis --release      # launch TUI (release — recommended)
```

Tests are **inline** (`#[cfg(test)] mod tests`) co-located with source — there
are no `tests/` directories.

Environment (loaded via `dotenvy` from `.env`, see `.env.example`):

| Var | Purpose |
| --- | --- |
| `DEEPSEEK_API` | **Required.** DeepSeek API key |
| `BASE_URL` | API base URL (default `https://api.deepseek.com`) |
| `DEFAULT_PRO_MODEL` | Main agent model (default `deepseek-v4-pro`) |
| `DEFAULT_FLASH_MODEL` | Cheap model for compaction/profile synthesis (default `deepseek-v4-flash`) |
| `LOOMIS_LOG` | `tracing` EnvFilter (default `info`); e.g. `agent=debug` |

## Architecture

**Rust 2024** agent framework, Tokio async. Cargo workspace with
`members = ["core/*", "extensions/*", "bins/*"]`. Uses native async fn in traits (RPITIT) —
do NOT use `async-trait`. Prefer sync traits for dyn-dispatch; keep async work
in dedicated components.

### Crates & dependency graph

```
agent_oxide/
├── Cargo.toml              # [workspace] — members = ["core/*", "extensions/*", "bins/*"]
├── core/
│   ├── provider/           # LLMClient trait + shared types
│   ├── deepseek/           # DeepSeekClient — implements LLMClient
│   ├── tools/              # Tool trait, ToolRegistry, WorkspaceFs, ProgressStream
│   ├── tools-macros/       # #[tool] proc macro
│   ├── memory/             # Memory buffer, PendingHints, conversation persistence
│   └── engine/             # Agent (ReAct loop), AgentHook trait, AgentEvent, ResponseRouter
├── extensions/
│   ├── skills/             # SkillDef, SkillRegistry, ActiveSkills — skill discovery & loading
│   ├── hooks/              # MicroCompactHook + MacroCompactHook
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

### The ReAct loop (engine)

`Agent::run()` loops: hooks → LLM call (streaming) → tool execution →
observation appended to memory → repeat, until a final answer, `max_steps`
(50 in the bin), or error. The loop runs in a dedicated tokio task, separate
from the TUI thread; they communicate via `AgentEvent` over a single
`mpsc::unbounded_channel`.

Key `AgentEvent` variants: `RunStarted`, `Token`/`ReasoningToken`,
`ToolCallStart`, `ToolCall` (with `CallOrigin::Llm` vs `User`),
`ToolSuccessful`/`ToolRejected`/`ToolFailure`, `ToolProgress`,
`InterventionRequired`, `RunCompleted`, `RunFailed`, `Cancelled`, `Done`
(sentinel, always last).

### Key traits

- **`LLMClient`** (provider): native async fn. `DeepSeekClient` is the
  reference impl.
- **`Tool`** (tools): sync and object-safe. `execute_stream()` returns
  `ProgressStream` — short tools emit one `Progress::Done`; long-running tools
  (shell) emit `Progress::InProgress` updates then `Done`. Use
  `tokio::sync::mpsc` from a spawned thread for async I/O.
- **`#[tool]` proc macro** (tools-macros): annotate a struct with
  `#[tool(name = "...", description = "...", args = ArgsType)]`; the struct
  must define an inherent
  `execute_stream(&self, args: ArgsType) -> Result<ProgressStream, ToolError>`.
  JSON Schema is lazily derived from `ArgsType` via `schemars`.
- **`AgentHook`** (engine): **9** lifecycle callbacks, all default no-ops.
  Naming convention: `on_<event>` = observe only; `before_<action>` = can
  block by returning `Err`; `after_<action>` = observe result.
  Callbacks: `on_run_start`, `on_run_finish`, `on_step_start`, `on_llm_start`,
  `on_llm_end`, `on_llm_error`, `before_tool_call`, `after_tool_call`,
  `on_tool_failed`. Hooks run in registration order. For async work inside
  sync hooks (e.g. LLM summarisation), use `engine::block_on` — a bare
  `Handle::block_on` panics on tokio worker threads.

### AgentBuilder vs EngineContextBuilder

- `Agent::builder(client, model)` — simple: auto-creates Memory, seeds system
  prompt, collects tools.
- `EngineContext::builder(client, memory, tools, model)` — advanced: supply
  Memory + ToolRegistry, configure hooks, `max_steps`, `max_retries`,
  `streaming`, `pending_hints`. The bin uses this (50 steps, 3 retries,
  streaming on).

### Two-tier compaction (hooks crate)

Both run in `on_llm_start()`:

1. **MicroCompact** — clears old tool outputs in-place for `read`, `shell`,
   `grep`, `glob`, `edit`, `write`, `ls`, keeping the most recent 10 per
   tool. Cleared outputs are replaced by a contextual placeholder
   (`[Cleared: read src/fs.rs:10-59]`) parsed from the tool-call arguments,
   falling back to `[Old tool result content cleared]` when unparseable.
2. **MacroCompact** — checks `prompt_tokens` from the previous response
   (`Memory::last_usage`) against a **token** threshold (default 1,000,000).
   When over: drains old non-System messages (keeping last 10), summarises
   them via the flash model through `engine::block_on`, inserts the summary
   as a System message.

Key constants in `extensions/compact/src/compact.rs`: `DEFAULT_COMPACT_TOKEN_LIMIT`,
`DEFAULT_COMPACT_CHAR_LIMIT`, `DEFAULT_KEEP_LAST_N`,
`DEFAULT_KEEP_RECENT_TOOL_OUTPUTS`.

### Sandbox (5-layer defense)

| Layer | Component (crate) | Role |
| --- | --- | --- |
| 1 | `WorkspaceFs` (sandbox) | Path sandbox — canonicalization, file-size caps, extension blocklist, hidden-file protection, binary detection, TOCTOU re-check; read-only roots (`filesystem.read_only_paths`, defaults to the cargo registry cache) are readable via `read`/`ls`/`glob`/`grep` but never writable |
| 2 | `ShellFilter` (sandbox) | Command classification — auto-approve prefixes (`git`, `cargo`, …), deny patterns (`rm -rf /`, `sudo`), prompt for rest |
| 3 | `SandboxHook` (sandbox) | Orchestrator — quotas, user prompts via `InterventionRequired` + `ResponseRouter` rendezvous, audit log to `.loomis/audit.jsonl` |
| 4 | `EnvSanitizer` (sandbox) | Clears dangerous env vars in child processes |
| 5 | `Watchdog` (sandbox) | Kills process tree on timeout (`taskkill /F /T` on Windows) |

Config: `<workspace>/.loomis/config.toml` → `SandboxConfig` (safe defaults if
missing). Shell output is capped at 100 KB.

### Hook registration order (bin)

Hooks run in registration order:

1. `SystemPromptHook` — seed system prompts on run start
2. `ObservabilityHook` — full-chain trace event collection
3. `PersistenceHook` — save conversation after each run
4. `SkillHook` — maintain `[SKILL: ...]` System messages
5. `PlanModeHook` — plan-mode filtering + prompt injection (registered after
   Skill so toggling `/plan` only invalidates the cache prefix past the
   stable system prompt + skills)
6. `ProfileHook` — maintain `[PROFILE]` System message + synthesis
7. `TodoListHook` — maintain `[TODO]` System message (registered last among
   the injectors so the most volatile content lands at the tail of the
   stable System block — all injectors use `insert_before_history` (from the
   `hooks` crate), never `insert(0, …)`, to avoid invalidating the
   prompt-cache prefix)
8. `MacroCompactHook` — LLM summarisation (summary also placed via
   `insert_before_history`, after the System block)
9. `MicroCompactHook` — tool output clearing
10. `SandboxHook` — security sandbox

Order matters: System-prompt/skill/profile injectors run before compaction so
their messages survive; SandboxHook runs last so it sees the final tool call.

### Plan Mode

`/plan` toggles read-only mode (`PlanModeHook`, position 5). Allowed: `read`,
`ls`, `glob`, `grep`, `calculator`, `ask_user_question`, `todo`,
`task`/subagent, `enter_plan_mode`, `exit_plan_mode`, `write` (only to
`.loomis/plan.md`). Blocked: `edit`, `shell`, `write` elsewhere. `/approve`
exits plan mode and archives the plan to `.loomis/plan/<summary>.md`.

### Skills

`.md` files (YAML frontmatter + body) discovered at startup from
`<workspace>/.loomis/skills/` and `~/.loomis/skills/`; a `skill-generator.md`
is seeded on first run. Format: `---\nname: my-skill\ndescription: one
line\n---\n\n# Body`. Three components: `SkillRegistry` (skills crate,
discovery/parse), `SkillTool` (bin — the LLM-facing `skill(name=...)` tool,
writes to `ActiveSkills`), `SkillHook` (bin — injects `[SKILL: name]` System
messages via remove-then-reinsert). `ActiveSkills`
(`Arc<RwLock<HashMap<String, String>>>`) is shared between TUI, SkillTool,
and SkillHook. `/skill <name>` loads one manually.

### User Profiling

`ProfileHook` maintains `<workspace>/.loomis/profile.json` (human-readable,
hand-editable) in two tiers:

1. **Real-time rules** (zero tokens): CJK language detection (sticky once
   `zh-CN`), per-tool call/fail/success counters, session count.
2. **LLM synthesis** (every `SYNTHESIS_INTERVAL = 5` sessions): the flash
   model analyses the last `SYNTHESIS_CONTEXT_SIZE = 10` messages and
   extracts preferences, avoidances, expertise signals, coding conventions,
   verbosity. Merging is conservative — only non-empty fields overwrite.

A `[PROFILE]` System message is injected at the tail of the System block by
`on_llm_start()` (via `insert_before_history`)
(remove-then-reinsert, same pattern as SkillHook).

### Other patterns

- **`ResponseRouter`**: maps `request_id` → `SyncSender<InterventionResponse>`
  so multiple components can await user decisions concurrently; the TUI routes
  responses back through it.
- **`!command`**: user-typed `!` prefix runs shell immediately; output is
  shared with the agent via `ToolCall { origin: User }`.
- **Persistence**: auto-saves to `.loomis/threads/{name}.json` + `.md` after
  each turn; `/resume` reloads.
- **Subagent**: `SubagentTool<C>` wraps a child `Agent` with a filtered tool
  set (config: model, max_steps, timeout_secs).
- **Observability**: `ObservabilityHook` writes `TraceEvent`s to a shared
  `Arc<TraceStore>`; events also flow via `tracing` to
  `.loomis/logs/loomis.log` (daily rotation). Lock-free `RunMetrics` atomics
  power the TUI status bar.

### Runtime artifacts (`.loomis/` in the workspace)

`config.toml` (sandbox policy), `threads/` (saved conversations),
`plan.md` + `plan/` (plan mode), `profile.json`, `skills/`,
`logs/loomis.log`, `audit.jsonl` (sandbox audit).

### TUI (`bins/loomis/src/tui/`)

ratatui + crossterm. Channels: `cmd_tx → cmd_rx` (TuiCommand),
`agent_tx → agent_rx` (AgentEvent). Keybindings: Enter (submit), Ctrl+C/Esc
(cancel), Ctrl+D (exit), PgUp/PgDown (scroll), Up/Down (history). Slash
commands: `/exit`, `/new`, `/plan`, `/approve`, `/save <name>`, `/resume`,
`/threads`, `/stats`, `/tools`, `/skill <name>`, `/init`, `/help`.
