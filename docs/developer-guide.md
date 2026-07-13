# Loomis 开发者指南

Loomis 是一个模块化的 Rust Agent 框架（Rust 2024 edition，Tokio 异步运行时），工作空间名为 `agent_oxide`。本指南教你如何使用这个框架构建自己的 AI Agent。

## 目录

1. [架构概览](#1-架构概览)
2. [快速开始：构建一个最小 Agent](#2-快速开始构建一个最小-agent)
3. [实现 LLM Provider](#3-实现-llm-provider)
4. [实现 Tool（工具）](#4-实现-tool工具)
5. [Progress 流式进度](#5-progress-流式进度)
6. [实现 Hook（生命周期钩子）](#6-实现-hook生命周期钩子)
7. [Memory（会话记忆）与会话压缩](#7-memory会话记忆与会话压缩)
8. [组装：把所有组件连接在一起](#8-组装把所有组件连接在一起)
9. [Streaming Events 与 TUI 集成](#9-streaming-events-与-tui-集成)
10. [WorkspaceFs 文件沙箱](#10-workspacefs-文件沙箱)
11. [多层沙箱系统](#11-多层沙箱系统)
12. [Subagent 子代理系统](#12-subagent-子代理系统)
13. [对话持久化](#13-对话持久化)
14. [完整示例：代码审查 Agent](#14-完整示例代码审查-agent)

---

## 1. 架构概览

### 项目架构

![项目的四层架构](./assets/architecture.jpg)

### 工作空间结构

```
agent_oxide/
├── Cargo.toml              # [workspace] — members = ["libs/*", "bins/*"]
├── libs/
│   ├── provider/           # LLMClient trait + 共享类型 (Message, ToolCall, ToolDef 等)
│   ├── deepseek/           # DeepSeekClient — 实现 LLMClient (SSE 流式)
│   ├── tools/              # Tool trait, ToolRegistry, WorkspaceFs 沙箱, Progress 流
│   ├── tools-macros/       # #[tool] 属性宏 (proc macro)
│   ├── memory/             # Memory (消息缓冲区), 持久化
│   ├── engine/             # Agent (ReAct 循环), AgentHook trait, AgentEvent 流
│   ├── hooks/              # 开箱即用的 Hook 实现 (MicroCompact, MacroCompact)
│   └── subagent/           # SubagentTool — 将子 Agent 包装为 Tool
├── bins/
│   └── loomis/             # 二进制 — 具体工具、沙箱系统、TUI、组装入口
└── docs/
    ├── developer-guide.md  # 你正在读的文档
    └── sandbox-architecture.md  # 沙箱安全架构详解
```

### Crate 依赖图

```
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

### 核心抽象一览

| Crate | 核心 Trait / 类型 | 作用 |
|-------|-------------------|------|
| `provider` | `LLMClient` | LLM 提供商抽象（generate / stream），Rust 2024 原生 async |
| `provider` | `Message`, `Role`, `ToolCall`, `ToolDef` | 对话消息和工具定义 |
| `tools` | `Tool` | 工具抽象（name, description, parameters, execute_stream） |
| `tools` | `ToolRegistry` | 工具注册表（按名称查找和分发执行） |
| `tools` | `Progress`, `ProgressStream` | 工具执行进度流（InProgress / Done） |
| `tools` | `WorkspaceFs` | 沙箱文件系统（所有路径限制在 workspace 内） |
| `tools-macros` | `#[tool]` | 属性宏 — 自动生成 `Tool` trait 实现 |
| `memory` | `Memory`, `SharedMemory` | 对话记忆缓冲区（`Arc<RwLock<Memory>>`） |
| `engine` | `Agent` | ReAct 循环 — `run()` / `run_with_events()` |
| `engine` | `AgentBuilder` | **新手入口** — 流式 API 建造 Agent |
| `engine` | `EngineContext` | 配置和依赖注入容器（高级 API） |
| `engine` | `AgentHook` | 生命周期回调（可拦截工具执行） |
| `engine` | `AgentEvent` | 通用流式事件（Token, ToolCall, ToolSuccessful, Done...） |
| `engine` | `RunOutcome` | 运行结果枚举（Success / Error / Cancelled） |
| `hooks` | `MicroCompactHook` | 工具输出微压缩（清除旧工具结果） |
| `hooks` | `MacroCompactHook` | LLM 摘要宏压缩（超出预算时调用便宜模型总结） |
| `subagent` | `SubagentTool` | 将子 Agent 作为 Tool 暴露给父 Agent |
| `subagent` | `SubagentConfig` | 子 Agent 配置（模型、超时、上下文继承等） |

---

## 2. 快速开始：构建一个最小 Agent

### 2.1 创建新项目

在你的工作空间 `Cargo.toml` 中添加一个二进制 crate：

```toml
# bins/my-agent/Cargo.toml
[package]
name = "my-agent"
version = "0.1.0"
edition = "2024"

[dependencies]
tokio = { workspace = true }
provider = { path = "../../libs/provider" }
deepseek = { path = "../../libs/deepseek" }
tools = { path = "../../libs/tools" }
memory = { path = "../../libs/memory" }
engine = { path = "../../libs/engine" }
serde = { workspace = true }
serde_json = { workspace = true }
schemars = { workspace = true }
```

并在根 `Cargo.toml` 的 `members` 中添加 `"bins/my-agent"`。

### 2.2 最小 Agent 的五分钟版本

使用 `Agent::builder()` — 最简单的入门方式：

```rust
// bins/my-agent/src/main.rs
use deepseek::DeepSeekClient;
use engine::Agent;

#[tokio::main]
async fn main() {
    // 1. 加载 API Key
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API").expect("DEEPSEEK_API not set");

    // 2. 创建 LLM 客户端
    let client = DeepSeekClient::new(&api_key);

    // 3. 一行创建 Agent — memory 自动创建，system prompt 自动注入
    let agent = Agent::builder(client, "deepseek-v4-pro")
        .system_prompt("你是一个有帮助的助手。")
        .max_steps(20)
        .streaming(false)     // 非流式，简单输出
        .build();

    println!("Agent 已启动。输入你的问题：");
    loop {
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
        let input = input.trim().to_string();

        if input.is_empty() || input == "exit" {
            break;
        }

        // 将用户输入推入 memory
        {
            let mut mem = agent.memory().write().unwrap();
            mem.push(provider::Message::new(provider::Role::User, &input));
        }

        match agent.run(&input).await {
            Ok(response) => println!("Agent: {response}"),
            Err(e) => eprintln!("错误: {e}"),
        }
    }
}
```

> **💡 提示**：`Agent::builder()` 自动创建了一个空的 `Memory`，并将 `system_prompt` 作为第一条 System 消息注入。如果你需要共享 memory（例如多个 Agent 共用），可以用 `.memory(my_shared_memory)` 传入预创建的 memory。

这就是一个可运行的最小 Agent。它没有工具，只会进行纯文本对话。下面我们将逐步添加更多能力。

---

## 3. 实现 LLM Provider

`LLMClient` trait 是框架与 LLM 服务交互的唯一接口。它定义在 [`libs/provider/src/client.rs`](../libs/provider/src/client.rs)，使用 **Rust 2024 原生 async trait**（RPITIT），**不需要** `#[async_trait]` 宏：

```rust
pub trait LLMClient: Send + Sync {
    /// 发送非流式补全请求
    fn generate(
        &self,
        req: CompletionRequest,
    ) -> impl Future<Output = Result<CompletionResponse, ProviderError>> + Send;

    /// 发送流式补全请求，返回 BoxStream
    fn stream(
        &self,
        req: CompletionRequest,
    ) -> impl Future<
        Output = Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError>,
    > + Send;
}
```

> ⚠️ **重要**：由于 Rust 2024 原生支持 `async fn` in traits with `impl Future` 返回类型，框架**不使用** `async-trait` crate。实现 provider 时直接写 `async fn`，不要引入 `#[async_trait]`。

### 3.1 核心请求/响应类型

**请求** — [`CompletionRequest`](../libs/provider/src/request.rs)：

```rust
pub struct CompletionRequest {
    pub messages: Vec<Message>,       // 对话历史
    pub model: String,                // 模型名称
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub stop: Option<Vec<String>>,
    pub stream: bool,                  // 是否流式
    pub tools: Option<Vec<ToolDef>>,  // 工具定义
    pub tool_choice: Option<ToolChoice>,
}
```

使用 Builder 模式构建：

```rust
let request = CompletionRequest::new("deepseek-v4-pro", messages)
    .with_stream(true)
    .with_tools(tools)
    .with_max_tokens(4096);
```

**消息** — [`Message`](../libs/provider/src/message.rs)：

```rust
pub struct Message {
    pub role: Role,                          // System | User | Assistant | Tool
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,   // Assistant 消息中的工具调用
    pub tool_call_id: Option<String>,        // Tool 消息中的调用 ID
    pub name: Option<String>,
}

// 便捷构造方法
Message::new(Role::User, "Hello")
Message::assistant_with_tools("", tool_calls)
Message::tool_result("call_123", "output text")
```

### 3.2 实现新的 Provider（以 Anthropic 为例）

```rust
// libs/anthropic/src/lib.rs
use futures_util::stream::BoxStream;
use provider::{CompletionRequest, CompletionResponse, LLMClient, ProviderError, StreamChunk};

pub struct AnthropicClient {
    api_key: String,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            http: reqwest::Client::new(),
        }
    }
}

impl LLMClient for AnthropicClient {
    async fn generate(
        &self,
        req: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        // 1. 将 CompletionRequest 转换为 Anthropic Messages API 格式
        // 2. 发送 HTTP POST 到 https://api.anthropic.com/v1/messages
        // 3. 将 Anthropic 响应转换回 CompletionResponse
        todo!("实现 Anthropic API 调用")
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError> {
        // 1. 同上，但设置 stream: true
        // 2. 解析 SSE 事件流
        // 3. 将每个 SSE 事件转换为 StreamChunk
        todo!("实现 Anthropic 流式 API")
    }
}
```

### 3.3 ProviderError 错误类型

```rust
pub enum ProviderError {
    Http(String),              // HTTP 传输错误（可重试）
    Api { status: u16, body: String },  // API 错误（5xx 可重试，4xx 不重试）
    Parse(String),             // 解析错误（不可重试）
    StreamingNotSupported,     // Provider 不支持流式
}
```

参考实现：[`libs/deepseek/`](../libs/deepseek/) — 包含完整的 SSE 流式解析管道。

---

## 4. 实现 Tool（工具）

### 4.1 Tool Trait 定义

[`Tool`](../libs/tools/src/tool.rs) trait 是同步的（`Send + Sync`），核心方法是 `execute_stream`，返回一个 `ProgressStream`：

```rust
pub trait Tool: Send + Sync {
    /// 工具名称 — 映射到 API 请求中的 function.name
    fn name(&self) -> &str;

    /// 给模型看的人类可读描述
    fn description(&self) -> &str;

    /// 工具参数的 JSON Schema
    fn parameters(&self) -> Value;

    /// 执行工具，返回 ProgressStream（Progress::InProgress + Progress::Done）
    fn execute_stream(&self, args: &str) -> Result<ProgressStream, ToolError>;

    /// 自动生成的：转换为 provider::ToolDef
    fn to_def(&self) -> provider::ToolDef { ... }
}
```

**关键变化（相比旧版）**：`execute()` 已被 `execute_stream()` 取代。每个工具返回一个 `ProgressStream` — 即使是瞬时完成的工具，也返回单元素流（一个 `Progress::Done`）。长时间运行的工具（如 shell）可以发多个 `Progress::InProgress` 更新，最后以 `Progress::Done` 结束。

### 4.2 使用 `#[tool]` 宏（推荐）

`#[tool]` 属性宏（定义在 `tools-macros` crate，由 `tools` crate 重导出）自动生成 `Tool` trait 的实现，你只需要：

1. 定义一个 `Deserialize + JsonSchema` 参数结构体
2. 用 `#[tool(...)]` 标注结构体
3. 实现一个 inherent `execute_stream` 方法

**示例：Echo 工具**

```rust
use schemars::JsonSchema;
use serde::Deserialize;
use tools::{Progress, ProgressStream, ToolError, tool};

/// 参数结构体 — 同时用于 JSON Schema 生成和反序列化
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct EchoArgs {
    #[schemars(description = "The text to echo back.")]
    pub text: String,
}

#[tool(
    name = "echo",
    description = "Echo the input text back unchanged. Use for testing.",
    args = EchoArgs        // ← 必须与上面定义的结构体匹配
)]
pub struct EchoTool;

impl EchoTool {
    fn execute_stream(&self, args: EchoArgs) -> Result<ProgressStream, ToolError> {
        Ok(ProgressStream::done(args.text))
    }
}
```

**`#[tool]` 宏会自动生成：**
- `Tool::name()` → `"echo"`
- `Tool::description()` → description 字符串
- `Tool::parameters()` → 从 `EchoArgs` 自动生成 JSON Schema（通过 `OnceLock` 缓存）
- `Tool::execute_stream()` → 反序列化 JSON 为 `EchoArgs`，然后调用 `EchoTool::execute_stream`

**关键规则：**
- `args` 参数结构体必须 `#[derive(JsonSchema, Deserialize)]`
- `#[serde(deny_unknown_fields)]` 强烈推荐（拒绝未知字段，避免 LLM 产生的幻觉参数被静默忽略）
- inherent 方法的签名必须是 `fn execute_stream(&self, args: ArgsType) -> Result<ProgressStream, ToolError>`
- 结构体通常是一个 Unit struct（`pub struct EchoTool;`），依赖通过 `Arc<WorkspaceFs>` 等字段注入

### 4.3 手动实现 Tool trait

如果不想用 proc macro，也可以手动实现：

```rust
use serde_json::Value;
use tools::{ProgressStream, Tool, ToolError, generate_schema};

pub struct MyTool;

impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }

    fn description(&self) -> &str { "Does something useful." }

    fn parameters(&self) -> Value {
        generate_schema::<MyArgs>()  // 自动从类型生成 JSON Schema
    }

    fn execute_stream(&self, raw_args: &str) -> Result<ProgressStream, ToolError> {
        let args: MyArgs = serde_json::from_str(raw_args)
            .map_err(|e| ToolError::InvalidArgs(format!("invalid args: {e}")))?;
        // ... 执行业务逻辑 ...
        Ok(ProgressStream::done("result".into()))
    }
}
```

### 4.4 带 WorkspaceFs 依赖的工具

大多数文件操作工具需要 `WorkspaceFs` — 通过 `Arc<WorkspaceFs>` 注入：

```rust
#[tool(
    name = "read",
    description = "Read a file from the workspace...",
    args = ReadArgs
)]
pub struct ReadTool {
    fs: Arc<WorkspaceFs>,  // ← 通过 Arc 共享
}

impl ReadTool {
    pub fn new(fs: Arc<WorkspaceFs>) -> Self {
        Self { fs }
    }

    fn execute_stream(&self, args: ReadArgs) -> Result<ProgressStream, ToolError> {
        let content = self.fs
            .read(&args.file_path, args.offset, args.limit)
            .map_err(|e| match e {
                FsError::NotFound(_) => ToolError::InvalidArgs(e.to_string()),
                _ => ToolError::Execution(e.to_string()),
            })?;
        Ok(ProgressStream::done(content))
    }
}
```

### 4.5 ToolError 错误类型

```rust
pub enum ToolError {
    /// 参数格式/验证错误 → 告诉 LLM 修正参数
    InvalidArgs(String),
    /// 工具执行失败 → 告诉 LLM 操作失败了
    Execution(String),
}
```

### 4.6 ToolRegistry（工具注册表）

[`ToolRegistry`](../libs/tools/src/registry.rs) 管理所有注册的工具：

```rust
let mut registry = ToolRegistry::new();

// 注册工具（key = Tool::name()）
registry.register(Arc::new(EchoTool));
registry.register(Arc::new(ReadTool::new(workspace.clone())));

// 查询
assert!(registry.has("echo"));
let tool: Option<&Arc<dyn Tool>> = registry.get("echo");

// 生成 LLM API 请求所需的 ToolDef 列表
let tool_defs: Vec<ToolDef> = registry.to_tool_defs();

// 按名称分发执行 — 返回 ProgressStream
let result = registry.execute_stream("echo", r#"{"text":"hi"}"#);

// 列出所有工具名
let names: Vec<&str> = registry.iter().map(|(n, _)| n).collect();
```

---

## 5. Progress 流式进度

### 5.1 Progress 枚举

[`Progress`](../libs/tools/src/progress.rs) 是工具执行过程中发出的事件：

```rust
pub enum Progress {
    /// 中间更新 — 工具仍在运行，TUI 显示在工具标题下方
    InProgress(String),
    /// 最终结果 — 工具执行完成，推入 memory 作为 LLM 的 observation
    Done(String),
}
```

### 5.2 ProgressStream

`ProgressStream` 是 `Pin<Box<dyn Stream<Item = Progress> + Send>>` 的包装类型，实现了 `Stream` trait。

**大多数工具**只需要返回单个 `Done`：

```rust
use tools::{Progress, ProgressStream};

fn execute_stream(&self, args: MyArgs) -> Result<ProgressStream, ToolError> {
    let result = do_work(args)?;
    Ok(ProgressStream::done(result))
}
```

**长时间运行的工具**（如 shell）可以发出多个 `InProgress` 更新：

```rust
fn execute_stream(&self, args: ShellArgs) -> Result<ProgressStream, ToolError> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    // 在独立线程中运行命令
    std::thread::spawn(move || {
        let mut child = Command::new("cargo").args(["build"]).spawn()?;

        // 定期发送进度更新
        tx.send(Progress::InProgress("Compiling...".into())).ok();
        // ... 等待完成 ...
        tx.send(Progress::Done("Build succeeded".into())).ok();
    });

    // 将 mpsc receiver 包装为 ProgressStream
    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
    Ok(ProgressStream::from(Box::pin(stream)))
}
```

### 5.3 Agent 循环中的消费

Agent 循环自动消费 `ProgressStream`：
- `Progress::InProgress(msg)` → 发出 `AgentEvent::ToolProgress`
- `Progress::Done(output)` → 作为 tool result 推入 memory，发出 `AgentEvent::ToolSuccessful`

---

## 6. 实现 Hook（生命周期钩子）

[`AgentHook`](../libs/engine/src/hooks.rs) trait 让你在 Agent 执行的各个阶段插入自定义逻辑。所有方法都有默认空实现 — 你只需要实现你关心的事件。

```rust
pub trait AgentHook: Send + Sync {
    // ── Run 生命周期 ──
    fn on_run_start(&self, session_id: &str, user_input: &str) {}
    fn on_run_finish(&self, session_id: &str, outcome: &RunOutcome) {}

    // ── Step 生命周期 ──
    fn on_step_start(&self, session_id: &str, step: usize, max_steps: usize) {}

    // ── LLM 生命周期 ──
    fn on_llm_start(&self, session_id: &str, memory: &SharedMemory) {}
    fn on_llm_end(&self, session_id: &str, response: &Message) {}
    fn on_llm_error(&self, session_id: &str, error: &ProviderError, attempt: usize, will_retry: bool) {}

    // ── Tool 生命周期 ──
    fn before_tool_call(&self, session_id: &str, tool_call: &ToolCall) -> Result<(), AgentError> { Ok(()) }
    fn after_tool_call(&self, session_id: &str, tool_call: &ToolCall, observation: &str) {}
    fn on_tool_failed(&self, session_id: &str, tool_call: &ToolCall, error: &str) {}
}
```

### 命名约定

| 前缀 | 含义 |
|------|------|
| `on_<event>` | 纯通知 — 不能干预 |
| `before_<action>` | 可干预 — 返回 `Err` 阻止执行 |
| `after_<action>` | 观察结果 — 不能干预 |

### 6.1 日志钩子示例

```rust
use engine::{AgentError, AgentHook};
use provider::ToolCall;

pub struct CliLoggerHook;

impl AgentHook for CliLoggerHook {
    fn on_run_start(&self, _session_id: &str, user_input: &str) {
        eprintln!("🚀 Starting run: {}", &user_input[..user_input.len().min(80)]);
    }

    fn on_llm_start(&self, _session_id: &str, _memory: &SharedMemory) {
        eprintln!("⏳ Agent thinking...");
    }

    fn before_tool_call(
        &self,
        _session_id: &str,
        tool: &ToolCall,
    ) -> Result<(), AgentError> {
        eprintln!("🔧 Executing: {}", tool.function.name);
        Ok(())  // 总是允许
    }

    fn after_tool_call(
        &self,
        _session_id: &str,
        tool: &ToolCall,
        observation: &str,
    ) {
        let preview: String = observation.chars().take(80).collect();
        eprintln!("✅ {} → {}", tool.function.name, preview);
    }

    fn on_run_finish(&self, _session_id: &str, outcome: &RunOutcome) {
        match outcome {
            RunOutcome::Success { answer } => {
                eprintln!("✅ Run complete: {}", &answer[..answer.len().min(80)]);
            }
            RunOutcome::Error { error } => eprintln!("❌ Run failed: {error}"),
            RunOutcome::Cancelled => eprintln!("⚠️ Run cancelled"),
        }
    }
}
```

### 6.2 工具审批钩子（拦截危险操作）

这是 `before_tool_call` 的核心用例 — 在工具执行前拦截并决定是否放行：

```rust
use std::sync::{Arc, Mutex, OnceLock};
use engine::{AgentError, AgentEvent, AgentHook, InterveneRequest, InterveneResponse};
use provider::ToolCall;
use tokio::sync::mpsc;

pub struct DangerousCommandApprovalHook {
    agent_tx: OnceLock<mpsc::UnboundedSender<AgentEvent>>,
    intervene_rx: Mutex<std::sync::mpsc::Receiver<InterveneResponse>>,
}

impl DangerousCommandApprovalHook {
    pub fn new() -> (Self, std::sync::mpsc::SyncSender<InterveneResponse>) {
        let (tx, rx) = std::sync::mpsc::sync_channel::<InterveneResponse>(0);
        (Self { agent_tx: OnceLock::new(), intervene_rx: Mutex::new(rx) }, tx)
    }

    pub fn set_agent_tx(&self, tx: mpsc::UnboundedSender<AgentEvent>) {
        let _ = self.agent_tx.set(tx);
    }
}

impl AgentHook for DangerousCommandApprovalHook {
    fn before_tool_call(
        &self,
        _session_id: &str,
        tool: &ToolCall,
    ) -> Result<(), AgentError> {
        // 只拦截 shell 工具
        if tool.function.name != "shell" {
            return Ok(());
        }

        let command = parse_shell_command(&tool.function.arguments);

        // 通过统一的 AgentEvent 通道发送干预请求
        if let Some(tx) = self.agent_tx.get() {
            let _ = tx.send(AgentEvent::NeedUserIntervene(InterveneRequest {
                request_id: uuid_v4(),
                title: "Approve shell command?".into(),
                description: command.clone(),
                options: vec!["Approve".into(), "Deny".into(), "Other…".into()],
            }));
        }

        // 阻塞等待用户响应
        let response = self.intervene_rx.lock().unwrap().recv().unwrap_or(
            InterveneResponse { chosen: Some(1), custom_text: None } // deny on error
        );

        match response.chosen {
            Some(0) => Ok(()),        // "Approve"
            Some(2) => Ok(()),        // "Other…" — 用户提供自定义输入
            _ => Err(AgentError::ToolRejected {
                name: "shell".into(),
                reason: "User denied shell command execution".into(),
            }),
        }
    }
}
```

**`InterveneRequest` 的三种使用模式：**

| 场景 | `options` | 用户操作 | `InterveneResponse` |
|------|-----------|----------|---------------------|
| Yes/No 确认 | `["Approve", "Deny"]` | 选 Approve | `{ chosen: Some(0), custom_text: None }` |
| 多选一 | `["cargo build", "make", "Other…"]` | 选 make | `{ chosen: Some(1), custom_text: None }` |
| 自定义输入 | `["Approve", "Deny", "Other…"]` | 选 Other… 后输入文本 | `{ chosen: Some(2), custom_text: Some("pip install") }` |

选项以 `…`（U+2026）结尾时，应用层自动弹出文本输入框。

**关键机制：** 当 `before_tool_call` 返回 `Err(AgentError::ToolRejected)` 时，Agent 循环会：
1. 跳过工具执行
2. 将拒绝消息作为 Tool Result 推入 memory
3. 发出 `AgentEvent::ToolRejected`
4. LLM 会看到拒绝消息并可以调整策略

### 6.3 组合多个 Hook

Hook 按注册顺序依次执行（顺序很重要！）：

```rust
// 典型顺序：MacroCompact → MicroCompact → Sandbox
let agent = Agent::builder(client, "deepseek-v4-pro")
    .hook(MacroCompactHook::new(...))   // 1. 先做 LLM 摘要
    .hook(MicroCompactHook::new(...))   // 2. 再做工具输出清理
    .hook(SandboxHook::new(...))        // 3. 最后做安全审批
    .build();
```

---

## 7. Memory（会话记忆）与会话压缩

### 7.1 Memory 基础

[`Memory`](../libs/memory/src/memory.rs) 是一个简单的消息缓冲区，线程安全地通过 `SharedMemory`（即 `Arc<RwLock<Memory>>`）共享。**Memory 本身不再包含压缩逻辑** — 压缩完全由 `hooks` crate 中的 Hook 处理。

```rust
use memory::{Memory, SharedMemory};
use provider::{Message, Role};

let memory: SharedMemory = Arc::new(std::sync::RwLock::new(Memory::new()));

// 写入
{
    let mut mem = memory.write().unwrap();
    mem.push(Message::new(Role::System, "你是一个编码助手。"));
    mem.push(Message::new(Role::User, "帮我写一个函数。"));
}

// 读取
{
    let mem = memory.read().unwrap();
    let messages: &[Message] = mem.messages();
    let context: Vec<Message> = mem.to_context_vec(); // clone
    let total_chars: usize = mem.total_chars();
    let count: usize = mem.message_count();
}
```

### 7.2 MemoryBuilder

```rust
let memory = Memory::builder()
    .with_messages(preloaded_history)
    .build();
```

### 7.3 两阶段压缩（Hook 实现）

压缩由 `hooks` crate 中的两个 Hook 处理，在 `on_llm_start` 中执行：

#### 微压缩（MicroCompactHook）

在每次 LLM 调用前，清除旧工具输出内容，替换为 `[Old tool result content cleared]`。保留每个可压缩工具最近 N 条输出。

```rust
use hooks::MicroCompactHook;
use std::collections::HashSet;

let hook = MicroCompactHook::new(
    5,  // 保留最近 5 条输出
    HashSet::from([
        "read".into(), "shell".into(), "grep".into(),
        "glob".into(), "edit".into(), "write".into(), "ls".into(),
    ]),
);
```

#### 宏压缩（MacroCompactHook）

当会话总字符数超过阈值时，排出旧的非 System 消息，调用便宜模型生成摘要，将摘要插入为新的 System 消息。

```rust
use hooks::MacroCompactHook;

let hook = MacroCompactHook::new(
    "deepseek-v4-flash".into(),  // 摘要用便宜模型
    2_000_000,                    // 字符预算阈值
    10,                           // 保留最近 10 条非 System 消息
    flash_client,                 // LLM 客户端（可以是不同的 client 实例）
);
```

> **注意**：`MacroCompactHook` 使用 `tokio::runtime::Handle::block_on` 阻塞等待 LLM 摘要调用。由于 Agent 循环运行在独立的 tokio task 中，这不影响 TUI 主线程。

---

## 8. 组装：把所有组件连接在一起

### 8.1 EngineContext 与 AgentBuilder

[`EngineContext`](../libs/engine/src/context.rs) 是所有依赖的容器：

```rust
pub struct EngineContext<C: LLMClient> {
    pub llm: C,                          // LLM provider
    pub memory: SharedMemory,            // 共享对话记忆
    pub tools: Arc<ToolRegistry>,        // 工具注册表
    pub hooks: Vec<Box<dyn AgentHook>>,  // 生命周期钩子（按顺序执行）
    pub model: String,                   // 模型名称
    pub max_steps: usize,                // 最大 ReAct 循环步数（默认 50）
    pub max_retries: usize,              // 网络错误最大重试次数（默认 3）
    pub streaming: bool,                 // 是否启用 SSE 流式（默认 true）
}
```

> ⚠️ **重要变化**：压缩不再通过 `EngineContext` 字段配置。压缩现在通过 Hook 实现。注册 `MicroCompactHook` 和 `MacroCompactHook` 即可。

**方式 1（推荐新手）：`Agent::builder()`** — 最简单，自动创建 Memory、自动注入 system prompt：

```rust
let agent = Agent::builder(client, "deepseek-v4-pro")
    .system_prompt("You are a helpful assistant.")
    .tool(my_tool)
    .hook(my_hook)
    .max_steps(50)
    .build();
```

**方式 2（高级用户）：`EngineContext::builder()`** — 需要手动管理 Memory 和 ToolRegistry：

```rust
let ctx = EngineContext::builder(client, memory, tools, "deepseek-v4-pro")
    .hook(my_hook)
    .max_steps(50)
    .max_retries(3)
    .streaming(true)
    .build();
let agent = Agent::new(ctx);
```

**方式 3（完全手动）：结构体字面量** — 向后兼容，所有字段均为 `pub`：

```rust
let ctx = EngineContext {
    llm: client,
    memory: memory.clone(),
    tools: registry,
    hooks: vec![],
    model: "deepseek-v4-pro".into(),
    max_steps: 50,
    max_retries: 3,
    streaming: true,
};
let agent = Agent::new(ctx);
```

### 8.2 标准组装模式

参考 [`loomis::app::build_coding_agent`](../bins/loomis/src/app.rs)：

**简洁风格（推荐）— `Agent::builder()`：**

```rust
use deepseek::DeepSeekClient;
use engine::Agent;
use hooks::{MacroCompactHook, MicroCompactHook};

let client = DeepSeekClient::new(api_key);
let compact_client = DeepSeekClient::new(api_key);  // 独立实例

let agent = Agent::builder(client, "deepseek-v4-pro")
    .system_prompt("You are a helpful coding assistant.")
    .tool(Arc::new(CalculatorTool))
    .tool(ReadTool::new(workspace.clone()))
    .tool(ShellTool::new(workspace_root, Duration::from_secs(30)))
    .hook(MacroCompactHook::new(
        "deepseek-v4-flash".into(), 2_000_000, 10, compact_client,
    ))
    .hook(MicroCompactHook::new(5, compactable_tools))
    .hook(Box::new(SandboxHook::new(...)))
    .max_steps(50)
    .build();
```

**高级风格 — `EngineContext::builder()`：**

```rust
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use deepseek::DeepSeekClient;
use engine::{Agent, AgentEvent, EngineContext};
use memory::{Memory, SharedMemory};
use provider::{Message, Role};
use tokio::sync::mpsc;
use tools::{ToolRegistry, WorkspaceFs};

pub fn build_coding_agent(
    api_key: &str,
    workspace_root: &Path,
    model: &str,
    flash_model: &str,
) -> (Agent<DeepSeekClient>, SharedMemory, mpsc::UnboundedReceiver<AgentEvent>) {
    // 1. 创建事件通道（单一通道承载所有事件）
    let (agent_tx, agent_rx) = mpsc::unbounded_channel::<AgentEvent>();

    // 2. 创建文件沙箱
    let workspace = WorkspaceFs::new(workspace_root)
        .expect("无法创建 workspace");
    let workspace = Arc::new(workspace);

    // 3. 创建并注册工具
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CalculatorTool));
    registry.register(Arc::new(ReadTool::new(workspace.clone())));
    registry.register(Arc::new(WriteTool::new(workspace.clone())));
    registry.register(Arc::new(ShellTool::new(
        workspace_root.to_path_buf(),
        Duration::from_secs(30),
    )));
    let registry = Arc::new(registry);

    // 4. 创建 Memory
    let memory: SharedMemory = Arc::new(std::sync::RwLock::new(Memory::new()));

    // 5. 创建 LLM 客户端
    let client = DeepSeekClient::new(api_key);
    let compact_client = DeepSeekClient::new(api_key);

    // 6. 创建 Hooks（顺序很重要！）
    let hooks: Vec<Box<dyn engine::AgentHook>> = vec![
        Box::new(MacroCompactHook::new(flash_model.into(), 2_000_000, 10, compact_client)),
        Box::new(MicroCompactHook::new(5, compactable_tools)),
        Box::new(SandboxHook::new(shell_filter, resource_tracker, audit_logger)),
    ];

    // 7. 组装 EngineContext
    let ctx = EngineContext::builder(client, memory.clone(), registry, model.to_string())
        .hooks(hooks)
        .max_steps(50)
        .max_retries(3)
        .streaming(true)
        .build();

    // 8. 创建 Agent
    let agent = Agent::new(ctx);

    // 9. 注入 System Prompt
    {
        let mut mem = memory.write().unwrap();
        mem.push(Message::new(Role::System, SYSTEM_PROMPT));
    }

    (agent, memory, agent_rx)
}
```

### 8.3 Agent 启动方法

```rust
// 方式 1：简单运行（无事件流）
let result: String = agent.run("你好，帮我写个排序函数").await?;

// 方式 2：带事件流（用于 TUI 实时渲染）
let (tx, rx) = mpsc::unbounded_channel();
let result: String = agent.run_with_events("用户输入", tx).await?;

// Rx 端在另一个 task 中消费事件
while let Some(event) = rx.recv().await {
    match event {
        AgentEvent::Token(t) => print!("{t}"),
        AgentEvent::ToolCall { name, .. } => println!("\n[{name}] starting..."),
        AgentEvent::ToolSuccessful { name, .. } => println!("[{name}] done"),
        AgentEvent::Done => break,
        _ => {}
    }
}
```

---

## 9. Streaming Events 与 TUI 集成

### 9.1 Agent 事件类型

[`AgentEvent`](../libs/engine/src/agent.rs) 涵盖框架可能产生的所有事件：

```rust
#[derive(Debug, Clone)]
pub enum AgentEvent {
    // ── Run 生命周期 ──
    RunStarted { session_id: String, user_input: String },

    // ── LLM 流式输出 ──
    Token(String),                        // 流式文本 token
    ReasoningToken(String),               // 推理 token（reasoning 模型）

    // ── 工具/命令调用（LLM + 用户统一）──
    ToolCall {
        id: String,
        name: String,
        arguments: String,
        origin: CallOrigin,               // Llm | User
    },
    ToolSuccessful { id: String, name: String, output: String },
    ToolRejected { id: String, name: String, reason: String },
    ToolFailure { id: String, name: String, error: String },
    ToolProgress { id: String, name: String, message: String },

    // ── 交互式用户干预 ──
    NeedUserIntervene(InterveneRequest),

    // ── Run 终结 ──
    RunCompleted { answer: String },
    RunFailed { error: String },
    Cancelled,

    // ── 流结束 ──
    Done,
}
```

**`CallOrigin`** 区分调用来源：
- `CallOrigin::Llm` — LLM 决定调用的工具
- `CallOrigin::User` — 用户通过 `!command` 直接执行的命令

**Tool 结果已拆分为三种事件**（替代旧版统一的 `ToolResult`）：
- `ToolSuccessful` — 工具执行成功
- `ToolRejected` — Hook（如沙箱）阻止了工具执行
- `ToolFailure` — 工具执行失败或未找到

**Run 结果**：
- `RunCompleted { answer }` — Agent 成功完成，携带最终回答
- `RunFailed { error }` — Agent 因错误终止
- `Cancelled` — 用户取消了运行（如 Ctrl+C）
- `Done` — **永远是最后一个事件**，无论成功、失败还是取消

### 9.2 通道拓扑（TUI 架构）

Loomis 的 TUI 使用**单通道**架构：

```text
TUI 线程                          Agent 任务 (tokio::spawn)
─────────                          ────────────────────────
cmd_tx ───────── TuiCommand ──────→ cmd_rx
agent_rx ←────── AgentEvent ─────── agent_tx    (LLM token / tool call / intervene / done)
```

### 9.3 消费事件流

```rust
// 在 tokio::spawn 中运行 agent
tokio::spawn(async move {
    let result = agent.run_with_events(&user_input, agent_tx.clone()).await;
    // run_with_events 内部会自动发送 RunStarted, RunCompleted/RunFailed, Done
    match result {
        Ok(answer) => eprintln!("Agent done: {answer}"),
        Err(e) => eprintln!("Agent error: {e}"),
    }
});

// 在主线程消费事件
while let Some(event) = agent_rx.blocking_recv() {
    match event {
        AgentEvent::RunStarted { user_input, .. } => {
            println!("🚀 Processing: {user_input}");
        }
        AgentEvent::Token(text) => print!("{text}"),
        AgentEvent::ReasoningToken(text) => print!("[think: {text}]"),
        AgentEvent::ToolCall { name, origin, .. } => {
            match origin {
                CallOrigin::User => println!("\n$ 执行: {name}"),
                CallOrigin::Llm => println!("\n🔧 调用工具: {name}"),
            }
        }
        AgentEvent::ToolSuccessful { output, .. } => {
            let preview = output.chars().take(200).collect::<String>();
            println!("结果: {preview}");
        }
        AgentEvent::ToolRejected { reason, .. } => {
            println!("⛔ 被拒绝: {reason}");
        }
        AgentEvent::ToolFailure { error, .. } => {
            println!("❌ 工具失败: {error}");
        }
        AgentEvent::ToolProgress { message, .. } => {
            println!("  …{message}");
        }
        AgentEvent::NeedUserIntervene(req) => {
            println!("\n⚡ {}: {}", req.title, req.description);
            // 应用层展示选项并收集用户响应
        }
        AgentEvent::RunCompleted { answer } => {
            println!("\n✅ Done: {}", &answer[..answer.len().min(100)]);
        }
        AgentEvent::RunFailed { error } => {
            println!("\n❌ Failed: {error}");
        }
        AgentEvent::Cancelled => println!("\n⚠️ Cancelled"),
        AgentEvent::Done => break,
    }
}
```

---

## 10. WorkspaceFs 文件沙箱

[`WorkspaceFs`](../libs/tools/src/fs.rs) 确保所有文件操作都在 `workspace_root` 内进行：

```rust
use tools::WorkspaceFs;

let fs = WorkspaceFs::new("/path/to/project")?;

// 路径解析 — 自动规范化并检查是否逃逸沙箱
let resolved = fs.resolve("src/main.rs")?;  // → /path/to/project/src/main.rs
// fs.resolve("../etc/passwd") → Err(FsError::PathEscapesWorkspace)

// 读文件（支持 offset 和 limit）
let content = fs.read("src/main.rs", Some(10), Some(50))?;

// 写文件（自动创建父目录）
fs.write("new/file.txt", "hello world")?;

// 编辑文件行
fs.edit_lines("src/main.rs", 5, 10, "replacement\ncontent")?;

// Glob 匹配
let files = fs.glob("src/**/*.rs")?;

// Grep 搜索
let matches = fs.grep("TODO", Some("src/**/*.rs"))?;

// 列出目录
let entries = fs.ls(Some("src/"))?;
```

**安全特性：**
- 所有路径通过 `canonicalize` 规范化
- 逃逸沙箱的路径返回 `FsError::PathEscapesWorkspace`
- 写操作自动创建父目录
- TOCTOU 二次校验（resolve 时检查一次，实际操作时再检查一次）
- 文件大小限制（`max_read_bytes` / `max_write_bytes`）
- 扩展名黑名单（`.exe`, `.dll`, `.so` 等）
- 隐藏文件保护（`.` 开头的文件）
- 二进制内容检测（拒绝 NULL 字节）
- Glob/Grep 结果自动转为相对于 workspace root 的路径
- 沙箱行为完全由 [`SandboxConfig`](../libs/tools/src/sandbox/config.rs) 驱动，支持 `.loomis/config.toml` 配置

---

## 11. 多层沙箱系统

Loomis 实现了纵深防御的沙箱架构。详细文档见 [`sandbox-architecture.md`](./sandbox-architecture.md)。

### 11.1 架构总览

一个 LLM 工具调用经过以下**四层**检查：

```
Agent Loop (engine)
  │
  └─→ SandboxHook::before_tool_call()     ← 第 1 层：Hook 策略判断
        │
        ├── ResourceTracker 配额检查
        └── ShellFilter 命令分类 → Blocked / AutoApproved / RequiresApproval
              │
              ▼
  └─→ Tool::execute_stream()              ← 第 2 层：工具层强制
        │
        ├── WorkspaceFs 路径沙箱（文件工具）
        ├── ShellFilter 二次检查（shell 工具）
        ├── EnvSanitizer 环境变量清洗
        └── Watchdog 进程超时杀
              │
              ▼
  └─→ SandboxHook::after_tool_call()      ← 第 3 层：审计记录
        │
        ├── ResourceTracker 更新计数
        └── AuditLogger 写入 .loomis/audit.jsonl
```

### 11.2 沙箱组件

| 组件 | 位置 | 职责 |
|------|------|------|
| `SandboxHook` | `bins/loomis/src/hooks/sandbox_hook.rs` | 统一入口 — 配额检查、命令分类、用户审批、审计 |
| `ShellFilter` | `bins/loomis/src/sandbox/shell_filter.rs` | 命令分类：strict allowlist → deny patterns → auto-approve → prompt |
| `ResourceTracker` | `bins/loomis/src/sandbox/resource_tracker.rs` | 配额跟踪（总操作数、并发 shell 数） |
| `AuditLogger` | `bins/loomis/src/sandbox/audit_logger.rs` | 全链路审计日志（`.loomis/audit.jsonl` + 内存环形缓冲） |
| `EnvSanitizer` | `bins/loomis/src/sandbox/env_sanitizer.rs` | 清除危险环境变量，仅保留安全白名单 |
| `SandboxConfig` | `libs/tools/src/sandbox/config.rs` | 配置驱动 — 所有策略从 `.loomis/config.toml` 加载 |

### 11.3 ShellFilter 分类优先级

```
1. Strict allowlist   →  binary 不在白名单? → Blocked
2. Deny patterns      →  完整命令匹配正则?   → Blocked
3. Auto-approve       →  命令前缀匹配?       → AutoApproved
4. Fallthrough        →  以上都不匹配        → RequiresApproval
```

### 11.4 配置示例（`.loomis/config.toml`）

```toml
[filesystem]
max_read_bytes = 1048576
max_write_bytes = 524288
forbid_binary_writes = true
forbid_hidden_file_writes = true
blocked_write_extensions = [".exe", ".dll", ".so", ".dylib", ".sys", ".bin"]

[shell]
default_timeout_secs = 30
max_timeout_secs = 120
max_output_bytes = 100000
sanitize_environment = true

[shell.auto_approve]
prefixes = ["cargo", "git", "npm", "node", "python", "python3",
            "dir", "echo", "type", "ls", "cat", "head", "tail", "wc",
            "pwd", "date", "which", "where", "printenv"]

[shell.deny_patterns]
patterns = ["rm -rf\\s+(/|~)", "sudo\\s+", "shutdown", "reboot",
            "mkfs\\.", "dd\\s+if=", ">\\s*/dev/"]

# [shell.allowed_commands]
# binaries = ["cargo", "git"]  # 取消注释启用严格白名单模式

[quotas]
max_total_operations = 10000
max_concurrent_shells = 2

[audit]
enabled = true
log_file = ".loomis/audit.jsonl"
```

### 11.5 关键设计原则

1. **双重防线** — Hook 层做策略判断，工具层做技术强制执行。即使 Hook 层被绕过，工具层仍会拦截。
2. **Fail closed** — 任何检查失败都阻止执行。配置缺失时使用最严格的安全默认值。
3. **纵深防御** — ShellFilter 在 Hook 层和 Tool 层各执行一次，互为备份。
4. **全链路审计** — 每一步都记录到 `.loomis/audit.jsonl`，事后可追溯。
5. **配置即策略** — 所有安全检查由 `SandboxConfig` 驱动，用户可通过 `.loomis/config.toml` 调节安全等级。

---

## 12. Subagent 子代理系统

[`SubagentTool`](../libs/subagent/src/tool.rs) 让你将子 Agent 包装为一个 Tool，父 Agent 可以调用它来完成复杂子任务。

### 12.1 核心概念

当父 Agent 的 LLM 调用 `task` 工具时：
1. 创建一个新的 `Agent` 实例（子 Agent）
2. 子 Agent 拥有独立的 Memory 和**过滤后的**工具集
3. 子 Agent 运行到完成（或超时）
4. 子 Agent 的结果作为 Tool Result 返回给父 Agent

### 12.2 SubagentConfig

```rust
pub struct SubagentConfig {
    /// 子 Agent 使用的模型（通常用便宜/快速的模型）
    pub model: String,

    /// 子 Agent 的 system prompt
    pub system_prompt: String,

    /// 最大 ReAct 循环步数
    pub max_steps: usize,         // 默认 25

    /// 最大重试次数
    pub max_retries: usize,       // 默认 2

    /// 是否启用流式
    pub streaming: bool,           // 默认 true

    /// 硬超时（秒），超时后子 Agent 被终止，返回超时消息
    pub timeout_secs: Option<u64>, // 默认 Some(120)

    /// 从父 Agent 的 Memory 中继承最后 N 条非 System 消息
    /// 用于跨委托保持对话连续性
    pub inherit_context_messages: Option<usize>, // 默认 None
}
```

### 12.3 使用示例

```rust
use std::sync::Arc;
use subagent::{SubagentTool, SubagentConfig, filter_tools};

// 1. 从父 Agent 的工具注册表中筛选出只读工具
let subagent_tools = Arc::new(filter_tools(
    &parent_registry,
    &["read", "grep", "glob", "ls", "calculator"],
));

// 2. 创建 SubagentTool
let subagent = SubagentTool::new(
    llm_client.clone(),
    SubagentConfig {
        model: "deepseek-v4-flash".into(),
        system_prompt: "\
You are a focused sub-agent with read-only access.
Complete the assigned task carefully and report your findings concisely.\
        ".into(),
        max_steps: 25,
        timeout_secs: Some(120),
        ..Default::default()
    },
    subagent_tools,
    parent_memory.clone(),  // 共享父 Agent 的 memory
);

// 3. 注册到父 Agent 的工具注册表
parent_registry.register(Arc::new(subagent));
```

### 12.4 filter_tools 工具过滤

```rust
use subagent::filter_tools;
use std::sync::Arc;

// 从父注册表中筛选特定工具
let read_only = Arc::new(filter_tools(
    &parent_registry,
    &["read", "grep", "glob"],  // 只保留这些工具
));

assert!(read_only.has("read"));
assert!(!read_only.has("write"));  // write 工具被过滤掉
```

---

## 13. 对话持久化

[`memory::persistence`](../libs/memory/src/persistence.rs) 模块提供会话的保存和恢复。注意：所有函数现在需要 `workspace_root: &Path` 参数来定位 `.loomis/` 目录。

```rust
use memory::persistence::{
    save_conversation, load_conversation, list_threads,
    ThreadInfo, default_thread_name, generate_thread_name,
    read_current_thread, write_current_thread,
};

// 保存当前会话（需要 workspace_root 参数）
save_conversation("my_session", workspace_root, &memory)?;
// → 写入 .loomis/threads/my_session.json 和 .md

// 列出所有会话
let threads: Vec<ThreadInfo> = list_threads(workspace_root)?;
for t in &threads {
    println!("{} - {} messages", t.name, t.message_count);
}

// 恢复历史会话
let history = load_conversation("my_session", workspace_root)?;
let memory = Memory::from(history.messages);

// 读取/写入当前线程标记
write_current_thread("my_session", workspace_root)?;
let current = read_current_thread(workspace_root);  // → Some("my_session")

// 获取默认线程名
let name = default_thread_name(workspace_root);
// → 返回 read_current_thread 的结果，或 "autosave"
```

### ThreadInfo

```rust
#[derive(Debug, Clone)]
pub struct ThreadInfo {
    pub name: String,           // 会话名（无 .json 后缀）
    pub saved_at: String,       // ISO 时间戳
    pub message_count: usize,   // 消息总数
    pub total_chars: usize,     // 总字符数
}
```

### 持久化文件格式

保存时生成两个文件：
- `.loomis/threads/{name}.json` — 完整对话的 JSON（version + saved_at + messages）
- `.loomis/threads/{name}.md` — Markdown 格式的人类可读版本

---

## 14. 完整示例：代码审查 Agent

这是一个从零构建的完整 Agent，它会读取代码文件并提供审查意见。展示了 Tool、Hook、ProgressStream 和事件流的完整用法。

```rust
// bins/code-reviewer/src/main.rs
use std::sync::Arc;

use deepseek::DeepSeekClient;
use engine::{Agent, AgentError, AgentEvent, AgentHook, RunOutcome};
use memory::SharedMemory;
use provider::{Message, Role, ToolCall};
use schemars::JsonSchema;
use serde::Deserialize;
use tools::{Progress, ProgressStream, ToolError, WorkspaceFs, tool};

// ══════════════════════════════════════════════════════════════════════
// 工具：读取文件
// ══════════════════════════════════════════════════════════════════════

#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeReviewArgs {
    #[schemars(description = "要审查的文件的路径")]
    pub file_path: String,
}

#[tool(
    name = "review_file",
    description = "读取一个源代码文件并返回其内容以供审查。只读操作。",
    args = CodeReviewArgs
)]
struct ReviewFileTool {
    fs: Arc<WorkspaceFs>,
}

impl ReviewFileTool {
    fn new(fs: Arc<WorkspaceFs>) -> Self { Self { fs } }

    fn execute_stream(&self, args: CodeReviewArgs) -> Result<ProgressStream, ToolError> {
        let content = self.fs
            .read(&args.file_path, None, None)
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        Ok(ProgressStream::done(content))
    }
}

// ══════════════════════════════════════════════════════════════════════
// Hook: 记录审查进度
// ══════════════════════════════════════════════════════════════════════

struct ReviewProgressHook;

impl AgentHook for ReviewProgressHook {
    fn on_run_start(&self, _session_id: &str, user_input: &str) {
        println!("🚀 Starting review: {}", user_input);
    }

    fn before_tool_call(
        &self,
        _session_id: &str,
        tool: &ToolCall,
    ) -> Result<(), AgentError> {
        println!("📖 Reviewing: {}", tool.function.arguments);
        Ok(())
    }

    fn after_tool_call(
        &self,
        _session_id: &str,
        tool: &ToolCall,
        observation: &str,
    ) {
        let lines = observation.lines().count();
        println!("✅ Read {} ({} lines)", tool.function.name, lines);
    }

    fn on_run_finish(&self, _session_id: &str, outcome: &RunOutcome) {
        match outcome {
            RunOutcome::Success { .. } => println!("✅ Review complete"),
            RunOutcome::Error { error } => eprintln!("❌ Review failed: {error}"),
            RunOutcome::Cancelled => println!("⚠️ Review cancelled"),
        }
    }
}

// ══════════════════════════════════════════════════════════════════════
// System Prompt
// ══════════════════════════════════════════════════════════════════════

const SYSTEM_PROMPT: &str = "\
你是一个资深的代码审查专家。你的任务是：
1. 使用 review_file 工具读取用户指定的文件
2. 对代码进行全面的审查：
   - 逻辑错误和潜在 bug
   - 安全漏洞
   - 性能和内存问题
   - 代码风格和改进建议
3. 将发现的问题按严重程度分类（严重 / 中等 / 建议）
4. 给出具体的、可操作的改进建议

只审查用户请求的文件，不要主动去探索项目结构。";

// ══════════════════════════════════════════════════════════════════════
// 主函数
// ══════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API").expect("DEEPSEEK_API not set");
    let cwd = std::env::current_dir().unwrap();

    // ── 沙箱 ──────────────────────────────────────────
    let fs = Arc::new(WorkspaceFs::new(&cwd).expect("workspace init failed"));

    // ── LLM ───────────────────────────────────────────
    let client = DeepSeekClient::new(&api_key);

    // ── 创建 Agent（Builder 一行搞定）───────────────
    let agent = Agent::builder(client, "deepseek-v4-pro")
        .system_prompt(SYSTEM_PROMPT)
        .tool(ReviewFileTool::new(fs))
        .hook(Box::new(ReviewProgressHook))
        .max_steps(15)
        .streaming(true)
        .build();

    // ── Event channel ─────────────────────────────────
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    println!("=== 代码审查 Agent ===");
    println!("输入要审查的文件路径（相对于当前目录）：");

    let mut file_path = String::new();
    std::io::stdin().read_line(&mut file_path).unwrap();
    let file_path = file_path.trim().to_string();

    let prompt = format!("请审查文件: {file_path}");

    {
        let mut mem = agent.memory().write().unwrap();
        mem.push(Message::new(Role::User, &prompt));
    }

    // ── 运行并消费流式事件 ──────────────────────────
    let tx_clone = tx.clone();
    let handle = tokio::spawn(async move {
        let result = agent.run_with_events(&prompt, tx_clone).await;
        // run_with_events 会自动发送 RunCompleted/RunFailed + Done
        if let Err(e) = result {
            eprintln!("Agent error: {e}");
        }
    });

    while let Some(event) = rx.recv().await {
        match event {
            AgentEvent::Token(text) => print!("{text}"),
            AgentEvent::ToolCall { name, .. } => {
                println!("\n━━━ 调用工具: {name} ━━━");
            }
            AgentEvent::ToolSuccessful { output, .. } => {
                let lines = output.lines().count();
                println!("[完成] 读取了 {lines} 行");
            }
            AgentEvent::ToolFailure { error, .. } => {
                println!("[失败] {error}");
            }
            AgentEvent::NeedUserIntervene(req) => {
                println!("\n⚡ {}: {}", req.title, req.description);
                // 在生产代码中，这里应展示选项并收集用户响应
            }
            AgentEvent::RunCompleted { .. } | AgentEvent::RunFailed { .. } => {}
            AgentEvent::Done => break,
            _ => {}
        }
    }

    handle.await.unwrap();
    println!("\n=== 审查完成 ===");
}
```

### 运行

```bash
cargo run -p code-reviewer
# 输入: src/main.rs
# Agent 会读取文件内容，然后给出详细的代码审查意见
```

---

## 附录 A：开发建议

### A.1 设计原则

1. **工具要原子化** — 一个工具做一件事。LLM 更擅长组合简单工具而不是使用复杂工具。
2. **描述要详细** — `Tool::description()` 是 LLM 选择工具的唯一依据。写明使用场景和 NOT to use 的场景。
3. **参数 Schema 要精确** — 使用 `#[schemars(description = "...")]` 给每个字段添加描述。
4. **Hook 按职责分离** — 压缩（MicroCompact + MacroCompact）和安全（Sandbox）分属不同 Hook。注册顺序很重要：先压缩，后安全。
5. **同步 Tool trait** — Tool 是同步的。如果工具需要异步 I/O，使用 `ProgressStream` + `tokio::sync::mpsc` 从独立线程发送进度事件。
6. **单一事件通道** — 所有事件（LLM 输出、工具结果、用户干预）都通过同一个 `AgentEvent` 通道发送，不创建额外的 side channel。
7. **子 Agent 要限制权限** — Subagent 应使用过滤后的只读工具集，防止子 Agent 执行危险操作。

### A.2 调试技巧

```rust
// 1. 查看 Agent 的完整对话历史
let mem = agent.memory().read().unwrap();
for msg in mem.messages() {
    println!("[{:?}] {}", msg.role, msg.content);
}

// 2. 使用 Builder 设置较低的最大步数来调试工具循环
let agent = Agent::builder(client, "deepseek-v4-pro")
    .system_prompt("...")
    .max_steps(3)  // 限制 3 步，方便观察
    .streaming(false) // 非流式输出更易读
    .build();

// 3. 用 ProgressStream::poll_done() 在测试中同步等待工具结果
let mut stream = tool.execute_stream(r#"{"key":"val"}"#).unwrap();
let result = stream.poll_done();  // 阻塞直到 Done
println!("Tool result: {result}");
```

### A.3 添加 Provider 的检查清单

- [ ] 实现 `LLMClient::generate()` — 非流式（使用 Rust 2024 原生 async，**不要**引入 `#[async_trait]`）
- [ ] 实现 `LLMClient::stream()` — SSE 流式
- [ ] 正确映射请求格式（Message → Provider API）
- [ ] 正确映射响应格式（Provider API → CompletionResponse/StreamChunk）
- [ ] 处理错误重试（5xx → 重试，4xx → 不重试）
- [ ] 处理 Tool Call 的流式和非流式返回

### A.4 添加工具的检查清单

- [ ] 定义参数结构体（`Deserialize + JsonSchema`）
- [ ] 用 `#[tool(name, description, args)]` 标注或手动实现 `Tool` trait
- [ ] 实现 inherent `execute_stream` 方法，返回 `Result<ProgressStream, ToolError>`
- [ ] 瞬时完成的工具用 `ProgressStream::done(result)`
- [ ] 长时间运行的工具通过 mpsc channel 发送 `Progress::InProgress` + `Progress::Done`
- [ ] 处理参数错误 → `ToolError::InvalidArgs`
- [ ] 处理执行错误 → `ToolError::Execution`
- [ ] 如果需要文件访问，通过 `Arc<WorkspaceFs>` 注入
- [ ] 在组装函数中注册到 `ToolRegistry`

### A.5 添加 Hook 的检查清单

- [ ] 确定需要监听的生命周期事件（on_run_start / on_llm_start / before_tool_call / ...）
- [ ] 对于只读通知，实现 `on_*` 方法
- [ ] 对于可拦截操作，实现 `before_*` 方法，返回 `Result<(), AgentError>`
- [ ] 如需用户干预，通过 `AgentEvent::NeedUserIntervene` 发请求，通过 `SyncSender<InterveneResponse>` 收响应
- [ ] 注册时注意顺序：压缩类 Hook 通常排前面，安全类 Hook 排后面

---

## 附录 B：现有代码参考

| 想了解... | 看这个文件 |
|-----------|-----------|
| Agent 循环核心逻辑 | [`libs/engine/src/agent.rs`](../libs/engine/src/agent.rs) |
| AgentBuilder | [`libs/engine/src/builder.rs`](../libs/engine/src/builder.rs) |
| EngineContext / EngineContextBuilder | [`libs/engine/src/context.rs`](../libs/engine/src/context.rs) |
| Hook trait 定义 | [`libs/engine/src/hooks.rs`](../libs/engine/src/hooks.rs) |
| Tool trait + `#[tool]` 宏 | [`libs/tools/src/tool.rs`](../libs/tools/src/tool.rs) + [`libs/tools-macros/src/lib.rs`](../libs/tools-macros/src/lib.rs) |
| Progress / ProgressStream | [`libs/tools/src/progress.rs`](../libs/tools/src/progress.rs) |
| ToolRegistry | [`libs/tools/src/registry.rs`](../libs/tools/src/registry.rs) |
| WorkspaceFs 沙箱 | [`libs/tools/src/fs.rs`](../libs/tools/src/fs.rs) |
| SandboxConfig | [`libs/tools/src/sandbox/config.rs`](../libs/tools/src/sandbox/config.rs) |
| Memory | [`libs/memory/src/memory.rs`](../libs/memory/src/memory.rs) |
| 持久化 | [`libs/memory/src/persistence.rs`](../libs/memory/src/persistence.rs) |
| MicroCompactHook / MacroCompactHook | [`libs/hooks/src/compact.rs`](../libs/hooks/src/compact.rs) |
| SubagentTool / SubagentConfig | [`libs/subagent/src/tool.rs`](../libs/subagent/src/tool.rs) + [`libs/subagent/src/config.rs`](../libs/subagent/src/config.rs) |
| LLMClient trait | [`libs/provider/src/client.rs`](../libs/provider/src/client.rs) |
| Message / ToolCall 类型 | [`libs/provider/src/message.rs`](../libs/provider/src/message.rs) |
| DeepSeek Client 实现 | [`libs/deepseek/src/lib.rs`](../libs/deepseek/src/lib.rs) |
| 组装入口（完整 wiring） | [`bins/loomis/src/app.rs`](../bins/loomis/src/app.rs) |
| SandboxHook | [`bins/loomis/src/hooks/sandbox_hook.rs`](../bins/loomis/src/hooks/sandbox_hook.rs) |
| ShellFilter | [`bins/loomis/src/sandbox/shell_filter.rs`](../bins/loomis/src/sandbox/shell_filter.rs) |
| ResourceTracker | [`bins/loomis/src/sandbox/resource_tracker.rs`](../bins/loomis/src/sandbox/resource_tracker.rs) |
| AuditLogger | [`bins/loomis/src/sandbox/audit_logger.rs`](../bins/loomis/src/sandbox/audit_logger.rs) |
| EnvSanitizer | [`bins/loomis/src/sandbox/env_sanitizer.rs`](../bins/loomis/src/sandbox/env_sanitizer.rs) |
| 计算器工具 | [`bins/loomis/src/tools/calculator.rs`](../bins/loomis/src/tools/calculator.rs) |
| 文件读取工具（WorkspaceFs） | [`bins/loomis/src/tools/read.rs`](../bins/loomis/src/tools/read.rs) |
| Shell 工具 | [`bins/loomis/src/tools/shell.rs`](../bins/loomis/src/tools/shell.rs) |
| TUI 集成 | [`bins/loomis/src/tui/mod.rs`](../bins/loomis/src/tui/mod.rs) |
| 沙箱架构详解 | [`docs/sandbox-architecture.md`](./sandbox-architecture.md) |
