#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PromptSpec {
    pub role: &'static str,
    pub version: &'static str,
    pub system_prompt: &'static str,
    pub output_format: OutputFormat,
    pub tool_boundary: ToolBoundary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    FreeText,
    StructuredJson { schema_hint: &'static str },
    Mixed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolBoundary {
    NoTools,
    ReadOnly,
    ReadWrite,
}

pub const SPEC_PLANNING_INTAKE: PromptSpec = PromptSpec {
    role: "planning_intake",
    version: "1.0",
    system_prompt: concat!(
        "You are the planning intake advisor. Distill the incoming request into a ",
        "clear problem statement, constraints, risks, and missing context. Return ",
        "structured intake data that helps downstream planning without inventing facts."
    ),
    output_format: OutputFormat::StructuredJson {
        schema_hint: "problem, goals, constraints, open_questions, risks",
    },
    tool_boundary: ToolBoundary::NoTools,
};

pub const SPEC_PLANNER: PromptSpec = PromptSpec {
    role: "planner",
    version: "1.0",
    system_prompt: concat!(
        "You are the execution planner. Turn validated intake data into a minimal, ",
        "ordered plan with dependencies, verification steps, and completion criteria. ",
        "Prefer concrete steps over abstract advice."
    ),
    output_format: OutputFormat::StructuredJson {
        schema_hint: "summary, steps, dependencies, verification, assumptions",
    },
    tool_boundary: ToolBoundary::NoTools,
};

pub const SPEC_ORCHESTRATOR: PromptSpec = PromptSpec {
    role: "orchestrator",
    version: "1.0",
    system_prompt: concat!(
        "You are the orchestrator. Dispatch work to the best role, monitor results, ",
        "retry when justified, and surface the next action with concise rationale. ",
        "Balance control flow decisions with practical operator guidance."
    ),
    output_format: OutputFormat::Mixed,
    tool_boundary: ToolBoundary::NoTools,
};

pub const SPEC_EXPLORE: PromptSpec = PromptSpec {
    role: "explore",
    version: "1.0",
    system_prompt: concat!(
        "You are the exploration specialist. Map the codebase quickly, identify the ",
        "most relevant files, summarize existing behavior, and gather evidence needed ",
        "for implementation decisions while staying read-only."
    ),
    output_format: OutputFormat::FreeText,
    tool_boundary: ToolBoundary::ReadOnly,
};

pub const SPEC_LIBRARIAN: PromptSpec = PromptSpec {
    role: "librarian",
    version: "1.0",
    system_prompt: "reserved for future implementation",
    output_format: OutputFormat::FreeText,
    tool_boundary: ToolBoundary::ReadOnly,
};

pub const SPEC_VERIFICATION_CRITIQUE: PromptSpec = PromptSpec {
    role: "verification_critique",
    version: "1.0",
    system_prompt: concat!(
        "You are the verification critique role. Review proposed or completed work for ",
        "gaps, risks, missing checks, and weak assumptions. Return structured findings ",
        "that help the system tighten quality before completion."
    ),
    output_format: OutputFormat::StructuredJson {
        schema_hint: "status, findings, risks, missing_checks, recommendations",
    },
    tool_boundary: ToolBoundary::NoTools,
};

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn specs() -> [&'static PromptSpec; 6] {
        [
            &SPEC_PLANNING_INTAKE,
            &SPEC_PLANNER,
            &SPEC_ORCHESTRATOR,
            &SPEC_EXPLORE,
            &SPEC_LIBRARIAN,
            &SPEC_VERIFICATION_CRITIQUE,
        ]
    }

    #[test]
    fn test_prompt_spec_fields_are_non_empty() {
        for spec in specs() {
            assert!(!spec.role.is_empty());
            assert!(!spec.version.is_empty());
            assert!(!spec.system_prompt.is_empty());

            if let OutputFormat::StructuredJson { schema_hint } = spec.output_format {
                assert!(!schema_hint.is_empty());
            }
        }
    }

    #[test]
    fn test_prompt_spec_versions_use_major_minor_format() {
        for spec in specs() {
            let parts: Vec<_> = spec.version.split('.').collect();
            assert_eq!(
                parts.len(),
                2,
                "version '{}' must use major.minor",
                spec.version
            );
            assert!(parts.iter().all(|part| !part.is_empty()));
            assert!(parts
                .iter()
                .all(|part| part.chars().all(|ch| ch.is_ascii_digit())));
        }
    }

    #[test]
    fn test_prompt_spec_roles_are_unique() {
        let mut unique_roles = HashSet::new();

        for spec in specs() {
            assert!(
                unique_roles.insert(spec.role),
                "duplicate role '{}' found in prompt specs",
                spec.role
            );
        }
    }

    #[test]
    fn test_prompt_spec_boundaries_match_role_contracts() {
        assert!(matches!(
            SPEC_PLANNING_INTAKE.output_format,
            OutputFormat::StructuredJson { schema_hint } if !schema_hint.is_empty()
        ));
        assert_eq!(SPEC_PLANNING_INTAKE.tool_boundary, ToolBoundary::NoTools);

        assert!(matches!(
            SPEC_PLANNER.output_format,
            OutputFormat::StructuredJson { schema_hint } if !schema_hint.is_empty()
        ));
        assert_eq!(SPEC_PLANNER.tool_boundary, ToolBoundary::NoTools);

        assert_eq!(SPEC_ORCHESTRATOR.output_format, OutputFormat::Mixed);
        assert_eq!(SPEC_ORCHESTRATOR.tool_boundary, ToolBoundary::NoTools);

        assert_eq!(SPEC_EXPLORE.output_format, OutputFormat::FreeText);
        assert_eq!(SPEC_EXPLORE.tool_boundary, ToolBoundary::ReadOnly);

        assert_eq!(SPEC_LIBRARIAN.output_format, OutputFormat::FreeText);
        assert_eq!(SPEC_LIBRARIAN.tool_boundary, ToolBoundary::ReadOnly);

        assert!(matches!(
            SPEC_VERIFICATION_CRITIQUE.output_format,
            OutputFormat::StructuredJson { schema_hint } if !schema_hint.is_empty()
        ));
        assert_eq!(
            SPEC_VERIFICATION_CRITIQUE.tool_boundary,
            ToolBoundary::NoTools
        );
    }

    #[test]
    fn test_prompt_spec_role_names_match_expected_values() {
        assert_eq!(SPEC_PLANNING_INTAKE.role, "planning_intake");
        assert_eq!(SPEC_PLANNER.role, "planner");
        assert_eq!(SPEC_ORCHESTRATOR.role, "orchestrator");
        assert_eq!(SPEC_EXPLORE.role, "explore");
        assert_eq!(SPEC_LIBRARIAN.role, "librarian");
        assert_eq!(SPEC_VERIFICATION_CRITIQUE.role, "verification_critique");
    }
}
