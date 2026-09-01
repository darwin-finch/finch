// Output Manager - Buffers output AND writes to stdout for scrollback
//
// This module provides an abstraction layer that captures all output
// (user messages, Claude responses, tool output, status info, errors)
// into a structured buffer AND writes it to stdout immediately with ANSI colors.
// This enables terminal scrollback while maintaining TUI compatibility.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};
use std::sync::{Arc, Mutex, RwLock};

use crate::cli::messages::{
    ActivityMessage, BrainParticipantMessage, LiveToolMessage, Message, MessageId, MessageRef,
    OperationMessage, ProgramOutputMessage, ProgramSourceMessage, StaticMessage,
    StreamingResponseMessage, UserQueryMessage, WorkUnit,
};
use crate::runtime::VmEffectEnvelope;
use crate::vm::{HostSideEffect, TypedValue, UiOperation, VmSideEffect};

/// Maximum number of messages to keep in the circular buffer
const MAX_BUFFER_SIZE: usize = 1000;

/// Host-side projection of portable VM output events into Finch's reactive
/// scrollback. The VM itself never imports `WorkUnit` or terminal rendering;
/// this adapter owns the handle-to-view mapping for one attached client.
#[derive(Clone)]
pub struct VmOutputProjection {
    output: Arc<OutputManager>,
    default_response: VmDefaultResponse,
    handles: Arc<Mutex<HashMap<String, Arc<ProgramOutputMessage>>>>,
    /// The portable effect protocol is at-least-once at the application
    /// boundary: a reconnecting host may replay a journal suffix.  Keep the
    /// per-run cursor with this client-local projection so a duplicate cannot
    /// append a second response chunk or advance a progress display twice.
    ///
    /// This is intentionally not the durable cursor.  A Brain/application
    /// event log will persist that acknowledgement; this guard protects a
    /// live attached client while the runtime remains embedder-neutral.
    next_effect_sequence: Arc<Mutex<HashMap<uuid::Uuid, u64>>>,
    /// Effects may cross a host/event-loop boundary from different workers.
    /// Retain a suffix that arrives before its prefix instead of dropping it:
    /// once the missing event arrives, the whole contiguous run is projected
    /// in journal order. Durable acknowledgement/replay is still owned by the
    /// later Brain event log, but a live client must not lose output merely
    /// because its local delivery was reordered.
    pending_effects: Arc<Mutex<HashMap<uuid::Uuid, BTreeMap<u64, VmEffectEnvelope>>>>,
}

#[derive(Clone)]
#[doc(hidden)]
pub enum VmDefaultResponse {
    Program(Arc<ProgramOutputMessage>),
    LegacyWorkUnit(Arc<WorkUnit>),
    ToolActivity { unit: Arc<WorkUnit>, row_idx: usize },
}

/// Compatibility adapter accepted by [`VmOutputProjection::new`].
pub trait IntoVmDefaultResponse {
    #[doc(hidden)]
    fn into_vm_default_response(self) -> VmDefaultResponse;
}

impl IntoVmDefaultResponse for Arc<ProgramOutputMessage> {
    fn into_vm_default_response(self) -> VmDefaultResponse {
        VmDefaultResponse::Program(self)
    }
}

impl IntoVmDefaultResponse for Arc<WorkUnit> {
    fn into_vm_default_response(self) -> VmDefaultResponse {
        VmDefaultResponse::LegacyWorkUnit(self)
    }
}

impl VmDefaultResponse {
    fn append(&self, text: &str) {
        match self {
            Self::Program(message) => {
                message.append_output(text);
            }
            Self::LegacyWorkUnit(unit) => {
                unit.append_response(text);
            }
            Self::ToolActivity { unit, row_idx } => {
                unit.append_row_body_line(*row_idx, text.to_string());
            }
        }
    }
}

impl std::fmt::Debug for VmOutputProjection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Projections deliberately contain client-local UI state. Do not leak
        // a WorkUnit or OutputManager through debug logs merely because an
        // effect is travelling over the event-loop bus.
        formatter
            .debug_struct("VmOutputProjection")
            .field(
                "open_handles",
                &self
                    .handles
                    .lock()
                    .map(|handles| handles.len())
                    .unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}

