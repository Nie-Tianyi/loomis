/// Provider-agnostic error type for LLM API interactions.
#[derive(Debug)]
pub enum ProviderError {
    /// Network / transport error.
    Http(String),
    /// API returned a non-2xx status.
    Api { status: u16, body: String },
    /// Failed to parse the response.
    Parse(String),
    /// Streaming is not supported.
    StreamingNotSupported,
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(msg) => write!(f, "HTTP error: {msg}"),
            Self::Api { status, body } => write!(f, "API error ({status}): {body}"),
            Self::Parse(msg) => write!(f, "parse error: {msg}"),
            Self::StreamingNotSupported => write!(f, "streaming is not supported"),
        }
    }
}

impl std::error::Error for ProviderError {}
