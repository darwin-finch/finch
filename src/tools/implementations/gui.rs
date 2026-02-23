// GUI automation tools (macOS only)
//
// Provides three tools for controlling macOS GUI applications:
// - GuiClick: Click UI elements by coordinates
// - GuiType: Type text into focused fields
// - GuiInspect: Query UI hierarchy
//
// NOTE: Full implementation requires testing on macOS with proper accessibility permissions

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

/// GuiClick tool - Click UI elements by coordinates
pub struct GuiClickTool;

#[async_trait]
impl Tool for GuiClickTool {
    fn name(&self) -> &str {
        "gui_click"
    }

    fn description(&self) -> &str {
        "Click a UI element on macOS by coordinates (x, y). \
         Requires Accessibility permissions. \
         Example: Click at screen position (500, 300)"
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "x": {
                    "type": "number",
                    "description": "X coordinate on screen (pixels from left)"
                },
                "y": {
                    "type": "number",
                    "description": "Y coordinate on screen (pixels from top)"
                },
                "button": {
                    "type": "string",
                    "description": "Mouse button to click: 'left', 'right', or 'middle' (default: 'left')",
                    "enum": ["left", "right", "middle"]
                },
                "double_click": {
                    "type": "boolean",
                    "description": "Whether to perform a double-click (default: false)"
                }
            }),
            required: vec!["x".to_string(), "y".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let x = input["x"]
            .as_f64()
            .context("Missing or invalid 'x' parameter")?;
        let y = input["y"]
            .as_f64()
            .context("Missing or invalid 'y' parameter")?;
        let button = input["button"].as_str().unwrap_or("left");
        let double_click = input["double_click"].as_bool().unwrap_or(false);

        // Perform the click
        perform_click(x, y, button, double_click)?;

        Ok(format!(
            "✓ Clicked {} button at ({}, {}){}",
            button,
            x as i32,
            y as i32,
            if double_click { " (double-click)" } else { "" }
        ))
    }
}

/// GuiType tool - Type text into focused fields
pub struct GuiTypeTool;

#[async_trait]
impl Tool for GuiTypeTool {
    fn name(&self) -> &str {
        "gui_type"
    }

    fn description(&self) -> &str {
        "Type text into the currently focused text field on macOS. \
         Requires Accessibility permissions. \
         Example: Type 'hello world' into active field"
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "text": {
                    "type": "string",
                    "description": "Text to type into the focused field"
                },
                "delay_ms": {
                    "type": "number",
                    "description": "Delay between keystrokes in milliseconds (default: 0)"
                }
            }),
            required: vec!["text".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let text = input["text"]
            .as_str()
            .context("Missing or invalid 'text' parameter")?;
        let delay_ms = input["delay_ms"].as_u64().unwrap_or(0);

        // Type the text
        type_text(text, delay_ms)?;

        Ok(format!(
            "✓ Typed {} characters{}",
            text.len(),
            if delay_ms > 0 {
                format!(" with {}ms delay", delay_ms)
            } else {
                String::new()
            }
        ))
    }
}

/// GuiInspect tool - Query UI hierarchy
pub struct GuiInspectTool;

#[async_trait]
impl Tool for GuiInspectTool {
    fn name(&self) -> &str {
        "gui_inspect"
    }

    fn description(&self) -> &str {
        "Inspect the UI hierarchy on macOS to find window titles, button labels, etc. \
         Requires Accessibility permissions. \
         Returns information about visible windows and UI elements."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "query": {
                    "type": "string",
                    "description": "What to inspect: 'windows' (list windows), 'focused' (focused element), or 'screen' (screen info)"
                }
            }),
            required: vec!["query".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let query = input["query"]
            .as_str()
            .context("Missing or invalid 'query' parameter")?;

        match query {
            "screen" => inspect_screen(),
            "windows" => inspect_windows(),
            "focused" => inspect_focused(),
            _ => anyhow::bail!("Invalid query type. Use 'windows', 'focused', or 'screen'"),
        }
    }
}

/// Inspect screen information (works!)
fn inspect_screen() -> Result<String> {
    use core_graphics::display::CGDisplay;

    let main_display = CGDisplay::main();
    let bounds = main_display.bounds();

    Ok(format!(
        "Screen Information:\n\
         • Resolution: {}x{}\n\
         • Origin: ({}, {})\n\
         • Display ID: {}\n\
         • Active: {}\n\
         \n\
         This information can help determine click coordinates for gui_click.",
        bounds.size.width as u32,
        bounds.size.height as u32,
        bounds.origin.x as i32,
        bounds.origin.y as i32,
        main_display.id,
        if main_display.is_active() {
            "Yes"
        } else {
            "No"
        }
    ))
}

