#[derive(thiserror::Error, Debug)]
pub enum LlmError {
    #[error("authentication failed: {message}")]
    AuthFailed { message: String },

    #[error("request timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    #[error("rate limited by provider")]
    RateLimited,

    #[error("invalid response from provider: {message}")]
    InvalidResponse { message: String },

    #[error("transport error: {source}")]
    Transport {
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("provider not configured: {message}")]
    NotConfigured { message: String },

    #[error("unsupported kimi auth mode: {mode} (expected 'api' or 'coding_plan')")]
    UnsupportedAuthMode { mode: String },
}
