use async_trait::async_trait;
use futures_util::stream::BoxStream;

use crate::error::ProviderError;
use crate::request::CompletionRequest;
use crate::response::CompletionResponse;
use crate::stream::StreamChunk;

/// Abstraction over an LLM provider.
///
/// Implementations handle provider-specific HTTP details, authentication,
/// and wire-protocol parsing. The rest of the agent framework only depends
/// on this trait — never on a concrete provider type.
#[async_trait]
pub trait LLMClient: Send + Sync {
    /// Send a non-streaming completion request.
    async fn generate(
        &self,
        req: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError>;

    /// Send a streaming completion request.
    ///
    /// Returns a [`BoxStream`] that yields chunks as they arrive.
    /// The `'static` lifetime means the stream owns all its data
    /// and does not borrow from `&self`.
    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, ProviderError>>, ProviderError>;
}
