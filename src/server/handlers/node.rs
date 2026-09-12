use super::*;

/// Handle GET /v1/status - Get server and model status
pub(super) async fn get_status(
    State(server): State<Arc<AgentServer>>,
) -> Result<Json<StatusResponse>, AppError> {
    use crate::models::GeneratorState;

    let state = server.generator_state().read().await;

    let generator_status = match &*state {
        GeneratorState::Initializing => GeneratorStatus::Initializing,
        GeneratorState::Downloading {
            model_name,
            progress,
        } => GeneratorStatus::Downloading {
            model_size: model_name.clone(),
            file_name: progress.file_name.clone(),
            current_file: progress.current_file,
            total_files: progress.total_files,
        },
        GeneratorState::Loading { model_name } => GeneratorStatus::Loading {
            model_size: model_name.clone(),
        },
        GeneratorState::Ready { model_name, .. } => GeneratorStatus::Ready {
            model_size: model_name.clone(),
        },
        GeneratorState::Failed { error } => GeneratorStatus::Failed {
            error: error.clone(),
        },
        GeneratorState::NotAvailable => GeneratorStatus::NotAvailable,
    };

    let response = StatusResponse {
        generator: generator_status,
        // A count, not a hydration (#364). `list()` replays every Brain's
        // event log and opens its effect-audit databases; this probe wants
        // only how many there are.
        named_brains: server.brain_store().count_unhydrated(),
        training_enabled: false,
    };

    Ok(Json(response))
}

/// Handle GET /health - Health check endpoint
pub async fn health_check(
    State(server): State<Arc<AgentServer>>,
) -> Result<Json<HealthStatus>, AppError> {
    // `list()` here was a full store hydration on the unauthenticated probe
    // that gates every `finch` launch: `DaemonClient::connect` ->
    // `ensure_daemon_running` -> `health_check_succeeds` -> GET /health, under
    // a 500 ms client timeout whose expiry costs the launch an unconditional
    // two-second sleep (#364, and see #344). Health needs the count.
    //
    // Three observable differences, all deliberate:
    //
    // 1. it cannot fail, so one unreadable Brain no longer 500s the probe;
    // 2. it counts a Brain whose replay would fail, which is the truthful
    //    answer to "how many Brains are there";
    // 3. `pending_brain_terminalizations` below is now read from a registry
    //    that this request no longer populates. `ensure_loaded` is what
    //    discovers unreconciled disconnect intents on disk and re-registers
    //    them, so a probe can report `healthy` where the hydrating version
    //    would have reported `degraded` with a true count.
    //
    // On (3), precisely, because an earlier version of this comment got it
    // wrong: it claimed the one-second schedule tick in `serve_on_listener`
    // would repopulate the registry, bounding the window to that tick. It does
    // not. That tick calls the same `list()`, so on a store holding a Brain
    // that cannot be replayed it fails every second and registers nothing --
    // the count stays 0 for as long as the Brain stays broken.
    //
    // That is not a regression this introduces, and hydrating here would not
    // fix it. Before, `/health` returned 500, `health_check_succeeds` read a
    // non-2xx as daemon absence, the launch slept two seconds and continued
    // with no daemon client -- and the schedules were dead the whole time
    // anyway. The failure was loud in the wrong place; now it is quiet in the
    // wrong place. Neither reports the actual problem, which is that schedule
    // delivery and Brain listing are all-or-nothing across the store (#371).
    // What `/health` should mean for a corrupt Brain is #344's.
    let named_brains = server.brain_store().count_unhydrated();
    let pending_brain_terminalizations = server
        .brain_store()
        .pending_disconnect_terminalization_retries();
    let status = HealthStatus {
        status: if pending_brain_terminalizations == 0 {
            "healthy"
        } else {
            "degraded"
        }
        .to_string(),
        uptime_seconds: server.uptime().as_secs(),
        named_brains,
        pending_brain_terminalizations,
    };

    Ok(Json(status))
}

/// Handle GET /metrics - Prometheus metrics endpoint
pub async fn metrics_endpoint(
    State(server): State<Arc<AgentServer>>,
) -> Result<Response, AppError> {
    // Only what is measured. This used to emit a constant
    // `finch_queries_total 0`, which a scraper cannot distinguish from "no
    // queries yet" — a fabricated series reported as live truth, and the thing
    // #131 asks to stop.
    //
    // Request-lifecycle counters (accepted, in-flight, completed, failed,
    // cancelled, streamed), routing aggregates and token usage are the rest of
    // #131. They belong on the canonical request lifecycle rather than a
    // parallel counter invented here, so this exposes uptime and nothing else
    // until they exist.
    let metrics = format!(
        "# HELP finch_daemon_uptime_seconds Seconds this server has been running, not counting host suspend.\n\
         # TYPE finch_daemon_uptime_seconds gauge\n\
         finch_daemon_uptime_seconds {}\n",
        server.uptime().as_secs_f64()
    );

    Ok((StatusCode::OK, metrics).into_response())
}

/// Handle GET /v1/node/info — return this node's identity and capabilities
pub async fn handle_node_info() -> Result<Json<serde_json::Value>, AppError> {
    use crate::config::load_config;
    use crate::node::NodeInfo;

    let has_teacher = load_config()
        .map(|c| c.active_teacher().is_some())
        .unwrap_or(false);
    let info = NodeInfo::load(has_teacher)?;
    Ok(Json(serde_json::to_value(&info)?))
}

/// Test seam for the production node-info response with explicit state.
///
/// This intentionally has no ambient-HOME fallback: integration fixtures
/// must supply a disposable Finch state directory.
#[doc(hidden)]
#[cfg(unix)]
pub async fn handle_node_info_from_state_directory(
    state: crate::node::IsolatedNodeTestState,
    has_teacher_api: bool,
) -> Result<Json<serde_json::Value>, AppError> {
    let info = state.load_node_info(has_teacher_api)?;
    Ok(Json(serde_json::to_value(&info)?))
}

/// Handle GET /v1/node/stats — return this node's work statistics
pub async fn handle_node_stats() -> Result<Json<serde_json::Value>, AppError> {
    use crate::node::WorkTracker;

    let stats = WorkTracker::load_persisted()?;
    Ok(Json(serde_json::to_value(&stats)?))
}

/// Test seam for the production node-stats response with explicit state.
#[doc(hidden)]
#[cfg(unix)]
pub async fn handle_node_stats_from_state_directory(
    state: crate::node::IsolatedNodeTestState,
) -> Result<Json<serde_json::Value>, AppError> {
    use crate::node::WorkTracker;

    let stats = WorkTracker::load_persisted_from_state_directory(state.descriptor())?;
    Ok(Json(serde_json::to_value(&stats)?))
}
