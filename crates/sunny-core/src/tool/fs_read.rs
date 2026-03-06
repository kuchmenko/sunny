use std::path::{Path, PathBuf};

use tracing::info;

use crate::orchestrator::events::{
    EVENT_TOOL_EXEC_END, EVENT_TOOL_EXEC_ERROR, EVENT_TOOL_EXEC_START, OUTCOME_ERROR,
    OUTCOME_SUCCESS,
};
use crate::tool::ToolError;

/// 1 MiB — default ceiling before `FileTooLarge` is returned.
const DEFAULT_MAX_BYTES: u64 = 1_048_576;

/// Leading bytes inspected for null-byte (binary) detection.
const BINARY_CHECK_LEN: usize = 8192;

#[derive(Debug)]
pub struct FileContent {
    pub path: PathBuf,
    pub content: String,
    pub size_bytes: u64,
}

pub struct FileReader {
    pub max_bytes: u64,
    pub denylist_extensions: Vec<String>,
    pub denylist_names: Vec<String>,
    pub denylist_prefixes: Vec<String>,
}

impl Default for FileReader {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            denylist_extensions: vec![".key".to_string(), ".pem".to_string(), ".p12".to_string()],
            denylist_names: vec![".env".to_string()],
            denylist_prefixes: vec![".env.".to_string()],
        }
    }
}

impl FileReader {
    pub fn read(&self, path: &Path) -> Result<FileContent, ToolError> {
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "fs_read", path = %path.display());

