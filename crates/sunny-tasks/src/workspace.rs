pub struct WorkspaceDetector;

impl WorkspaceDetector {
    pub fn detect(start_path: &std::path::Path) -> Option<std::path::PathBuf> {
        let mut current = start_path.to_path_buf();
        loop {
            if current.join(".git").exists() {
                return Some(current);
            }
            if !current.pop() {
                return None;
            }
        }
    }

    pub fn detect_cwd() -> Option<std::path::PathBuf> {
        std::env::current_dir().ok().and_then(|p| Self::detect(&p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_finds_git_root() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let root = dir.path().join("repo");
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).expect("should create nested dirs");
        std::fs::create_dir_all(root.join(".git")).expect("should create .git dir");

        let detected = WorkspaceDetector::detect(&nested).expect("should detect git root");
        assert_eq!(detected, root);
    }

    #[test]
    fn test_detect_returns_none_when_no_git() {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).expect("should create nested dirs");

        let detected = WorkspaceDetector::detect(&nested);
        assert!(detected.is_none());
    }
}
