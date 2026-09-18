//! Event-loop-owned tool-execution protocol.
//!
//! Provider and local generators emit semantic tool-call events. They never
//! execute Finch tools. [`ToolLoop`] is the single lifecycle for parse,
//! validate, admit, execute-once, cancel, timeout, disconnect, retry, and
//! late-result ignore. REPL and scheduler both drive this type.

use finch_providers::EventProvenance;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Brain/run/provider/model identity pinned for one tool round.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolLoopIdentity {
    /// Provider name as the host knows it (never a secret).
    pub provider: String,
    /// Model name as the host knows it (never a secret).
    pub model: String,
    /// Named Brain id when this round belongs to one.
    pub brain: Option<String>,
    /// Durable run id when this round belongs to one.
    pub run_id: Option<String>,
}

/// Names the model was offered this turn, and names the host can execute.
#[derive(Debug, Clone, Default)]
pub struct ToolCatalog {
    offered: HashSet<String>,
    executable: HashSet<String>,
}

impl ToolCatalog {
    /// Catalog where offered names are also executable.
    pub fn offered(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let offered: HashSet<String> = names.into_iter().map(Into::into).collect();
        Self {
            executable: offered.clone(),
            offered,
        }
    }

    /// Split offered-this-turn from host-executable names.
    pub fn new(
        offered: impl IntoIterator<Item = impl Into<String>>,
        executable: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            offered: offered.into_iter().map(Into::into).collect(),
            executable: executable.into_iter().map(Into::into).collect(),
        }
    }

    fn classify(&self, name: &str) -> Result<(), RejectReason> {
        if name.is_empty() {
            return Err(RejectReason::UnknownTool);
        }
        if !self.offered.contains(name) {
            return Err(RejectReason::UnsupportedTool);
        }
        if !self.executable.is_empty() && !self.executable.contains(name) {
            return Err(RejectReason::UnknownTool);
        }
        Ok(())
    }
}

/// Why a tool call must not execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    /// Two complete calls claimed the same id.
    DuplicateId,
    /// Concatenated argument fragments were not valid JSON.
    MalformedArguments {
        /// Secret-free parse detail.
        detail: String,
    },
    /// Name is empty or not executable on this host.
    UnknownTool,
    /// Name was not offered on this turn's tool list.
    UnsupportedTool,
    /// Complete payload did not match accumulated deltas.
    ArgumentMismatch {
        /// Secret-free mismatch detail.
        detail: String,
    },
    /// Tool-call id was empty.
    EmptyId,
}

impl RejectReason {
    /// Speakable typed-result body. Never execute after this.
    pub fn typed_message(&self, id: &str, name: &str) -> String {
        match self {
            Self::DuplicateId => {
                format!("tool call {id} ({name}) was rejected: duplicate tool-call id")
            }
            Self::MalformedArguments { detail } => {
                format!("tool call {id} ({name}) was rejected: malformed arguments ({detail})")
            }
            Self::UnknownTool => {
                format!("tool call {id} ({name}) was rejected: unknown tool")
            }
            Self::UnsupportedTool => {
                format!("tool call {id} ({name}) was rejected: tool was not offered this turn")
            }
            Self::ArgumentMismatch { detail } => {
                format!("tool call {id} ({name}) was rejected: argument mismatch ({detail})")
            }
            Self::EmptyId => "tool call was rejected: empty tool-call id".to_string(),
        }
    }
}

/// Why the loop will not admit further execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolLoopTerminal {
    /// Every admitted call has appended a result.
    Completed,
    /// Caller cancelled the round.
    Cancelled,
    /// A call timed out.
    TimedOut,
    /// Transport or peer disconnected.
    Disconnected {
        /// Secret-free reason.
        reason: String,
    },
    /// Host failed before a successful terminal.
    Failed {
        /// Secret-free cause.
        cause: String,
    },
}

/// Validated call the host may execute at most once.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedCall {
    /// Tool-call id pinned for the round.
    pub id: String,
    /// Tool name after catalog checks.
    pub name: String,
    /// Parsed arguments.
    pub input: Value,
    /// Provider/model provenance from the originating event.
    pub provenance: EventProvenance,
}

