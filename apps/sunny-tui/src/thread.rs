//! Thread data model for sunny-tui.
//!
//! A Thread represents a bounded work unit wrapping an AgentSession.
//! V1: single thread. V2: multiple concurrent threads.

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Lifecycle state of a thread.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadStatus {
    /// Actively streaming a response.
    Active,
    /// Idle, waiting for user input.
    Idle,
    /// Paused by user or system.
    Paused,
    /// Waiting for approval of a tool call.
    WaitingApproval,
    /// Completed successfully.
    Completed,
    /// Failed with an error.
    Failed,
}

/// A single message in the thread.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum ThreadMessage {
    User {
        content: String,
        timestamp: DateTime<Utc>,
    },
    Assistant {
        /// Accumulated response content (markdown).
        content: String,
        /// Thinking/reasoning content (dimmed during stream, hidden after).
        thinking: String,
        /// Tool calls made in this response.
        tool_calls: Vec<ToolCallDisplay>,
        timestamp: DateTime<Utc>,
        /// True while streaming is in progress.
        is_streaming: bool,
    },
    System {
        content: String,
        timestamp: DateTime<Utc>,
    },
}

/// Status of a single tool call invocation.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallStatus {
    /// Tool is currently executing.
    Running,
    /// Tool completed successfully.
    Completed,
    /// Tool execution failed.
    Failed,
}

/// Display state for a single tool call in the thread view.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ToolCallDisplay {
    /// Unique tool call ID.
    pub id: String,
    /// Tool name (e.g. "shell_exec", "fs_read").
    pub name: String,
    /// Short preview of arguments (truncated JSON).
    pub args_preview: String,
    /// Full arguments JSON.
    pub full_args: String,
    /// Tool result text, set when completed.
    pub result: Option<String>,
    /// Current execution status.
    pub status: ToolCallStatus,
    /// Whether the detail view is collapsed (default: true after completion).
    pub collapsed: bool,
}

#[allow(dead_code)]
impl ToolCallDisplay {
    /// Create a new running tool call.
    pub fn new_running(
        id: impl Into<String>,
        name: impl Into<String>,
        args: impl Into<String>,
    ) -> Self {
        let full_args = args.into();
        let args_preview: String = full_args.chars().take(60).collect();
        let args_preview = if full_args.len() > 60 {
            format!("{args_preview}…")
        } else {
            args_preview
        };
        Self {
            id: id.into(),
            name: name.into(),
            args_preview,
            full_args,
            result: None,
            status: ToolCallStatus::Running,
            collapsed: false,
        }
    }

    /// Mark as completed with a result.
    pub fn complete(mut self, result: impl Into<String>) -> Self {
        self.result = Some(result.into());
        self.status = ToolCallStatus::Completed;
        self.collapsed = true;
        self
    }

    /// Mark as failed with an error message.
    pub fn fail(mut self, error: impl Into<String>) -> Self {
        self.result = Some(error.into());
        self.status = ToolCallStatus::Failed;
        self.collapsed = true;
        self
    }

    /// Toggle expand/collapse state.
    pub fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
    }
}

/// A thread represents a bounded work unit with lifecycle.
///
/// V1: one active thread at a time.
/// V2: multiple concurrent threads in a tree view.
#[allow(dead_code)]
pub struct Thread {
    /// Unique thread ID.
    pub id: Uuid,
    /// Human-readable thread title (auto-generated or user-set).
    pub title: String,
    /// Current lifecycle status.
    pub status: ThreadStatus,
    /// All messages in this thread.
    pub messages: Vec<ThreadMessage>,
    /// Thread creation time.
    pub created_at: DateTime<Utc>,
    /// Last updated time.
    pub updated_at: DateTime<Utc>,
}

