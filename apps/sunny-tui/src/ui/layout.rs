//! Layout helpers for sunny-tui.
//!
//! Centers content with a max-width constraint, like web chat UIs.

use ratatui::layout::Rect;

/// Max content width before centering kicks in.
const MAX_CONTENT_WIDTH: u16 = 120;

/// Center the content area with a max width.
///
/// If terminal is wider than MAX_CONTENT_WIDTH, content is centered
/// with equal padding on both sides. Otherwise full width is used.
pub fn centered_content(area: Rect) -> Rect {
    if area.width <= MAX_CONTENT_WIDTH {
        return area;
    }
    let padding = (area.width - MAX_CONTENT_WIDTH) / 2;
    Rect::new(
        area.x + padding,
        area.y,
        MAX_CONTENT_WIDTH,
        area.height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_centered_narrow_terminal_returns_full_width() {
        let area = Rect::new(0, 0, 80, 24);
        let result = centered_content(area);
        assert_eq!(result, area);
    }

    #[test]
    fn test_centered_wide_terminal_constrains_width() {
        let area = Rect::new(0, 0, 200, 40);
        let result = centered_content(area);
        assert_eq!(result.width, MAX_CONTENT_WIDTH);
        assert_eq!(result.height, 40);
        assert_eq!(result.x, 40); // (200 - 120) / 2
    }

    #[test]
    fn test_centered_exact_width_returns_full() {
        let area = Rect::new(0, 0, MAX_CONTENT_WIDTH, 30);
        let result = centered_content(area);
        assert_eq!(result, area);
    }
}