        let path_str = path.display().to_string();

        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            if self.denylist_names.iter().any(|n| n == file_name) {
                let err = ToolError::SensitiveFileDenied { path: path_str };
                info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_read", outcome = OUTCOME_ERROR, error_kind = "SensitiveFileDenied", error_message = %err);
                return Err(err);
            }
            if self
                .denylist_prefixes
                .iter()
                .any(|p| file_name.starts_with(p.as_str()))
            {
                let err = ToolError::SensitiveFileDenied { path: path_str };
                info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_read", outcome = OUTCOME_ERROR, error_kind = "SensitiveFileDenied", error_message = %err);
                return Err(err);
            }
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let dotted = format!(".{ext}");
                if self.denylist_extensions.contains(&dotted) {
                    let err = ToolError::SensitiveFileDenied { path: path_str };
                    info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_read", outcome = OUTCOME_ERROR, error_kind = "SensitiveFileDenied", error_message = %err);
                    return Err(err);
                }
            }
        }

        let metadata = std::fs::metadata(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ToolError::PathNotFound {
                path: path_str.clone(),
            },
            std::io::ErrorKind::PermissionDenied => ToolError::PermissionDenied {
                path: path_str.clone(),
            },
            _ => ToolError::ExecutionFailed {
                source: Box::new(e),
            },
        }).map_err(|err| {
            let error_kind = match &err {
                ToolError::PathNotFound { .. } => "PathNotFound",
                ToolError::PermissionDenied { .. } => "PermissionDenied",
                ToolError::ExecutionFailed { .. } => "ExecutionFailed",
                _ => "Unknown",
            };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_read", outcome = OUTCOME_ERROR, error_kind = error_kind, error_message = %err);
            err
        })?;

        let size = metadata.len();
        if size > self.max_bytes {
            let err = ToolError::FileTooLarge {
                path: path_str,
                size,
                limit: self.max_bytes,
            };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_read", outcome = OUTCOME_ERROR, error_kind = "FileTooLarge", error_message = %err);
            return Err(err);
        }

        let bytes = std::fs::read(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::PermissionDenied => ToolError::PermissionDenied {
                path: path_str.clone(),
            },
            _ => ToolError::ExecutionFailed {
                source: Box::new(e),
            },
        }).map_err(|err| {
            let error_kind = match &err {
                ToolError::PermissionDenied { .. } => "PermissionDenied",
                ToolError::ExecutionFailed { .. } => "ExecutionFailed",
                _ => "Unknown",
            };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_read", outcome = OUTCOME_ERROR, error_kind = error_kind, error_message = %err);
            err
        })?;

        let check_end = bytes.len().min(BINARY_CHECK_LEN);
        if bytes[..check_end].contains(&0u8) {
            let err = ToolError::BinaryFileSkipped { path: path_str };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_read", outcome = OUTCOME_ERROR, error_kind = "BinaryFileSkipped", error_message = %err);
            return Err(err);
        }

        let content = String::from_utf8(bytes).map_err(|e| ToolError::ExecutionFailed {
            source: Box::new(e),
        }).map_err(|err| {
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_read", outcome = OUTCOME_ERROR, error_kind = "ExecutionFailed", error_message = %err);
            err
        })?;

        let result = FileContent {
            path: path.to_path_buf(),
            content,
            size_bytes: size,
        };

        info!(name: EVENT_TOOL_EXEC_END, tool_name = "fs_read", outcome = OUTCOME_SUCCESS, size_bytes = size);

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use tracing_test::traced_test;

    fn temp_file_with(content: &[u8], suffix: &str) -> NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(suffix)
            .tempfile()
            .expect("test: create temp file");
        f.write_all(content).expect("test: write temp file");
        f.flush().expect("test: flush temp file");
        f
    }

    #[test]
    fn test_fs_read_utf8_file() {
        let f = temp_file_with(b"hello world\n", ".txt");
        let reader = FileReader::default();
        let result = reader.read(f.path()).expect("should read UTF-8 file");
        assert_eq!(result.content, "hello world\n");
        assert_eq!(result.size_bytes, 12);
        assert_eq!(result.path, f.path());
    }

    #[test]
    fn test_fs_read_respects_size_cap() {
        let f = temp_file_with(&[b'a'; 2048], ".txt");
        let reader = FileReader {
            max_bytes: 1024,
            ..FileReader::default()
        };
        let err = reader.read(f.path()).unwrap_err();
        match err {
            ToolError::FileTooLarge { size, limit, .. } => {
                assert_eq!(size, 2048);
                assert_eq!(limit, 1024);
            }
            other => panic!("expected FileTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn test_fs_read_skips_binary_file() {
        let mut data = b"some text".to_vec();
        data.push(0u8);
        data.extend_from_slice(b"more text");
        let f = temp_file_with(&data, ".bin");
        let reader = FileReader::default();
        let err = reader.read(f.path()).unwrap_err();
        assert!(
            matches!(err, ToolError::BinaryFileSkipped { .. }),
            "expected BinaryFileSkipped, got {err:?}"
        );
    }

    #[test]
    fn test_fs_read_denylisted_env_file() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let env_path = dir.path().join(".env");
        std::fs::write(&env_path, "SECRET=abc").expect("test: write .env");
        let reader = FileReader::default();
        let err = reader.read(&env_path).unwrap_err();
        assert!(
            matches!(err, ToolError::SensitiveFileDenied { .. }),
            "expected SensitiveFileDenied, got {err:?}"
        );
    }

    #[test]
    fn test_fs_read_denylisted_key_file() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let key_path = dir.path().join("server.key");
        std::fs::write(&key_path, "KEY DATA").expect("test: write server.key");
        let reader = FileReader::default();
        let err = reader.read(&key_path).unwrap_err();
        assert!(
            matches!(err, ToolError::SensitiveFileDenied { .. }),
            "expected SensitiveFileDenied, got {err:?}"
        );
    }

    #[test]
    fn test_fs_read_denylisted_pem_file() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let pem_path = dir.path().join("cert.pem");
        std::fs::write(&pem_path, "CERT DATA").expect("test: write cert.pem");
        let reader = FileReader::default();
        let err = reader.read(&pem_path).unwrap_err();
        assert!(
            matches!(err, ToolError::SensitiveFileDenied { .. }),
            "expected SensitiveFileDenied, got {err:?}"
        );
    }

    #[test]
    fn test_fs_read_denylisted_p12_file() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let p12_path = dir.path().join("keystore.p12");
        std::fs::write(&p12_path, "P12 DATA").expect("test: write keystore.p12");
        let reader = FileReader::default();
        let err = reader.read(&p12_path).unwrap_err();
        assert!(
            matches!(err, ToolError::SensitiveFileDenied { .. }),
            "expected SensitiveFileDenied, got {err:?}"
        );
    }

    #[test]
    fn test_fs_read_denylisted_env_variant() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let env_prod = dir.path().join(".env.production");
        std::fs::write(&env_prod, "DB_URL=secret").expect("test: write .env.production");
        let reader = FileReader::default();
        let err = reader.read(&env_prod).unwrap_err();
        assert!(
            matches!(err, ToolError::SensitiveFileDenied { .. }),
            "expected SensitiveFileDenied, got {err:?}"
        );
    }

    #[test]
    fn test_fs_read_nonexistent_file() {
        let reader = FileReader::default();
        let err = reader.read(Path::new("/nonexistent.txt")).unwrap_err();
        assert!(
            matches!(err, ToolError::PathNotFound { .. }),
            "expected PathNotFound, got {err:?}"
        );
    }

    #[test]
    fn test_fs_read_empty_file() {
        let f = temp_file_with(b"", ".txt");
        let reader = FileReader::default();
        let result = reader.read(f.path()).expect("should read empty file");
        assert_eq!(result.content, "");
        assert_eq!(result.size_bytes, 0);
    }

    #[traced_test]
    #[test]
    fn test_fs_read_tracing_events() {
        let f = temp_file_with(b"test content", ".txt");
        let reader = FileReader::default();
        let result = reader.read(f.path()).expect("should read file");

        // Verify success event was logged with correct fields
        assert!(
            logs_contain("tool_name=\"fs_read\""),
            "should log tool_name field"
        );
        assert!(
            logs_contain("outcome=\"success\""),
            "should log success outcome"
        );
        assert!(logs_contain("size_bytes=12"), "should log size_bytes");
        assert_eq!(result.size_bytes, 12);
    }

    #[traced_test]
    #[test]
    fn test_fs_read_tracing_error() {
        let reader = FileReader::default();
        let err = reader
            .read(std::path::Path::new("/nonexistent.txt"))
            .unwrap_err();

        // Verify error event was logged with correct fields
        assert!(
            logs_contain("tool_name=\"fs_read\""),
            "should log tool_name field"
        );
        assert!(
            logs_contain("outcome=\"error\""),
            "should log error outcome"
        );
        assert!(
            logs_contain("error_kind=\"PathNotFound\""),
            "should include error_kind"
        );
        assert!(matches!(err, ToolError::PathNotFound { .. }));
    }
}
