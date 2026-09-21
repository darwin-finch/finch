//! Compatibility facade for client-local typed messages.
//!
//! New dependencies use `finch_messages` directly; the root CLI retains this
//! path while application call sites migrate without a behavioral change.

pub use finch_messages::{
    random_spinner_verb, AgentActivityView, AgentToolView, BrainParticipantMessage,
    ComponentAction, LiveToolMessage, Message, MessageId, MessageRef, MessageStatus,
    OperationMessage, OperationRow, OperationRowStatus, OutputVm, ProgramSourceVm, ProgressMessage,
    SayTurnStatus, SayTurnView, StaticMessage, StaticMessageType, StreamingResponseMessage,
    ToggleProgram, ToolExecutionMessage, UserQueryMessage, WorkRow, WorkRowPresentation,
    WorkRowStatus, WorkRowView, WorkUnit, WorkUnitHead, WorkUnitPresentation, WorkUnitView,
    WorkUnitViewModel,
};
