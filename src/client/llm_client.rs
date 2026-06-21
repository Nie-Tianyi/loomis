use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};

// ── Error ───────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum DeepSeekError {
    /// reqwest error (connection, timeout, DNS, TLS).
    Http(reqwest::Error),
    /// API returned a non-2xx status.
    Api { status: u16, body: String },
    /// Failed to deserialize the response.
    Parse(String),
    /// Streaming is not supported yet.
    StreamingNotSupported,
}

impl std::fmt::Display for DeepSeekError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "HTTP error: {e}"),
            Self::Api { status, body } => write!(f, "API error ({status}): {body}"),
            Self::Parse(e) => write!(f, "parse error: {e}"),
            Self::StreamingNotSupported => write!(f, "streaming is not yet supported"),
        }
    }
}

impl std::error::Error for DeepSeekError {}

// ── Request ─────────────────────────────────────────────────────────────────

/// Exact match of the DeepSeek `/chat/completions` request body.
#[derive(Clone, Debug, Serialize)]
pub struct DeepSeekRequest {
    pub messages: Vec<Message>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default)]
    pub logprobs: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u32>,
}

impl DeepSeekRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            messages,
            model: model.into(),
            thinking: None,
            reasoning_effort: None,
            max_tokens: None,
            response_format: None,
            stop: None,
            stream: false,
            stream_options: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            logprobs: false,
            top_logprobs: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Present when role is `assistant` and the model wants to call tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Present when role is `tool` — the id of the tool call this message responds to.
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

    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub r#type: ToolCallType,
    pub function: ToolCallFunction,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallType {
    Function,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// JSON-encoded arguments string.
    pub arguments: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Thinking {
    #[serde(rename = "type")]
    pub r#type: ThinkingType,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingType {
    Enabled,
    Disabled,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResponseFormat {
    #[serde(rename = "type")]
    pub r#type: ResponseFormatType,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormatType {
    Text,
    JsonObject,
}

/// Matches the DeepSeek `tools` array element.
#[derive(Clone, Debug, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub r#type: ToolDefType,
    pub function: FunctionDef,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolDefType {
    Function,
}

#[derive(Clone, Debug, Serialize)]
pub struct FunctionDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// Matches the DeepSeek `tool_choice` field.
#[derive(Clone, Debug)]
pub enum ToolChoice {
    /// `"none"` — never call a tool.
    None,
    /// `"auto"` — model decides.
    Auto,
    /// `"required"` — model must call a tool.
    Required,
    /// `{"type": "function", "function": {"name": "..."}}` — force a specific function.
    Specific {
        r#type: ToolDefType,
        function: ToolChoiceFunction,
    },
}

impl Serialize for ToolChoice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::None => serializer.serialize_str("none"),
            Self::Auto => serializer.serialize_str("auto"),
            Self::Required => serializer.serialize_str("required"),
            Self::Specific { r#type, function } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", r#type)?;
                map.serialize_entry("function", function)?;
                map.end()
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolChoiceFunction {
    pub name: String,
}

// ── Response ────────────────────────────────────────────────────────────────

/// Exact match of the DeepSeek `/chat/completions` response body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeepSeekResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
    #[serde(default)]
    pub system_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChoiceMessage,
    #[serde(default)]
    pub logprobs: Option<serde_json::Value>,
    pub finish_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChoiceMessage {
    pub role: String,
    pub content: Option<String>,
    /// Thinking/reasoning output from DeepSeek-R1 / V4 thinking mode.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// Present when the model emits tool calls.
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ── Client ──────────────────────────────────────────────────────────────────

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

pub struct DeepSeekClient {
    api_key: String,
    base_url: String,
    http: HttpClient,
}

impl DeepSeekClient {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            http: HttpClient::new(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Send a non-streaming chat completion request.
    ///
    /// Returns an error if `request.stream` is `true`. Use [`Self::stream`] instead.
    pub async fn send(&self, request: DeepSeekRequest) -> Result<DeepSeekResponse, DeepSeekError> {
        if request.stream {
            return Err(DeepSeekError::StreamingNotSupported);
        }

        let url = format!("{}/v1/chat/completions", self.base_url);

        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(DeepSeekError::Http)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(DeepSeekError::Api {
                status: status.as_u16(),
                body,
            });
        }

        response
            .json::<DeepSeekResponse>()
            .await
            .map_err(|e| DeepSeekError::Parse(e.to_string()))
    }

    /// Send a streaming chat completion request.
    ///
    /// The request's `stream` field is forced to `true`.
    pub async fn stream(
        &self,
        mut request: DeepSeekRequest,
    ) -> Result<DeepSeekStream, DeepSeekError> {
        request.stream = true;

        let url = format!("{}/v1/chat/completions", self.base_url);

        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(DeepSeekError::Http)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(DeepSeekError::Api {
                status: status.as_u16(),
                body,
            });
        }

        Ok(DeepSeekStream {
            response,
            buffer: Vec::new(),
            finished: false,
        })
    }
}

// ── Streaming ───────────────────────────────────────────────────────────────

/// Server-Sent Events (SSE) response stream from DeepSeek.
///
/// # What is SSE?
///
/// Server-Sent Events is a simple HTTP-based protocol for receiving real-time
/// data. Instead of sending one complete response then closing the connection,
/// the server keeps the connection open and sends multiple **events** over time.
///
/// Each event is plain text separated by a blank line (`\n\n`):
///
/// ```text
/// data: {"id":"...","choices":[{"delta":{"content":"Hello"}}]}
///
/// data: {"id":"...","choices":[{"delta":{"content":" world"}}]}
///
/// data: [DONE]
/// ```
///
/// DeepSeek uses SSE to stream chat completions. Each event carries one JSON
/// chunk — a tiny piece of the model's reply, typically a few characters.
/// The stream ends with the `data: [DONE]` sentinel.
///
/// # Usage
///
/// ```no_run
/// let mut stream: DeepSeekStream = client.stream(request).await?;
/// while let Some(chunk) = stream.next().await {
///     match chunk {
///         Ok(c) => print!("{}", c.choices[0].delta.content.as_deref().unwrap_or("")),
///         Err(e) => eprintln!("stream error: {e}"),
///     }
/// }
/// ```
pub struct DeepSeekStream {
    /// The HTTP response body. We read from it in small chunks as data arrives.
    response: reqwest::Response,
    /// Accumulates bytes read from the network that haven't yet formed a complete
    /// SSE event. SSE events are separated by `\n\n`, but network chunks can split
    /// an event mid-line, so we buffer until we see the separator.
    buffer: Vec<u8>,
    /// Set to `true` once the `data: [DONE]` sentinel is received.
    finished: bool,
}

/// A single chunk in a streaming DeepSeek response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeepSeekChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: Delta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Delta {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

// ── SSE Parsing Helpers ─────────────────────────────────────────────
//
// These functions understand the SSE wire format but are otherwise
// protocol-agnostic. They transform: raw bytes → event text → JSON string.

/// Find the end of the first complete SSE event in `buf`.
///
/// SSE separates events with a blank line — two consecutive `\n` characters.
/// This function returns the byte offset just past the first `\n\n`, so that
/// `buf[..pos]` is one complete event (including the trailing newlines).
///
/// Returns `None` if no complete event has arrived yet (need more data).
///
/// # Example
///
/// ```
/// let buf = b"data: {\"x\":1}\n\ndata: [DONE]\n\n";
/// let end = find_event_end(buf).unwrap(); // end = 15
/// assert_eq!(&buf[..end], b"data: {\"x\":1}\n\n");
/// ```
fn find_event_end(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len().saturating_sub(1) {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i + 2);
        }
    }
    None
}

/// Strip trailing `\r` and `\n` bytes from a byte slice.
///
/// After we drain an event from the buffer, the event bytes include the
/// terminating `\n\n`. This helper trims them so we get clean event text.
fn trim_trailing_newlines(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|&b| b != b'\r' && b != b'\n')
        .map_or(0, |p| p + 1);
    &bytes[..end]
}

