//! Error types for the tools system.

use std::fmt;

/// Error produced during tool execution.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolError {
    /// Tool runtime error (division by zero, invalid expression, etc.).
    Execution(String),
    /// Invalid arguments — JSON parse failure, missing required field, wrong type.
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

/// File-system operation error returned by [`WorkspaceFs`](crate::WorkspaceFs).
#[derive(Debug)]
pub enum FsError {
    PathEscapesWorkspace(String),
    FileTooLarge { path: String, size: u64, max: u64 },
    BinaryContentDetected(String),
    HiddenFileBlocked(String),
    ExtensionBlocked(String),
    NotFound(String),
    NotAFile(String),
    NotADirectory(String),
    Io(std::io::Error),
    Glob(String),
    Regex(String),
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathEscapesWorkspace(path) => {
                write!(f, "path escapes workspace: {path}")
            }
            Self::FileTooLarge { path, size, max } => {
                write!(
                    f,
                    "file too large for operation: {path} ({size} bytes, max {max})"
                )
            }
            Self::BinaryContentDetected(path) => {
                write!(f, "binary content detected, write blocked: {path}")
            }
            Self::HiddenFileBlocked(path) => {
                write!(f, "hidden file write blocked: {path}")
            }
            Self::ExtensionBlocked(path) => {
                write!(f, "file extension blocked: {path}")
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