#[allow(dead_code)]
impl Thread {
    /// Create a new empty thread.
    pub fn new(title: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            status: ThreadStatus::Idle,
            messages: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Add a user message to the thread.
    pub fn add_user_message(&mut self, content: impl Into<String>) {
        self.messages.push(ThreadMessage::User {
            content: content.into(),
            timestamp: Utc::now(),
        });
        self.updated_at = Utc::now();
    }

    /// Start a new streaming assistant message.
    pub fn start_assistant_message(&mut self) {
        self.status = ThreadStatus::Active;
        self.messages.push(ThreadMessage::Assistant {
            content: String::new(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            timestamp: Utc::now(),
            is_streaming: true,
        });
        self.updated_at = Utc::now();
    }

    /// Get a mutable reference to the last assistant message if it's streaming.
    pub fn current_assistant_message_mut(&mut self) -> Option<&mut ThreadMessage> {
        self.messages.iter_mut().rev().find(|m| {
            matches!(
                m,
                ThreadMessage::Assistant {
                    is_streaming: true,
                    ..
                }
            )
        })
    }

    /// Finish the current streaming assistant message.
    /// Marks all running tool calls as completed (no result text available from streaming).
    pub fn finish_assistant_message(&mut self) {
        if let Some(ThreadMessage::Assistant {
            is_streaming,
            tool_calls,
            ..
        }) = self.current_assistant_message_mut()
        {
            *is_streaming = false;
            // Mark any running tool calls as completed
            for tc in tool_calls.iter_mut() {
                if tc.status == ToolCallStatus::Running {
                    tc.status = ToolCallStatus::Completed;
                    tc.collapsed = true;
                    if tc.result.is_none() {
                        tc.result = Some("done".to_owned());
                    }
                }
            }
        }
        self.status = ThreadStatus::Idle;
        self.updated_at = Utc::now();
    }

    /// Finish the current streaming assistant message with an error.
    /// If no streaming message exists, pushes a new assistant message with the error.
    pub fn finish_assistant_message_with_error(&mut self, error: &str) {
        if let Some(ThreadMessage::Assistant {
            content,
            is_streaming,
            tool_calls,
            ..
        }) = self.current_assistant_message_mut()
        {
            if content.is_empty() {
                *content = error.to_owned();
            } else {
                content.push_str(&format!("\n\n{error}"));
            }
            *is_streaming = false;
            for tc in tool_calls.iter_mut() {
                if tc.status == ToolCallStatus::Running {
                    tc.status = ToolCallStatus::Failed;
                    tc.collapsed = true;
                }
            }
        } else {
            self.messages.push(ThreadMessage::Assistant {
                content: error.to_owned(),
                thinking: String::new(),
                tool_calls: Vec::new(),
                timestamp: Utc::now(),
                is_streaming: false,
            });
        }
        self.status = ThreadStatus::Idle;
        self.updated_at = Utc::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_new_is_idle() {
        let thread = Thread::new("My Thread");
        assert_eq!(thread.status, ThreadStatus::Idle);
        assert!(thread.messages.is_empty());
        assert_eq!(thread.title, "My Thread");
    }

    #[test]
    fn test_thread_add_user_message() {
        let mut thread = Thread::new("Test");
        thread.add_user_message("hello");
        assert_eq!(thread.messages.len(), 1);
        assert!(matches!(thread.messages[0], ThreadMessage::User { .. }));
    }

    #[test]
    fn test_thread_start_assistant_message_changes_status() {
        let mut thread = Thread::new("Test");
        thread.start_assistant_message();
        assert_eq!(thread.status, ThreadStatus::Active);
        assert_eq!(thread.messages.len(), 1);
        assert!(matches!(
            thread.messages[0],
            ThreadMessage::Assistant {
                is_streaming: true,
                ..
            }
        ));
    }

    #[test]
    fn test_thread_finish_assistant_message_restores_idle() {
        let mut thread = Thread::new("Test");
        thread.start_assistant_message();
        thread.finish_assistant_message();
        assert_eq!(thread.status, ThreadStatus::Idle);
        assert!(matches!(
            thread.messages[0],
            ThreadMessage::Assistant {
                is_streaming: false,
                ..
            }
        ));
    }

    #[test]
    fn test_tool_call_display_new_running() {
        let tc = ToolCallDisplay::new_running("id1", "fs_read", r#"{"path":"/foo"}"#);
        assert_eq!(tc.status, ToolCallStatus::Running);
        assert!(!tc.collapsed);
        assert!(tc.result.is_none());
    }

    #[test]
    fn test_tool_call_display_complete_collapses() {
        let tc = ToolCallDisplay::new_running("id1", "fs_read", "{}").complete("42 lines");
        assert_eq!(tc.status, ToolCallStatus::Completed);
        assert!(tc.collapsed);
        assert_eq!(tc.result.as_deref(), Some("42 lines"));
    }

    #[test]
    fn test_tool_call_display_fail_collapses_in_failed_state() {
        let tc = ToolCallDisplay::new_running("id1", "shell_exec", "{}").fail("exit code 1");
        assert_eq!(tc.status, ToolCallStatus::Failed);
        assert!(tc.collapsed);
    }

    #[test]
    fn test_tool_call_display_toggle_collapsed() {
        let mut tc = ToolCallDisplay::new_running("id1", "fs_read", "{}").complete("done");
        assert!(tc.collapsed);
        tc.toggle_collapsed();
        assert!(!tc.collapsed);
        tc.toggle_collapsed();
        assert!(tc.collapsed);
    }

    #[test]
    fn test_tool_call_args_preview_truncates() {
        let long_args = "x".repeat(100);
        let tc = ToolCallDisplay::new_running("id1", "test", &long_args);
        assert!(tc.args_preview.chars().count() <= 61);
        assert!(tc.args_preview.ends_with('…'));
    }

    #[test]
    fn test_thread_status_all_variants_construct() {
        let _ = ThreadStatus::Active;
        let _ = ThreadStatus::Idle;
        let _ = ThreadStatus::Paused;
        let _ = ThreadStatus::WaitingApproval;
        let _ = ThreadStatus::Completed;
        let _ = ThreadStatus::Failed;
    }
}