/// Extract the JSON payload from an SSE event's `data:` lines.
///
/// The SSE spec allows a single event to span multiple `data:` lines:
/// ```text
/// data: {"key":
/// data:  "value"}
/// ```
/// This function strips the `data: ` prefix from every line and joins them.
/// DeepSeek emits one JSON object per event on a single line, but this
/// implementation handles the multi-line case correctly too.
fn extract_sse_data(event_text: &str) -> String {
    event_text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect::<Vec<_>>()
        .join("")
}

impl DeepSeekStream {
    /// Read the next raw SSE event text from the network.
    ///
    /// This is the low-level reader. It pulls HTTP chunks, accumulates them
    /// in `self.buffer`, and yields one complete event (everything up to and
    /// including the terminating `\n\n`) at a time.
    ///
    /// Returns `None` when the HTTP response body has been fully consumed.
    async fn read_event(&mut self) -> Option<Result<String, DeepSeekError>> {
        loop {
            // ── Do we already have a complete event in the buffer? ──
            if let Some(end) = find_event_end(&self.buffer) {
                // Drain the bytes for this event (including trailing \n\n).
                let event_bytes: Vec<u8> = self.buffer.drain(..end).collect();
                let trimmed = trim_trailing_newlines(&event_bytes);
                let text = String::from_utf8_lossy(trimmed).into_owned();
                return Some(Ok(text));
            }

            // ── Not yet — read another chunk from the HTTP stream ──
            // `response.chunk()` returns the next piece of the body:
            //  • `Ok(Some(bytes))` — more data arrived
            //  • `Ok(None)`        — body is done
            //  • `Err(e)`          — network error
            match self.response.chunk().await {
                Ok(Some(bytes)) => {
                    self.buffer.extend_from_slice(&bytes);
                    // Loop back to check if we now have a full event.
                }
                Ok(None) => {
                    // Connection closed. Flush any leftover data.
                    if self.buffer.is_empty() {
                        return None;
                    }
                    let remaining = std::mem::take(&mut self.buffer);
                    let trimmed = trim_trailing_newlines(&remaining);
                    let text = String::from_utf8_lossy(trimmed).into_owned();
                    return Some(Ok(text));
                }
                Err(e) => return Some(Err(DeepSeekError::Http(e))),
            }
        }
    }

