# CLAUDE.md

## Build & Test

```bash
cargo build                        # debug build
cargo build --release              # release build
cargo test --all                   # all tests
cargo clippy --all                 # lint
cargo run -p loomis                # launch TUI
```

Set `DEEPSEEK_API` in `.env` before running.

## Architecture

**Rust 2024** agent framework, Tokio async, workspace `agent_oxide`. Use native async fn in traits — do NOT use `async-trait`. Prefer sync traits for dyn-dispatch; keep async work in dedicated components.

### Crates

| Crate | Role | Key types |
| --- | --- | --- |
| `provider` | Abstraction | `LLMClient` trait, `Message`, `Role`, `ToolCall`, `ToolDef`, `CompletionRequest`/`Response`, `ProviderError`, `Usage` |
| `deepseek` | Provider impl | `DeepSeekClient` (SSE streaming) |
| `tools` | Abstraction | `Tool` trait (sync, `Send+Sync`), `ToolRegistry`, `WorkspaceFs`, `SandboxConfig`, `ProgressStream` |
| `tools-macros` | Proc macro | `#[tool]` — generates `Tool` trait impl |
| `memory` | Abstraction | `Memory`, `SharedMemory`, `PendingHints`, `PersistenceConfig` |
| `hooks` | Concrete | `MicroCompactHook`, `MacroCompactHook<C>` |
| `engine` | Core loop | `Agent`, `AgentBuilder`, `AgentHook` trait, `AgentEvent`, `EngineContext`, `ResponseRouter`, `RunOutcome` |
| `subagent` | Concrete | `SubagentTool<C>`, `SubagentConfig` |
| `observability` | Abstraction | `TraceEvent`, `TraceStore`, `RunMetrics` |
| `skills` | Abstraction | `SkillDef`, `SkillRegistry`, `ActiveSkills` |
| `loomis` (bin) | Binary | 12 tools, 7 hooks, sandbox, TUI, `build_coding_agent()` |

### Key traits

- **`LLMClient`**: Abstraction over LLM providers. Native async fn, NOT `#[async_trait]`.
- **`Tool`**: Sync and object-safe. `execute_stream()` returns `ProgressStream` — short tools emit `Progress::Done`, long-running tools emit `Progress::InProgress` updates. Use `tokio::sync::mpsc` from a spawned thread for async I/O.
- **`AgentHook`**: 10 lifecycle callbacks (all default no-ops). `on_<event>` = observe; `before_<action>` = can block via `Err`; `after_<action>` = observe result. For async work in sync hooks, use `tokio::runtime::Handle::block_on`.

### AgentEvent stream

Single `mpsc::unbounded_channel`. Key variants: `RunStarted`, `Token`/`ReasoningToken`, `ToolCallStart`, `ToolCall` (with `CallOrigin::Llm` vs `User`), `ToolSuccessful`, `ToolRejected`, `ToolFailure`, `ToolProgress`, `InterventionRequired`, `RunCompleted`, `RunFailed`, `Cancelled`, `Done` (sentinel).

### AgentBuilder vs EngineContextBuilder

- `Agent::builder(client, model)` — simple: auto-creates Memory, seeds system prompt, collects tools.
- `EngineContext::builder(client, memory, tools, model)` — advanced: supply Memory + ToolRegistry, configure hooks, max_steps, max_retries, streaming.

### Compaction (two-tier)

1. **MicroCompact**: `on_llm_start()` clears old tool outputs from high-volume tools (read, shell, grep, glob, edit, write, ls) in-place.
2. **MacroCompact**: `on_llm_start()` checks `total_chars()`; when over threshold, summarises old non-System messages via LLM, inserts summary as System message.

### Sandbox (5-layer defense)

| Layer | Component | Role |
| --- | --- | --- |
| 1 | `WorkspaceFs` | Path sandbox — canonicalization, file-size caps, extension blocklist, binary detection |
| 2 | `ShellFilter` | Command classification — auto-approve (git, cargo), deny (rm -rf /, sudo), prompt rest |
| 3 | `SandboxHook` | Orchestrator — quotas, user prompts via `InterventionRequired`, audit logging |
| 4 | `EnvSanitizer` | Clears dangerous env vars before spawning child processes |
| 5 | Watchdog | Kills process tree on timeout |

Config: `.loomis/config.toml` → `SandboxConfig` (safe defaults if missing).

### Observability

`ObservabilityHook` captures lifecycle events with timing/token data, dispatched via `TraceStore::emit()` to `tracing` → `.loomis/logs/loomis.log` (daily rotation). `RunMetrics` atomics power the TUI status bar. Filter with `LOOMIS_LOG=agent=debug` (default: `info`).

### Plan Mode

`/plan` toggles read-only mode. Allowed tools: read, ls, glob, grep, calculator, ask_user_question, todo, task/subagent, write (only to `.loomis/plan.md`). Blocked: edit, shell, write (other files). `/approve` exits plan mode.

### Hook registration order

0. `SystemPromptHook` → 1. `PlanModeHook` → 2. `ObservabilityHook` → 3. `PersistenceHook` → 4. `TodoListHook` → 5. `SkillHook` → 6. `MacroCompactHook` → 7. `MicroCompactHook` → 8. `SandboxHook`

### Skills

`.md` files (YAML frontmatter + Markdown) discovered at startup from `<workspace>/.loomis/skills/` and `~/.loomis/skills/`. LLM calls `skill(name="...")` → `SkillTool` writes to `ActiveSkills` → `SkillHook` injects `[SKILL: name]` System messages. Format: `---\nname: my-skill\ndescription: one line\n---\n\n# Body`.

### Other patterns

- **`ResponseRouter`**: Maps `request_id` → `SyncSender<InterventionResponse>` for concurrent user interventions.
- **`!command`**: User-typed `!` prefix runs shell, output shared with agent via `ToolCall { origin: User }`.
- **Persistence**: Auto-saves to `.loomis/threads/{name}.json` + `.md` after each turn. `/resume` to load.
- **Subagent**: `SubagentTool<C>` wraps a child `Agent` with filtered tool set. Config: model, max_steps, timeout_secs.

### TUI

ratatui + crossterm. Channels: `cmd_tx → cmd_rx`, `agent_tx → agent_rx`. Keybindings: Enter (submit), Ctrl+C/Esc (cancel), Ctrl+D (exit), PgUp/PgDown (scroll), Up/Down (history). Slash commands: `/exit`, `/new`, `/plan`, `/approve`, `/save <name>`, `/resume`, `/threads`, `/stats`, `/skill <name>`, `/help`.
