//! [`EchoTool`] — 最简单的工具实现，将输入原样返回。
//!
//! 主要用于：
//! - 测试 tool-use 循环的端到端连通性
//! - 作为实现自定义工具的最小参考示例
//!
//! # 作为参考实现
//!
//! 如果你要写自己的工具，从复制这个文件开始，然后：
//! 1. 定义 args struct（#[derive(JsonSchema, Deserialize)]）
//! 2. 修改 `name()`、`description()` 的定义
//! 3. 在构造函数中调用 `generate_schema::<YourArgs>()`
//! 4. 在 `execute()` 中反序列化 args 并实现业务逻辑
//! 5. 返回 `Ok(result_string)` 或 `Err(ToolError::...)`

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::ToolError;
use super::schema::generate_schema;
use super::tool::Tool;

/// Echo 工具的参数。
#[derive(JsonSchema, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EchoArgs {
    /// The text to echo back verbatim. Only useful for testing the tool-call pipeline.
    #[schemars(
        description = "The text to echo back verbatim. Only useful for testing the tool-call pipeline."
    )]
    pub text: String,
}

/// 将输入文本原样返回的工具。
///
/// # 参数
///
/// ```json
/// {"text": "要回显的文本"}
/// ```
///
/// # 示例
///
/// ```
/// use agent_oxide::tools::EchoTool;
/// use agent_oxide::tools::Tool;
///
/// let tool = EchoTool::new();
/// assert_eq!(tool.name(), "echo");
/// let result = tool.execute(r#"{"text": "hello"}"#).unwrap();
/// assert_eq!(result, "hello");
/// ```
pub struct EchoTool {
    schema: Value,
}

impl EchoTool {
    pub fn new() -> Self {
        Self {
            schema: generate_schema::<EchoArgs>(),
        }
    }
}

impl Default for EchoTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echo the input text back unchanged. This is a no-op tool that exists solely \
         for testing and debugging the agent's tool-dispatch loop.\n\n\
         IMPORTANT: This tool has no practical use for real work. Do NOT call it in \
         normal conversation — it does nothing useful.\n\n\
         When to use: ONLY for verifying that the tool-call pipeline works correctly \
         during development or debugging.\n\n\
         When NOT to use: any actual task or user request."
    }

    fn parameters(&self) -> Value {
        self.schema.clone()
    }

    fn execute(&self, args: &str) -> Result<String, ToolError> {
        let args: EchoArgs = serde_json::from_str(args)
            .map_err(|e| ToolError::InvalidArgs(format!("invalid args: {e}")))?;
        Ok(args.text)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        assert_eq!(EchoTool::new().name(), "echo");
    }

    #[test]
    fn test_description() {
        assert!(EchoTool::new().description().contains("no-op"));
    }

    #[test]
    fn test_parameters_schema() {
        let params = EchoTool::new().parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["text"]["type"] == "string");
        assert_eq!(params["additionalProperties"], false);
    }

    #[test]
    fn test_execute_returns_text() {
        let result = EchoTool::new()
            .execute(r#"{"text": "hello world"}"#)
            .unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_execute_empty_string() {
        let result = EchoTool::new().execute(r#"{"text": ""}"#).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_execute_missing_field() {
        let err = EchoTool::new().execute(r#"{"wrong": "x"}"#).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_execute_bad_json() {
        let err = EchoTool::new().execute("garbage").unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_execute_extra_field_rejected() {
        // deny_unknown_fields should reject extra fields
        let err = EchoTool::new()
            .execute(r#"{"text": "hello", "extra": true}"#)
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn test_to_def_matches() {
        let def = EchoTool::new().to_def();
        assert_eq!(def.function.name, "echo");
        assert!(def.function.description.is_some());
        assert!(def.function.parameters.is_some());
    }
}
