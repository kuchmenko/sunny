use std::fmt;
use uuid::Uuid;

/// RequestId uniquely identifies a single CLI invocation/request through the orchestration pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub Uuid);

impl RequestId {
    /// Create a new RequestId with a random UUID v4.
    pub fn new() -> Self {
        RequestId(Uuid::new_v4())
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<RequestId> for String {
    fn from(id: RequestId) -> Self {
        id.to_string()
    }
}

/// PlanId uniquely identifies a single execution plan within a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlanId(pub Uuid);

impl PlanId {
    /// Create a new PlanId with a random UUID v4.
    pub fn new() -> Self {
        PlanId(Uuid::new_v4())
    }
}

impl Default for PlanId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for PlanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<PlanId> for String {
    fn from(id: PlanId) -> Self {
        id.to_string()
    }
}

/// StepId uniquely identifies a single step within an execution plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepId(pub Uuid);

impl StepId {
    /// Create a new StepId with a random UUID v4.
    pub fn new() -> Self {
        StepId(Uuid::new_v4())
    }
}

impl Default for StepId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for StepId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<StepId> for String {
    fn from(id: StepId) -> Self {
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_id_uniqueness() {
        let id1 = RequestId::new();
        let id2 = RequestId::new();
        assert_ne!(id1, id2, "RequestId::new() should generate unique IDs");
    }

    #[test]
    fn test_request_id_display() {
        let id = RequestId::new();
        let display_str = id.to_string();
        // UUID string format: 8-4-4-4-12 hex digits
        assert_eq!(display_str.len(), 36, "UUID string should be 36 chars");
        assert!(
            display_str.contains('-'),
            "UUID string should contain hyphens"
        );
    }

    #[test]
    fn test_request_id_into_string() {
        let id = RequestId::new();
        let s: String = id.into();
        assert_eq!(s, id.to_string());
    }

    #[test]
    fn test_plan_id_uniqueness() {
        let id1 = PlanId::new();
        let id2 = PlanId::new();
        assert_ne!(id1, id2, "PlanId::new() should generate unique IDs");
    }

    #[test]
    fn test_plan_id_display() {
        let id = PlanId::new();
        let display_str = id.to_string();
        assert_eq!(display_str.len(), 36, "UUID string should be 36 chars");
        assert!(
            display_str.contains('-'),
            "UUID string should contain hyphens"
        );
    }

    #[test]
    fn test_plan_id_into_string() {
        let id = PlanId::new();
        let s: String = id.into();
        assert_eq!(s, id.to_string());
    }

    #[test]
    fn test_step_id_uniqueness() {
        let id1 = StepId::new();
        let id2 = StepId::new();
        assert_ne!(id1, id2, "StepId::new() should generate unique IDs");
    }

    #[test]
    fn test_step_id_display() {
        let id = StepId::new();
        let display_str = id.to_string();
        assert_eq!(display_str.len(), 36, "UUID string should be 36 chars");
        assert!(
            display_str.contains('-'),
            "UUID string should contain hyphens"
        );
    }

    #[test]
    fn test_step_id_into_string() {
        let id = StepId::new();
        let s: String = id.into();
        assert_eq!(s, id.to_string());
    }

    #[test]
    fn test_default_implementations() {
        let req_id = RequestId::default();
        let plan_id = PlanId::default();
        let step_id = StepId::default();

        // Defaults should be unique
        assert_ne!(req_id, RequestId::default());
        assert_ne!(plan_id, PlanId::default());
        assert_ne!(step_id, StepId::default());
    }

    #[test]
    fn test_hash_trait() {
        use std::collections::HashSet;

        let id1 = RequestId::new();
        let id2 = RequestId::new();

        let mut set = HashSet::new();
        set.insert(id1);
        set.insert(id2);

        assert_eq!(set.len(), 2, "Different RequestIds should hash differently");
    }
}
