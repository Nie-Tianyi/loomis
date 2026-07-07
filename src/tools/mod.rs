//! 工具系统 — [`Tool`] trait、工具注册表 [`ToolRegistry`] 以及内置工具实现。
//!
//! # 架构
//!
//! ```text
//! tools/
//!   mod.rs        — 模块根，公开 re-export
//!   error.rs      — ToolError / FsError（工具执行错误）
//!   tool.rs       — Tool trait（工具抽象接口）
//!   registry.rs   — ToolRegistry（工具注册与分发）
//!   fs.rs         — WorkspaceFs（沙箱文件系统操作）
//!   calculator.rs — CalculatorTool（表达式求值工具）
//!   echo.rs       — EchoTool（回显工具，用于测试）
//!   tool_read.rs  — ReadTool（文件读取）
//!   tool_write.rs — WriteTool（文件写入）
//!   tool_edit.rs  — EditTool（行级编辑）
//!   tool_glob.rs  — GlobTool（文件模式匹配）
//!   tool_grep.rs  — GrepTool（内容搜索）
//!   tool_ls.rs    — LsTool（目录列表）
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

mod calculator;
mod echo;
mod error;
mod registry;
mod tool;

// File-editing tools
mod fs;
mod tool_edit;
mod tool_glob;
mod tool_grep;
mod tool_ls;
mod tool_read;
mod tool_write;

pub use calculator::CalculatorTool;
pub use echo::EchoTool;
pub use error::{FsError, ToolError};
pub use registry::{ToolRegistry, tool_to_def};
pub use tool::{Tool, extract_string_arg};

pub use fs::{DirEntry, EntryType, GrepMatch, WorkspaceFs};
pub use tool_edit::EditTool;
pub use tool_glob::GlobTool;
pub use tool_grep::GrepTool;
pub use tool_ls::LsTool;
pub use tool_read::ReadTool;
pub use tool_write::WriteTool;
