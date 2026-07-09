use serde::{Serialize, Serializer};

/// A tool definition sent to the LLM as part of the request.
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

/// Controls how the model uses tools.
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
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_tool_choice_serialization() {
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
}
