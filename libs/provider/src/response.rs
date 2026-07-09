use serde::{Deserialize, Serialize};

use crate::message::ToolCall;

/// Provider-agnostic completion response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

/// The reason the model stopped generating tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    InsufficientSystemResource,
    /// Forward-compatibility catch-all.
    Other(String),
}

impl Serialize for FinishReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let s = match self {
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ToolCalls => "tool_calls",
            Self::ContentFilter => "content_filter",
            Self::InsufficientSystemResource => "insufficient_system_resource",
            Self::Other(s) => s.as_str(),
        };
        serializer.serialize_str(s)
    }
}

impl<'de> Deserialize<'de> for FinishReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "stop" => Self::Stop,
            "length" => Self::Length,
            "tool_calls" => Self::ToolCalls,
            "content_filter" => Self::ContentFilter,
            "insufficient_system_resource" => Self::InsufficientSystemResource,
            _ => Self::Other(s),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChoiceMessage,
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChoiceMessage {
    pub role: String,
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_finish_reason_serialize() {
        assert_eq!(
            serde_json::to_value(FinishReason::Stop).unwrap(),
            json!("stop")
        );
        assert_eq!(
            serde_json::to_value(FinishReason::ToolCalls).unwrap(),
            json!("tool_calls")
        );
    }

    #[test]
    fn test_finish_reason_deserialize() {
        let fr: FinishReason = serde_json::from_str(r#""stop""#).unwrap();
        assert_eq!(fr, FinishReason::Stop);
    }

    #[test]
    fn test_finish_reason_unknown_is_other() {
        let fr: FinishReason = serde_json::from_str(r#""future_reason""#).unwrap();
        assert_eq!(fr, FinishReason::Other("future_reason".into()));
    }
}