/// Perform a mouse click at the specified coordinates
fn perform_click(x: f64, y: f64, button: &str, double_click: bool) -> Result<()> {
    use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;

    // Parse button type
    let mouse_button = match button {
        "left" => CGMouseButton::Left,
        "right" => CGMouseButton::Right,
        "middle" => CGMouseButton::Center,
        _ => anyhow::bail!(
            "Invalid button type: {}. Use 'left', 'right', or 'middle'",
            button
        ),
    };

    // Determine event types based on button
    let (mouse_down_type, mouse_up_type) = match mouse_button {
        CGMouseButton::Left => (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp),
        CGMouseButton::Right => (CGEventType::RightMouseDown, CGEventType::RightMouseUp),
        CGMouseButton::Center => (CGEventType::OtherMouseDown, CGEventType::OtherMouseUp),
    };

    let point = CGPoint::new(x, y);
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .ok()
        .context("Failed to create event source - check Accessibility permissions")?;

    // First click
    let mouse_down = CGEvent::new_mouse_event(source.clone(), mouse_down_type, point, mouse_button)
        .ok()
        .context("Failed to create mouse down event")?;
    let mouse_up = CGEvent::new_mouse_event(source.clone(), mouse_up_type, point, mouse_button)
        .ok()
        .context("Failed to create mouse up event")?;

    mouse_down.post(CGEventTapLocation::HID);
    mouse_up.post(CGEventTapLocation::HID);

    // Second click if double-click requested
    if double_click {
        std::thread::sleep(std::time::Duration::from_millis(50)); // Brief delay between clicks

        let mouse_down2 =
            CGEvent::new_mouse_event(source.clone(), mouse_down_type, point, mouse_button)
                .ok()
                .context("Failed to create second mouse down event")?;
        let mouse_up2 = CGEvent::new_mouse_event(source, mouse_up_type, point, mouse_button)
            .ok()
            .context("Failed to create second mouse up event")?;

        mouse_down2.post(CGEventTapLocation::HID);
        mouse_up2.post(CGEventTapLocation::HID);
    }

    Ok(())
}

/// Type text by generating keyboard events
fn type_text(text: &str, delay_ms: u64) -> Result<()> {
    use core_graphics::event::{CGEvent, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .ok()
        .context("Failed to create event source - check Accessibility permissions")?;

    for ch in text.chars() {
        // Type the character using Unicode
        let key_down = CGEvent::new_keyboard_event(source.clone(), 0, true)
            .ok()
            .context("Failed to create key down event")?;
        key_down.set_string(ch.to_string().as_str());
        key_down.post(CGEventTapLocation::HID);

        let key_up = CGEvent::new_keyboard_event(source.clone(), 0, false)
            .ok()
            .context("Failed to create key up event")?;
        key_up.set_string(ch.to_string().as_str());
        key_up.post(CGEventTapLocation::HID);

        // Optional delay between keystrokes
        if delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
    }

    Ok(())
}

/// Inspect windows (basic implementation using AppleScript)
fn inspect_windows() -> Result<String> {
    // Use AppleScript to query window information
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(
            r#"
            tell application "System Events"
                set frontApp to name of first application process whose frontmost is true
                set appWindows to name of every window of application process frontApp
                return frontApp & "|" & (appWindows as string)
            end tell
        "#,
        )
        .output()
        .context("Failed to execute AppleScript - check permissions")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("AppleScript failed: {}", error);
    }

    let result = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = result.trim().split('|').collect();

    if parts.len() >= 2 {
        let app_name = parts[0];
        let windows = parts[1];

        Ok(format!(
            "Windows:\n\
             • Active Application: {}\n\
             • Windows: {}\n\
             \n\
             Note: This uses AppleScript. For advanced UI inspection,\n\
             use macOS Accessibility Inspector.app",
            app_name,
            if windows.is_empty() { "None" } else { windows }
        ))
    } else {
        Ok(format!(
            "Could not query windows.\n\
             Raw output: {}\n\
             \n\
             Ensure Accessibility permissions are granted.",
            result
        ))
    }
}

/// Inspect focused element (basic implementation)
fn inspect_focused() -> Result<String> {
    // Use AppleScript to query focused element
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(r#"
            tell application "System Events"
                set frontApp to name of first application process whose frontmost is true
                try
                    set focusedElement to description of (get value of attribute "AXFocusedUIElement" of application process frontApp)
                    return frontApp & "|" & focusedElement
                on error
                    return frontApp & "|No focused element"
                end try
            end tell
        "#)
        .output()
        .context("Failed to execute AppleScript - check permissions")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("AppleScript failed: {}", error);
    }

    let result = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = result.trim().split('|').collect();

    if parts.len() >= 2 {
        let app_name = parts[0];
        let focused = parts[1];

        Ok(format!(
            "Focused Element:\n\
             • Application: {}\n\
             • Element: {}\n\
             \n\
             Use this information to determine where gui_type will send text.",
            app_name, focused
        ))
    } else {
        Ok(format!(
            "Could not query focused element.\n\
             Raw output: {}\n\
             \n\
             Ensure Accessibility permissions are granted.",
            result
        ))
    }
}