/// Call that must produce a typed error and never execute.
#[derive(Debug, Clone, PartialEq)]
pub struct RejectedCall {
    /// Tool-call id when one was present.
    pub id: String,
    /// Tool name when one was present.
    pub name: String,
    /// Best-effort input for history; `Null` when arguments never parsed.
    pub input: Value,
    /// Why execution is forbidden.
    pub reason: RejectReason,
}

/// One observed call after [`ToolLoop::finish_observation`].
#[derive(Debug, Clone, PartialEq)]
pub enum PreparedCall {
    /// Catalog-valid, JSON-valid, unique id.
    Ready(ValidatedCall),
    /// Fail-closed; emit a typed result and do not execute.
    Rejected(RejectedCall),
}

impl PreparedCall {
    /// Id used to stage the assistant tool_use and the matching result.
    pub fn id(&self) -> &str {
        match self {
            Self::Ready(call) => &call.id,
            Self::Rejected(call) => &call.id,
        }
    }

    /// Name used to stage the assistant tool_use.
    pub fn name(&self) -> &str {
        match self {
            Self::Ready(call) => &call.name,
            Self::Rejected(call) => &call.name,
        }
    }

    /// Input used to stage the assistant tool_use.
    pub fn input(&self) -> &Value {
        match self {
            Self::Ready(call) => &call.input,
            Self::Rejected(call) => &call.input,
        }
    }
}

/// Result the loop will append at most once per id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLoopResult {
    /// Matching tool-call id.
    pub id: String,
    /// Speakable content.
    pub content: String,
    /// Whether this is a failure/typed-reject result.
    pub is_error: bool,
}

impl ToolLoopResult {
    /// Successful execution output.
    pub fn success(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    /// Failure or typed reject.
    pub fn error(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            content: content.into(),
            is_error: true,
        }
    }

    /// Typed result for a rejected call. Never paired with an execution.
    pub fn from_reject(call: &RejectedCall) -> Self {
        Self::error(&call.id, call.reason.typed_message(&call.id, &call.name))
    }
}

/// Why [`ToolLoop::admit_execution`] refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitError {
    /// Round already terminalized.
    Terminal,
    /// Id was never prepared as Ready, or was rejected.
    NotReady,
    /// This id already started execution.
    AlreadyAdmitted,
}

/// Outcome of observing a delta or complete event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserveOutcome {
    /// Fragments recorded; call is not yet complete.
    Accumulating,
    /// Call is ready or rejected; further identical completes are idempotent.
    Settled,
    /// Event ignored because the round is already terminal.
    Late,
}

#[derive(Debug, Clone)]
enum CallState {
    Accumulating {
        name: Option<String>,
        raw_args: String,
        provenance: EventProvenance,
    },
    Ready(ValidatedCall),
    Rejected(RejectedCall),
}

/// Single tool-round lifecycle. Sync protocol; execution stays outside.
pub struct ToolLoop {
    identity: ToolLoopIdentity,
    catalog: ToolCatalog,
    order: Vec<String>,
    calls: HashMap<String, CallState>,
    admitted: HashSet<String>,
    appended: HashSet<String>,
    terminal: Option<ToolLoopTerminal>,
}

impl ToolLoop {
    /// Start a round pinned to `identity` and the offered/executable catalog.
    pub fn new(identity: ToolLoopIdentity, catalog: ToolCatalog) -> Self {
        Self {
            identity,
            catalog,
            order: Vec::new(),
            calls: HashMap::new(),
            admitted: HashSet::new(),
            appended: HashSet::new(),
            terminal: None,
        }
    }

    /// Identity recorded for this round.
    pub fn identity(&self) -> &ToolLoopIdentity {
        &self.identity
    }

    /// True after cancel, timeout, disconnect, failure, or completed drain.
    pub fn is_terminal(&self) -> bool {
        self.terminal.is_some()
    }

    /// Terminal reason when the round has ended.
    pub fn terminal(&self) -> Option<&ToolLoopTerminal> {
        self.terminal.as_ref()
    }

    /// Number of ids that started execution. At most one start per id.
    pub fn execution_starts(&self) -> usize {
        self.admitted.len()
    }

    /// Number of ids that appended a result. At most one append per id.
    pub fn results_appended(&self) -> usize {
        self.appended.len()
    }

