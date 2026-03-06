pub mod error;
pub mod fs_read;
pub mod fs_scan;
pub mod policy;
pub mod text_grep;

pub use error::ToolError;
pub use fs_read::{FileContent, FileReader};
pub use fs_scan::FileScanner;
pub use policy::ToolPolicy;
pub use text_grep::{GrepMatch, GrepResult, TextGrep};

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn test_tool_error_display() {
        // Test PathNotFound
        let err = ToolError::PathNotFound {
            path: "/nonexistent".to_string(),
        };
        assert_eq!(err.to_string(), "path not found: /nonexistent");

        // Test PermissionDenied
        let err = ToolError::PermissionDenied {
            path: "/root/secret".to_string(),
        };
        assert_eq!(err.to_string(), "permission denied: /root/secret");

        // Test FileTooLarge
        let err = ToolError::FileTooLarge {
            path: "/huge.bin".to_string(),
            size: 1_000_000,
            limit: 500_000,
        };
        assert_eq!(
            err.to_string(),
            "file too large: /huge.bin (1000000 bytes, limit 500000 bytes)"
        );

        // Test ScanLimitExceeded
        let err = ToolError::ScanLimitExceeded {
            found: 10_000,
            limit: 5_000,
        };
        assert_eq!(
            err.to_string(),
            "scan limit exceeded: found 10000 files, limit 5000"
        );

        // Test SensitiveFileDenied
        let err = ToolError::SensitiveFileDenied {
            path: "/.ssh/id_rsa".to_string(),
        };
        assert_eq!(err.to_string(), "sensitive file denied: /.ssh/id_rsa");

        // Test BinaryFileSkipped
        let err = ToolError::BinaryFileSkipped {
            path: "/binary.exe".to_string(),
        };
        assert_eq!(err.to_string(), "binary file skipped: /binary.exe");

        // Test ExecutionFailed
        let source_err: Box<dyn Error + Send + Sync> = Box::new(std::io::Error::other("io error"));
        let err = ToolError::ExecutionFailed { source: source_err };
        assert_eq!(err.to_string(), "tool execution failed");
    }

    #[test]
    fn test_tool_error_source_chain() {
        // Create a source error
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let source_err: Box<dyn Error + Send + Sync> = Box::new(io_err);

        // Create ExecutionFailed with source
        let err = ToolError::ExecutionFailed { source: source_err };

        // Verify error message
        assert_eq!(err.to_string(), "tool execution failed");

        // Verify source chain exists
        let source = err.source();
        assert!(
            source.is_some(),
            "ExecutionFailed should have a source error"
        );

        // Verify source message
        if let Some(src) = source {
            assert_eq!(src.to_string(), "access denied");
        }
    }
}
