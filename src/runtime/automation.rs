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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationAvailability {
    pub state: AutomationState,
    pub backend: &'static str,
    pub operations: Vec<&'static str>,
}

/// Whether a native permission request was made for this process.
///
/// This is deliberately separate from [`AutomationState`]. Apple's prompt is
/// asynchronous, so requesting it is not evidence that Accessibility access
/// was granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationPromptDisposition {
    NotNeeded,
    Requested,
    SuppressedNonInteractive,
    SuppressedRemote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationPermissionResult {
    pub availability: AutomationAvailability,
    pub prompt: AutomationPromptDisposition,
}

/// Session facts that determine whether it is safe to raise a native prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutomationPromptContext {
    pub interactive: bool,
    pub remote_session: bool,
}

impl AutomationPromptContext {
    /// Build prompt policy for the current launch. SSH sessions never raise a
    /// prompt on the remote Mac's desktop, even when the SSH client has a TTY.
    pub fn for_current_session(interactive: bool) -> Self {
        use std::io::IsTerminal;

        Self {
            interactive: interactive
                && std::io::stdin().is_terminal()
                && std::io::stdout().is_terminal(),
            remote_session: std::env::var_os("SSH_CONNECTION").is_some()
                || std::env::var_os("SSH_TTY").is_some(),
        }
    }
}

fn permission_launch_context() -> String {
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        "SSH session".to_string()
    } else if let Some(program) = std::env::var_os("TERM_PROGRAM") {
        program.to_string_lossy().into_owned()
    } else if let Some(bundle) = std::env::var_os("__CFBundleIdentifier") {
        bundle.to_string_lossy().into_owned()
    } else {
        "unknown launcher".to_string()
    }
}

/// Conservative key for scoping prior permission observations. It is not a
/// TCC identity: macOS may choose a responsible app/signing identity that
/// Finch cannot derive from public Accessibility APIs.
pub fn permission_context_key() -> String {
    let executable = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown Finch executable".to_string());
    format!(
        "executable={executable};launcher={}",
        permission_launch_context()
    )
}

