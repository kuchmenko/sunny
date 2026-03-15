//! Agent session and tool composition.
pub mod approval;
pub mod executor;
pub mod session;
pub mod tools;
pub use approval::{
    AlwaysAllowGate, AlwaysDenyGate, GateDecision, HumanApprovalGate, SharedApprovalGate,
};
pub use executor::{ExecutionOutcome, TaskExecutor};
pub use session::{AgentError, AgentSession};
pub use tools::{build_tool_definitions, build_tool_executor, build_tool_policy};
