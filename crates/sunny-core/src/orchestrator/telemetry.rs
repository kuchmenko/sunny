use std::time::Duration;

/// Trait-based telemetry hook interface for orchestrator dispatch.
///
/// All methods have default no-op implementations, so consumers only
/// override what they need. The `NoopTelemetry` zero-cost default is
/// used when no telemetry is configured.
pub trait DispatchTelemetry: Send + Sync {
    /// Called before dispatch resolution and agent send.
    fn on_dispatch_start(&self, _agent_name: &str) {}

    /// Called after a successful dispatch, with elapsed wall time.
    fn on_dispatch_success(&self, _agent_name: &str, _duration: Duration) {}

    /// Called after a failed dispatch, with error description and elapsed wall time.
    fn on_dispatch_error(&self, _agent_name: &str, _error: &str, _duration: Duration) {}
}

/// No-op telemetry implementation. Zero overhead when telemetry is not configured.
pub struct NoopTelemetry;

impl DispatchTelemetry for NoopTelemetry {}
