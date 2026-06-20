use serde::{Deserialize, Serialize};
use std::fmt;

// ── Message / Role ──────────────────────────────────────────────────────────

/// A single chat message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Tool calls emitted by an assistant message (only when `role` is [`Role::Assistant`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// The tool call this message responds to (only when `role` is [`Role::Tool`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Convenience: create an assistant message containing tool calls.
    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    /// Convenience: create a tool-result message.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// An LLM function/tool call.
///
/// Standard across OpenAI, Anthropic, and compatible APIs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned identifier for this call.
    pub id: String,
    /// Function (tool) name.
    #[serde(alias = "function")]
    pub name: String,
    /// JSON-encoded arguments.
    pub arguments: String,
}

// ── Request / Response / Error ──────────────────────────────────────────────

/// Definition of a tool (function) available for the model to call.
///
/// Sent in [`LlmRequest::tools`] to declare what functions the model may invoke.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDef {
    /// Always `"function"` for current LLM APIs.
    #[serde(rename = "type")]
    pub r#type: String,
    /// The function descriptor.
    pub function: FunctionDef,
}

/// Descriptor for a single function within a [`ToolDef`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionDef {
    /// Function name (a-z, A-Z, 0-9, underscores, hyphens; max 64 chars).
    pub name: String,
    /// Natural-language description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema describing the function's input parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    /// Whether the API should enforce strict schema adherence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// Controls how the model selects tools from [`LlmRequest::tools`].
///
/// Supported by OpenAI, Anthropic, Gemini, DeepSeek, Ollama, Mistral.
#[derive(Clone, Debug)]
pub enum ToolChoice {
    /// Don't call any tools, even if available.
    None,
    /// Model decides whether to call a tool (default when `tools` is set).
    Auto,
    /// Model must call at least one tool.
    Required,
    /// Force a specific tool by name.
    Specific { name: String },
}

impl Serialize for ToolChoice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::None => serializer.serialize_str("none"),
            Self::Auto => serializer.serialize_str("auto"),
            Self::Required => serializer.serialize_str("required"),
            Self::Specific { name } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "function")?;
                map.serialize_entry("function", &ToolChoiceFunction { name: name.clone() })?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ToolChoice {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        match &value {
            // String form: "none", "auto", "required"
            serde_json::Value::String(s) => match s.as_str() {
                "none" => Ok(Self::None),
                "auto" => Ok(Self::Auto),
                "required" => Ok(Self::Required),
                other => Err(Error::custom(format!("unknown tool_choice: {other}"))),
            },
            // Object form: {"type": "function", "function": {"name": "..."}}
            serde_json::Value::Object(_) => {
                let specific: ToolChoiceObject =
                    serde_json::from_value(value.clone()).map_err(Error::custom)?;
                Ok(Self::Specific {
                    name: specific.function.name,
                })
            }
            _ => Err(Error::custom("tool_choice must be a string or object")),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ToolChoiceObject {
    #[serde(rename = "type")]
    r#type: String,
    function: ToolChoiceFunction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolChoiceFunction {
    pub name: String,
}

// ── Request / Response / Error ──────────────────────────────────────────────

/// Provider-agnostic request to an LLM.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmRequest {
    /// Conversation messages.
    pub messages: Vec<Message>,
    /// Model override.
    pub model: Option<String>,
    /// Whether to use server-sent events instead of a single JSON response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Available tools (functions) the model may call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    /// Controls which tool (if any) the model calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Sampling temperature (0.0–2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Maximum tokens to generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Core sampling (nucleus sampling).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Stop sequences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

/// Reason the model stopped generating.
///
/// Standard across all major providers (OpenAI, Anthropic, Gemini, DeepSeek, etc.).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Natural stop or stop sequence matched.
    Stop,
    /// Reached the maximum token limit.
    Length,
    /// Model is returning tool calls.
    ToolCalls,
    /// Output blocked by content filter.
    ContentFilter,
}

/// Provider-agnostic response from an LLM.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmResponse {
    /// The generated text (may be empty when the model emits a tool call).
    pub content: String,
    /// Which model produced this response.
    pub model: String,
    /// Why the model stopped.
    pub finish_reason: FinishReason,
    /// Tool calls emitted by the model, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Token usage, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Errors that can occur when communicating with an LLM provider.
#[derive(Debug)]
pub enum LlmError {
    /// Connection, timeout, DNS, etc.
    Http(String),
    /// Provider returned a non-2xx status.
    Api { status: u16, body: String },
    /// Wrong or expired credentials.
    Unauthorized,
    /// Rate limit exceeded.
    RateLimited,
    /// Failed to deserialize the response.
    Parse(String),
}

impl fmt::Display for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(msg) => write!(f, "HTTP error: {msg}"),
            Self::Api { status, body } => write!(f, "API error ({status}): {body}"),
            Self::Unauthorized => write!(f, "unauthorized"),
            Self::RateLimited => write!(f, "rate limited"),
            Self::Parse(msg) => write!(f, "parse error: {msg}"),
        }
    }
}

