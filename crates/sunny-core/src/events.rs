//! Event taxonomy constants for tool observability.

pub const EVENT_TOOL_EXEC_START: &str = "tool.exec.start";
pub const EVENT_TOOL_EXEC_END: &str = "tool.exec.end";
pub const EVENT_TOOL_EXEC_ERROR: &str = "tool.exec.error";
pub const EVENT_TOOL_EXEC_DEPTH: &str = "tool.exec.depth";
pub const EVENT_TOOL_EXEC_TIMEOUT: &str = "tool.exec.timeout";
pub const EVENT_TOOL_CANCELLED: &str = "tool.exec.cancelled";
pub const OUTCOME_SUCCESS: &str = "success";
pub const OUTCOME_ERROR: &str = "error";
