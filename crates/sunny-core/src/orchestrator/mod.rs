pub mod classifier;
pub mod context;
pub mod error;
pub mod events;
pub mod executor;
pub mod handle;
pub mod intake;
pub mod intent;
pub mod interactive;
pub mod plan;
pub mod planner;
pub mod prompt_spec;
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
pub use intake::{
    ComplexityHint, IntakeAdvisor, IntakeAdvisorError, PlanHints, PlanningIntake,
    PlanningIntakeInput, PlanningIntakeVerdict, RawIntakeAdvice, WorkspaceContext,
};
pub use intent::{Intent, IntentKind, PlanPolicy};
pub use interactive::InteractiveOrchestrator;
pub use plan::{ExecutionPlan, PlanStep, StepOutcome, StepState};
pub use planner::ExecutionProfile;
pub use planner::HeuristicLoopPlanner;
pub use prompt_spec::{
    OutputFormat, PromptSpec, ToolBoundary, SPEC_EXPLORE, SPEC_LIBRARIAN, SPEC_ORCHESTRATOR,
    SPEC_PLANNER, SPEC_PLANNING_INTAKE, SPEC_VERIFICATION_CRITIQUE,
};
pub use registry::AgentRegistry;

pub use routing::{CapabilityRouter, IntentRouter, NameRouting, RoutingStrategy, TieBreakPolicy};
pub use supervision::RestartPolicy;
pub use telemetry::{DispatchTelemetry, NoopTelemetry};
