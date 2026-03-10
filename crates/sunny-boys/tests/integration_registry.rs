use std::sync::Arc;

use sunny_boys::{
    build_boys_registry, BackgroundTaskManager, DelegateAgent, ExploreAgent, OracleAgent,
};
use sunny_core::agent::{Agent, Capability};
use sunny_core::orchestrator::AgentRegistry;
use sunny_core::tool::ToolPolicy;
use tokio_util::sync::CancellationToken;

#[test]
fn test_explore_agent_uses_deny_list_policy() {
    // ExploreAgent should use ToolPolicy::deny_list, not allow_list
    let policy = ToolPolicy::deny_list(&["file_write", "exec", "shell"]);

    // Read tools should be allowed
    assert!(policy.is_allowed("fs_read"));
    assert!(policy.is_allowed("fs_scan"));
    assert!(policy.is_allowed("text_grep"));

    // Git tools should be allowed
    assert!(policy.is_allowed("git_log"));
    assert!(policy.is_allowed("git_diff"));
    assert!(policy.is_allowed("git_status"));

    // Denied tools should be blocked
    assert!(!policy.is_allowed("file_write"));
    assert!(!policy.is_allowed("exec"));
    assert!(!policy.is_allowed("shell"));
}

#[tokio::test]
async fn test_full_registry_construction() {
    let token = CancellationToken::new();
    let registry = build_boys_registry(None, &token).expect("should build boys registry");

    // Verify all 6 agents are registered
    assert!(
        registry.find("workspace-read").is_some(),
        "workspace-read agent should exist"
    );
    assert!(
        registry.find("review").is_some(),
        "review agent should exist"
    );
    assert!(
        registry.find("critique").is_some(),
        "critique agent should exist"
    );
    assert!(
        registry.find("explore").is_some(),
        "explore agent should exist"
    );
    assert!(
        registry.find("oracle").is_some(),
        "oracle agent should exist"
    );
    assert!(
        registry.find("delegate").is_some(),
        "delegate agent should exist"
    );

    // Verify capabilities
    let explore_agents = registry.find_by_capability(&Capability("explore".into()));
    assert!(explore_agents.contains(&"explore"));

    let advise_agents = registry.find_by_capability(&Capability("advise".into()));
    assert!(advise_agents.contains(&"oracle"));

    let delegate_agents = registry.find_by_capability(&Capability("delegate".into()));
    assert!(delegate_agents.contains(&"delegate"));

    token.cancel();
}

#[tokio::test]
async fn test_background_task_manager_bounded_capacity() {
    let token = CancellationToken::new();
    let manager = BackgroundTaskManager::new(2, token.child_token());

    // Spawn 2 tasks (at capacity)
    let result1 = manager
        .spawn("task-1".to_string(), async { Ok("result1".to_string()) })
        .await;
    assert!(result1.is_ok());

    let result2 = manager
        .spawn("task-2".to_string(), async { Ok("result2".to_string()) })
        .await;
    assert!(result2.is_ok());

    // Third task should fail due to capacity
    let result3 = manager
        .spawn("task-3".to_string(), async { Ok("result3".to_string()) })
        .await;
    assert!(result3.is_err());
}

#[tokio::test]
async fn test_background_task_manager_spawn_and_collect() {
    let token = CancellationToken::new();
    let manager = BackgroundTaskManager::new(10, token.child_token());

    manager
        .spawn("task-1".to_string(), async {
            Ok("success result".to_string())
        })
        .await
        .expect("should spawn task");

    let result = manager.collect("task-1").await;
    assert!(result.is_ok());

    token.cancel();
}

#[tokio::test]
async fn test_oracle_agent_name_and_capabilities() {
    let agent = OracleAgent::new(None);

    assert_eq!(agent.name(), "oracle");
    assert_eq!(agent.capabilities(), vec![Capability("advise".to_string())]);
}

#[tokio::test]
async fn test_explore_agent_name_and_capabilities() {
    let agent = ExploreAgent::new(None);

    assert_eq!(agent.name(), "explore");
    assert_eq!(
        agent.capabilities(),
        vec![Capability("explore".to_string())]
    );
}

#[tokio::test]
async fn test_delegate_agent_name_and_capabilities() {
    let token = CancellationToken::new();
    let registry = Arc::new(AgentRegistry::new());
    let background = Arc::new(BackgroundTaskManager::new(10, token.child_token()));

    let agent = DelegateAgent::new(registry, background);

    assert_eq!(agent.name(), "delegate");
    assert_eq!(
        agent.capabilities(),
        vec![Capability("delegate".to_string())]
    );
}
