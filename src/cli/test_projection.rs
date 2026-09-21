//! Test-only projection for application assertions over message presentation.
//!
//! Application tests should compose the public message and UI-model contracts,
//! not reach into the renderer's private ViewModel adapter.

use finch_messages::Message;
use finch_theme::ColorScheme;

pub(crate) use finch_ui_model::{NodeRole, TranscriptNode};

pub(crate) fn try_project_for_test(
    message: &dyn Message,
    colors: &ColorScheme,
) -> Option<TranscriptNode> {
    message
        .work_unit_view(colors)
        .map(|view| finch_ui_model::project_work_unit(&view))
}