    /// Record an incremental argument fragment.
    pub fn observe_delta(
        &mut self,
        id: String,
        name: Option<String>,
        arguments_delta: String,
        provenance: EventProvenance,
    ) -> ObserveOutcome {
        if self.terminal.is_some() {
            return ObserveOutcome::Late;
        }
        if id.is_empty() {
            self.reject(
                generated_empty_id(),
                name.unwrap_or_default(),
                Value::Null,
                RejectReason::EmptyId,
            );
            return ObserveOutcome::Settled;
        }
        self.remember_id(&id);
        if let Some(CallState::Ready(_) | CallState::Rejected(_)) = self.calls.get(&id) {
            // Complete already settled this id. A trailing delta is dual
            // encoding or a late fragment, not a second call.
            return ObserveOutcome::Settled;
        }
        if let Some(CallState::Accumulating { name: existing, .. }) = self.calls.get(&id) {
            if let (Some(incoming), Some(have)) = (name.as_ref(), existing.as_ref()) {
                if incoming != have {
                    self.reject(
                        id,
                        incoming.clone(),
                        Value::Null,
                        RejectReason::ArgumentMismatch {
                            detail: "tool name changed while arguments accumulated".to_string(),
                        },
                    );
                    return ObserveOutcome::Settled;
                }
            }
        }
        let entry = self
            .calls
            .entry(id)
            .or_insert_with(|| CallState::Accumulating {
                name: None,
                raw_args: String::new(),
                provenance: provenance.clone(),
            });
        if let CallState::Accumulating {
            name: have,
            raw_args,
            provenance: stored,
        } = entry
        {
            if let Some(incoming) = name {
                *have = Some(incoming);
            }
            raw_args.push_str(&arguments_delta);
            *stored = provenance;
        }
        ObserveOutcome::Accumulating
    }

    /// Record a complete tool call (native or translated from a content block).
    pub fn observe_complete(
        &mut self,
        id: String,
        name: String,
        input: Value,
        provenance: EventProvenance,
    ) -> ObserveOutcome {
        if self.terminal.is_some() {
            return ObserveOutcome::Late;
        }
        if id.is_empty() {
            self.reject(generated_empty_id(), name, input, RejectReason::EmptyId);
            return ObserveOutcome::Settled;
        }
        self.remember_id(&id);
        if let Some(existing) = self.calls.get(&id) {
            match existing {
                CallState::Ready(call) if call.name == name && call.input == input => {
                    return ObserveOutcome::Settled;
                }
                CallState::Rejected(_) => return ObserveOutcome::Settled,
                CallState::Ready(_) => {
                    self.reject(id, name, input, RejectReason::DuplicateId);
                    return ObserveOutcome::Settled;
                }
                CallState::Accumulating { .. } => {}
            }
        }

        if let Some(CallState::Accumulating {
            name: acc_name,
            raw_args,
            ..
        }) = self.calls.get(&id).cloned()
        {
            if let Some(acc_name) = acc_name {
                if acc_name != name {
                    self.reject(
                        id,
                        name,
                        input,
                        RejectReason::ArgumentMismatch {
                            detail: "complete name did not match accumulated name".to_string(),
                        },
                    );
                    return ObserveOutcome::Settled;
                }
            }
            if !raw_args.is_empty() {
                match parse_arguments(&raw_args) {
                    Ok(parsed) if parsed == input => {}
                    Ok(_) => {
                        self.reject(
                            id,
                            name,
                            input,
                            RejectReason::ArgumentMismatch {
                                detail:
                                    "complete JSON did not match accumulated argument fragments"
                                        .to_string(),
                            },
                        );
                        return ObserveOutcome::Settled;
                    }
                    Err(detail) => {
                        self.reject(id, name, input, RejectReason::MalformedArguments { detail });
                        return ObserveOutcome::Settled;
                    }
                }
            }
        }

        match self.catalog.classify(&name) {
            Ok(()) => {
                self.calls.insert(
                    id.clone(),
                    CallState::Ready(ValidatedCall {
                        id,
                        name,
                        input,
                        provenance,
                    }),
                );
            }
            Err(reason) => self.reject(id, name, input, reason),
        }
        ObserveOutcome::Settled
    }

