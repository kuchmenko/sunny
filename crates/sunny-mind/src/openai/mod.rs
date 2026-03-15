//! OpenAI provider for sunny-mind.
//!
//! Implements the [`LlmProvider`] trait for the OpenAI API
//! (`api.openai.com/v1/chat/completions`).

pub(crate) mod credentials;
pub mod oauth;
pub mod provider;
pub(crate) mod stream_parser;

pub use provider::OpenAiProvider;
