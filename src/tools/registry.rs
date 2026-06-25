//! [`ToolRegistry`] — 具名工具的注册表，支持注册、查找和分发执行。
//!
//! # 设计说明
//!
//! ## 为什么用 `HashMap<String, Arc<dyn Tool>>`？
//!
//! - `String` 键：以 [`Tool::name()`] 为 key，O(1) 查找。
//! - `Arc<dyn Tool>` 值：多所有者共享，tokio 任务间安全传递。
//! - `dyn Tool`：异构工具集合 — 不同类型可以共存于同一个注册表中。
//!
//! ## `execute()` 返回值设计
//!
//! 返回 `Option<Result<String, ToolError>>`：
//! - `None` — 未找到该名称的工具（由调用者决定如何处理）
//! - `Some(Ok(s))` — 工具执行成功
//! - `Some(Err(e))` — 工具执行失败
//!
//! 两层嵌套让调用者可以区分"工具不存在"和"工具执行失败"两种场景。

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::client::ToolDef;

use super::tool::Tool;
use super::ToolError;

/// 具名工具的注册表。线程安全（内部使用 `Arc<dyn Tool>`）。
///
/// # 示例
///
/// ```
/// use std::sync::Arc;
/// use agent_oxide::tools::{ToolRegistry, EchoTool};
///
/// let mut registry = ToolRegistry::new();
/// registry.register(Arc::new(EchoTool));
///
/// // 查找
/// assert!(registry.has("echo"));
///
/// // 转为 API 请求用的 ToolDef 列表
/// let defs = registry.to_tool_defs();
/// assert_eq!(defs.len(), 1);
///
/// // 分发执行
/// let result = registry.execute("echo", r#"{"text": "hello"}"#);
/// assert_eq!(result.unwrap().unwrap(), "hello");
/// ```
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// 创建一个空注册表。
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// 注册一个工具，以 [`Tool::name()`] 为键。
    ///
    /// 如果同名工具已存在，旧工具将被替换。
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_owned(), tool);
    }

    /// 按名称查找工具，返回其 `Arc` 引用。未找到则返回 `None`。
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// 检查指定名称的工具是否已注册。
    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// 返回已注册工具的数量。
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// 注册表是否为空。
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// 遍历所有已注册工具，产生 `(name, tool)` 对。
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<dyn Tool>)> {
        self.tools.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// 将所有已注册工具转为 [`ToolDef`] 列表，可直接放入
    /// [`DeepSeekRequest::tools`](crate::core::client::DeepSeekRequest::tools)。
    pub fn to_tool_defs(&self) -> Vec<ToolDef> {
        self.tools.values().map(|t| t.as_ref().to_def()).collect()
    }

    /// 按名称分发执行。
    ///
    /// - `None`：未找到名为 `name` 的工具。
    /// - `Some(Ok(result))`：工具执行成功。
    /// - `Some(Err(e))`：工具执行失败。
    pub fn execute(&self, name: &str, args: &str) -> Option<Result<String, ToolError>> {
        self.tools.get(name).map(|tool| tool.execute(args))
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 将任意 `&dyn Tool` 转为 [`ToolDef`]。
///
/// 等价于调用 [`Tool::to_def()`]，但接受 trait object 引用。
/// 当你没有 ownership 但有 `&dyn Tool` 时使用此函数。
pub fn tool_to_def(tool: &dyn Tool) -> ToolDef {
    tool.to_def()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{CalculatorTool, EchoTool};

    #[test]
    fn test_new_is_empty() {
        let r = ToolRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn test_default_is_empty() {
        let r = ToolRegistry::default();
        assert!(r.is_empty());
    }

    #[test]
    fn test_register_and_get() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(EchoTool));
        assert!(r.has("echo"));
        assert!(r.get("echo").is_some());
    }

    #[test]
    fn test_get_missing_returns_none() {
        let r = ToolRegistry::new();
        assert!(r.get("nonexistent").is_none());
        assert!(!r.has("nonexistent"));
    }

    #[test]
    fn test_len_tracks_registrations() {
        let mut r = ToolRegistry::new();
        assert_eq!(r.len(), 0);
        r.register(Arc::new(EchoTool));
        assert_eq!(r.len(), 1);
        r.register(Arc::new(CalculatorTool));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn test_is_empty_toggles() {
        let mut r = ToolRegistry::new();
        assert!(r.is_empty());
        r.register(Arc::new(EchoTool));
        assert!(!r.is_empty());
    }

    #[test]
    fn test_iter_yields_all_entries() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(EchoTool));
        r.register(Arc::new(CalculatorTool));
        let names: Vec<&str> = r.iter().map(|(n, _)| n).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"echo"));
        assert!(names.contains(&"calculator"));
    }

    #[test]
    fn test_register_replaces_existing() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(EchoTool));
        r.register(Arc::new(EchoTool)); // 同名替换
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn test_execute_success() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(EchoTool));
        let result = r.execute("echo", r#"{"text": "hi"}"#);
        let Some(Ok(output)) = result else {
            panic!("expected Some(Ok(_)), got {result:?}");
        };
        assert_eq!(output, "hi");
    }

    #[test]
    fn test_execute_missing_tool() {
        let r = ToolRegistry::new();
        assert!(r.execute("echo", r#"{"text": "hi"}"#).is_none());
    }

    #[test]
    fn test_execute_tool_error() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(CalculatorTool));
        let result = r.execute("calculator", r#"{"expression": "1/0"}"#);
        let Some(Err(_)) = result else {
            panic!("expected Some(Err(_)), got {result:?}");
        };
    }

    #[test]
    fn test_to_tool_defs_count_and_names() {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(EchoTool));
        r.register(Arc::new(CalculatorTool));
        let defs = r.to_tool_defs();
        assert_eq!(defs.len(), 2);
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(names.contains(&"echo"));
        assert!(names.contains(&"calculator"));
    }

    #[test]
    fn test_tool_to_def_free_function() {
        let tool = EchoTool;
        let def = tool_to_def(&tool);
        assert_eq!(def.function.name, "echo");
        assert_eq!(
            def.function.description.as_deref(),
            Some(EchoTool.description())
        );
        assert!(def.function.parameters.is_some());
    }

    #[test]
    fn test_tool_to_def_equals_to_def_method() {
        let tool = CalculatorTool;
        let from_fn = tool_to_def(&tool);
        let from_method = tool.to_def();
        assert_eq!(from_fn.function.name, from_method.function.name);
        assert_eq!(
            from_fn.function.description,
            from_method.function.description
        );
    }
}