    /// Close observation. Unfinished accumulators fail closed as malformed.
    pub fn finish_observation(&mut self) -> Vec<PreparedCall> {
        let ids = self.order.clone();
        for id in &ids {
            let Some(CallState::Accumulating {
                name,
                raw_args,
                provenance,
            }) = self.calls.get(id).cloned()
            else {
                continue;
            };
            let name = name.unwrap_or_default();
            match parse_arguments(&raw_args) {
                Ok(input) => {
                    self.observe_complete(id.clone(), name, input, provenance);
                }
                Err(detail) => {
                    self.reject(
                        id.clone(),
                        name,
                        Value::Null,
                        RejectReason::MalformedArguments { detail },
                    );
                }
            }
        }
        ids.into_iter()
            .filter_map(|id| match self.calls.get(&id) {
                Some(CallState::Ready(call)) => Some(PreparedCall::Ready(call.clone())),
                Some(CallState::Rejected(call)) => Some(PreparedCall::Rejected(call.clone())),
                Some(CallState::Accumulating { .. }) | None => None,
            })
            .collect()
    }

    /// Admit execution for a ready id. At most once; never after terminal.
    pub fn admit_execution(&mut self, id: &str) -> Result<ValidatedCall, AdmitError> {
        if self.terminal.is_some() {
            return Err(AdmitError::Terminal);
        }
        if self.admitted.contains(id) {
            return Err(AdmitError::AlreadyAdmitted);
        }
        match self.calls.get(id) {
            Some(CallState::Ready(call)) => {
                self.admitted.insert(id.to_string());
                Ok(call.clone())
            }
            _ => Err(AdmitError::NotReady),
        }
    }

    /// Append a result at most once. Late results after terminal are dropped.
    pub fn append_result(&mut self, result: ToolLoopResult) -> Option<ToolLoopResult> {
        if self.terminal.is_some() {
            return None;
        }
        if self.appended.contains(&result.id) {
            return None;
        }
        self.appended.insert(result.id.clone());
        Some(result)
    }

    /// End the round. Returns false when it was already terminal.
    pub fn terminalize(&mut self, terminal: ToolLoopTerminal) -> bool {
        if self.terminal.is_some() {
            return false;
        }
        self.terminal = Some(terminal);
        true
    }

    fn remember_id(&mut self, id: &str) {
        if !self.order.iter().any(|existing| existing == id) {
            self.order.push(id.to_string());
        }
    }

    fn reject(&mut self, id: String, name: String, input: Value, reason: RejectReason) {
        self.remember_id(&id);
        self.calls.insert(
            id.clone(),
            CallState::Rejected(RejectedCall {
                id,
                name,
                input,
                reason,
            }),
        );
    }
}

fn parse_arguments(raw: &str) -> Result<Value, String> {
    if raw.is_empty() {
        return Err("arguments were empty".to_string());
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(value) if value.is_object() => Ok(value),
        Ok(_) => Err("arguments were not a JSON object".to_string()),
        Err(error) => Err(error.to_string()),
    }
}

