//! Event taxonomy constants for sunny-boys observability.
//!
//! Defines canonical event names used by agent implementations
//! for structured logging and tracing. All event names follow
//! dotted naming convention.

pub const EVENT_TOOL_METRICS: &str = "tool.exec.metrics";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_constants_follow_naming_convention() {
        let events = vec![EVENT_TOOL_METRICS];

        for event in events {
            assert!(!event.is_empty(), "Event must not be empty");
            assert!(
                event.contains('.'),
                "Event '{event}' must follow dotted naming convention"
            );
            assert!(
                !event.contains('_'),
                "Event '{event}' must use dots, not underscores"
            );
        }
    }
}
