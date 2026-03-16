//! Plan domain types and error handling.
//!
//! This crate provides the core domain types for plans, decisions, constraints, and goals.
//! It is the foundation for plan storage (Task 2), schema (Task 3), categories (Task 4),
//! and events (Task 5).

pub mod category;
pub mod deviation;
pub mod error;
pub mod events;
pub mod handoff;
pub mod model;
pub mod orchestrator;
pub mod planner_agent;
pub mod replan;
pub mod schema;
pub mod store;
pub mod tools;

pub use error::PlanError;
pub use model::{
    Constraint, ConstraintType, Decision, DecisionAuthor, DecisionType, Goal, GoalPriority,
    GoalStatus, Plan, PlanMode, PlanStatus,
};
pub use orchestrator::PlanOrchestrator;
pub use store::{PlanState, PlanStore};
