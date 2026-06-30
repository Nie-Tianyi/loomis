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

// ── FsError ─────────────────────────────────────────────────────────────────

/// 文件系统操作错误。
///
/// 由 [`WorkspaceFs`](crate::tools::WorkspaceFs) 方法返回，
/// 在工具 `execute()` 中转换为 [`ToolError`]。
///
/// # 与 `ToolError` 的关系
///
/// `FsError` 是底层 I/O 错误；`ToolError` 是 LLM 可见的错误。
/// 工具实现负责将 `FsError` 映射为合适的 `ToolError` 变体。
#[derive(Debug)]
pub enum FsError {
    /// 路径解析后超出了 workspace 根目录。
    PathEscapesWorkspace(String),
    /// 文件或目录不存在。
    NotFound(String),
    /// 期望文件，但路径指向目录。
    NotAFile(String),
    /// 期望目录，但路径指向文件。
    NotADirectory(String),
    /// 底层 I/O 错误。
    Io(std::io::Error),
    /// glob 模式解析失败。
    Glob(String),
    /// 正则表达式编译失败。
    Regex(String),
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathEscapesWorkspace(path) => {
                write!(f, "path escapes workspace: {path}")
            }
            Self::NotFound(path) => write!(f, "not found: {path}"),
            Self::NotAFile(path) => write!(f, "not a file: {path}"),
            Self::NotADirectory(path) => write!(f, "not a directory: {path}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Glob(msg) => write!(f, "glob error: {msg}"),
            Self::Regex(msg) => write!(f, "regex error: {msg}"),
        }
    }
}

impl std::error::Error for FsError {}

impl From<std::io::Error> for FsError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<glob::PatternError> for FsError {
    fn from(e: glob::PatternError) -> Self {
        Self::Glob(e.to_string())
    }
}

impl From<regex::Error> for FsError {
    fn from(e: regex::Error) -> Self {
        Self::Regex(e.to_string())
    }
}
