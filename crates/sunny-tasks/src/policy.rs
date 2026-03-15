use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapabilityPolicyEntry {
    pub policy: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_rhs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_ops: Option<Vec<String>>,
    pub added_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PolicyFile {
    #[serde(default)]
    pub capabilities: HashMap<String, CapabilityPolicyEntry>,
}

impl PolicyFile {
    pub fn load(workspace_root: &Path) -> Result<Self, crate::error::TaskError> {
        let path = Self::path(workspace_root);
        if !path.exists() {
            return Ok(Self::default());
        }

        let raw = std::fs::read_to_string(path)?;
        toml::from_str::<Self>(&raw)
            .map_err(|e| crate::error::TaskError::Io(std::io::Error::other(e.to_string())))
    }

    pub fn save(&self, workspace_root: &Path) -> Result<(), crate::error::TaskError> {
        let path = Self::path(workspace_root);
        let parent = path.parent().ok_or_else(|| {
            crate::error::TaskError::Io(std::io::Error::other("invalid policy path"))
        })?;
        std::fs::create_dir_all(parent)?;
        let encoded = toml::to_string_pretty(self)
            .map_err(|e| crate::error::TaskError::Io(std::io::Error::other(e.to_string())))?;
        std::fs::write(path, encoded)?;
        Ok(())
    }

    pub fn path(workspace_root: &Path) -> PathBuf {
        workspace_root.join(".sunny").join("policy.toml")
    }

    pub fn set_capability(
        &mut self,
        workspace_root: &Path,
        name: &str,
        entry: CapabilityPolicyEntry,
    ) -> Result<(), crate::error::TaskError> {
        self.capabilities.insert(name.to_string(), entry);
        self.save(workspace_root)
    }

    pub fn revoke(
        &mut self,
        workspace_root: &Path,
        name: &str,
    ) -> Result<bool, crate::error::TaskError> {
        let existed = self.capabilities.remove(name).is_some();
        self.save(workspace_root)?;
        Ok(existed)
    }

    pub fn is_workspace_granted(&self, capability: &str, pattern: Option<&str>) -> bool {
        let Some(entry) = self.capabilities.get(capability) else {
            return false;
        };

        if entry.policy != "workspace" && entry.policy != "global" {
            return false;
        }

        if capability == "shell_pipes" {
            let Some(rhs) = pattern else {
                return false;
            };
            return entry
                .allowed_rhs
                .as_ref()
                .is_some_and(|values| values.iter().any(|v| v == rhs));
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_policy_load_returns_empty_when_missing() {
        let dir = tempfile::tempdir().expect("should create temp dir");

        let policy = PolicyFile::load(dir.path()).expect("should load missing policy as empty");

        assert!(policy.capabilities.is_empty());
    }

    #[test]
    fn test_policy_save_and_reload_roundtrip() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let mut policy = PolicyFile::default();
        policy.capabilities.insert(
            "shell_pipes".to_string(),
            CapabilityPolicyEntry {
                policy: "workspace".to_string(),
                allowed_rhs: Some(vec!["tail".to_string(), "grep".to_string()]),
                allowed_ops: None,
                added_at: "2026-03-15T14:23:00Z".to_string(),
            },
        );

        policy.save(dir.path()).expect("should save policy");
        let loaded = PolicyFile::load(dir.path()).expect("should load policy");

        assert_eq!(loaded.capabilities.len(), 1);
        assert!(loaded.capabilities.contains_key("shell_pipes"));
    }

    #[test]
    fn test_policy_set_and_revoke_capability() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let mut policy = PolicyFile::default();

        policy
            .set_capability(
                dir.path(),
                "git_write",
                CapabilityPolicyEntry {
                    policy: "session".to_string(),
                    allowed_rhs: None,
                    allowed_ops: Some(vec!["commit".to_string()]),
                    added_at: "2026-03-15T15:01:00Z".to_string(),
                },
            )
            .expect("should set capability");
        assert!(policy.capabilities.contains_key("git_write"));

        let revoked = policy
            .revoke(dir.path(), "git_write")
            .expect("should revoke capability");
        assert!(revoked);
        assert!(!policy.capabilities.contains_key("git_write"));
    }

    #[test]
    fn test_is_workspace_granted_checks_rhs() {
        let mut policy = PolicyFile::default();
        policy.capabilities.insert(
            "shell_pipes".to_string(),
            CapabilityPolicyEntry {
                policy: "workspace".to_string(),
                allowed_rhs: Some(vec!["tail".to_string(), "grep".to_string()]),
                allowed_ops: None,
                added_at: "2026-03-15T14:23:00Z".to_string(),
            },
        );

        assert!(policy.is_workspace_granted("shell_pipes", Some("tail")));
        assert!(!policy.is_workspace_granted("shell_pipes", Some("wc")));
    }

    #[test]
    fn test_is_workspace_granted_false_when_not_present() {
        let policy = PolicyFile::default();
        assert!(!policy.is_workspace_granted("shell_pipes", Some("tail")));
    }
}
