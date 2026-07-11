# 🦀 Loomis

<p align="center">
  <img src="https://img.shields.io/badge/Rust-2024%20edition-orange?style=flat-square&logo=rust" alt="Rust 2024">
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT License">
  <img src="https://img.shields.io/badge/状态-MVP-green?style=flat-square" alt="Status: MVP">
  <img src="https://img.shields.io/badge/DeepSeek-API-4B6BFB?style=flat-square" alt="DeepSeek API">
</p>

<p align="center">
  <em>一个基于模块化 Workspace 架构的 Rust AI 编码助手。<br>
  Tokio 异步 · Trait 工具抽象 · SSE 流式响应 · 终端交互界面 · 可复用 Crate 设计</em>
</p>

<p align="center">
  <img src="UI_screenshot.png" alt="Loomis TUI 截图" width="80%">
</p>

---

## 项目简介

Loomis 是一个纯 Rust（2024 edition）编写的 **AI 编码助手**。它在终端中运行，提供完整的 TUI 聊天界面，能够在沙箱化的工作区内读写文件、用 glob 和 grep 搜索代码，并通过 SSE 协议实时流式输出模型回复——全部基于 DeepSeek API。

项目采用 **Cargo Workspace** 架构，将抽象（`provider`, `tools`, `memory`, `engine`）与实现（`deepseek`, `hooks`, `subagent`, `loomis`）分离为独立 crate，使核心组件可以在其他 Agent 项目中直接复用。

## 特性

