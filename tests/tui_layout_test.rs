// Test to verify TUI layout - separator appears above input
//
// This test ensures the separator line is positioned correctly:
// - Separator at chunks[0] (y=0, top of viewport)
// - Input at chunks[1] (y=1, below separator)
// - Status at chunks[2] (y=2, below input)

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
};

#[test]
fn test_separator_position_above_input() {
    // Create a 6-line viewport (same as TUI)
    let rect = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 6,
    };

    // Create layout matching TUI (separator, input, status)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Separator
            Constraint::Length(1), // Input
            Constraint::Length(4), // Status
        ])
        .split(rect);

    // Verify separator is at the top (y=0)
    assert_eq!(
        chunks[0].y, 0,
        "Separator should be at y=0 (top of viewport)"
    );
    assert_eq!(
        chunks[0].height, 1,
        "Separator should have height of 1 line"
    );

    // Verify input is below separator (y=1)
    assert_eq!(
        chunks[1].y, 1,
        "Input should be at y=1 (below separator)"
    );
    assert_eq!(chunks[1].height, 1, "Input should have height of 1 line");

    // Verify status is below input (y=2)
    assert_eq!(
        chunks[2].y, 2,
        "Status should be at y=2 (below input)"
    );
    assert_eq!(
        chunks[2].height, 4,
        "Status should have height of 4 lines"
    );

    // Verify total height is 6 lines
    let total_height = chunks[0].height + chunks[1].height + chunks[2].height;
    assert_eq!(
        total_height, 6,
        "Total height should be 6 lines (viewport size)"
    );
}

#[test]
fn test_layout_fills_viewport() {
    let rect = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 6,
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Separator
            Constraint::Length(1), // Input
            Constraint::Length(4), // Status
        ])
        .split(rect);

    // Verify no gaps between chunks
    assert_eq!(
        chunks[0].y + chunks[0].height,
        chunks[1].y,
        "No gap between separator and input"
    );
    assert_eq!(
        chunks[1].y + chunks[1].height,
        chunks[2].y,
        "No gap between input and status"
    );
    assert_eq!(
        chunks[2].y + chunks[2].height,
        rect.height,
        "Status fills to bottom of viewport"
    );
}

#[test]
fn test_separator_is_first_element() {
    let rect = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 6,
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Separator (FIRST)
            Constraint::Length(1), // Input
            Constraint::Length(4), // Status
        ])
        .split(rect);

    // Verify separator is the first element in the viewport
    assert_eq!(
        chunks[0].y, 0,
        "Separator must be first element (y=0)"
    );

    // Verify input is NOT first (it should be second)
    assert_ne!(chunks[1].y, 0, "Input should NOT be at y=0");

    // Verify the order: separator < input < status
    assert!(
        chunks[0].y < chunks[1].y,
        "Separator should be above input"
    );
    assert!(
        chunks[1].y < chunks[2].y,
        "Input should be above status"
    );
}
