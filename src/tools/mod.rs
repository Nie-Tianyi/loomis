//! 工具系统 — [`Tool`] trait、工具注册表 [`ToolRegistry`] 以及示例工具实现。
//!
//! # 架构
//!
//! ```text
//! tools/
//!   mod.rs        — 模块根，公开 re-export
//!   error.rs      — ToolError（工具执行错误）
//!   tool.rs       — Tool trait（工具抽象接口）
//!   registry.rs   — ToolRegistry（工具注册与分发）
//!   calculator.rs — CalculatorTool（表达式求值工具）
//!   echo.rs       — EchoTool（回显工具，用于测试）
//! ```
//!
//! # 与 `core::client` 的集成
//!
//! | 方向 | 函数 / 方法 | 说明 |
//! |------|------------|------|
//! | Tool → API | [`tool_to_def`] / [`Tool::to_def`] | 将工具描述转为 [`ToolDef`](crate::core::client::ToolDef)，放入请求的 `tools` 字段 |
//! | API → Tool | [`ToolRegistry::execute`] | 根据模型返回的 `ToolCall` 分发执行 |
//! | Tool → Message | [`Message::tool_result`](crate::core::client::Message::tool_result) | 将执行结果包装为 `role: tool` 消息 |
//!
//! # 实现自定义工具
//!
//! 参考 [`Tool`] trait 的文档中的示例，或查看 [`EchoTool`] 的源码作为最小实现模板。

mod error;
mod tool;
mod registry;
mod calculator;
mod echo;

pub use error::ToolError;
pub use tool::{extract_string_arg, Tool};
pub use registry::{tool_to_def, ToolRegistry};
pub use calculator::CalculatorTool;
pub use echo::EchoTool;
