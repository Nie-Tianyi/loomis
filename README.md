# Agent Oxide

一个用 Rust 从零构建的 AI Agent 框架，基于 Tokio 异步运行时，以**自动调研助手 (Auto-Researcher)** 为实战目标——输入一个问题，Agent 能自主思考、调用工具、生成 Markdown 调研报告。

## 特性

- **DeepSeek API 客户端** — 完整的类型化请求/响应定义，支持 SSE 流式输出，自定义 `FinishReason` 向前兼容
- **Agent 主循环** — `run_loop()` 批量模式 + `run_with_events()` 实时流式模式，`max_steps` 防死循环，自动工具调用分发
- **工具系统** — `Tool` trait + `ToolRegistry`，内置计算器、文件读写/编辑、Glob/Grep 搜索、目录列表等工具，支持 `WorkspaceFs` 沙箱隔离
- **记忆模块** — 可配置阈值 + 两阶段压缩（`drain_for_compact` → `apply_compact`），`Arc<RwLock<Memory>>` 多任务共享，System 消息永不被压缩
- **TUI 交互界面** — ratatui + crossterm 打造的终端聊天界面，实时流式 Token 显示、工具调用状态、滚动历史、斜杠命令

## 快速开始

### 前置条件

- Rust 工具链（2024 edition）
- DeepSeek API Key

### 配置

在项目根目录创建 `.env` 文件：

```bash
DEEPSEEK_API=your_api_key_here
```

### 编译与运行

```bash
# 调试构建
cargo build

# 发布构建
cargo build --release

# 启动 TUI 模式（默认）
cargo run

# 启动传统命令行模式
cargo run -- --no-tui
```

### 测试与检查

```bash
cargo test                    # 运行所有测试
cargo test tui                # 仅运行 TUI 模块测试
cargo test -p agent_oxide -- test_find_event_end  # 运行单个测试
cargo clippy                  # 代码检查
```

## 架构概览

```
agent_oxide/
├── Cargo.toml
└── src/
    ├── main.rs               # 入口：TUI（默认）/ --no-tui 传统模式
    ├── lib.rs                 # 库根，统一导出
    ├── core/
    │   ├── mod.rs
    │   ├── agent.rs           # Agent 核心主循环
    │   └── client/
    │       ├── mod.rs         # 扁平重导出
    │       ├── client.rs      # DeepSeekClient（send / stream）
    │       ├── request.rs     # 请求类型定义
    │       ├── response.rs    # 响应类型定义
    │       ├── stream.rs      # SSE 流解析管道
    │       └── error.rs       # DeepSeekError
    ├── memory/
    │   └── mod.rs             # Memory / SharedMemory / MemoryBuilder / 压缩
    ├── tools/
    │   ├── mod.rs             # Tool trait + ToolRegistry + 重导出
    │   ├── tool.rs            # Tool trait 定义 + extract_string_arg
    │   ├── registry.rs        # 工具注册与分发
    │   ├── error.rs           # ToolError / FsError
    │   ├── fs.rs              # WorkspaceFs 沙箱文件系统
    │   ├── calculator.rs      # 计算器工具（递归下降解析器）
    │   ├── echo.rs            # 最小参考实现
    │   ├── tool_read.rs       # 文件读取工具
    │   ├── tool_write.rs      # 文件写入工具
    │   ├── tool_edit.rs       # 文件编辑工具
    │   ├── tool_glob.rs       # Glob 文件搜索工具
    │   ├── tool_grep.rs       # Grep 正则搜索工具
    │   └── tool_ls.rs         # 目录列表工具
    └── tui/
        ├── mod.rs             # 模块根
        ├── app.rs             # 核心状态机 + ChatMessage + apply_event
        ├── ui.rs              # ratatui 渲染（三面板布局）
        └── event.rs           # 事件循环 + Agent 桥接
```

### 核心模块

| 模块 | 职责 |
| ---- | ---- |
| `core/client/` | DeepSeek API 客户端——类型化请求/响应、SSE 流式处理 |
| `core/agent.rs` | Agent 主循环——`run_loop()` 批量模式、`run_with_events()` 流式模式、`max_steps` 保护 |
| `memory/` | 对话记忆——可配置压缩、两阶段 `drain`/`apply`、`compact_with_deepseek` 便捷函数 |
| `tools/` | 工具系统——`Tool` trait、`ToolRegistry`、`WorkspaceFs` 沙箱、7 个内置工具 |
| `tui/` | ratatui 终端界面——滚动历史、实时流式显示、工具调用状态、斜杠命令 |

### SSE 流式管道

```
HTTP chunk → buffer → find_event_end (\n\n) → trim_trailing_newlines
→ extract_sse_data (strip "data: ") → parse JSON → DeepSeekChunk
```

### 关键设计模式

- **向前兼容枚举**: `FinishReason::Other(String)` 捕获未知值，避免反序列化失败
- **两阶段压缩**: `drain_for_compact()` + `apply_compact()` 将记忆模块与 LLM 提供者解耦，System 消息永不参与压缩
- **WorkspaceFs 沙箱**: 所有文件操作工具通过 `Arc<WorkspaceFs>` 代理，`resolve()` 规范化路径并拒绝工作区外的访问
- **异步桥接 (TUI)**: TUI 事件循环在主线程同步运行，Agent 在 `tokio::spawn` 后台任务运行，通过 `mpsc::unbounded_channel` 通信

## 开发路线

### 第一阶段（已完成 ✅）

- [x] `core/client/` — DeepSeek API 客户端
- [x] `memory/` — 对话记忆 + 可配置压缩
- [x] `tools/` — Tool trait + ToolRegistry + CalculatorTool + EchoTool
- [x] `tools/fs.rs` + 文件编辑工具 — WorkspaceFs 沙箱 + 6 个文件系统工具
- [x] `core/agent.rs` — Agent 主循环 + 流式事件
- [x] `tui/` — ratatui 终端聊天界面

### 第二阶段（计划中）

- [ ] 宏编程 + `schemars` 自动生成工具 Schema
- [ ] 模板引擎与提示词管理
- [ ] 结构化输出（`response_format`）
- [ ] 增强的流式交互体验

### 第三阶段（远期）

- [ ] RAG + 向量数据库（长期记忆与知识库）
- [ ] Web 服务 + 桌面客户端（Axum / Tauri）
- [ ] 可观测性（`tracing` 全链路埋点）

## License

MIT
