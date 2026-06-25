//! 工具系统的错误类型。
//!
//! 设计原则：与 `memory::MemoryError` 保持一致的风格 —
//! `#[derive(Debug, Clone)]` + 手动 `Display` + 空 `Error` impl。
//! `Clone` 是可行的，因为所有内部数据都是 `String`（不像
//! `DeepSeekError` 包裹了不可 Clone 的 `reqwest::Error`）。

use std::fmt;

/// 工具执行过程中可能产生的错误。
///
/// # 变体
///
/// | 变体 | 含义 | 示例 |
/// |------|------|------|
/// | [`Execution`](ToolError::Execution) | 工具运行时逻辑错误 | 除零、表达式语法错误 |
/// | [`InvalidArgs`](ToolError::InvalidArgs) | 参数 JSON 解析或校验失败 | 缺少必填字段、JSON 格式错误 |
#[derive(Debug, Clone, PartialEq)]
pub enum ToolError {
    /// 工具运行时错误（除零、无效表达式等）。
    Execution(String),
    /// 参数 JSON 无法解析，或缺少必填字段、字段类型错误。
    InvalidArgs(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Execution(reason) => write!(f, "tool execution error: {reason}"),
            Self::InvalidArgs(reason) => write!(f, "invalid tool arguments: {reason}"),
        }
    }
}

impl std::error::Error for ToolError {}