fn generated_empty_id() -> String {
    format!("empty-id-{}", uuid::Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(sequence: u64) -> EventProvenance {
        EventProvenance {
            provider: "test".into(),
            model: "script-1".into(),
            event: "tool_call".into(),
            sequence,
            opaque_replay: None,
        }
    }

    fn identity() -> ToolLoopIdentity {
        ToolLoopIdentity {
            provider: "test".into(),
            model: "script-1".into(),
            brain: Some("brain-1".into()),
            run_id: Some("run-1".into()),
        }
    }

    fn catalog() -> ToolCatalog {
        ToolCatalog::new(["read", "glob"], ["read", "glob", "bash"])
    }

    fn loop_open() -> ToolLoop {
        ToolLoop::new(identity(), catalog())
    }

    #[test]
    fn test_tool_loop_deltas_accumulate_to_exact_complete_json() {
        let mut tool_loop = loop_open();
        assert_eq!(
            tool_loop.observe_delta(
                "call-1".into(),
                Some("read".into()),
                "{\"file".into(),
                provenance(1),
            ),
            ObserveOutcome::Accumulating
        );
        tool_loop.observe_delta(
            "call-1".into(),
            None,
            "_path\":\"/tmp/a\"}".into(),
            provenance(2),
        );
        tool_loop.observe_complete(
            "call-1".into(),
            "read".into(),
            serde_json::json!({"file_path": "/tmp/a"}),
            provenance(3),
        );
        let prepared = tool_loop.finish_observation();
        assert_eq!(
            prepared.len(),
            1,
            "exact accumulation must yield one call: {prepared:?}"
        );
        match &prepared[0] {
            PreparedCall::Ready(call) => {
                assert_eq!(call.id, "call-1");
                assert_eq!(call.input["file_path"], "/tmp/a");
                assert_eq!(call.provenance.provider, "test");
            }
            other => panic!("expected ready call, got {other:?}"),
        }
    }

    #[test]
    fn test_tool_loop_complete_mismatching_accumulated_json_fails_closed() {
        let mut tool_loop = loop_open();
        tool_loop.observe_delta(
            "call-1".into(),
            Some("read".into()),
            "{\"file_path\":\"/tmp/a\"}".into(),
            provenance(1),
        );
        tool_loop.observe_complete(
            "call-1".into(),
            "read".into(),
            serde_json::json!({"file_path": "/tmp/other"}),
            provenance(2),
        );
        let prepared = tool_loop.finish_observation();
        match &prepared[0] {
            PreparedCall::Rejected(call) => {
                assert!(
                    matches!(call.reason, RejectReason::ArgumentMismatch { .. }),
                    "mismatch must fail closed, not execute the complete payload: {call:?}"
                );
            }
            other => panic!("expected rejected mismatch, got {other:?}"),
        }
        assert!(
            tool_loop.admit_execution("call-1").is_err(),
            "mismatched complete must never be admitted"
        );
    }

    #[test]
    fn test_tool_loop_malformed_arguments_never_execute() {
        let mut tool_loop = loop_open();
        tool_loop.observe_delta(
            "call-1".into(),
            Some("read".into()),
            "{\"file_path\":".into(),
            provenance(1),
        );
        let prepared = tool_loop.finish_observation();
        match &prepared[0] {
            PreparedCall::Rejected(call) => {
                assert!(
                    matches!(call.reason, RejectReason::MalformedArguments { .. }),
                    "truncated JSON must fail closed: {call:?}"
                );
                let result = ToolLoopResult::from_reject(call);
                assert!(result.is_error);
                assert!(
                    result.content.contains("malformed arguments"),
                    "typed result must name the reject: {}",
                    result.content
                );
            }
            other => panic!("expected malformed reject, got {other:?}"),
        }
        assert_eq!(
            tool_loop.admit_execution("call-1"),
            Err(AdmitError::NotReady),
            "malformed call must never start execution"
        );
    }

    #[test]
    fn test_tool_loop_empty_accumulated_arguments_fail_closed() {
        let mut tool_loop = loop_open();
        tool_loop.observe_delta(
            "call-1".into(),
            Some("read".into()),
            String::new(),
            provenance(1),
        );
        let prepared = tool_loop.finish_observation();
        match &prepared[0] {
            PreparedCall::Rejected(call) => {
                assert!(
                    matches!(call.reason, RejectReason::MalformedArguments { .. }),
                    "empty fragments must fail closed, not execute as {{}}: {call:?}"
                );
            }
            other => panic!("expected malformed reject for empty args, got {other:?}"),
        }
        assert_eq!(
            tool_loop.admit_execution("call-1"),
            Err(AdmitError::NotReady)
        );
    }

    #[test]
    fn test_tool_loop_empty_catalog_rejects_every_name() {
        let mut tool_loop = ToolLoop::new(identity(), ToolCatalog::offered(Vec::<String>::new()));
        tool_loop.observe_complete(
            "call-1".into(),
            "read".into(),
            serde_json::json!({"file_path": "/tmp/a"}),
            provenance(1),
        );
        let prepared = tool_loop.finish_observation();
        match &prepared[0] {
            PreparedCall::Rejected(call) => {
                assert_eq!(
                    call.reason,
                    RejectReason::UnsupportedTool,
                    "empty offered set means nothing was offered this turn: {call:?}"
                );
            }
            other => panic!("expected unsupported, got {other:?}"),
        }
    }

    #[test]
    fn test_tool_loop_duplicate_id_fails_closed_without_second_execution() {
        let mut tool_loop = loop_open();
        tool_loop.observe_complete(
            "call-1".into(),
            "read".into(),
            serde_json::json!({"file_path": "/tmp/a"}),
            provenance(1),
        );
        tool_loop.observe_complete(
            "call-1".into(),
            "read".into(),
            serde_json::json!({"file_path": "/tmp/b"}),
            provenance(2),
        );
        let prepared = tool_loop.finish_observation();
        assert_eq!(prepared.len(), 1);
        assert!(
            matches!(prepared[0], PreparedCall::Rejected(_)),
            "conflicting duplicate id must reject the call: {prepared:?}"
        );
        assert_eq!(
            tool_loop.admit_execution("call-1"),
            Err(AdmitError::NotReady)
        );
    }

    #[test]
    fn test_tool_loop_dual_encoding_same_payload_is_one_call() {
        let mut tool_loop = loop_open();
        let input = serde_json::json!({"file_path": "/tmp/a"});
        tool_loop.observe_complete("call-1".into(), "read".into(), input.clone(), provenance(1));
        let outcome =
            tool_loop.observe_complete("call-1".into(), "read".into(), input, provenance(2));
        assert_eq!(outcome, ObserveOutcome::Settled);
        let prepared = tool_loop.finish_observation();
        assert_eq!(prepared.len(), 1);
        assert!(matches!(prepared[0], PreparedCall::Ready(_)));
        let admitted = tool_loop.admit_execution("call-1").expect("first admit");
        assert_eq!(admitted.id, "call-1");
        assert_eq!(
            tool_loop.admit_execution("call-1"),
            Err(AdmitError::AlreadyAdmitted),
            "idempotent dual encoding must not admit a second execution"
        );
        assert_eq!(tool_loop.execution_starts(), 1);
    }

    #[test]
    fn test_tool_loop_unknown_and_unsupported_never_execute() {
        let mut tool_loop = loop_open();
        tool_loop.observe_complete(
            "call-unknown".into(),
            "not_a_tool".into(),
            serde_json::json!({}),
            provenance(1),
        );
        tool_loop.observe_complete(
            "call-bash".into(),
            "bash".into(),
            serde_json::json!({"command": "ls"}),
            provenance(2),
        );
        let prepared = tool_loop.finish_observation();
        assert_eq!(prepared.len(), 2, "{prepared:?}");
        match &prepared[0] {
            PreparedCall::Rejected(call) => {
                assert_eq!(call.reason, RejectReason::UnsupportedTool);
            }
            other => panic!("unoffered tool must be unsupported: {other:?}"),
        }
        match &prepared[1] {
            PreparedCall::Rejected(call) => {
                assert_eq!(
                    call.reason,
                    RejectReason::UnsupportedTool,
                    "executable-but-not-offered must not run: {call:?}"
                );
            }
            other => panic!("expected unsupported bash, got {other:?}"),
        }
        let mut leaked_alias = ToolLoop::new(identity(), ToolCatalog::offered(["spawn_agent"]));
        leaked_alias.observe_complete(
            "call-alias".into(),
            "finch_spawn_agent".into(),
            serde_json::json!({"task": "x"}),
            provenance(1),
        );
        let prepared = leaked_alias.finish_observation();
        match &prepared[0] {
            PreparedCall::Rejected(call) => {
                assert_eq!(
                    call.reason,
                    RejectReason::UnsupportedTool,
                    "a provider wire alias must not execute: {call:?}"
                );
            }
            other => panic!("leaked ChatGPT wire alias must not execute: {other:?}"),
        }
        assert_eq!(
            leaked_alias.execution_starts(),
            0,
            "unknown wire names must cause no tool effect"
        );

        let mut unknown_only = ToolLoop::new(identity(), ToolCatalog::new(["ghost"], ["read"]));
        unknown_only.observe_complete(
            "call-ghost".into(),
            "ghost".into(),
            serde_json::json!({}),
            provenance(1),
        );
        let prepared = unknown_only.finish_observation();
        match &prepared[0] {
            PreparedCall::Rejected(call) => assert_eq!(call.reason, RejectReason::UnknownTool),
            other => panic!("offered-but-not-executable must be unknown: {other:?}"),
        }
    }

    #[test]
    fn test_tool_loop_late_result_after_terminal_is_ignored() {
        let mut tool_loop = loop_open();
        tool_loop.observe_complete(
            "call-1".into(),
            "read".into(),
            serde_json::json!({"file_path": "/tmp/a"}),
            provenance(1),
        );
        let _ = tool_loop.finish_observation();
        tool_loop
            .admit_execution("call-1")
            .expect("admit before cancel");
        assert!(tool_loop.terminalize(ToolLoopTerminal::Cancelled));
        assert!(!tool_loop.terminalize(ToolLoopTerminal::TimedOut));
        assert_eq!(
            tool_loop.observe_complete(
                "call-2".into(),
                "read".into(),
                serde_json::json!({"file_path": "/tmp/b"}),
                provenance(3),
            ),
            ObserveOutcome::Late
        );
        assert_eq!(
            tool_loop.admit_execution("call-1"),
            Err(AdmitError::Terminal)
        );
        assert!(
            tool_loop
                .append_result(ToolLoopResult::success("call-1", "late file contents"))
                .is_none(),
            "late success after cancel must not append"
        );
        assert_eq!(tool_loop.results_appended(), 0);
        assert_eq!(tool_loop.execution_starts(), 1);
    }

    #[test]
    fn test_tool_loop_timeout_appends_once_then_ignores_late_success() {
        let mut tool_loop = loop_open();
        tool_loop.observe_complete(
            "call-1".into(),
            "read".into(),
            serde_json::json!({"file_path": "/tmp/a"}),
            provenance(1),
        );
        let _ = tool_loop.finish_observation();
        tool_loop.admit_execution("call-1").expect("admit");
        let timeout = tool_loop
            .append_result(ToolLoopResult::error(
                "call-1",
                "timed out after 30 seconds",
            ))
            .expect("timeout result is the one appended result");
        assert!(timeout.is_error);
        assert!(tool_loop.terminalize(ToolLoopTerminal::TimedOut));
        assert!(
            tool_loop
                .append_result(ToolLoopResult::success("call-1", "late"))
                .is_none(),
            "late success after timeout must not append a second result"
        );
        assert_eq!(tool_loop.results_appended(), 1);
        assert_eq!(tool_loop.execution_starts(), 1);
    }

    #[test]
    fn test_tool_loop_retry_of_admitted_id_does_not_execute_again() {
        let mut tool_loop = loop_open();
        let input = serde_json::json!({"file_path": "/tmp/a"});
        tool_loop.observe_complete("call-1".into(), "read".into(), input.clone(), provenance(1));
        let _ = tool_loop.finish_observation();
        tool_loop.admit_execution("call-1").expect("first");
        // A retried complete for the same id+payload is dual-encoding/idempotent
        // observation, but admit stays once.
        tool_loop.observe_complete("call-1".into(), "read".into(), input, provenance(2));
        assert_eq!(
            tool_loop.admit_execution("call-1"),
            Err(AdmitError::AlreadyAdmitted)
        );
        let first = tool_loop
            .append_result(ToolLoopResult::success("call-1", "ok"))
            .expect("first result");
        assert!(!first.is_error);
        assert!(tool_loop
            .append_result(ToolLoopResult::success("call-1", "retry"))
            .is_none());
        assert_eq!(tool_loop.execution_starts(), 1);
        assert_eq!(tool_loop.results_appended(), 1);
    }

    #[test]
    fn test_tool_loop_disconnect_drops_late_complete_and_result() {
        let mut tool_loop = loop_open();
        tool_loop.observe_complete(
            "call-1".into(),
            "read".into(),
            serde_json::json!({}),
            provenance(1),
        );
        let _ = tool_loop.finish_observation();
        assert!(tool_loop.terminalize(ToolLoopTerminal::Disconnected {
            reason: "peer closed".into(),
        }));
        assert_eq!(
            tool_loop.observe_delta("call-1".into(), None, "{}".into(), provenance(2)),
            ObserveOutcome::Late
        );
        assert_eq!(
            tool_loop.admit_execution("call-1"),
            Err(AdmitError::Terminal)
        );
        assert!(tool_loop
            .append_result(ToolLoopResult::success("call-1", "late"))
            .is_none());
        assert_eq!(tool_loop.execution_starts(), 0);
        assert_eq!(tool_loop.results_appended(), 0);
    }

    #[test]
    fn test_tool_loop_identity_is_pinned_on_construction() {
        let tool_loop = loop_open();
        assert_eq!(tool_loop.identity().brain.as_deref(), Some("brain-1"));
        assert_eq!(tool_loop.identity().run_id.as_deref(), Some("run-1"));
        assert_eq!(tool_loop.identity().provider, "test");
        assert_eq!(tool_loop.identity().model, "script-1");
    }
}