    /// Return the next parsed `DeepSeekChunk`, or `None` when the stream ends.
    ///
    /// This is the main public API. It orchestrates the full pipeline:
    ///
    /// 1. **Read** raw bytes from the HTTP response until a complete SSE event
    ///    arrives (events end with `\n\n`).
    /// 2. **Extract** the JSON payload from the event's `data:` lines.
    /// 3. **Parse** the JSON into a strongly-typed `DeepSeekChunk`.
    ///
    /// Empty events (comments, keepalive pings, blank lines between events)
    /// are silently skipped. The `data: [DONE]` sentinel ends the stream.
    pub async fn next(&mut self) -> Option<Result<DeepSeekChunk, DeepSeekError>> {
        if self.finished {
            return None;
        }

        loop {
            // Step 1 & 2: read raw event + extract `data:` payload.
            let event_text = match self.read_event().await? {
                Ok(t) => t,
                Err(e) => {
                    self.finished = true;
                    return Some(Err(e));
                }
            };

            let data = extract_sse_data(&event_text);

            // Empty data → comment, keepalive, or whitespace between events.
            // Skip it and read the next event.
            if data.is_empty() {
                continue;
            }

            // The `[DONE]` sentinel means the stream is complete.
            // It's not valid JSON, so we handle it before deserialization.
            if data.trim() == "[DONE]" {
                self.finished = true;
                return None;
            }

            // Step 3: deserialize the JSON into our typed chunk.
            match serde_json::from_str::<DeepSeekChunk>(&data) {
                Ok(chunk) => return Some(Ok(chunk)),
                Err(e) => {
                    self.finished = true;
                    return Some(Err(DeepSeekError::Parse(e.to_string())));
                }
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = DeepSeekRequest::new(
            "deepseek-chat",
            vec![
                Message::new(Role::System, "You are helpful"),
                Message::new(Role::User, "Hi"),
            ],
        );
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""model":"deepseek-chat""#));
        assert!(json.contains(r#""role":"system""#));
        assert!(json.contains(r#""stream":false"#));
        assert!(json.contains(r#""logprobs":false"#));
    }

    #[test]
    fn test_request_with_thinking() {
        let req = DeepSeekRequest {
            thinking: Some(Thinking {
                r#type: ThinkingType::Enabled,
            }),
            reasoning_effort: Some(ReasoningEffort::High),
            ..DeepSeekRequest::new("deepseek-v4-pro", vec![])
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""thinking":{"type":"enabled"}"#));
        assert!(json.contains(r#""reasoning_effort":"high""#));
    }

    #[test]
    fn test_response_deserialization() {
        let raw = r#"{
            "id": "abc-123",
            "object": "chat.completion",
            "created": 1781984231,
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello!",
                    "reasoning_content": "The user said hi...",
                    "tool_calls": null
                },
                "logprobs": null,
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 56,
                "total_tokens": 66
            },
            "system_fingerprint": "fp_9954b31ca7"
        }"#;
        let resp: DeepSeekResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.id, "abc-123");
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("Hello!"));
        assert_eq!(
            resp.choices[0].message.reasoning_content.as_deref(),
            Some("The user said hi...")
        );
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(resp.usage.as_ref().unwrap().total_tokens, 66);
    }

    #[test]
    fn test_message_new() {
        let msg = Message::new(Role::User, "Hello");
        assert_eq!(msg.content, "Hello");
        assert!(matches!(msg.role, Role::User));
    }

    #[test]
    fn test_role_serialization() {
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), r#""system""#);
    }

    #[test]
    fn test_tool_choice_serialization() {
        use serde_json::json;

        assert_eq!(
            serde_json::to_value(&ToolChoice::None).unwrap(),
            json!("none")
        );
        assert_eq!(
            serde_json::to_value(&ToolChoice::Auto).unwrap(),
            json!("auto")
        );
        assert_eq!(
            serde_json::to_value(&ToolChoice::Required).unwrap(),
            json!("required")
        );
        assert_eq!(
            serde_json::to_value(&ToolChoice::Specific {
                r#type: ToolDefType::Function,
                function: ToolChoiceFunction { name: "f".into() },
            })
            .unwrap(),
            json!({"type": "function", "function": {"name": "f"}})
        );
    }

    #[test]
    fn test_chunk_deserialization() {
        // Simulates an SSE "data:" line
        let raw = r#"{
            "id": "chatcmpl-xxx",
            "object": "chat.completion.chunk",
            "created": 1781984231,
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "delta": {
                    "content": "Hello!",
                    "reasoning_content": null
                },
                "finish_reason": null
            }],
            "usage": null
        }"#;
        let chunk: DeepSeekChunk = serde_json::from_str(raw).unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello!"));
        assert_eq!(chunk.choices[0].finish_reason, None);
    }

    #[test]
    fn test_chunk_with_tool_call() {
        let raw = r#"{
            "id": "c",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"Beijing\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": null
        }"#;
        let chunk: DeepSeekChunk = serde_json::from_str(raw).unwrap();
        let tc = &chunk.choices[0].delta.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.id, "call_1");
        assert_eq!(tc.function.name, "get_weather");
        assert_eq!(tc.function.arguments, r#"{"city":"Beijing"}"#);
        assert_eq!(
            chunk.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
    }

    #[test]
    fn test_find_event_end() {
        let buf = b"data: {\"x\":1}\n\ndata: {\"y\":2}\n\n";
        let end = find_event_end(buf).unwrap();
        // First event including the \n\n separator
        assert_eq!(&buf[..end], b"data: {\"x\":1}\n\n");
    }

    #[test]
    fn test_find_event_end_no_match() {
        assert!(find_event_end(b"").is_none());
        assert!(find_event_end(b"data: partial").is_none());
        assert!(find_event_end(b"data: almost\n").is_none());
    }

    #[test]
    fn test_extract_sse_data_single_line() {
        let event = "data: {\"x\":1}";
        assert_eq!(extract_sse_data(event), "{\"x\":1}");
    }

    #[test]
    fn test_extract_sse_data_multi_line() {
        let event = "data: {\"x\":\ndata: 1}";
        assert_eq!(extract_sse_data(event), "{\"x\":1}");
    }

    #[test]
    fn test_extract_sse_data_skips_non_data_lines() {
        let event = ":comment\n:another comment\ndata: {\"x\":1}\n";
        assert_eq!(extract_sse_data(event), "{\"x\":1}");
    }

    #[test]
    fn test_trim_trailing_newlines() {
        assert_eq!(trim_trailing_newlines(b"hello\n\n"), b"hello");
        assert_eq!(trim_trailing_newlines(b"hello"), b"hello");
        assert_eq!(trim_trailing_newlines(b"\n\n"), b"");
        assert_eq!(trim_trailing_newlines(b""), b"");
    }
}
