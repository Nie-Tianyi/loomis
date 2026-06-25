//! [`Tool`] trait — 工具系统的核心抽象。
//!
//! # 设计说明
//!
//! ## 为什么是 sync？
//!
//! 当前 trait 的方法是同步的（`fn execute` 而非 `async fn execute`）。
//! 这是有意为之：
//!
//! - **示例工具是 CPU 密集型的**（表达式求值、字符串操作），无需 async。
//! - **对象安全** — 同步 trait 天然支持 `dyn Tool`，无需 `async_trait` 宏。
//! - **可扩展** — 如果将来需要网络调用等 async 操作，可以在 agent loop 中
//!   用 `tokio::task::spawn_blocking` 包裹，或将 trait 演进为 async。
//!
//! ## 为什么是 `Send + Sync`？
//!
//! [`ToolRegistry`] 将工具存储为 `Arc<dyn Tool>`，在多个 tokio 任务间共享。
//! `Send + Sync` 保证跨线程安全。
//!
//! ## JSON Schema 手动构建
//!
//! `parameters()` 返回 [`serde_json::Value`]，用 [`serde_json::json!`] 宏手动拼接。
//! 这避免了引入 `schemars` 等重量级依赖，适合教学目的。

use serde_json::Value;

use super::ToolError;

/// 一个可供 LLM 调用的工具。
///
/// # 必须实现的方法
///
/// | 方法 | 用途 |
/// |------|------|
/// | [`name`](Tool::name) | 工具名称，对应 API 请求中的 `function.name` |
/// | [`description`](Tool::description) | 工具描述，帮助模型决定何时调用 |
/// | [`parameters`](Tool::parameters) | JSON Schema 参数定义（手动构建） |
/// | [`execute`](Tool::execute) | 执行工具逻辑，接收 JSON 参数字符串，返回结果字符串 |
///
/// # 可选方法
///
/// | 方法 | 默认实现 |
/// |------|---------|
/// | [`to_def`](Tool::to_def) | 将 `self` 转换为 API 请求用的 `ToolDef` |
pub trait Tool: Send + Sync {
    /// 工具名称 — 用作 API 请求中 `function.name` 的值。
    fn name(&self) -> &str;

    /// 人类可读的工具描述，展示给模型以帮助其决定何时调用。
    fn description(&self) -> &str;

    /// 描述工具所期望参数的 JSON Schema。
    ///
    /// 必须用 [`serde_json::json!`] 手动构建。无参工具返回
    /// `json!({"type": "object", "properties": {}})` 即可。
    fn parameters(&self) -> Value;

    /// 使用给定的 JSON 编码参数字符串执行工具逻辑。
    ///
    /// 成功时返回结果字符串，失败时返回 [`ToolError`]。
    fn execute(&self, args: &str) -> Result<String, ToolError>;

    /// 将当前工具转换为可直接放入 API 请求的
    /// [`ToolDef`](crate::core::client::ToolDef)。
    ///
    /// 此方法有默认实现，通常无需覆盖。
    fn to_def(&self) -> crate::core::client::ToolDef {
        crate::core::client::ToolDef {
            r#type: crate::core::client::ToolDefType::Function,
            function: crate::core::client::FunctionDef {
                name: self.name().to_owned(),
                description: Some(self.description().to_owned()),
                parameters: Some(self.parameters()),
            },
        }
    }
}

/// 从 JSON 参数字符串中提取指定名称的字符串字段。
///
/// 这是工具实现中最常见的模式 — 解析 JSON → 取字段 → 校验类型。
/// 提取此辅助函数可以减少模板代码。
///
/// # 示例
///
/// ```
/// use agent_oxide::tools::extract_string_arg;
///
/// let text = extract_string_arg(r#"{"text": "hello"}"#, "text").unwrap();
/// assert_eq!(text, "hello");
/// ```
pub fn extract_string_arg(args: &str, field: &str) -> Result<String, ToolError> {
    let v: Value = serde_json::from_str(args)
        .map_err(|e| ToolError::InvalidArgs(format!("invalid JSON: {e}")))?;

    v.get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| ToolError::InvalidArgs(format!("missing '{field}' field")))
}