/// Human-readable context for the process asking macOS for TCC trust. The app
/// named by the native prompt/System Settings is authoritative; the executable
/// and launcher below are diagnostic hints, not a claim about responsible-code
/// attribution.
pub fn permission_target_description() -> String {
    let executable = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "the current Finch executable".to_string());
    format!(
        "current Finch process: PID {}\nexecutable: {executable}\nlauncher hint: {} (diagnostic only, not the macOS TCC identity)\nuse the app name macOS shows in its Accessibility prompt/Settings",
        std::process::id(),
        permission_launch_context()
    )
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

    /// Explicitly request the platform permission, when appropriate, and then
    /// perform a fresh passive check. This never generates automation input.
    pub fn request_permission(
        &self,
        context: AutomationPromptContext,
    ) -> AutomationPermissionResult {
        request_permission_flow(
            self.availability(),
            context,
            platform::request_permission,
            || self.availability(),
        )
    }

    /// Open the macOS Accessibility privacy pane after an explicit user action.
    /// Returns `false` when UI launch is suppressed for a remote/headless session.
    pub fn open_permission_settings(&self, context: AutomationPromptContext) -> Result<bool> {
        if !should_open_permission_settings(self.enabled, context) {
            return Ok(false);
        }
        platform::open_permission_settings()?;
        Ok(true)
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

fn should_open_permission_settings(enabled: bool, context: AutomationPromptContext) -> bool {
    enabled && context.interactive && !context.remote_session
}

fn request_permission_flow(
    before: AutomationAvailability,
    context: AutomationPromptContext,
    request: impl FnOnce(),
    verify: impl FnOnce() -> AutomationAvailability,
) -> AutomationPermissionResult {
    if before.state != AutomationState::PermissionRequired {
        return AutomationPermissionResult {
            availability: before,
            prompt: AutomationPromptDisposition::NotNeeded,
        };
    }
    if !context.interactive {
        return AutomationPermissionResult {
            availability: before,
            prompt: AutomationPromptDisposition::SuppressedNonInteractive,
        };
    }
    if context.remote_session {
        return AutomationPermissionResult {
            availability: before,
            prompt: AutomationPromptDisposition::SuppressedRemote,
        };
    }

    request();
    AutomationPermissionResult {
        // AXIsProcessTrustedWithOptions documents that prompting is
        // asynchronous and does not affect its return value. Re-check via the
        // passive API instead of interpreting "prompt requested" as consent.
        availability: verify(),
        prompt: AutomationPromptDisposition::Requested,
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use anyhow::Context;
    use core_foundation::base::{Boolean, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::{CFString, CFStringRef};
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
        fn AXIsProcessTrusted() -> Boolean;
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> Boolean;
        static kAXTrustedCheckOptionPrompt: CFStringRef;
    }

    pub fn availability() -> AutomationAvailability {
        let trusted = unsafe { AXIsProcessTrusted() != 0 };
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

    pub fn request_permission() {
        // kAXTrustedCheckOptionPrompt is a framework-owned CFString. The get
        // rule balances the wrapper without taking ownership of that static.
        let prompt_key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
        let options = CFDictionary::from_CFType_pairs(&[(prompt_key, CFBoolean::true_value())]);
        // Apple documents this call as an asynchronous prompt request. Its
        // Boolean return is intentionally ignored; the caller verifies trust
        // with AXIsProcessTrusted after this function returns.
        let _ = unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) };
    }

    pub fn open_permission_settings() -> Result<()> {
        let status = std::process::Command::new("/usr/bin/open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .status()
            .context("failed to open macOS Accessibility settings")?;
        if !status.success() {
            bail!("macOS could not open Accessibility settings (status {status})");
        }
        Ok(())
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

    pub fn request_permission() {}

    pub fn open_permission_settings() -> Result<()> {
        bail!("Accessibility settings are unsupported on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn availability(state: AutomationState) -> AutomationAvailability {
        AutomationAvailability {
            state,
            backend: if matches!(
                state,
                AutomationState::Available | AutomationState::PermissionRequired
            ) {
                "test-native"
            } else {
                "none"
            },
            operations: if state == AutomationState::Available {
                vec!["click"]
            } else {
                Vec::new()
            },
        }
    }

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

    #[test]
    fn permission_flow_maps_disabled_and_unsupported_without_prompting() {
        for state in [AutomationState::Disabled, AutomationState::Unsupported] {
            let prompted = Cell::new(false);
            let result = request_permission_flow(
                availability(state),
                AutomationPromptContext {
                    interactive: true,
                    remote_session: false,
                },
                || prompted.set(true),
                || panic!("verification should not run"),
            );
            assert_eq!(result.availability.state, state);
            assert_eq!(result.prompt, AutomationPromptDisposition::NotNeeded);
            assert!(!prompted.get());
        }
    }

    #[test]
    fn permission_flow_reports_requested_but_keeps_denied_state_truthful() {
        let prompted = Cell::new(false);
        let result = request_permission_flow(
            availability(AutomationState::PermissionRequired),
            AutomationPromptContext {
                interactive: true,
                remote_session: false,
            },
            || prompted.set(true),
            || availability(AutomationState::PermissionRequired),
        );
        assert!(prompted.get());
        assert_eq!(result.prompt, AutomationPromptDisposition::Requested);
        assert_eq!(
            result.availability.state,
            AutomationState::PermissionRequired
        );
    }

    #[test]
    fn permission_flow_verifies_granted_state_after_request() {
        let result = request_permission_flow(
            availability(AutomationState::PermissionRequired),
            AutomationPromptContext {
                interactive: true,
                remote_session: false,
            },
            || {},
            || availability(AutomationState::Available),
        );
        assert_eq!(result.prompt, AutomationPromptDisposition::Requested);
        assert_eq!(result.availability.state, AutomationState::Available);
    }

    #[test]
    fn permission_flow_treats_revoked_grant_as_permission_required() {
        let result = request_permission_flow(
            availability(AutomationState::PermissionRequired),
            AutomationPromptContext {
                interactive: false,
                remote_session: false,
            },
            || panic!("headless flow must not prompt"),
            || panic!("headless flow must not verify after a prompt"),
        );
        assert_eq!(
            result.availability.state,
            AutomationState::PermissionRequired
        );
        assert_eq!(
            result.prompt,
            AutomationPromptDisposition::SuppressedNonInteractive
        );
    }

    #[test]
    fn permission_flow_suppresses_remote_prompt() {
        let result = request_permission_flow(
            availability(AutomationState::PermissionRequired),
            AutomationPromptContext {
                interactive: true,
                remote_session: true,
            },
            || panic!("remote flow must not prompt"),
            || panic!("remote flow must not verify after a prompt"),
        );
        assert_eq!(result.prompt, AutomationPromptDisposition::SuppressedRemote);
    }

    #[test]
    fn permission_flow_preserves_available_state_without_prompting() {
        let result = request_permission_flow(
            availability(AutomationState::Available),
            AutomationPromptContext {
                interactive: true,
                remote_session: false,
            },
            || panic!("available flow must not prompt"),
            || panic!("available flow must not verify after a prompt"),
        );
        assert_eq!(result.availability.state, AutomationState::Available);
        assert_eq!(result.prompt, AutomationPromptDisposition::NotNeeded);
    }

    #[test]
    fn settings_action_policy_suppresses_disabled_headless_and_remote_launches() {
        for (enabled, context) in [
            (
                false,
                AutomationPromptContext {
                    interactive: true,
                    remote_session: false,
                },
            ),
            (
                true,
                AutomationPromptContext {
                    interactive: false,
                    remote_session: false,
                },
            ),
            (
                true,
                AutomationPromptContext {
                    interactive: true,
                    remote_session: true,
                },
            ),
        ] {
            assert!(!should_open_permission_settings(enabled, context));
        }
        assert!(should_open_permission_settings(
            true,
            AutomationPromptContext {
                interactive: true,
                remote_session: false,
            }
        ));
    }

    #[test]
    fn permission_target_is_explicitly_non_authoritative() {
        let description = permission_target_description();
        assert!(description.contains("executable:"));
        assert!(description.contains(&format!(
            "current Finch process: PID {}",
            std::process::id()
        )));
        assert!(description.contains("launcher hint:"));
        assert!(description.contains("diagnostic only, not the macOS TCC identity"));
        assert!(description.contains("use the app name macOS shows"));
    }
}
