# 从零构建 Agent 框架 (Rust版)：架构蓝图与开发指南
使用 Rust 构建 Agent，我们将充分享受强类型带来的安全感。利用 serde 处理 JSON，利用 Trait 抽象大模型和工具，可以让整个主控循环异常坚固。

我们将整个项目分为三个阶段（MVP -> 进阶 -> 生产级），并以一个具体的实战项目为目标。
## 实战项目建议：自动调研助手 (Auto-Researcher)
**目标**：你输入一个问题（例如：“对比 2024 年大模型上下文技术的最新进展”），Agent 能够自主思考，调用“搜索引擎”工具查找资料，调用“网页读取”工具提取内容，最后在本地生成一份 Markdown 格式的调研报告。

## 第一阶段：最小可行性产品 (MVP) - 让核心转起来
这一阶段的目标是跑通主控循环引擎。基于 Tokio 异步运行时，手动处理 JSON 和工具调用。

### 1. LLM 客户端封装 (`llm_client.rs`)
- 引入 `reqwest` 处理异步 HTTP 请求，`serde` 和 `serde_json` 处理序列化
- 定义核心结构体：`Message`、`Role`（Enum：System/User/Assistant/Tool）、`ToolCall`
- 定义一个 trait `LlmClient`，实现发送请求并返回结果的方法

### 2. 基础记忆模块 (`memory.rs`)
- 定义结构体 `Memory { messages: Vec<Message> }`
  > 注意：多异步任务共享上下文时，需包装为 `Arc<Mutex<Memory>>` 或 `Arc<RwLock<Memory>>`
- 实现基础方法：`push()`、`get_context()`
- 达到1M上下文长度自动出发compact，调用小模型压缩记忆

### 3. 工具系统基础 (`tools.rs`)
- 定义 `Tool` Trait，包含方法：
  - `name()`：工具名称
  - `description()`：工具描述
  - `parameters()`：返回 `serde_json::Value` 格式 Schema
  - `async fn execute(&self, args: &str) -> Result<String, Error>`：工具执行逻辑
- 实现 1~2 个示例工具结构体（如 `CalculatorTool`）
- 手动拼接 JSON Schema 返回

### 4. 核心主循环 (`agent.rs`)
- 构建异步主函数 `async fn run_loop()`，循环主体 `loop { ... }`
- 调用 LLM 后通过 `match` 匹配返回结果分支：
  1. 纯文本回复：终止循环并输出结果
  2. 工具调用 `tool_calls`：解析参数、批量执行本地工具，将工具结果封装为 `Role::Tool` 写入记忆
- 附加能力：失败重试、最大执行步数 `max_steps` 防死循环

## 第二阶段：进阶能力 - 释放 Rust 宏与类型系统的威力
拥抱 Rust 宏编程，消除重复样板代码，优化开发体验。

### 5. 工具 Schema 自动生成 (`macros / schemars`)
- 引入 `schemars` 库
- 核心优势：工具入参定义为 `Args` 结构体，通过 `#[derive(JsonSchema, Deserialize)]` 注解
- 使用 `schemars::schema_for!(Args)` 一键生成 OpenAI/Gemini 标准 JSON Schema，彻底手写 Schema

### 6. 模板引擎与提示词管理 (`prompt.rs`)
- 可选库：`minijinja` / `askama`（编译期安全模板引擎）
- 将复杂 System Prompt 抽离为独立模板文件，动态注入角色、时间等变量

### 7. 异步流式输出 (Streaming)
- 依赖库：`reqwest-eventsource`、`futures::stream::StreamExt`
- 基于 `tokio::sync::mpsc` 通道实时推送文字流至终端
- 支持 `Stream<Item = Result<String, Error>>` 类型转换，兼容复杂流式响应

### 8. 结构化输出 (Structured Output)
- 依托 Rust 强类型反序列化能力
- 结合大模型 `response_format` 结构化输出特性，直接将返回内容反序列化为自定义结构体（如 `ResearchReport`）
```rust
serde_json::from_str::<ResearchReport>(&response.content)
```
- 全程类型安全，无需手动解析字符串

## 第三阶段：高级特性 - 迈向生产级（选做）
适配复杂业务场景，完善生产环境配套架构。

### 9. 长期记忆与知识库 (RAG & Vector DB)
- 向量库选型：`qdrant-rust` / `pgvector`（配合 `sqlx`）
- 完整流程：文档切片拆分 → 调用 Embedding 接口生成向量 → 存入向量数据库
- 主循环前置逻辑：每次请求前检索向量库，注入相关历史上下文

### 10. 交互界面 (TUI / UI)
- 终端界面：使用 `ratatui` 搭建命令行交互面板
- Web/桌面端：
  - `Axum`：提供后端 HTTP API
  - `Tauri`：打包跨平台轻量桌面客户端

### 11. 可观测性 (Observability)
- 日志库：`tracing`（替代原生 `println!` / `log`）
- 配套 `tracing-subscriber`，分层埋点：HTTP 请求、Agent 主循环、工具执行
- 完整链路 Span 日志，快速排查模型幻觉、工具调用崩溃问题

# 推荐 Cargo 项目目录结构
```
my_agent_framework/
├── Cargo.toml
└── src/
    ├── main.rs               # Tokio 异步运行时程序入口
    ├── core/
    │   ├── mod.rs
    │   ├── agent.rs          # Agent 核心主循环逻辑
    │   └── llm_client.rs     # LLM API 请求封装
    ├── memory.rs             # 记忆、上下文管理模块
    ├── tools/
    │   ├── mod.rs
    │   ├── registry.rs       # 工具注册分发器 + Tool Trait 定义
    │   ├── search_tool.rs    # 搜索引擎工具实现
    │   └── file_tool.rs      # 本地文件读写工具实现
    ├── prompts/
    │   ├── mod.rs
    │   └── templates/        # minijinja 提示词模板文件
```