//! JSON Schema auto-generation helper for tool parameters.
//!
//! Wraps [`schemars`] to produce OpenAI/DeepSeek-compatible JSON Schema
//! from typed Rust structs. The generated schema is cached in each tool
//! and cloned on the hot path (`parameters()` is called every agent step).

use schemars::JsonSchema;
use serde_json::Value;

/// Generates an OpenAI-compatible JSON Schema `Value` from a type
/// implementing [`JsonSchema`].
///
/// # Post-processing
///
/// 1. Strips the `"$schema"` URI — not part of the OpenAI tool schema contract.
/// 2. Ensures `"additionalProperties": false` — schemars does not set this by
///    default for struct schemas, but it is best practice for tool parameters
///    (prevents the model from hallucinating extra parameters).
///
/// # Example
///
/// ```rust
/// use schemars::JsonSchema;
/// use serde::Deserialize;
/// use loomis::tools::generate_schema;
///
/// #[derive(JsonSchema)]
/// struct MyArgs {
///     #[schemars(description = "A required string field.")]
///     name: String,
///     #[schemars(description = "An optional integer.")]
///     count: Option<i32>,
/// }
///
/// let schema = generate_schema::<MyArgs>();
/// assert_eq!(schema["type"], "object");
/// assert_eq!(schema["additionalProperties"], false);
/// assert!(schema.get("$schema").is_none());
/// ```
pub fn generate_schema<T: JsonSchema>() -> Value {
    let root_schema = schemars::schema_for!(T);
    let mut value = serde_json::to_value(&root_schema)
        .expect("schemars RootSchema must always serialize to JSON");

    if let Value::Object(map) = &mut value {
        // Strip `$schema` — not part of the OpenAI tool parameters contract.
        map.remove("$schema");

        // Ensure `additionalProperties` is always false.
        // schemars does not set this for struct schemas, but it is
        // a best practice for tool parameters.
        map.entry("additionalProperties")
            .or_insert(Value::Bool(false));
    }

    value
}

#[cfg(test)]
mod tests {
    use super::*;

    use schemars::JsonSchema;
    use serde::Deserialize;

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct SimpleArgs {
        #[schemars(description = "A required string.")]
        text: String,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct OptionalArgs {
        #[schemars(description = "Required.")]
        file_path: String,

        #[schemars(description = "Optional.")]
        offset: Option<u64>,
    }

    #[test]
    fn generated_schema_has_type_object() {
        let schema = generate_schema::<SimpleArgs>();
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn generated_schema_has_additional_properties_false() {
        let schema = generate_schema::<SimpleArgs>();
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn generated_schema_strips_dollar_schema() {
        let schema = generate_schema::<SimpleArgs>();
        assert!(schema.get("$schema").is_none());
    }

    #[test]
    fn required_fields_in_properties_and_required() {
        let schema = generate_schema::<SimpleArgs>();
        assert!(schema["properties"].get("text").is_some());
        let required = &schema["required"];
        assert!(
            required
                .as_array()
                .unwrap()
                .contains(&Value::String("text".into()))
        );
    }

    #[test]
    fn optional_fields_not_in_required() {
        let schema = generate_schema::<OptionalArgs>();
        // "offset" is Option<u64>, should not be in required
        let required = &schema["required"];
        let required_names: Vec<&str> = required
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required_names.contains(&"file_path"));
        assert!(!required_names.contains(&"offset"));
    }

    #[test]
    fn optional_fields_in_properties() {
        let schema = generate_schema::<OptionalArgs>();
        assert!(schema["properties"].get("offset").is_some());
    }

    #[derive(JsonSchema, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RoundTripArgs {
        text: String,
        count: u64,
    }

    #[test]
    fn round_trip_deserialize() {
        let json = r#"{"text": "hello", "count": 42}"#;
        let args: RoundTripArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.text, "hello");
        assert_eq!(args.count, 42);
    }

    #[test]
    fn deny_unknown_fields_rejects_extra() {
        let json = r#"{"text": "hello", "count": 42, "extra": true}"#;
        let result: Result<RoundTripArgs, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