impl std::error::Error for LlmError {}

// ── Trait ───────────────────────────────────────────────────────────────────

/// Trait for LLM clients — one implementation per provider.
///
/// Each provider (OpenAI, Anthropic, Ollama, …) maps [`LlmRequest`] to its
/// native wire format and parses the raw response back into [`LlmResponse`].
///
/// # Example
///
/// ```ignore
/// struct OpenAiClient { api_key: String, client: reqwest::Client }
///
/// impl LlmClient for OpenAiClient {
///     async fn send(&self, request: LlmRequest) -> Result<LlmResponse, LlmError> {
///         // 1. convert LlmRequest → OpenAI chat-completion JSON
///         // 2. POST to https://api.openai.com/v1/chat/completions
///         // 3. parse the response into LlmResponse
///         todo!()
///     }
/// }
/// ```
///
/// The trait uses native `async fn` (stable in Rust 2024 edition). If you need
/// `dyn LlmClient` for dynamic dispatch, add `async-trait` to `Cargo.toml` and
/// annotate with `#[async_trait]`.
pub trait LlmClient {
    /// Send a request to the LLM and return the response.
    async fn send(&self, request: LlmRequest) -> Result<LlmResponse, LlmError>;
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_new() {
        let msg = Message::new(Role::User, "Hello");
        assert_eq!(msg.content, "Hello");
        assert!(matches!(msg.role, Role::User));
    }

    #[test]
    fn test_role_serialization() {
        let json = serde_json::to_string(&Role::System).unwrap();
        assert_eq!(json, "\"system\"");
    }

    #[test]
    fn test_llm_error_display() {
        let err = LlmError::Unauthorized;
        assert_eq!(err.to_string(), "unauthorized");

        let err = LlmError::Api {
            status: 429,
            body: "too many requests".into(),
        };
        assert!(err.to_string().contains("429"));
    }

    #[test]
    fn test_tool_choice_serialization() {
        // String variants
        assert_eq!(
            serde_json::to_string(&ToolChoice::None).unwrap(),
            r#""none""#
        );
        assert_eq!(
            serde_json::to_string(&ToolChoice::Auto).unwrap(),
            r#""auto""#
        );
        assert_eq!(
            serde_json::to_string(&ToolChoice::Required).unwrap(),
            r#""required""#
        );

        // Specific tool
        let specific = ToolChoice::Specific {
            name: "get_weather".into(),
        };
        let json = serde_json::to_string(&specific).unwrap();
        assert!(json.contains(r#""type":"function""#));
        assert!(json.contains(r#""name":"get_weather""#));
    }

    #[test]
    fn test_tool_choice_deserialization() {
        // From string
        let tc: ToolChoice = serde_json::from_str(r#""auto""#).unwrap();
        assert!(matches!(tc, ToolChoice::Auto));

        // From object
        let tc: ToolChoice =
            serde_json::from_str(r#"{"type":"function","function":{"name":"f"}}"#).unwrap();
        assert!(matches!(tc, ToolChoice::Specific { .. }));
    }

    #[test]
    fn test_llm_request_with_tools() {
        let req = LlmRequest {
            messages: vec![Message::new(Role::User, "hi")],
            model: None,
            stream: Some(false),
            tools: Some(vec![ToolDef {
                r#type: "function".into(),
                function: FunctionDef {
                    name: "get_weather".into(),
                    description: Some("Get the weather".into()),
                    parameters: None,
                    strict: None,
                },
            }]),
            tool_choice: Some(ToolChoice::Required),
            temperature: None,
            max_tokens: None,
            top_p: None,
            stop: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""tool_choice":"required""#));
        assert!(json.contains(r#""stream":false"#));
        assert!(json.contains(r#""tools":"#));
    }

    #[test]
    fn test_finish_reason_serialization() {
        assert_eq!(
            serde_json::to_string(&FinishReason::Stop).unwrap(),
            r#""stop""#
        );
        assert_eq!(
            serde_json::to_string(&FinishReason::Length).unwrap(),
            r#""length""#
        );
        assert_eq!(
            serde_json::to_string(&FinishReason::ToolCalls).unwrap(),
            r#""tool_calls""#
        );
        assert_eq!(
            serde_json::to_string(&FinishReason::ContentFilter).unwrap(),
            r#""content_filter""#
        );

        // Round-trip
        let fr: FinishReason = serde_json::from_str(r#""stop""#).unwrap();
        assert!(matches!(fr, FinishReason::Stop));
    }
}
