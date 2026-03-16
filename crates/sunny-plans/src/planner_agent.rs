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
    let mut names: Vec<String> = build_plan_tool_definitions()
        .into_iter()
        .map(|definition| definition.name)
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
}

pub struct PlannerSystemPromptBuilder;

impl PlannerSystemPromptBuilder {
    pub fn build(ctx: &PlannerPromptContext) -> String {
        let mut sections = vec![Self::layer_role(ctx)];

        if let Some(handoff_context) = &ctx.handoff_context {
            sections.push(Self::layer_handoff_context(handoff_context));
        }

        if let Some(plan_state) = &ctx.plan_state {
            sections.push(Self::layer_plan_state(plan_state));
        }

        sections.push(Self::layer_workspace(&ctx.workspace_root));
        sections.join("\n\n")
    }

    fn layer_role(ctx: &PlannerPromptContext) -> String {
        format!(
            "You are the Planner Agent for {} mode planning.\n\n\
             Your role:\n\
             - Analyze the codebase and requirements using investigation tools\n\
             - Build and refine execution plans using plan tools\n\
             - Record decisions, constraints, and goals\n\
             - Only read-only investigation tools + plan tools available to you",
            ctx.mode
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