- **模块化 Workspace 架构** — 8 个 lib crate + 1 个 bin crate：`provider`（LLM 抽象）、`deepseek`（DeepSeek 实现）、`tools`（工具系统 + 沙箱）、`tools-macros`（`#[tool]` 宏）、`memory`（记忆管理 + 持久化）、`hooks`（开箱即用的 MicroCompact/MacroCompact 钩子）、`engine`（Agent 主循环）、`subagent`（子 Agent 即 Tool）、`loomis`（TUI + 具体工具 + 沙箱系统）
- **可插拔 LLM 提供者** — `LLMClient` trait 抽象（Rust 2024 原生 async），`DeepSeekClient` 作为参考实现，可扩展 OpenAI / Anthropic 等
- **Agent 主循环** — `run()` 简单模式 + `run_with_events()` 实时流式模式，通过 `mpsc` 通道推送事件，`max_steps` 防止无限循环
- **AgentHook 生命周期** — 10 个钩子方法：`on_run_start` → `on_step_start` → `on_llm_start` → `on_llm_end`/`on_llm_error` → `before_tool_call`（可拒绝执行）→ `after_tool_call`/`on_tool_failed` → `on_run_finish`
- **可插拔工具系统** — `Tool` trait + `ToolRegistry`，`execute_stream()` 返回 `ProgressStream`（支持实时 `InProgress` 更新）。内置计算器、Shell 命令执行、文件读写/编辑、glob 文件匹配、grep 内容搜索、ls 目录列表等工具，统一由 `WorkspaceFs` 沙箱隔离
- **对话记忆管理** — `Memory` 为纯消息缓冲区。双层压缩架构由 `hooks` crate 提供：`MicroCompactHook`（工具输出清理，`on_llm_start` 中替换旧内容为占位符）和 `MacroCompactHook`（字符预算超限时调用便宜 LLM 生成摘要）
- **对话持久化** — 自动保存到 `.loomis/threads/`（JSON + Markdown），多线程命名管理，`/resume` 弹窗选择器恢复历史对话，`/save <name>` 手动命名快照
- **子 Agent 系统** — `SubagentTool` 将子 Agent 包装为 Tool，父 Agent 可委托复杂子任务。子 Agent 拥有独立的 Memory 和过滤后的只读工具集，支持超时控制和上下文继承
- **多层沙箱系统** — 纵深防御：`WorkspaceFs`（路径沙箱）→ `ShellFilter`（命令分类）→ `SandboxHook`（配额 + 审批）→ `EnvSanitizer`（环境清洗）。配置由 `.loomis/config.toml` 驱动
- **终端交互界面** — [ratatui](https://ratatui.rs/) + [crossterm](https://crates.io/crates/crossterm) 打造，支持实时流式 Token、工具调用状态展示、滚动历史、斜杠命令、线程选择器弹窗

## 快速开始

### 前置条件

- [Rust](https://www.rust-lang.org/tools/install) 工具链（2024 edition）
- [DeepSeek API](https://platform.deepseek.com/) 密钥

### 环境配置

```bash
# 克隆仓库
git clone https://github.com/Nie-Tianyi/loomis.git
cd loomis

# 创建 .env 文件
echo 'DEEPSEEK_API=sk-your-key-here' > .env
```

### 运行

```bash
# 启动 TUI 模式（默认）
cargo run -p loomis --release

# 或使用传统命令行模式（即将迁移）
cargo run -p loomis --release -- --no-tui
```

### 测试与检查

```bash
cargo test --all           # 运行所有测试
cargo build -p loomis      # 仅构建二进制 crate
cargo clippy --all         # 代码检查
```

## 架构详解

### Workspace 结构

```text
agent_oxide/                        # Cargo Workspace 根
├── Cargo.toml                      # [workspace] members = ["libs/*", "bins/*"]
├── libs/                           # 库 crate（抽象 + 可复用组件）
│   ├── provider/                   # LLMClient trait + Message/ToolCall/ToolDef 等共享类型
│   ├── deepseek/                   # DeepSeekClient（实现 LLMClient）+ SSE 流解析
│   ├── tools/                      # Tool trait + ToolRegistry + WorkspaceFs + ProgressStream
│   ├── tools-macros/               # #[tool] proc macro — 自动生成 Tool trait impl
│   ├── memory/                     # Memory（消息缓冲区）+ 持久化
│   ├── hooks/                      # MicroCompactHook + MacroCompactHook（开箱即用的压缩钩子）
│   ├── engine/                     # Agent（ReAct 循环）+ AgentHook trait + AgentEvent 流
│   └── subagent/                   # SubagentTool — 将子 Agent 包装为 Tool
├── bins/                           # 二进制 crate
│   └── loomis/                     # 具体工具 + 沙箱系统 + TUI + 组装 + main.rs
└── docs/
    ├── developer-guide.md
    └── sandbox-architecture.md
```

### 依赖关系图

```text
provider (无内部依赖)
    ↑
    ├── deepseek ──────── (实现 provider::LLMClient)
    ├── tools ─────────── (使用 provider::ToolDef)
    ├── memory ────────── (使用 provider::Message)
    ↑
    ├── engine ────────── (使用 provider + tools + memory)
    │       ↑
    ├── hooks ─────────── (使用 provider + memory + engine)
    │       ↑
    ├── subagent ──────── (使用 provider + tools + engine + memory)
    │       ↑
    └── loomis (bin) ──── (使用所有 libs)
```

### Crate 职责

| Crate | 位置 | 角色 | 核心类型 |
|-------|------|------|----------|
| `provider` | `libs/` | 抽象 | `LLMClient` trait, `Message`, `Role`, `ToolCall`, `ToolDef`, `CompletionRequest`, `CompletionResponse`, `ProviderError`, `StreamChunk`, `Delta` |
| `deepseek` | `libs/` | 具体实现 | `DeepSeekClient` (impl `LLMClient`), `DeepSeekStream` (SSE 解析管道), `DeepSeekRequest`, `DeepSeekError` |
| `tools` | `libs/` | 抽象 | `Tool` trait (sync, `Send+Sync`), `ToolRegistry`, `WorkspaceFs`, `SandboxConfig`, `Progress`, `ProgressStream`, `ToolError`, `FsError`, `generate_schema` |
| `tools-macros` | `libs/` | Proc Macro | `#[tool]` 属性宏 — 从 struct 定义自动生成 `Tool` trait 实现 |
| `memory` | `libs/` | 抽象 | `Memory` (消息缓冲区), `SharedMemory` (`Arc<RwLock<Memory>>`), `MemoryBuilder`, 持久化 (`save_conversation`/`load_conversation`/`list_threads`) |
| `hooks` | `libs/` | 具体实现 | `MicroCompactHook` (工具输出清理), `MacroCompactHook<C>` (LLM 摘要压缩), `DEFAULT_COMPACT_CHARS`, `DEFAULT_KEEP_LAST_N` |
| `engine` | `libs/` | 抽象 | `Agent`, `AgentBuilder`, `AgentEvent` (13 个变体), `AgentError`, `AgentHook` trait (10 个方法), `EngineContext`, `EngineContextBuilder`, `CallOrigin`, `InterveneRequest`, `InterveneResponse`, `RunOutcome` |
| `subagent` | `libs/` | 具体实现 | `SubagentTool<C>` (实现 `Tool`，生成子 Agent), `SubagentConfig`, `filter_tools()` |
| `loomis` | `bins/` | 二进制 + 库 | 具体工具 (CalculatorTool, ReadTool, ShellTool 等), 沙箱系统 (SandboxHook, ShellFilter, AuditLogger, ResourceTracker, EnvSanitizer), TUI (ratatui), `build_coding_agent()`, `main.rs` |

### Agent 主循环 (`libs/engine/`)

Agent 是整个框架的控制中心，对外提供两种运行模式：

| 模式 | 传输方式 | 首字节延迟 | 适用场景 |
| ---- | -------- | --------- | -------- |
| **流式**（默认） | SSE (`text/event-stream`) | ~100ms | 交互式聊天、实时 UI |
| **非流式** | JSON (`application/json`) | 完整响应时间 | 批量处理、调试 |

**运行流程**：

```text
run(user_input) / run_with_events(user_input, tx)
  ├─ [流式]  run_streaming_loop()
  └─ [非流]  run_non_streaming_loop()
       │
       ├─ on_run_start hooks         — 任务开始
       ├─ on_step_start hooks        — 每轮循环开始
       ├─ on_llm_start hooks         — LLM 调用前（压缩在此执行）
       ├─ stream_with_retry()       — 指数退避重试瞬态错误
       ├─ on_llm_end hooks           — LLM 返回后
       │
       ├─ 有 tool_calls? → before_tool_call hooks（可拒绝执行）
       │                  → execute_stream() → ProgressStream
       │                    ├─ InProgress → AgentEvent::ToolProgress
       │                    └─ Done → AgentEvent::ToolSuccessful
       │                  → after_tool_call hooks
       │                  → push 结果 → 继续循环
       └─ 纯文本?       → push 到 memory → RunCompleted → Done
```

**关键设计决策**：

1. **泛型 `Agent<C: LLMClient>`** — 通过静态分发实现零成本抽象，无需 `Box<dyn>` 的间接开销
2. **同步 `Tool` trait + `ProgressStream`** — `Tool` trait 是对象安全的，无需 `async_trait`。`execute_stream()` 返回 `ProgressStream`，Agent 循环异步 poll。长任务通过 `tokio::sync::mpsc` 从独立线程发送进度事件
3. **Hook 实现的压缩** — 压缩不再内置于 `EngineContext` 或 `Agent`。通过注册 `MicroCompactHook` 和 `MacroCompactHook` 实现，在 `on_llm_start` 中自动执行
4. **AgentHook 10 个生命周期方法** — `on_run_start` → `on_step_start` → `on_llm_start` → `on_llm_end`/`on_llm_error` → `before_tool_call` → `after_tool_call`/`on_tool_failed` → `on_run_finish`。所有方法有默认空实现
5. **`Agent::builder()` 简化入口** — 只需 LLM client + model，自动创建 Memory 和 ToolRegistry，通过 `.tool()`/`.hook()` 注册组件

### SSE 流式管道 (`libs/deepseek/stream.rs`)

```text
HTTP chunk → buffer → find_event_end(\n\n) → trim_trailing_newlines
           → extract_sse_data("data: ") → [DONE]? 跳过
                                         → parse JSON → StreamChunk
                                                     ├─ content → Token
                                                     ├─ reasoning_content → ReasoningToken
                                                     └─ tool_calls → StreamAccumulator → ToolCall（流结束后发送）
```

### 工具系统 (`libs/tools/` + `bins/loomis/src/tools/`)

**层次架构**：

- **`libs/tools/`** — 抽象层：`Tool` trait, `ToolRegistry`, `WorkspaceFs`, `Progress`, `ProgressStream`, `ToolError`, `FsError`, `generate_schema`
- **`libs/tools-macros/`** — `#[tool]` proc macro：自动生成 `Tool` trait 实现
- **`bins/loomis/src/tools/`** — 具体实现：`CalculatorTool`, `EchoTool`, `ReadTool`, `WriteTool`, `EditTool`, `GlobTool`, `GrepTool`, `LsTool`, `ShellTool`

**Tool trait** — 同步、对象安全，返回 `ProgressStream`：

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;
    fn execute_stream(&self, args: &str) -> Result<ProgressStream, ToolError>;
    fn to_def(&self) -> ToolDef { .. }  // 默认实现
}

// ProgressStream — 即时工具用 ProgressStream::done()
// 长时工具通过 mpsc channel 发送 Progress::InProgress + Progress::Done
pub enum Progress {
    InProgress(String),  // 中间更新
    Done(String),         // 最终结果
}
```

**`#[tool]` 宏** — 声明式工具定义：

```rust
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    #[schemars(description = "Path to the file")]
    pub file_path: String,
}

#[tool(
    name = "read",
    description = "Read a file from the workspace.",
    args = ReadArgs
)]
pub struct ReadTool { fs: Arc<WorkspaceFs> }

impl ReadTool {
    fn execute_stream(&self, args: ReadArgs) -> Result<ProgressStream, ToolError> {
        let content = self.fs.read(&args.file_path, None, None)?;
        Ok(ProgressStream::done(content))
    }
}
```

**WorkspaceFs 沙箱** — 所有文件工具共享 `Arc<WorkspaceFs>`，路径规范化后验证不超出 `workspace_root`。强制执行文件大小限制、扩展名黑名单、隐藏文件保护和二进制内容检测。具备 TOCTOU 二次规范化防 symlink 交换攻击。策略由 [`SandboxConfig`](libs/tools/src/sandbox/config.rs) 驱动（从 `.loomis/config.toml` 加载）。

### 沙箱系统 (`bins/loomis/src/sandbox/`)

多层纵深防御的沙箱架构：

| 层 | 组件 | 职责 |
| :-- | :--- | :--- |
| 1 | `WorkspaceFs` | 路径沙箱 — 路径规范化 + 大小/类型限制 + TOCTOU 防护 |
| 2 | `ShellFilter` | 命令分类 — auto_approve → deny_patterns → 弹出确认 |
| 3 | `SandboxHook` | 统一 AgentHook — 配额检查 → 命令分类 → 自动/拒绝/提示 |
| 4 | `EnvSanitizer` | 环境清洗 — 清空后仅恢复安全白名单变量 |
| 5 | `ResourceTracker` | 会话配额 — 操作计数 + 并发 Shell 限制 |
| 6 | `AuditLogger` | 审计追踪 — JSONL 文件记录所有操作决策 |

**配置驱动**：`.loomis/config.toml` 控制所有策略。不存在文件时使用内置安全默认值。

**混合审批模式**：

- `auto_approve.prefixes` 中的命令（`git`, `cargo`, `npm`, `python` 等）→ 自动放行
- 匹配 `deny_patterns` 的命令（`rm -rf /`, `sudo`, `shutdown` 等）→ 直接拒绝
- 其他命令 → 弹出交互式干预提示（支持 Approve/Deny/Other…，↑↓ 导航选项）

### 记忆与持久化 (`libs/memory/` + `libs/hooks/`)

**双层压缩架构** — 两层协同工作，在保持对话结构的同时控制上下文大小。两层均实现为 `AgentHook`（位于 `hooks` crate），在 `on_llm_start` 中自动执行。

#### 第 1 层：MicroCompact（工具输出清理）

`MicroCompactHook` — 轻量级压缩，在每次 LLM API 调用前自动执行。将旧工具输出（read、shell、grep、glob、edit、write、ls）的内容替换为 `[Old tool result content cleared]`，同时保留最近 `keep_recent` 条输出不变。

- **零 API 成本** — 纯字符串替换，无额外 LLM 调用
- **保留结构** — tool-call / tool-result 配对关系完整保留，模型仍能理解对话流程
- **注册方式**：`Agent::builder(...).hook(MicroCompactHook::new(5, compactable_tools))`

#### 第 2 层：LLM 摘要压缩

`MacroCompactHook<C>` — 当对话总字符数超过 `threshold`（默认 2M 字符）时触发。排出旧的非 System 消息，调用便宜模型生成摘要，将摘要作为新的 System 消息插入。

- System 消息永不被排空
- 字符数阈值而非 Token 数（2M 字符 ≈ 500k–1M Token）
- 摘要调用通过 `tokio::runtime::Handle::block_on` 阻塞 Agent 循环（不影响 TUI 主线程）
- 注册方式：`Agent::builder(...).hook(MacroCompactHook::new(flash_model, 2_000_000, 10, compact_client))`

**Memory** 本身是纯消息缓冲区（`pub messages: Vec<Message>`），不再包含压缩逻辑。所有压缩由 Hook 处理。

### TUI 交互界面 (`bins/loomis/src/tui/`)

ratatui + crossterm 实现的终端聊天界面。

**通道拓扑**：

```text
TUI 线程                                 Agent 任务 (tokio::spawn)
─────────                               ────────────────────────
cmd_tx ───── TuiCommand ──────────────→ cmd_rx
agent_rx ←── AgentEvent ─────────────── agent_tx  (Token, ToolCall, NeedUserIntervene, Done)
```

**键盘操作**：Enter（发送）、Ctrl+C（取消/退出）、Esc（取消）、PgUp/PgDown（滚动）、↑/↓（历史/干预选项导航）、←/→/Home/End（光标移动）。`!<命令>` 前缀直接在 TUI 中执行 Shell 命令。干预提示下用 ↑↓ 选选项，Enter 确认，Esc 取消。

**斜杠命令**：`/exit`, `/new`, `/save <name>`, `/resume [name]`, `/threads`, `/stats`, `/tools`, `/help`。

### 与其他项目集成

Lib crate 可独立用于其他 Agent 项目：

```toml
# 仅需 LLM 抽象 + DeepSeek 客户端
[dependencies]
provider = { git = "https://github.com/Nie-Tianyi/loomis.git", branch = "master" }
deepseek = { git = "https://github.com/Nie-Tianyi/loomis.git", branch = "master" }

# 或完整 Agent 框架
engine = { git = "https://github.com/Nie-Tianyi/loomis.git", branch = "master" }
```

```rust
use deepseek::DeepSeekClient;
use engine::Agent;

// 使用 Agent::builder() — 最简单的组装方式
let agent = Agent::builder(DeepSeekClient::new("sk-..."), "deepseek-v4-pro")
    .system_prompt("You are a helpful assistant.")
    .tool(my_custom_tool)
    .hook(my_hook)
    .max_steps(50)
    .build();

let answer = agent.run("Hello!").await?;
```

## 开发路线

### 第一阶段 — MVP ✅

- [x] `libs/provider` — LLMClient trait + Message/ToolCall/ToolDef 等共享类型
- [x] `libs/deepseek` — DeepSeekClient + SSE 流解析
- [x] `libs/tools` — Tool trait + ToolRegistry + WorkspaceFs 沙箱 + ProgressStream
- [x] `libs/tools-macros` — `#[tool]` proc macro
- [x] `libs/memory` — Memory + 持久化
- [x] `libs/engine` — Agent ReAct 循环 + AgentHook 生命周期 + AgentEvent 流
- [x] `libs/hooks` — MicroCompactHook + MacroCompactHook
- [x] `libs/subagent` — SubagentTool + SubagentConfig + filter_tools
- [x] `bins/loomis` — 具体工具 + 沙箱系统 + Hook + TUI + 组装

### 第二阶段 — 进阶

- [x] `schemars` 驱动的自动 JSON Schema 生成
- [x] 多层沙箱系统（ShellFilter + SandboxHook + AuditLogger + ResourceTracker）
- [x] 双层压缩架构（MicroCompact 工具输出清理 + MacroCompact LLM 摘要，均通过 Hook 实现）
- [x] 子 Agent 系统（SubagentTool — 委托复杂子任务给独立 Agent）
- [ ] 其他 LLM 提供者实现（`libs/openai/`, `libs/anthropic/`）
- [ ] Jinja 风格提示词模板引擎
- [ ] 结构化输出

### 第三阶段 — 生产级

- [ ] RAG + 向量数据库
- [ ] `tracing` 全链路可观测性
- [ ] HTTP API + Web 客户端
- [ ] 发布 lib crate 到 crates.io

## 参与贡献

欢迎提交 Issue 和 PR！

```bash
git checkout -b feature/your-feature
cargo test --all
cargo clippy --all
# ... 提交、推送、创建 PR
```

## 许可证

MIT © 2026
