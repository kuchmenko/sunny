use std::collections::HashSet;
use std::path::Path;

use crate::tool::fs_scan::FileScanner;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceExtensions {
    extensions: HashSet<String>,
}

impl WorkspaceExtensions {
    pub fn discover(top_entries: &[String], scanner: &FileScanner) -> Self {
        let mut extensions = HashSet::new();

        for entry in top_entries {
            collect_from_path_string(entry, &mut extensions);
            let path = Path::new(entry);

            if path.is_file() {
                collect_from_path(path, &mut extensions);
                continue;
            }

            if !path.is_dir() {
                continue;
            }

            let shallow_scanner = FileScanner {
                max_depth: 1,
                ..FileScanner {
                    max_files: scanner.max_files,
                    max_depth: scanner.max_depth,
                    ignore_dirs: scanner.ignore_dirs.clone(),
                }
            };

            if let Ok(result) = shallow_scanner.scan(path) {
                for file in result.files {
                    if let Some(ext) = file.extension {
                        if let Some(normalized) = normalize_extension(&ext) {
                            extensions.insert(normalized);
                        }
                    }
                }
            }
        }

        if extensions.is_empty() {
            return Self::common_extensions();
        }

        Self { extensions }
    }

    pub fn contains_extension(&self, ext: &str) -> bool {
        normalize_extension(ext)
            .map(|normalized| self.extensions.contains(&normalized))
            .unwrap_or(false)
    }

    pub fn is_code_file(&self, path: &str) -> bool {
        let normalized_path = path.to_ascii_lowercase();
        self.extensions
            .iter()
            .any(|ext| normalized_path.ends_with(ext))
    }

    pub fn common_extensions() -> Self {
        let extensions = [
            ".rs", ".toml", ".md", ".py", ".js", ".ts", ".tsx", ".jsx", ".go", ".java", ".kt",
            ".rb", ".php", ".c", ".cpp", ".h", ".cs", ".swift", ".yaml", ".yml", ".json", ".xml",
            ".html", ".css", ".sql", ".sh", ".scala", ".dart", ".lua", ".proto",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        Self { extensions }
    }
}

fn collect_from_path_string(path: &str, extensions: &mut HashSet<String>) {
    if let Some(ext) = Path::new(path).extension().and_then(|value| value.to_str()) {
        if let Some(normalized) = normalize_extension(ext) {
            extensions.insert(normalized);
        }
    }
}

fn collect_from_path(path: &Path, extensions: &mut HashSet<String>) {
    if let Some(ext) = path.extension().and_then(|value| value.to_str()) {
        if let Some(normalized) = normalize_extension(ext) {
            extensions.insert(normalized);
        }
    }
}

fn normalize_extension(ext: &str) -> Option<String> {
    let trimmed = ext.trim();
    if trimmed.is_empty() {
        return None;
    }

    let dotted = if trimmed.starts_with('.') {
        trimmed.to_ascii_lowercase()
    } else {
        format!(".{}", trimmed.to_ascii_lowercase())
    };

    if dotted == "." {
        None
    } else {
        Some(dotted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_discover_finds_rs_and_toml() {
        let dir = tempdir().expect("create temp dir");
        let src = dir.path().join("src");
        fs::create_dir_all(&src).expect("create src dir");
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"").expect("write cargo");
        fs::write(src.join("main.rs"), "fn main() {}\n").expect("write rust file");

        let top_entries = vec![
            dir.path().join("Cargo.toml").display().to_string(),
            src.display().to_string(),
        ];

        let extensions = WorkspaceExtensions::discover(&top_entries, &FileScanner::default());

        assert!(extensions.contains_extension(".rs"));
        assert!(extensions.contains_extension(".toml"));
    }

    #[test]
    fn test_common_extensions_includes_python() {
        let extensions = WorkspaceExtensions::common_extensions();
        assert!(extensions.contains_extension(".py"));
    }

    #[test]
    fn test_is_code_file_for_various_languages() {
        let extensions = WorkspaceExtensions::common_extensions();

        assert!(extensions.is_code_file("src/main.rs"));
        assert!(extensions.is_code_file("app/server.py"));
        assert!(extensions.is_code_file("web/index.TSX"));
        assert!(extensions.is_code_file("pkg/api.go"));
        assert!(!extensions.is_code_file("README"));
    }
}
