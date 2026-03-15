//! User configuration loaded from ~/.sunny/config.toml and .sunny/config.toml.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Permissions-related user preferences.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PermissionsConfig {
    /// Write .sunny/policy.toml when approving a command inline.
    /// Disabled by default — approvals go to DB only.
    #[serde(default)]
    pub sync_policy_on_approve: bool,

    /// Auto-deny any capability request without prompting.
    /// Useful for CI / headless environments.
    #[serde(default)]
    pub headless: bool,
}

/// Task-runner preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TasksConfig {
    /// Maximum number of tasks that can run concurrently per workspace.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
}

fn default_max_concurrent() -> usize {
    3
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self {
            max_concurrent: default_max_concurrent(),
        }
    }
}

/// Model routing configuration — maps complexity categories to model names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsConfig {
    /// Model for quick, mechanical work (file reads, small edits, formatting).
    #[serde(default = "default_quick")]
    pub quick: String,
    /// Model for standard implementation work (functions, tests, moderate logic).
    #[serde(default = "default_standard")]
    pub standard: String,
    /// Model for deep reasoning (architecture, multi-file refactoring, debugging).
    #[serde(default = "default_deep")]
    pub deep: String,
    /// Fallback model when no category matches or category is absent.
    #[serde(default = "default_default")]
    pub default: String,
}

fn default_quick() -> String {
    "claude-haiku-4-5".into()
}
fn default_standard() -> String {
    "claude-sonnet-4-6".into()
}
fn default_deep() -> String {
    "gpt-5.4".into()
}
fn default_default() -> String {
    "claude-sonnet-4-6".into()
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            quick: default_quick(),
            standard: default_standard(),
            deep: default_deep(),
            default: default_default(),
        }
    }
}

impl ModelsConfig {
    /// Resolve a category name to the configured model string.
    ///
    /// Returns `self.default` for any unrecognized category.
    pub fn resolve_category(&self, category: &str) -> &str {
        match category {
            "quick" => &self.quick,
            "standard" => &self.standard,
            "deep" => &self.deep,
            _ => &self.default,
        }
    }
}

/// Full user configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserConfig {
    #[serde(default)]
    pub permissions: PermissionsConfig,
    #[serde(default)]
    pub tasks: TasksConfig,
    /// Model routing configuration — maps categories to model strings.
    #[serde(default)]
    pub models: ModelsConfig,
}

impl UserConfig {
    /// Load config. Workspace config overrides global config. Missing files are fine.
    /// Never errors — returns defaults on any problem.
    pub fn load(workspace_root: Option<&Path>) -> Self {
        let global = Self::load_file(Self::global_path().as_deref());
        let workspace = workspace_root
            .map(|root| Self::load_file(Some(&Self::workspace_path(root))))
            .unwrap_or_default();
        Self::merge(global, workspace)
    }

    /// Path to global config: ~/.sunny/config.toml
    pub fn global_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".sunny").join("config.toml"))
    }

    /// Path to workspace config: <root>/.sunny/config.toml
    pub fn workspace_path(root: &Path) -> PathBuf {
        root.join(".sunny").join("config.toml")
    }

    fn load_file(path: Option<&Path>) -> Self {
        let path = match path {
            Some(p) if p.exists() => p,
            _ => return Self::default(),
        };
        match std::fs::read_to_string(path) {
            Ok(raw) => toml::from_str::<Self>(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Workspace values override global values.
    fn merge(global: Self, workspace: Self) -> Self {
        // For booleans: workspace explicit true overrides global false.
        // For numbers: workspace non-default overrides global.
        // Implementation: workspace always wins (last-write wins per field).
        // Since we can't tell "was this explicitly set", workspace overrides entirely.
        Self {
            permissions: PermissionsConfig {
                sync_policy_on_approve: workspace.permissions.sync_policy_on_approve
                    || global.permissions.sync_policy_on_approve,
                headless: workspace.permissions.headless || global.permissions.headless,
            },
            tasks: TasksConfig {
                max_concurrent: if workspace.tasks.max_concurrent != default_max_concurrent() {
                    workspace.tasks.max_concurrent
                } else {
                    global.tasks.max_concurrent
                },
            },
            models: ModelsConfig {
                quick: if workspace.models.quick != default_quick() {
                    workspace.models.quick
                } else {
                    global.models.quick
                },
                standard: if workspace.models.standard != default_standard() {
                    workspace.models.standard
                } else {
                    global.models.standard
                },
                deep: if workspace.models.deep != default_deep() {
                    workspace.models.deep
                } else {
                    global.models.deep
                },
                default: if workspace.models.default != default_default() {
                    workspace.models.default
                } else {
                    global.models.default
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_user_config_defaults() {
        let config = UserConfig::default();
        assert!(!config.permissions.sync_policy_on_approve);
        assert!(!config.permissions.headless);
        assert_eq!(config.tasks.max_concurrent, 3);
    }

    #[test]
    fn test_user_config_load_missing_file_returns_defaults() {
        let dir = tempdir().expect("tempdir");
        let config = UserConfig::load(Some(dir.path()));
        assert_eq!(config.tasks.max_concurrent, 3);
    }

    #[test]
    fn test_user_config_load_workspace_file() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".sunny")).expect("create .sunny");
        std::fs::write(
            dir.path().join(".sunny").join("config.toml"),
            "[tasks]\nmax_concurrent = 5\n",
        )
        .expect("write config");
        let config = UserConfig::load(Some(dir.path()));
        assert_eq!(config.tasks.max_concurrent, 5);
    }

    #[test]
    fn test_user_config_headless_from_workspace() {
        let dir = tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".sunny")).expect("create .sunny");
        std::fs::write(
            dir.path().join(".sunny").join("config.toml"),
            "[permissions]\nheadless = true\n",
        )
        .expect("write config");
        let config = UserConfig::load(Some(dir.path()));
        assert!(config.permissions.headless);
    }
}
