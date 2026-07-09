//! [`Tool`] trait — the core abstraction for LLM-callable tools.
//!
//! ## Why sync?
//!
//! The trait methods are synchronous (`fn execute`, not `async fn execute`).
//! This is intentional:
//!
//! - **Object-safe** — sync traits naturally support `dyn Tool` without `async_trait`.
//! - **All current tools are CPU-bound** (expression eval, string ops, file I/O).
//! - **Extensible** — if async tools are needed later, wrap in `spawn_blocking`
//!   or add an `AsyncTool` sub-trait.

use serde_json::Value;

use super::ToolError;

/// A tool that can be called by an LLM.
///
/// # Required methods
///
/// | Method | Purpose |
/// |--------|---------|
/// | [`name`](Tool::name) | Tool name, maps to `function.name` in API requests |
/// | [`description`](Tool::description) | Human-readable description for the model |
/// | [`parameters`](Tool::parameters) | JSON Schema for the tool's arguments |
/// | [`execute`](Tool::execute) | Execute the tool, returns result string or error |
pub trait Tool: Send + Sync {
    /// Tool name — used as `function.name` in API requests.
    fn name(&self) -> &str;

    /// Human-readable description shown to the model.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's expected arguments.
    fn parameters(&self) -> Value;

    /// Execute the tool with JSON-encoded argument string.
    fn execute(&self, args: &str) -> Result<String, ToolError>;

    /// Convert to a [`provider::ToolDef`] for API requests.
    fn to_def(&self) -> provider::ToolDef {
        provider::ToolDef {
            r#type: provider::ToolDefType::Function,
            function: provider::FunctionDef {
                name: self.name().to_owned(),
                description: Some(self.description().to_owned()),
                parameters: Some(self.parameters()),
            },
        }
    }
}