impl VmOutputProjection {
    pub fn new<T: IntoVmDefaultResponse>(output: Arc<OutputManager>, default_response: T) -> Self {
        let default_response = match default_response.into_vm_default_response() {
            VmDefaultResponse::LegacyWorkUnit(unit)
                if unit.status() != crate::cli::messages::MessageStatus::InProgress =>
            {
                VmDefaultResponse::Program(output.start_program_output())
            }
            default_response => default_response,
        };
        Self {
            output,
            default_response,
            handles: Arc::new(Mutex::new(HashMap::new())),
            next_effect_sequence: Arc::new(Mutex::new(HashMap::new())),
            pending_effects: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Bind VM effects to a genuine tool activity without creating a second
    /// top-level program-output artifact for the tool's internal execution.
    pub fn for_tool_activity(
        output: Arc<OutputManager>,
        activity: Arc<WorkUnit>,
        row_idx: usize,
    ) -> Self {
        Self {
            output,
            default_response: VmDefaultResponse::ToolActivity {
                unit: activity,
                row_idx,
            },
            handles: Arc::new(Mutex::new(HashMap::new())),
            next_effect_sequence: Arc::new(Mutex::new(HashMap::new())),
            pending_effects: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Project a portable, correlated VM effect exactly once and in its
    /// journal order. Returns every effect newly applied by this call. A
    /// replayed effect returns an empty vector; an out-of-order suffix is
    /// retained until its missing prefix arrives. Returning the complete
    /// drained prefix lets the event loop apply host-specific lifecycle work
    /// (such as a proposal request) for buffered effects as well as render
    /// their output.
    pub fn project_envelope(&self, envelope: VmEffectEnvelope) -> Vec<VmEffectEnvelope> {
        let execution_id = envelope.execution_id;
        let mut cursors = self
            .next_effect_sequence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut pending = self
            .pending_effects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let expected = cursors.entry(execution_id).or_insert(0);
        if envelope.effect.sequence < *expected {
            return Vec::new();
        }
        let queue = pending.entry(execution_id).or_default();
        if queue.contains_key(&envelope.effect.sequence) {
            // A duplicate future event is still pending; retain the first
            // copy and wait for the missing prefix.
            return Vec::new();
        }
        queue.insert(envelope.effect.sequence, envelope);

        let mut projected = Vec::new();
        while let Some(next) = queue.remove(expected) {
            // Keep the cursor lock while projecting. This makes the sequence
            // guard meaningful even if a non-terminal embedder calls this
            // adapter from several worker threads: later events cannot render
            // ahead of an earlier event that has reserved its sequence.
            self.project_for_execution(Some(execution_id), &next.effect);
            projected.push(next);
            *expected += 1;
        }
        if queue.is_empty() {
            pending.remove(&execution_id);
        }
        projected
    }

    /// Apply one ordered VM event. Unknown or already-closed handles are
    /// ignored here: the typed runtime has already enforced ownership and
    /// generation before an event reaches a host projection.
    pub fn project(&self, effect: &VmSideEffect) {
        self.project_for_execution(None, effect);
    }

    fn project_for_execution(&self, execution_id: Option<uuid::Uuid>, effect: &VmSideEffect) {
        match &effect.event {
            HostSideEffect::Emit { text } => {
                self.default_response.append(text);
            }
            HostSideEffect::Request { .. } => {}
            HostSideEffect::Ui {
                operation,
                target,
                text,
                progress,
            } => self.project_ui(
                execution_id,
                *operation,
                target.as_ref(),
                text.as_deref(),
                progress.as_ref(),
            ),
        }
    }

    /// Append host-rendered context (for example a proposal lifecycle notice)
    /// to this projection's response port. This is intentionally separate
    /// from `project`: the portable VM event remains unchanged and another
    /// embedder may choose a different presentation for it.
    pub fn append_default(&self, text: &str) {
        self.default_response.append(text);
    }

    fn project_ui(
        &self,
        execution_id: Option<uuid::Uuid>,
        operation: UiOperation,
        target: Option<&TypedValue>,
        text: Option<&str>,
        progress: Option<&crate::vm::UiProgress>,
    ) {
        let Some(handle) = output_handle(target) else {
            return;
        };
        match operation {
            UiOperation::Create => {
                let unit = if let Some(execution_id) = execution_id {
                    let stable = uuid::Uuid::new_v5(
                        &execution_id,
                        format!("output-handle:{handle}").as_bytes(),
                    );
                    self.output
                        .start_program_output_with_id(MessageId::from_uuid(stable))
                } else {
                    self.output.start_program_output()
                };
                // A handle is a VM-owned reactive artifact, not a second
                // assistant reply.  Give it the same plain output chrome as
                // `say` output so progress/status updates never acquire the
                // conversational bullet merely because they are addressable.
                unit.set_output_handle(text.unwrap_or("Working"));
                self.handles
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(handle.to_string(), unit);
            }
            UiOperation::Append => {
                if let (Some(unit), Some(text)) = (self.unit(handle), text) {
                    unit.append_output(text);
                }
            }
            UiOperation::Replace => {
                if let (Some(unit), Some(text)) = (self.unit(handle), text) {
                    unit.replace_output(text);
                }
            }
            UiOperation::Status => {
                if let (Some(unit), Some(text)) = (self.unit(handle), text) {
                    unit.set_transient_status(Some(text.to_string()));
                }
            }
            UiOperation::Progress => {
                if let (Some(unit), Some(progress)) = (self.unit(handle), progress) {
                    unit.set_output_progress(progress.completed, progress.total);
                }
            }
            UiOperation::Complete => {
                if let Some(unit) = self.remove_unit(handle) {
                    unit.set_transient_status(None);
                    unit.set_complete();
                }
            }
            UiOperation::Fail => {
                if let Some(unit) = self.remove_unit(handle) {
                    unit.set_transient_status(None);
                    if let Some(text) = text {
                        unit.replace_output(text);
                    }
                    unit.set_failed();
                }
            }
        }
    }

    fn unit(&self, handle: &str) -> Option<Arc<ProgramOutputMessage>> {
        self.handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(handle)
            .cloned()
    }

    fn remove_unit(&self, handle: &str) -> Option<Arc<ProgramOutputMessage>> {
        self.handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(handle)
    }
}

fn output_handle(target: Option<&TypedValue>) -> Option<&str> {
    match target {
        Some(TypedValue::Resource { kind, handle, .. }) if kind == "output-handle" => Some(handle),
        _ => None,
    }
}

// OutputMessage enum removed - now using trait-based messages only

/// Thread-safe output buffer manager
pub struct OutputManager {
    /// Whether to write output to stdout immediately (for scrollback)
    write_to_stdout: Arc<RwLock<bool>>,
    /// Buffering mode - true = accumulate for batch flush, false = immediate write
    buffering_mode: Arc<RwLock<bool>>,
    /// Pending lines waiting to be flushed (used when buffering_mode = true)
    pending_flush: Arc<RwLock<Vec<String>>>,
    /// Trait-based message storage (reactive updates)
    messages: Arc<RwLock<Vec<MessageRef>>>,
    /// Roots whose canonical bytes reached native history. Only these roots
    /// are eligible for bounded retention eviction.
    committed_ids: Arc<RwLock<std::collections::HashSet<MessageId>>>,
    /// Bounded tombstones for roots evicted after native commit. This closes
    /// the retention/re-registration race without growing exact-once state
    /// for the lifetime of the process.
    retired_ids: Arc<Mutex<RetiredMessageIds>>,
    /// Color scheme for message formatting
    colors: crate::config::ColorScheme,
}

#[derive(Default)]
struct RetiredMessageIds {
    order: std::collections::VecDeque<MessageId>,
    ids: std::collections::HashSet<MessageId>,
}

impl OutputManager {
    /// Create a new OutputManager
    pub fn new(colors: crate::config::ColorScheme) -> Self {
        Self {
            write_to_stdout: Arc::new(RwLock::new(true)), // Enabled by default, but main.rs disables immediately for TUI
            buffering_mode: Arc::new(RwLock::new(false)), // Default: immediate write
            pending_flush: Arc::new(RwLock::new(Vec::new())),
            messages: Arc::new(RwLock::new(Vec::new())),
            committed_ids: Arc::new(RwLock::new(std::collections::HashSet::new())),
            retired_ids: Arc::new(Mutex::new(RetiredMessageIds::default())),
            colors,
        }
    }

    /// Enable writing to stdout (for TUI mode with scrollback)
    pub fn enable_stdout(&self) {
        *self.write_to_stdout.write().unwrap() = true;
    }

    /// Disable writing to stdout (for testing or special modes)
    pub fn disable_stdout(&self) {
        *self.write_to_stdout.write().unwrap() = false;
    }

    /// Enable buffering mode - accumulate writes for batch flush
    pub fn enable_buffering(&self) {
        *self.buffering_mode.write().unwrap() = true;
    }

    /// Disable buffering mode - writes go to stdout immediately
    pub fn disable_buffering(&self) {
        *self.buffering_mode.write().unwrap() = false;
    }

    /// Drain all pending output lines for flushing
    pub fn drain_pending(&self) -> Vec<String> {
        let mut pending = self.pending_flush.write().unwrap();
        std::mem::take(&mut *pending)
    }

    /// Check if there are pending lines to flush
    pub fn has_pending(&self) -> bool {
        !self.pending_flush.read().unwrap().is_empty()
    }

    // Old enum-based methods removed - using trait-based messages only

    // ========================================================================
    // Trait-based message API (new reactive system)
    // ========================================================================

    /// Add a trait-based message to the buffer
    pub fn add_trait_message(&self, message: MessageRef) {
        let mut messages = self.messages.write().unwrap();
        if messages
            .iter()
            .any(|existing| existing.id() == message.id())
            || self
                .retired_ids
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .ids
                .contains(&message.id())
        {
            return;
        }
        messages.push(Arc::clone(&message));
        drop(messages);
        // Immediate/buffered output happens only after registration wins the
        // object-level MessageId dedupe race.
        self.write_trait_to_terminal(&message);
        self.prune_committed_roots();
    }

    // get_trait_messages(), trait_message_count(), clear_trait_messages() removed
    // Use get_messages(), len(), clear() instead

    /// Write a trait-based message to terminal
    fn write_trait_to_terminal(&self, message: &MessageRef) {
        let formatted = message.format(&self.colors);

        let buffering = *self.buffering_mode.read().unwrap();
        let write_stdout = *self.write_to_stdout.read().unwrap();

        if buffering {
            // Buffering mode: accumulate for batch flush
            self.pending_flush.write().unwrap().push(formatted);
        } else if write_stdout {
            // Immediate mode: write to stdout
            let mut stdout = io::stdout();
            let _ = write!(stdout, "{}\r\n", formatted);
            let _ = stdout.flush();
        }
    }

    // ========================================================================
    // Legacy OutputMessage API (for backward compatibility)
    // ========================================================================

    /// Write a user message
    pub fn write_user(&self, content: impl Into<String>) {
        let msg = Arc::new(UserQueryMessage::new(content));
        self.add_trait_message(msg);
    }

    /// Project an attributed shared-Brain participant message. `invokes_model`
    /// distinguishes an addressed prompt from relay-only conversation.
    pub fn write_brain_participant(
        &self,
        subject: impl Into<String>,
        content: impl Into<String>,
        invokes_model: bool,
    ) {
        let msg = Arc::new(BrainParticipantMessage::new(
            subject,
            content,
            invokes_model,
        ));
        self.add_trait_message(msg);
    }

    /// Write a provider response (can be called incrementally for streaming)
    pub fn write_response(&self, content: impl Into<String>) {
        let msg = StreamingResponseMessage::new();
        msg.append_chunk(&content.into());
        msg.set_complete();
        self.add_trait_message(Arc::new(msg));
    }

    /// Append to the last provider response (for streaming)
    pub fn append_response(&self, content: impl Into<String>) {
        // For now, just create a new message
        // TODO: In future, find last StreamingResponseMessage and append
        self.write_response(content);
    }

    /// Write tool execution output
    pub fn write_tool(&self, tool_name: impl Into<String>, content: impl Into<String>) {
        let formatted = format!("[{}] {}", tool_name.into(), content.into());
        let msg = Arc::new(StaticMessage::plain(formatted));
        self.add_trait_message(msg);
    }

    /// Write pre-formatted tool output (Claude Code-style, with ANSI colors already embedded)
    pub fn write_tool_raw(&self, content: impl Into<String>) {
        let msg = Arc::new(StaticMessage::plain(content));
        self.add_trait_message(msg);
    }

    /// Create and register a live tool message that supports streaming updates.
    /// Returns the Arc so the caller can append content and mark complete.
    pub fn start_live_tool(&self, header: impl Into<String>) -> Arc<LiveToolMessage> {
        let msg = Arc::new(LiveToolMessage::new(header));
        self.add_trait_message(Arc::clone(&msg) as MessageRef);
        msg
    }

    /// Create and register an OperationMessage that groups tool-call rows for
    /// one generation turn.  Returns the Arc so callers can add rows and mark
    /// the operation complete.
    pub fn start_operation(&self, header: impl Into<String>) -> Arc<OperationMessage> {
        let msg = Arc::new(OperationMessage::new(header));
        self.add_trait_message(Arc::clone(&msg) as MessageRef);
        msg
    }

    /// Create and register a WorkUnit for one AI generation turn.
    /// Returns the Arc so callers can update tokens, add tool rows, and mark complete.
    pub fn start_work_unit(&self, verb: impl Into<String>) -> Arc<WorkUnit> {
        let wu = Arc::new(WorkUnit::new(verb));
        self.add_trait_message(Arc::clone(&wu) as MessageRef);
        wu
    }

    /// Register a replayable WorkUnit using identity derived from its
    /// canonical event envelope rather than frontend construction time.
    pub fn start_work_unit_with_id(&self, id: MessageId, verb: impl Into<String>) -> Arc<WorkUnit> {
        let wu = Arc::new(WorkUnit::with_id(id, verb));
        self.add_trait_message(Arc::clone(&wu) as MessageRef);
        wu
    }

    /// Register stable run activity which cannot later be retyped as source or output.
    pub fn start_activity_with_id(
        &self,
        id: MessageId,
        title: impl Into<String>,
    ) -> Arc<ActivityMessage> {
        let message = Arc::new(ActivityMessage::with_id(id, title));
        self.add_trait_message(Arc::clone(&message) as MessageRef);
        message
    }

    /// Register a source artifact with a fresh local identity.
    pub fn start_program_source(&self, language: impl Into<String>) -> Arc<ProgramSourceMessage> {
        let message = Arc::new(ProgramSourceMessage::new(language));
        self.add_trait_message(Arc::clone(&message) as MessageRef);
        message
    }

    /// Register a source artifact with a canonical stable identity.
    pub fn start_program_source_with_id(
        &self,
        id: MessageId,
        language: impl Into<String>,
    ) -> Arc<ProgramSourceMessage> {
        let message = Arc::new(ProgramSourceMessage::with_id(id, language));
        self.add_trait_message(Arc::clone(&message) as MessageRef);
        message
    }

    /// Register a program-output artifact with a fresh local identity.
    pub fn start_program_output(&self) -> Arc<ProgramOutputMessage> {
        let message = Arc::new(ProgramOutputMessage::new());
        self.add_trait_message(Arc::clone(&message) as MessageRef);
        message
    }

    /// Register a program-output artifact with a canonical stable identity.
    pub fn start_program_output_with_id(&self, id: MessageId) -> Arc<ProgramOutputMessage> {
        let message = Arc::new(ProgramOutputMessage::with_id(id));
        self.add_trait_message(Arc::clone(&message) as MessageRef);
        message
    }

    /// Write status information (deprecated - use write_progress or write_info)
    pub fn write_status(&self, content: impl Into<String>) {
        // Route to progress for backward compatibility
        self.write_progress(content);
    }

    /// Write error message
    pub fn write_error(&self, content: impl Into<String>) {
        let msg = Arc::new(StaticMessage::error(content));
        self.add_trait_message(msg);
    }

    /// Write progress update
    pub fn write_progress(&self, content: impl Into<String>) {
        let msg = Arc::new(StaticMessage::plain(content));
        self.add_trait_message(msg);
    }

    /// Write system information message (help, patterns, stats)
    pub fn write_info(&self, content: impl Into<String>) {
        let msg = Arc::new(StaticMessage::plain(content));
        self.add_trait_message(msg);
    }

    /// Get all messages (for rendering)
    pub fn get_messages(&self) -> Vec<MessageRef> {
        self.messages.read().unwrap().clone()
    }

    /// Get the last N messages
    pub fn get_last_messages(&self, n: usize) -> Vec<MessageRef> {
        let messages = self.messages.read().unwrap();
        let start = messages.len().saturating_sub(n);
        messages.iter().skip(start).cloned().collect()
    }

    /// Clear all messages
    pub fn clear(&self) {
        let removed = std::mem::take(&mut *self.messages.write().unwrap());
        self.retire_committed_ids(removed.into_iter().map(|message| message.id()));
    }

    /// Remove one transient projection after its contents have been adopted
    /// by a durable grouped work unit.
    pub fn remove_message(&self, id: crate::cli::messages::MessageId) {
        let removed = {
            let mut messages = self.messages.write().unwrap();
            let before = messages.len();
            messages.retain(|message| message.id() != id);
            messages.len() != before
        };
        if removed {
            self.retire_committed_ids([id]);
        }
    }

    /// Record an exact native-history commit and prune only old committed
    /// roots. Live and completed-but-uncommitted roots are never evicted.
    pub fn mark_committed(&self, ids: impl IntoIterator<Item = MessageId>) {
        self.committed_ids.write().unwrap().extend(ids);
        self.prune_committed_roots();
    }

    fn retire_committed_ids(&self, ids: impl IntoIterator<Item = MessageId>) {
        let mut committed = self.committed_ids.write().unwrap();
        let mut retired = self
            .retired_ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for id in ids {
            if !committed.remove(&id) || !retired.ids.insert(id) {
                continue;
            }
            retired.order.push_back(id);
        }
        while retired.order.len() > MAX_BUFFER_SIZE {
            if let Some(expired) = retired.order.pop_front() {
                retired.ids.remove(&expired);
            }
        }
    }

    fn prune_committed_roots(&self) {
        let committed = self.committed_ids.read().unwrap().clone();
        let mut messages = self.messages.write().unwrap();
        while messages.len() > MAX_BUFFER_SIZE {
            let Some(index) = messages
                .iter()
                .position(|message| committed.contains(&message.id()))
            else {
                break;
            };
            let removed = messages.remove(index).id();
            self.committed_ids.write().unwrap().remove(&removed);
            let mut retired = self
                .retired_ids
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if retired.ids.insert(removed) {
                retired.order.push_back(removed);
            }
            while retired.order.len() > MAX_BUFFER_SIZE {
                if let Some(expired) = retired.order.pop_front() {
                    retired.ids.remove(&expired);
                }
            }
        }
    }

    /// Get the number of messages in the buffer
    pub fn len(&self) -> usize {
        self.messages.read().unwrap().len()
    }

    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.messages.read().unwrap().is_empty()
    }
}

impl Default for OutputManager {
    fn default() -> Self {
        Self::new(crate::config::ColorScheme::default())
    }
}

impl Clone for OutputManager {
    fn clone(&self) -> Self {
        Self {
            write_to_stdout: Arc::clone(&self.write_to_stdout),
            buffering_mode: Arc::clone(&self.buffering_mode),
            pending_flush: Arc::clone(&self.pending_flush),
            messages: Arc::clone(&self.messages),
            committed_ids: Arc::clone(&self.committed_ids),
            retired_ids: Arc::clone(&self.retired_ids),
            colors: self.colors.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::messages::{Message, MessageKind};

    fn silent_manager() -> OutputManager {
        let m = OutputManager::new(crate::config::ColorScheme::default());
        m.disable_stdout();
        m
    }

    #[test]
    fn test_basic_message_count() {
        let manager = silent_manager();

        manager.write_user("Hello");
        manager.write_response("Hi there!");
        manager.write_tool("read", "File contents...");

        assert_eq!(manager.len(), 3);
        assert_eq!(manager.get_messages().len(), 3);
    }

    #[test]
    fn duplicate_root_message_id_registers_once() {
        let manager = silent_manager();
        let id = MessageId::new();
        let first: MessageRef = Arc::new(WorkUnit::with_id(id, "first"));
        let duplicate: MessageRef = Arc::new(WorkUnit::with_id(id, "duplicate"));
        manager.add_trait_message(first);
        manager.add_trait_message(duplicate);
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn retention_never_evicts_live_or_uncommitted_roots() {
        let manager = silent_manager();
        let live = manager.start_work_unit("live");
        let terminal_uncommitted = manager.start_work_unit("terminal-uncommitted");
        terminal_uncommitted.set_complete();
        for index in 0..=MAX_BUFFER_SIZE {
            let message = Arc::new(WorkUnit::new(format!("complete-{index}")));
            message.set_complete();
            let id = message.id();
            manager.add_trait_message(message);
            manager.mark_committed([id]);
        }
        assert!(manager
            .get_messages()
            .iter()
            .any(|message| message.id() == live.id()));
        assert!(manager
            .get_messages()
            .iter()
            .any(|message| message.id() == terminal_uncommitted.id()));
        assert!(manager.len() <= MAX_BUFFER_SIZE + 2);
    }

    #[test]
    fn evicted_committed_root_id_cannot_be_registered_again() {
        let manager = silent_manager();
        let first = Arc::new(WorkUnit::new("first"));
        first.set_complete();
        let retired_id = first.id();
        manager.add_trait_message(first);
        manager.mark_committed([retired_id]);
        for index in 0..MAX_BUFFER_SIZE {
            let message = Arc::new(WorkUnit::new(format!("retained-{index}")));
            message.set_complete();
            let id = message.id();
            manager.add_trait_message(message);
            manager.mark_committed([id]);
        }
        assert!(!manager
            .get_messages()
            .iter()
            .any(|message| message.id() == retired_id));

        let duplicate: MessageRef = Arc::new(WorkUnit::with_id(retired_id, "duplicate"));
        manager.add_trait_message(duplicate);
        assert_eq!(manager.len(), MAX_BUFFER_SIZE);
    }

    #[test]
    fn vm_output_projection_legacy_work_unit_preserves_live_and_terminal_output() {
        let manager = Arc::new(silent_manager());
        let legacy = manager.start_work_unit("legacy output");
        let projection = VmOutputProjection::new(Arc::clone(&manager), Arc::clone(&legacy));
        projection.project(&VmSideEffect {
            protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
            sequence: 0,
            requirement: crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::SessionEmit,
                selector: crate::vm::ResourceSelector::None,
            },
            event: HostSideEffect::Emit {
                text: "live".into(),
            },
            output: Vec::new(),
            origin: crate::vm::SourceOrigin::generated("test"),
        });
        assert_eq!(legacy.content(), "live");
        assert!(legacy.children().is_empty());

        legacy.set_complete();
        let terminal_projection =
            VmOutputProjection::new(Arc::clone(&manager), Arc::clone(&legacy));
        terminal_projection.project(&VmSideEffect {
            protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
            sequence: 1,
            requirement: crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::SessionEmit,
                selector: crate::vm::ResourceSelector::None,
            },
            event: HostSideEffect::Emit {
                text: "late".into(),
            },
            output: Vec::new(),
            origin: crate::vm::SourceOrigin::generated("test"),
        });
        assert_eq!(legacy.content(), "live");
        assert!(manager
            .get_messages()
            .iter()
            .any(|message| message.kind() == MessageKind::Output && message.content() == "late"));
    }

    fn output_effect(
        operation: UiOperation,
        handle: &str,
        text: Option<&str>,
        progress: Option<crate::vm::UiProgress>,
    ) -> VmSideEffect {
        VmSideEffect {
            protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
            sequence: 1,
            requirement: crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::SessionEmit,
                selector: crate::vm::ResourceSelector::None,
            },
            event: HostSideEffect::Ui {
                operation,
                target: Some(TypedValue::Resource {
                    kind: "output-handle".into(),
                    handle: handle.into(),
                    generation: 0,
                }),
                text: text.map(str::to_string),
                progress,
            },
            output: Vec::new(),
            origin: crate::vm::SourceOrigin::generated("test"),
        }
    }

    #[test]
    fn vm_output_projection_keeps_explicit_handles_independent() {
        let manager = Arc::new(silent_manager());
        let response = manager.start_program_output();
        let projection = VmOutputProjection::new(Arc::clone(&manager), Arc::clone(&response));

        projection.project(&VmSideEffect {
            protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
            sequence: 0,
            requirement: crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::SessionEmit,
                selector: crate::vm::ResourceSelector::None,
            },
            event: HostSideEffect::Emit {
                text: "answer".into(),
            },
            output: Vec::new(),
            origin: crate::vm::SourceOrigin::generated("test"),
        });
        projection.project(&VmSideEffect {
            protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
            sequence: 1,
            requirement: crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::SessionEmit,
                selector: crate::vm::ResourceSelector::None,
            },
            event: HostSideEffect::Emit {
                text: " next".into(),
            },
            output: Vec::new(),
            origin: crate::vm::SourceOrigin::generated("test"),
        });
        projection.project(&output_effect(
            UiOperation::Create,
            "download",
            Some("Download"),
            None,
        ));
        projection.project(&output_effect(
            UiOperation::Status,
            "download",
            Some("connecting"),
            None,
        ));
        projection.project(&output_effect(
            UiOperation::Progress,
            "download",
            None,
            Some(crate::vm::UiProgress {
                completed: 2,
                total: Some(5),
            }),
        ));
        projection.project(&output_effect(
            UiOperation::Complete,
            "download",
            None,
            None,
        ));

        let messages = manager.get_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(response.content(), "answer next");
        assert!(messages[1].content().is_empty());
        assert_eq!(
            messages[1].status(),
            crate::cli::messages::MessageStatus::Complete
        );
        let rendered = messages[1].format(&crate::config::ColorScheme::default());
        assert!(rendered.contains("Download"));
        assert!(rendered.contains("2 / 5"));
        assert!(
            !rendered.contains("connecting"),
            "transient status must disappear after output-complete"
        );
        assert!(
            !rendered.contains('⏺'),
            "explicit VM output handles must not render as assistant replies"
        );
    }

    #[test]
    fn vm_tool_projection_retains_activity_semantics_without_extra_output_message() {
        use crate::cli::messages::MessageKind;

        let manager = Arc::new(silent_manager());
        let activity = manager.start_activity_with_id(MessageId::new(), "Brain activity");
        let row_idx = activity.add_tool("submit_program");
        let projection = VmOutputProjection::for_tool_activity(
            Arc::clone(&manager),
            activity.work_unit(),
            row_idx,
        );
        projection.project(&VmSideEffect {
            protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
            sequence: 0,
            requirement: crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::SessionEmit,
                selector: crate::vm::ResourceSelector::None,
            },
            event: HostSideEffect::Emit {
                text: "tool output".into(),
            },
            output: Vec::new(),
            origin: crate::vm::SourceOrigin::generated("test"),
        });

        let messages = manager.get_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].kind(), MessageKind::Activity);
        let activity_children = messages[0].children();
        assert_eq!(activity_children[0].kind(), MessageKind::ToolCall);
        let tool_children = activity_children[0].children();
        assert_eq!(tool_children.len(), 2);
        assert_eq!(tool_children[1].kind(), MessageKind::ToolOutput);
        assert_eq!(
            tool_children[1]
                .disclosure(&crate::config::ColorScheme::default())
                .unwrap()
                .body,
            vec!["tool output"]
        );
    }

    #[test]
    fn vm_output_projection_applies_envelopes_once_and_in_order() {
        let manager = Arc::new(silent_manager());
        let response = manager.start_program_output();
        let projection = VmOutputProjection::new(Arc::clone(&manager), Arc::clone(&response));
        let execution_id = uuid::Uuid::new_v4();
        let emit = |sequence, text: &str| VmEffectEnvelope {
            execution_id,
            effect: VmSideEffect {
                protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
                sequence,
                requirement: crate::vm::CapabilityRequirement {
                    capability: crate::vm::CapabilityKind::SessionEmit,
                    selector: crate::vm::ResourceSelector::None,
                },
                event: HostSideEffect::Emit { text: text.into() },
                output: Vec::new(),
                origin: crate::vm::SourceOrigin::generated("test"),
            },
        };

        let first = emit(0, "first");
        let third = emit(2, "third");
        let second = emit(1, " second");

        assert_eq!(projection.project_envelope(first.clone()).len(), 1);
        assert!(
            projection.project_envelope(first).is_empty(),
            "a replayed journal effect must not duplicate output"
        );
        assert!(
            projection.project_envelope(third).is_empty(),
            "a gap must be retained without rendering ahead"
        );
        let drained = projection.project_envelope(second);
        assert_eq!(
            drained
                .iter()
                .map(|effect| effect.effect.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2],
            "the missing event must drain the retained contiguous suffix"
        );
        assert_eq!(response.content(), "first secondthird");
    }

    #[test]
    fn replayed_output_handle_reconstructs_the_same_work_unit_id() {
        let execution_id = uuid::Uuid::from_u128(0x69);
        let envelope = VmEffectEnvelope {
            execution_id,
            effect: VmSideEffect {
                sequence: 0,
                ..output_effect(UiOperation::Create, "download", Some("Download"), None)
            },
        };
        let project_once = || {
            let manager = Arc::new(silent_manager());
            let response = manager.start_program_output();
            let projection = VmOutputProjection::new(Arc::clone(&manager), response);
            assert_eq!(projection.project_envelope(envelope.clone()).len(), 1);
            manager.get_messages()[1].id()
        };

        assert_eq!(project_once(), project_once());
    }

    #[test]
    fn test_is_empty_and_clear() {
        let manager = silent_manager();
        assert!(manager.is_empty());

        manager.write_user("a");
        assert!(!manager.is_empty());

        manager.clear();
        assert!(manager.is_empty());
        assert!(manager.committed_ids.read().unwrap().is_empty());
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn test_get_last_messages() {
        let manager = silent_manager();

        for i in 0..5 {
            manager.write_info(format!("msg {i}"));
        }

        let last = manager.get_last_messages(3);
        assert_eq!(last.len(), 3);
    }

    #[test]
    fn test_buffering_mode() {
        let manager = silent_manager();
        manager.enable_buffering();

        manager.write_user("buffered");
        assert!(manager.has_pending());

        let drained = manager.drain_pending();
        assert_eq!(drained.len(), 1);
        assert!(!manager.has_pending());
    }

    #[test]
    fn test_clone_shares_state() {
        let manager = silent_manager();
        let clone = manager.clone();

        manager.write_user("shared");
        // Clone sees the same underlying Arc data
        assert_eq!(clone.len(), 1);
    }

    #[test]
    fn test_start_live_tool_registered() {
        let manager = silent_manager();
        let _live = manager.start_live_tool("tool header");
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn test_start_work_unit_registered() {
        let manager = silent_manager();
        let _wu = manager.start_work_unit("thinking");
        assert_eq!(manager.len(), 1);
    }
}
