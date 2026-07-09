//! # DeepSeek — API client
//!
//! Concrete implementation of [`provider::LLMClient`] for the DeepSeek API.
//! Includes SSE streaming support and DeepSeek-specific request/response types.

#![allow(async_fn_in_trait)]

mod client;
mod error;
mod request;
mod response;
mod stream;

pub use client::DeepSeekClient;
pub use error::DeepSeekError;
pub use request::{
    DeepSeekRequest, ReasoningEffort, ResponseFormat, ResponseFormatType, Thinking, ThinkingType,
};
pub use stream::DeepSeekStream;
