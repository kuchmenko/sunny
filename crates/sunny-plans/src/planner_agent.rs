use crate::handoff::HandoffContext;
use crate::tools::definitions::build_plan_tool_definitions;

pub const INVESTIGATION_TOOL_NAMES: &[&str] = &[
    "fs_read",
    "fs_scan",
    "fs_glob",
    "text_grep",
    "grep_files",
    "git_log",
    "git_diff",
    "git_status",
    "codebase_search",
    "lsp_goto_definition",
    "lsp_find_references",
    "lsp_diagnostics",
    "lsp_symbols",
];

pub fn build_planner_tool_names() -> Vec<String> {
    let excluded = ["plan_create", "plan_replan"];
    let mut names: Vec<String> = build_plan_tool_definitions()
        .into_iter()
        .map(|definition| definition.name)
        .filter(|name| !excluded.contains(&name.as_str()))
        .collect();
    names.extend(
        INVESTIGATION_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_string()),
    );
    names
}

pub fn build_planner_tool_definitions() -> Vec<String> {
    build_planner_tool_names()
}

#[derive(Debug, Clone)]
pub struct PlannerAgent {
    pub plan_id: String,
}

impl PlannerAgent {
    pub fn build_tool_names() -> Vec<String> {
        build_planner_tool_names()
    }

    pub fn build_system_prompt(ctx: &PlannerPromptContext) -> String {
        PlannerSystemPromptBuilder::build(ctx)
    }
}

pub struct PlannerPromptContext {
    pub handoff_context: Option<HandoffContext>,
    pub plan_state: Option<String>,
    pub workspace_root: String,
    pub mode: String,
    pub plan_id: String,
}

pub struct PlannerSystemPromptBuilder;

impl PlannerSystemPromptBuilder {
    pub fn build(ctx: &PlannerPromptContext) -> String {
        let mut sections = vec![Self::layer_role(ctx)];

        sections.push(Self::layer_mode_instructions(ctx));

        if let Some(handoff_context) = &ctx.handoff_context {
            sections.push(Self::layer_handoff_context(handoff_context));
        }

        if let Some(plan_state) = &ctx.plan_state {
            sections.push(Self::layer_plan_state(plan_state));
        }

        sections.push(Self::layer_workspace(&ctx.workspace_root));
        sections.join("\n\n")
    }

    fn layer_role(_ctx: &PlannerPromptContext) -> String {
        "You are in Smart planning mode. Your ONLY job is to build execution plans using plan tools.\n\n\
             BEHAVIORAL MANDATE:\n\
             - ALL planning MUST use plan_* tools. NEVER output plans as raw text or markdown.\n\
             - You do NOT have access to mutation tools (fs_write, fs_edit, shell_exec). Do NOT attempt to execute tasks yourself.\n\n\
             Your role:\n\
             - Analyze the codebase and requirements using investigation tools\n\
             - Build and refine execution plans using plan tools\n\
             - Record decisions, constraints, and goals\n\
             - Only read-only investigation tools + plan tools available to you"
            .to_string()
    }

    fn layer_mode_instructions(ctx: &PlannerPromptContext) -> String {
        format!(
            "## Smart Mode Instructions\n\
             Active plan ID: {}. Use this ID for all plan_* tool calls.\n\n\
             Tool usage workflow:\n\
             1. Investigate codebase with investigation tools (fs_read, fs_scan, grep, lsp_*)\n\
             2. Query current plan state with plan_query_state\n\
             3. Add tasks with plan_add_task\n\
             4. Add dependencies with plan_add_dependency\n\
             5. Record decisions with plan_record_decision\n\
             6. Add constraints/goals as needed\n\
             7. Finalize with plan_finalize",
            ctx.plan_id
        )
    }

    fn layer_handoff_context(handoff_context: &HandoffContext) -> String {
        format!("## Handoff Context\n{}", handoff_context.structured)
    }

    fn layer_plan_state(plan_state: &str) -> String {
        format!("## Current Plan State\n{}", plan_state)
    }

    fn layer_workspace(workspace_root: &str) -> String {
        format!("Workspace: {}", workspace_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_planner_prompt_contains_planning_mandate() {
        let ctx = PlannerPromptContext {
            handoff_context: None,
            plan_state: None,
            workspace_root: "/test".to_string(),
            mode: "smart".to_string(),
            plan_id: "plan-123".to_string(),
        };
        let prompt = PlannerSystemPromptBuilder::build(&ctx);
        assert!(
            prompt.contains("plan tools"),
            "Prompt should mention plan tools"
        );
        assert!(
            prompt.contains("mutation"),
            "Prompt should mention mutation tool prohibition"
        );
        assert!(prompt.contains("plan-123"), "Prompt should contain plan_id");
    }

    #[test]
    fn test_planner_tool_names_excludes_create_and_replan() {
        let tool_names = build_planner_tool_names();
        assert!(
            !tool_names.contains(&"plan_create".to_string()),
            "plan_create should be excluded"
        );
        assert!(
            !tool_names.contains(&"plan_replan".to_string()),
            "plan_replan should be excluded"
        );
        assert!(
            tool_names.contains(&"plan_add_task".to_string()),
            "plan_add_task should be included"
        );
        assert!(
            tool_names.contains(&"fs_read".to_string()),
            "investigation tools should be included"
        );
    }

    #[test]
    fn test_planner_prompt_context_has_plan_id() {
        let ctx = PlannerPromptContext {
            handoff_context: None,
            plan_state: None,
            workspace_root: "/test".to_string(),
            mode: "smart".to_string(),
            plan_id: "test-plan-456".to_string(),
        };
        assert_eq!(ctx.plan_id, "test-plan-456");
    }
}
