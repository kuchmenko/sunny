pub mod capability_store;
pub mod config;
pub mod error;
pub mod model;
pub mod policy;
pub mod scheduler;
pub mod session;
pub mod store;
pub mod system_prompt;
pub mod verifier;
pub mod workspace;

pub use capability_store::CapabilityStore;
pub use config::UserConfig;
pub use error::TaskError;
pub use model::{
    capability_info, is_hard_blocked, AcceptCriteria, CapabilityInfo, CapabilityRequest,
    CapabilityRequestStatus, CapabilityRisk, CapabilityScope, CreateAcceptCriteriaInput,
    CreateTaskInput, CreateVerifyCommandInput, HumanQuestion, Task, TaskPathClaim, TaskStatus,
    VerifyCommand, Workspace, CAPABILITY_REGISTRY, HARD_BLOCKED_CAPABILITIES,
};
pub use policy::{CapabilityPolicyEntry, PolicyFile};
pub use scheduler::{TaskReadyEvent, TaskScheduler};
pub use session::{TaskOutcome, TaskSession};
pub use store::TaskStore;
pub use system_prompt::{
    CompletedDepResult, SiblingTask, SystemPromptBuilder, TaskPromptContext, WorkspaceSnapshot,
};
pub use verifier::{AcceptanceCriteriaVerifier, VerificationReport, VerificationResult};
pub use workspace::WorkspaceDetector;
