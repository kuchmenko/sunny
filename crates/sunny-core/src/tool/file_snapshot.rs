use std::path::{Path, PathBuf};
use std::time::SystemTime;

use dashmap::DashMap;
use tracing::info;

use crate::events::{EVENT_TOOL_EXEC_END, EVENT_TOOL_EXEC_START, OUTCOME_SUCCESS};
use crate::tool::ToolError;
/// Default tolerance in milliseconds for stale-read detection (25ms).
#[allow(dead_code)]
const DEFAULT_TOLERANCE_MS: u64 = 25;

/// FileSnapshot captures the modification time (mtime) of a file at a point in time.
/// Used to detect if a file has been modified externally since the snapshot was taken.
#[derive(Debug, Clone)]
pub struct FileSnapshot {
    /// Path to the file being tracked.
    pub path: PathBuf,
    /// Modification time in nanoseconds since UNIX_EPOCH.
    pub mtime_ns: u64,
}

impl FileSnapshot {
    /// Capture the current modification time of a file.
    ///
    /// # Arguments
    /// * `path` - Path to the file to snapshot
    ///
    /// # Returns
    /// * `Ok(FileSnapshot)` - Snapshot with current mtime
    /// * `Err(ToolError)` - If file cannot be stat'd
    pub fn capture(path: &Path) -> Result<Self, ToolError> {
        let path_str = path.display().to_string();
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "file_snapshot", operation = "capture", path = %path.display());

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
        })?;

        let mtime = metadata
            .modified()
            .map_err(|e| ToolError::ExecutionFailed {
                source: Box::new(e),
            })?;

        let duration = mtime.duration_since(SystemTime::UNIX_EPOCH).map_err(|e| {
            ToolError::ExecutionFailed {
                source: Box::new(e),
            }
        })?;

        let mtime_ns = duration.as_nanos() as u64;

        info!(name: EVENT_TOOL_EXEC_END, tool_name = "file_snapshot", operation = "capture", outcome = OUTCOME_SUCCESS, mtime_ns = mtime_ns);

        Ok(FileSnapshot {
            path: path.to_path_buf(),
            mtime_ns,
        })
    }

    /// Check if the file has been modified since this snapshot was taken.
    ///
    /// # Arguments
    /// * `tolerance_ms` - Tolerance in milliseconds. If current mtime is within
    ///   tolerance_ms of the snapshot mtime, returns false (not stale).
    ///
    /// # Returns
    /// * `Ok(true)` - File has been modified beyond tolerance
    /// * `Ok(false)` - File has not been modified (within tolerance)
    /// * `Err(ToolError)` - If file cannot be stat'd
    pub fn is_stale(&self, tolerance_ms: u64) -> Result<bool, ToolError> {
        let path_str = self.path.display().to_string();
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "file_snapshot", operation = "is_stale", path = %self.path.display(), tolerance_ms = tolerance_ms);

        let metadata = std::fs::metadata(&self.path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ToolError::PathNotFound {
                path: path_str.clone(),
            },
            std::io::ErrorKind::PermissionDenied => ToolError::PermissionDenied {
                path: path_str.clone(),
            },
            _ => ToolError::ExecutionFailed {
                source: Box::new(e),
            },
        })?;

        let mtime = metadata
            .modified()
            .map_err(|e| ToolError::ExecutionFailed {
                source: Box::new(e),
            })?;

        let duration = mtime.duration_since(SystemTime::UNIX_EPOCH).map_err(|e| {
            ToolError::ExecutionFailed {
                source: Box::new(e),
            }
        })?;

        let current_mtime_ns = duration.as_nanos() as u64;
        let tolerance_ns = tolerance_ms * 1_000_000;

        let is_stale = current_mtime_ns > self.mtime_ns + tolerance_ns;

        info!(name: EVENT_TOOL_EXEC_END, tool_name = "file_snapshot", operation = "is_stale", outcome = OUTCOME_SUCCESS, is_stale = is_stale);

        Ok(is_stale)
    }

    /// Update the snapshot after a write operation.
    /// Re-stats the file and updates the mtime.
    ///
    /// # Returns
    /// * `Ok(())` - Snapshot updated successfully
    /// * `Err(ToolError)` - If file cannot be stat'd
    pub fn update_after_write(&mut self) -> Result<(), ToolError> {
        let path_str = self.path.display().to_string();
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "file_snapshot", operation = "update_after_write", path = %self.path.display());

        let metadata = std::fs::metadata(&self.path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ToolError::PathNotFound {
                path: path_str.clone(),
            },
            std::io::ErrorKind::PermissionDenied => ToolError::PermissionDenied {
                path: path_str.clone(),
            },
            _ => ToolError::ExecutionFailed {
                source: Box::new(e),
            },
        })?;

        let mtime = metadata
            .modified()
            .map_err(|e| ToolError::ExecutionFailed {
                source: Box::new(e),
            })?;

        let duration = mtime.duration_since(SystemTime::UNIX_EPOCH).map_err(|e| {
            ToolError::ExecutionFailed {
                source: Box::new(e),
            }
        })?;

        self.mtime_ns = duration.as_nanos() as u64;

        info!(name: EVENT_TOOL_EXEC_END, tool_name = "file_snapshot", operation = "update_after_write", outcome = OUTCOME_SUCCESS, mtime_ns = self.mtime_ns);

        Ok(())
    }
}

