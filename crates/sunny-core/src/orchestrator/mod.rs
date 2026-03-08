pub mod classifier;
pub mod context;
pub mod error;
pub mod events;
pub mod executor;
pub mod handle;
pub mod intent;
pub mod plan;
pub mod planner;
pub mod registry;

pub mod routing;
pub mod supervision;
pub mod telemetry;

pub use classifier::IntentClassifier;
pub use context::{PlanId, RequestId, StepId};
pub use error::{OrchestratorError, PlanError, RegistryError};
pub use events::{
    EVENT_AGENT_MESSAGE_RECEIVED, EVENT_AGENT_MESSAGE_SENT, EVENT_CLI_COMMAND_END,
    EVENT_CLI_COMMAND_START, EVENT_DISPATCH_ERROR, EVENT_DISPATCH_START, EVENT_DISPATCH_SUCCESS,
    EVENT_PLAN_COMPLETED, EVENT_PLAN_CREATED, EVENT_PLAN_ERROR, EVENT_PLAN_UPDATED,
    EVENT_ROUTE_FAILED, EVENT_ROUTE_RESOLVED, EVENT_TOOL_EXEC_END, EVENT_TOOL_EXEC_ERROR,
    EVENT_TOOL_EXEC_START, OUTCOME_CANCELLED, OUTCOME_ERROR, OUTCOME_SUCCESS, OUTCOME_TIMEOUT,
};
pub use executor::{PlanExecutor, PlanOutcome, PlanResult};
pub use handle::OrchestratorHandle;
pub use intent::{Intent, IntentKind, PlanPolicy};
pub use plan::{ExecutionPlan, PlanStep, StepOutcome, StepState};
pub use planner::ExecutionProfile;
pub use registry::AgentRegistry;

pub use routing::{CapabilityRouter, IntentRouter, NameRouting, RoutingStrategy, TieBreakPolicy};
pub use supervision::RestartPolicy;
pub use telemetry::{DispatchTelemetry, NoopTelemetry};
