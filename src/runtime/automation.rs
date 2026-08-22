//! Optional, native desktop-automation capability.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationState {
    Disabled,
    Unsupported,
    PermissionRequired,
    Available,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationAvailability {
    pub state: AutomationState,
    pub backend: &'static str,
    pub operations: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum AutomationRequest {
    Availability,
    Displays,
    Windows,
    Click {
        x: f64,
        y: f64,
        #[serde(default = "default_button")]
        button: String,
        #[serde(default = "default_click_count")]
        count: u8,
    },
    Type {
        text: String,
        #[serde(default)]
        delay_ms: u64,
    },
}

fn default_button() -> String {
    "left".to_string()
}

const fn default_click_count() -> u8 {
    1
}

/// Configuration gate and platform dispatcher for automation operations.
#[derive(Debug)]
pub struct AutomationBroker {
    enabled: bool,
}

impl AutomationBroker {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    pub fn availability(&self) -> AutomationAvailability {
        if !self.enabled {
            return AutomationAvailability {
                state: AutomationState::Disabled,
                backend: "none",
                operations: Vec::new(),
            };
        }
        platform::availability()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn execute(&self, request: AutomationRequest) -> Result<Value> {
        if matches!(request, AutomationRequest::Availability) {
            return Ok(serde_json::to_value(self.availability())?);
        }
        let availability = self.availability();
        if availability.state != AutomationState::Available {
            bail!(
                "automation unavailable: {}",
                serde_json::to_string(&availability)?
            );
        }
        platform::execute(request)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use anyhow::Context;
    use core_graphics::display::CGDisplay;
    use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;
    use core_graphics::window::{
        create_window_list, kCGNullWindowID, kCGWindowListExcludeDesktopElements,
        kCGWindowListOptionOnScreenOnly,
    };

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    pub fn availability() -> AutomationAvailability {
        let trusted = unsafe { AXIsProcessTrusted() };
        AutomationAvailability {
            state: if trusted {
                AutomationState::Available
            } else {
                AutomationState::PermissionRequired
            },
            backend: "macos-native",
            operations: vec!["displays", "windows", "click", "type"],
        }
    }

    pub fn execute(request: AutomationRequest) -> Result<Value> {
        match request {
            AutomationRequest::Availability => unreachable!(),
            AutomationRequest::Displays => displays(),
            AutomationRequest::Windows => windows(),
            AutomationRequest::Click {
                x,
                y,
                button,
                count,
            } => click(x, y, &button, count),
            AutomationRequest::Type { text, delay_ms } => type_text(&text, delay_ms),
        }
    }

    fn displays() -> Result<Value> {
        let display = CGDisplay::main();
        let bounds = display.bounds();
        Ok(json!({
            "displays": [{
                "id": display.id,
                "main": true,
                "active": display.is_active(),
                "x": bounds.origin.x,
                "y": bounds.origin.y,
                "width": bounds.size.width,
                "height": bounds.size.height
            }]
        }))
    }

    fn windows() -> Result<Value> {
        let options = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements;
        let windows = create_window_list(options, kCGNullWindowID)
            .context("CoreGraphics did not return a window list")?;
        let ids = windows.iter().map(|id| *id).collect::<Vec<_>>();
        Ok(json!({ "window_ids": ids }))
    }

    fn click(x: f64, y: f64, button: &str, count: u8) -> Result<Value> {
        if !(1..=3).contains(&count) {
            bail!("click count must be between 1 and 3");
        }
        let button = match button {
            "left" => CGMouseButton::Left,
            "right" => CGMouseButton::Right,
            "middle" => CGMouseButton::Center,
            other => bail!("unsupported mouse button: {other}"),
        };
        let (down, up) = match button {
            CGMouseButton::Left => (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp),
            CGMouseButton::Right => (CGEventType::RightMouseDown, CGEventType::RightMouseUp),
            CGMouseButton::Center => (CGEventType::OtherMouseDown, CGEventType::OtherMouseUp),
        };
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok()
            .context("failed to create native event source")?;
        let point = CGPoint::new(x, y);
        for click_index in 1..=count {
            let mouse_down = CGEvent::new_mouse_event(source.clone(), down, point, button)
                .ok()
                .context("failed to create mouse-down event")?;
            mouse_down.set_integer_value_field(
                core_graphics::event::EventField::MOUSE_EVENT_CLICK_STATE,
                click_index.into(),
            );
            mouse_down.post(CGEventTapLocation::HID);
            let mouse_up = CGEvent::new_mouse_event(source.clone(), up, point, button)
                .ok()
                .context("failed to create mouse-up event")?;
            mouse_up.set_integer_value_field(
                core_graphics::event::EventField::MOUSE_EVENT_CLICK_STATE,
                click_index.into(),
            );
            mouse_up.post(CGEventTapLocation::HID);
        }
        Ok(json!({ "clicked": { "x": x, "y": y, "count": count } }))
    }

    fn type_text(text: &str, delay_ms: u64) -> Result<Value> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok()
            .context("failed to create native event source")?;
        for character in text.chars() {
            let key_down = CGEvent::new_keyboard_event(source.clone(), 0, true)
                .ok()
                .context("failed to create key-down event")?;
            key_down.set_string(&character.to_string());
            key_down.post(CGEventTapLocation::HID);
            let key_up = CGEvent::new_keyboard_event(source.clone(), 0, false)
                .ok()
                .context("failed to create key-up event")?;
            key_up.set_string(&character.to_string());
            key_up.post(CGEventTapLocation::HID);
            if delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
        }
        Ok(json!({ "typed_characters": text.chars().count() }))
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub fn availability() -> AutomationAvailability {
        AutomationAvailability {
            state: AutomationState::Unsupported,
            backend: "none",
            operations: Vec::new(),
        }
    }

    pub fn execute(_request: AutomationRequest) -> Result<Value> {
        bail!("native desktop automation is unsupported on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_broker_advertises_nothing() {
        let broker = AutomationBroker::new(false);
        assert_eq!(broker.availability().state, AutomationState::Disabled);
        assert!(broker.availability().operations.is_empty());
    }

    #[test]
    fn disabled_broker_rejects_mutation() {
        let broker = AutomationBroker::new(false);
        let error = broker
            .execute(AutomationRequest::Click {
                x: 1.0,
                y: 2.0,
                button: "left".to_string(),
                count: 1,
            })
            .unwrap_err();
        assert!(error.to_string().contains("automation unavailable"));
    }
}
