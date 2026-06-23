# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```bash
cargo build              # debug build
cargo build --release    # release build
cargo test               # run all tests
cargo test -p agent_oxide -- test_find_event_end  # run a single test
cargo clippy             # lint
```

Set `DeepSeek_API` in `.env` before running — `dotenvy` loads it at startup.

## Architecture

This is a **Rust agent framework** built from scratch (Rust 2024 edition, Tokio async). The target application is an auto-researcher that autonomously uses tools to produce Markdown research reports.

### Current phase (MVP)

| Module | Purpose |
| ------ | ------- |
| `src/core/client/` | DeepSeek API client — typed request/response, streaming SSE support |
| `src/core/agent.rs` | Agent scaffolding (empty — next to implement) |
| `src/memory/mod.rs` | Conversation memory — `Memory`, `SharedMemory`, compaction with two-phase async API |
| `src/main.rs` | Scratchpad: raw HTTP-level SSE demo (does **not** use the `client` module yet) |

The `core` module is the top-level crate root for the agent framework:

- **`src/core/mod.rs`** — declares `pub mod client; mod agent;`
- **`src/core/client/mod.rs`** — flat re-exports of all client types so callers `use core::client::*`

The `client` submodule is split by concern:

- **`error.rs`** — `DeepSeekError` enum (Http / Api / Parse / StreamingNotSupported)
- **`request.rs`** — `DeepSeekRequest`, `Message`, `Role`, `ToolCall`, `ToolChoice`, `ToolDef`, `FunctionDef`, `Thinking`, `ResponseFormat`, etc.
- **`response.rs`** — `DeepSeekResponse`, `FinishReason` (with `Other(String)` forward-compat), `Choice`, `ChoiceMessage`, `Usage`
- **`client.rs`** — `DeepSeekClient` — `send()` for non-streaming, `stream()` for SSE
- **`stream.rs`** — `DeepSeekStream`, `DeepSeekChunk`, `ChunkChoice`, `Delta` and the SSE parsing pipeline (3 layers: `read_event` → `extract_sse_data` → `serde_json::from_str`)

### SSE streaming pipeline

```text
HTTP chunk → buffer → find_event_end (\n\n) → trim_trailing_newlines → extract_sse_data (strip "data: ") → parse JSON → DeepSeekChunk
                                                                                          ↓
                                                                                   skip if empty / [DONE]
```

### Memory module (`src/memory/mod.rs`)

| Type | Purpose |
| ---- | ------- |
| `Memory` | Plain struct: `{ messages: Vec<Message>, compact_threshold: usize }` |
| `SharedMemory` | `Arc<RwLock<Memory>>` — for sharing across tokio tasks |
| `CompactSignal` | `Ok` / `NeedsCompact` — returned by `push()` |

**Core API**: `push(msg) -> CompactSignal`, `messages()`, `get_context()`, `to_context_vec()`
**Context length**: `total_chars()` (sums `content` lengths), `message_count()`, `needs_compact()`, `set_compact_threshold()`
**Compaction** — single async method that uses a flash model for summarisation. **System messages are never compacted** — they stay verbatim; only `User`, `Assistant`, and `Tool` messages are candidates:

```rust
// Automatically drains old non-System messages, sends them to the flash model,
// and inserts the summary as a System message at position 0.
mem.compact().await?;
// Result: [System("summary..."), System(original...), msg_recent_0, ..., msg_recent_9]
```

Uses `DEFAULT_FLASH_MODEL` env var (falls back to `"deepseek-chat"`) and `DeepSeek_API` for authentication.

Uses `Message` and `Role` from `crate::core::client` — `core/mod.rs` declares `pub mod client;` so this works.

### Key patterns

- **Forward-compat enum**: `FinishReason::Other(String)` catches unknown values rather than failing deserialization. Custom `Serialize`/`Deserialize` because `#[serde(rename_all)]` can't handle a catch-all variant.
- **SSE event buffering**: Network chunks can split an event mid-line. The stream accumulates bytes in a `Vec<u8>` buffer and only drains when `\n\n` appears.
- **Single-call async compaction**: `compact()` drains only non-System messages (keeping System messages verbatim), creates a `DeepSeekClient` pointed at the flash model, sends old messages for summarisation, and inserts the summary as a System message at position 0 — all in one async call.

### Roadmap (from README)

The project is in **Phase 1** (MVP). Completed and next:

- [x] `core/client/` — DeepSeek API client with typed request/response and SSE streaming
- [x] `memory/mod.rs` — conversation context with sliding window truncation (`Arc<RwLock<Memory>>`), push/get_context, two-phase compact API
- [ ] `tools.rs` — `Tool` trait + tool registry + 1–2 example tools
- [ ] `core/agent.rs` — main loop: LLM → match (text → done / tool_calls → execute → push to memory → loop), with `max_steps` guard (scaffolding exists)

Phases 2 and 3 cover macros/schemars for auto-schema, streaming UX via mpsc, structured output, RAG with vector DB, TUI, and observability with `tracing`.
