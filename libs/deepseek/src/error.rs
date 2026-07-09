use provider::ProviderError;

/// DeepSeek-specific error.
#[derive(Debug)]
pub enum DeepSeekError {
    Http(reqwest::Error),
    Api { status: u16, body: String },
    Parse(String),
    StreamingNotSupported,
}

impl std::fmt::Display for DeepSeekError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(e) => write!(f, "HTTP error: {e}"),
            Self::Api { status, body } => write!(f, "API error ({status}): {body}"),
            Self::Parse(e) => write!(f, "parse error: {e}"),
            Self::StreamingNotSupported => write!(f, "streaming is not supported"),
        }
    }
}

impl std::error::Error for DeepSeekError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(e) => Some(e),
            _ => None,
        }
    }
}

impl From<DeepSeekError> for ProviderError {
    fn from(e: DeepSeekError) -> Self {
        match e {
            DeepSeekError::Http(err) => ProviderError::Http(err.to_string()),
            DeepSeekError::Api { status, body } => ProviderError::Api { status, body },
            DeepSeekError::Parse(msg) => ProviderError::Parse(msg),
            DeepSeekError::StreamingNotSupported => ProviderError::StreamingNotSupported,
        }
    }
}