/// FileSnapshotStore provides thread-safe concurrent access to file snapshots.
/// Uses DashMap for lock-free concurrent reads and writes.
pub struct FileSnapshotStore {
    map: DashMap<PathBuf, FileSnapshot>,
}

impl FileSnapshotStore {
    /// Create a new empty FileSnapshotStore.
    pub fn new() -> Self {
        Self {
            map: DashMap::new(),
        }
    }

    /// Record a snapshot in the store, keyed by its path.
    ///
    /// # Arguments
    /// * `snapshot` - FileSnapshot to store
    pub fn record(&self, snapshot: FileSnapshot) {
        self.map.insert(snapshot.path.clone(), snapshot);
    }

    /// Retrieve a snapshot from the store by path.
    ///
    /// # Arguments
    /// * `path` - Path to look up
    ///
    /// # Returns
    /// * `Some(FileSnapshot)` - If snapshot exists
    /// * `None` - If no snapshot for this path
    pub fn get_snapshot(&self, path: &Path) -> Option<FileSnapshot> {
        self.map.get(path).map(|entry| entry.clone())
    }
}

impl Default for FileSnapshotStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::thread;
    use std::time::Duration;
    use tempfile::NamedTempFile;

    #[test]
    fn test_file_snapshot_capture_returns_mtime() {
        let mut temp_file = NamedTempFile::new().expect("failed to create temp file");
        temp_file
            .write_all(b"test content")
            .expect("failed to write to temp file");
        temp_file.flush().expect("failed to flush temp file");

        let path = temp_file.path();
        let snapshot = FileSnapshot::capture(path).expect("failed to capture snapshot");

        assert!(snapshot.mtime_ns > 0, "mtime_ns should be non-zero");
        assert_eq!(snapshot.path, path);
    }

    #[test]
    fn test_file_snapshot_detects_external_modification() {
        let mut temp_file = NamedTempFile::new().expect("failed to create temp file");
        temp_file
            .write_all(b"initial content")
            .expect("failed to write to temp file");
        temp_file.flush().expect("failed to flush temp file");

        let path = temp_file.path();
        let snapshot = FileSnapshot::capture(path).expect("failed to capture snapshot");

        // Sleep to ensure mtime changes (filesystem mtime granularity)
        thread::sleep(Duration::from_millis(50));

        // Modify the file externally
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("failed to open file for modification");
        file.write_all(b"modified content")
            .expect("failed to write modified content");
        file.flush().expect("failed to flush modified file");

        // Check if snapshot detects the modification
        let is_stale = snapshot
            .is_stale(DEFAULT_TOLERANCE_MS)
            .expect("failed to check staleness");
        assert!(is_stale, "snapshot should detect external modification");
    }

    #[test]
    fn test_file_snapshot_tolerance_prevents_false_positive() {
        let mut temp_file = NamedTempFile::new().expect("failed to create temp file");
        temp_file
            .write_all(b"test content")
            .expect("failed to write to temp file");
        temp_file.flush().expect("failed to flush temp file");

        let path = temp_file.path();
        let snapshot = FileSnapshot::capture(path).expect("failed to capture snapshot");

        // Immediately check staleness with high tolerance
        // Should return false because no time has passed
        let is_stale = snapshot
            .is_stale(1000) // 1000ms tolerance
            .expect("failed to check staleness");
        assert!(
            !is_stale,
            "snapshot should not be stale within tolerance window"
        );
    }
}
