//! Event taxonomy constants for orchestrator observability.
//!
//! Defines canonical event names and outcome constants used throughout
//! the orchestrator for structured logging and tracing. All event names
//! follow dotted naming convention compatible with `tracing::info!` spans.

pub const EVENT_TOOL_EXEC_START: &str = "tool.exec.start";
pub const EVENT_TOOL_EXEC_END: &str = "tool.exec.end";
pub const EVENT_TOOL_EXEC_ERROR: &str = "tool.exec.error";

pub const EVENT_DISPATCH_START: &str = "orchestrator.dispatch.start";
pub const EVENT_DISPATCH_SUCCESS: &str = "orchestrator.dispatch.success";
pub const EVENT_DISPATCH_ERROR: &str = "orchestrator.dispatch.error";

pub const EVENT_PLAN_CREATED: &str = "orchestrator.plan.created";
pub const EVENT_PLAN_UPDATED: &str = "orchestrator.plan.updated";
pub const EVENT_PLAN_COMPLETED: &str = "orchestrator.plan.completed";
pub const EVENT_PLAN_ERROR: &str = "orchestrator.plan.error";

pub const EVENT_ROUTE_RESOLVED: &str = "orchestrator.route.resolved";
pub const EVENT_ROUTE_FAILED: &str = "orchestrator.route.failed";

pub const EVENT_AGENT_MESSAGE_SENT: &str = "agent.message.sent";
pub const EVENT_AGENT_MESSAGE_RECEIVED: &str = "agent.message.received";
pub const EVENT_AGENT_MESSAGE_START: &str = "agent.message.start";
pub const EVENT_AGENT_MESSAGE_END: &str = "agent.message.end";
pub const EVENT_AGENT_MESSAGE_ERROR: &str = "agent.message.error";

pub const EVENT_CLI_COMMAND_START: &str = "cli.command.start";
pub const EVENT_CLI_COMMAND_END: &str = "cli.command.end";

pub const OUTCOME_SUCCESS: &str = "success";
pub const OUTCOME_ERROR: &str = "error";
pub const OUTCOME_TIMEOUT: &str = "timeout";
pub const OUTCOME_CANCELLED: &str = "cancelled";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_constants_exist() {
        assert!(!EVENT_TOOL_EXEC_START.is_empty());
        assert!(!EVENT_TOOL_EXEC_END.is_empty());
        assert!(!EVENT_TOOL_EXEC_ERROR.is_empty());
        assert!(!EVENT_DISPATCH_START.is_empty());
        assert!(!EVENT_DISPATCH_SUCCESS.is_empty());
        assert!(!EVENT_DISPATCH_ERROR.is_empty());
        assert!(!EVENT_PLAN_CREATED.is_empty());
        assert!(!EVENT_PLAN_UPDATED.is_empty());
        assert!(!EVENT_PLAN_COMPLETED.is_empty());
        assert!(!EVENT_PLAN_ERROR.is_empty());
        assert!(!EVENT_ROUTE_RESOLVED.is_empty());
        assert!(!EVENT_ROUTE_FAILED.is_empty());
        assert!(!EVENT_AGENT_MESSAGE_SENT.is_empty());
        assert!(!EVENT_AGENT_MESSAGE_RECEIVED.is_empty());
        assert!(!EVENT_AGENT_MESSAGE_START.is_empty());
        assert!(!EVENT_AGENT_MESSAGE_END.is_empty());
        assert!(!EVENT_AGENT_MESSAGE_ERROR.is_empty());
        assert!(!EVENT_CLI_COMMAND_START.is_empty());
        assert!(!EVENT_CLI_COMMAND_END.is_empty());
    }

    #[test]
    fn test_outcome_constants_exist() {
        assert!(!OUTCOME_SUCCESS.is_empty());
        assert!(!OUTCOME_ERROR.is_empty());
        assert!(!OUTCOME_TIMEOUT.is_empty());
        assert!(!OUTCOME_CANCELLED.is_empty());
    }

    #[test]
    fn test_event_naming_convention() {
        let events = vec![
            EVENT_TOOL_EXEC_START,
            EVENT_TOOL_EXEC_END,
            EVENT_TOOL_EXEC_ERROR,
            EVENT_DISPATCH_START,
            EVENT_DISPATCH_SUCCESS,
            EVENT_DISPATCH_ERROR,
            EVENT_PLAN_CREATED,
            EVENT_PLAN_UPDATED,
            EVENT_PLAN_COMPLETED,
            EVENT_PLAN_ERROR,
            EVENT_ROUTE_RESOLVED,
            EVENT_ROUTE_FAILED,
            EVENT_AGENT_MESSAGE_SENT,
            EVENT_AGENT_MESSAGE_RECEIVED,
            EVENT_AGENT_MESSAGE_START,
            EVENT_AGENT_MESSAGE_END,
            EVENT_AGENT_MESSAGE_ERROR,
            EVENT_CLI_COMMAND_START,
            EVENT_CLI_COMMAND_END,
        ];

        for event in events {
            assert!(
                event.contains('.'),
                "Event '{}' must follow dotted naming convention",
                event
            );
            assert!(
                !event.contains('_'),
                "Event '{}' must use dots, not underscores",
                event
            );
        }
    }

    #[test]
    fn test_outcome_naming_convention() {
        let outcomes = vec![
            OUTCOME_SUCCESS,
            OUTCOME_ERROR,
            OUTCOME_TIMEOUT,
            OUTCOME_CANCELLED,
        ];

        for outcome in outcomes {
            assert!(
                !outcome.contains('_'),
                "Outcome '{}' must use lowercase without underscores",
                outcome
            );
        }
    }

    #[test]
    fn test_canonical_route_event_values() {
        // These values are part of the public observability contract.
        // Changing them is a breaking change.
        assert_eq!(EVENT_ROUTE_RESOLVED, "orchestrator.route.resolved");
        assert_eq!(EVENT_ROUTE_FAILED, "orchestrator.route.failed");
    }
}
