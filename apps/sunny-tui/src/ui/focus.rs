//! Focus state machine for sunny-tui.
//!
//! `Focus` controls which widget receives keyboard input.
//! Global key intercepts run BEFORE focus-based dispatch.

use crossterm::event::{KeyCode, KeyModifiers};

/// Current keyboard focus.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Focus {
    /// User text input field (tui-textarea).
    #[default]
    Input,
    /// Scrollable thread view.
    ThreadView,
    /// Approval overlay — blocks all other input.
    ApprovalOverlay,
    /// Interview card overlay — blocks all other input until answered.
    InterviewCard,
    /// Session manager overlay.
    SessionManager,
}

/// The kind of action to take for a given key in the current focus context.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyDispatch {
    /// Pass key to the textarea.
    TextInput,
    /// Scroll thread view up by n lines.
    ScrollUp(usize),
    /// Scroll thread view down by n lines.
    ScrollDown(usize),
    /// Handle approval y/n/a key.
    Approval(char),
    /// Key consumed but no specific action.
    Consumed,
}

/// Dispatch a non-intercepted key based on current focus.
///
/// Returns how the App should handle the key.
/// Global intercepts (Ctrl+Q, Ctrl+C, Tab, Esc, Enter send, Ctrl+J newline) are handled
/// in App::handle_event() BEFORE this function is called.
#[allow(dead_code)]
pub fn dispatch_key(focus: &Focus, code: KeyCode, _modifiers: KeyModifiers) -> KeyDispatch {
    match focus {
        Focus::Input => KeyDispatch::TextInput,
        Focus::ThreadView => match code {
            KeyCode::Up => KeyDispatch::ScrollUp(3),
            KeyCode::Down => KeyDispatch::ScrollDown(3),
            KeyCode::PageUp => KeyDispatch::ScrollUp(10),
            KeyCode::PageDown => KeyDispatch::ScrollDown(10),
            _ => KeyDispatch::Consumed,
        },
        Focus::ApprovalOverlay => match code {
            KeyCode::Char(c @ ('y' | 'n' | 'a')) => KeyDispatch::Approval(c),
            _ => KeyDispatch::Consumed,
        },
        Focus::InterviewCard | Focus::SessionManager => KeyDispatch::Consumed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_focus_default_is_input() {
        assert_eq!(Focus::default(), Focus::Input);
    }

    #[test]
    fn test_focus_all_variants_construct() {
        let _ = Focus::Input;
        let _ = Focus::ThreadView;
        let _ = Focus::ApprovalOverlay;
    }

    #[test]
    fn test_dispatch_key_input_focus_returns_text_input() {
        let dispatch = dispatch_key(&Focus::Input, KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(dispatch, KeyDispatch::TextInput);
    }

    #[test]
    fn test_dispatch_key_thread_view_arrows() {
        assert_eq!(
            dispatch_key(&Focus::ThreadView, KeyCode::Up, KeyModifiers::NONE),
            KeyDispatch::ScrollUp(3)
        );
        assert_eq!(
            dispatch_key(&Focus::ThreadView, KeyCode::Down, KeyModifiers::NONE),
            KeyDispatch::ScrollDown(3)
        );
        assert_eq!(
            dispatch_key(&Focus::ThreadView, KeyCode::PageUp, KeyModifiers::NONE),
            KeyDispatch::ScrollUp(10)
        );
        assert_eq!(
            dispatch_key(&Focus::ThreadView, KeyCode::PageDown, KeyModifiers::NONE),
            KeyDispatch::ScrollDown(10)
        );
    }

    #[test]
    fn test_dispatch_key_approval_y_n_a() {
        assert_eq!(
            dispatch_key(
                &Focus::ApprovalOverlay,
                KeyCode::Char('y'),
                KeyModifiers::NONE
            ),
            KeyDispatch::Approval('y')
        );
        assert_eq!(
            dispatch_key(
                &Focus::ApprovalOverlay,
                KeyCode::Char('n'),
                KeyModifiers::NONE
            ),
            KeyDispatch::Approval('n')
        );
        assert_eq!(
            dispatch_key(
                &Focus::ApprovalOverlay,
                KeyCode::Char('a'),
                KeyModifiers::NONE
            ),
            KeyDispatch::Approval('a')
        );
    }
}
