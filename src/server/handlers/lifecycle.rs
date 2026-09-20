use super::*;

pub(super) async fn create_named_brain(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<CreateNamedBrainRequest>,
) -> Result<(StatusCode, Json<crate::brain::BrainSnapshot>), Response> {
    check_brain_bootstrap_access(&server, addr, &headers).await?;
    let snapshot = crate::server::BrainLifecycleService::from_server(&server)
        .create(&request.name)
        .await
        .map_err(brain_state_conflict)?;
    Ok((StatusCode::CREATED, Json(snapshot)))
}

pub(super) async fn list_named_brains(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Vec<NamedBrainListEntry>>, Response> {
    check_brain_bootstrap_access(&server, addr, &headers).await?;
    let mut result = Vec::new();
    for name in server
        .brain_store()
        .list()
        .map_err(|error| AppError(error).into_response())?
    {
        let snapshot = server
            .brain_store()
            .snapshot(&name)
            .map_err(|error| AppError(error).into_response())?;
        result.push(NamedBrainListEntry {
            name,
            environment: snapshot.environment,
            event_revision: snapshot.revision,
            retained_programs: snapshot.program_stack.len(),
            runner: snapshot.runner_lease,
        });
    }
    Ok(Json(result))
}

pub(super) async fn get_named_brain(
    State(server): State<Arc<AgentServer>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<crate::brain::BrainSnapshot>, Response> {
    authorize_named_brain(
        &server,
        &headers,
        &name,
        crate::brain::BrainCredentialScope::BrainRead,
    )?;
    server
        .brain_store()
        .snapshot(&name)
        .map(Json)
        .map_err(|error| AppError(error).into_response())
}

pub(super) async fn get_named_brain_capabilities(
    State(server): State<Arc<AgentServer>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<crate::brain::RemoteBrainCapabilities>, Response> {
    authorize_named_brain(
        &server,
        &headers,
        &name,
        crate::brain::BrainCredentialScope::BrainRead,
    )?;
    let snapshot = server
        .brain_store()
        .snapshot(&name)
        .map_err(|error| AppError(error).into_response())?;
    Ok(Json(crate::brain::RemoteBrainCapabilities {
        schema_version: 1,
        brain_id: snapshot.brain_id,
        brain: snapshot.name,
        environment: snapshot.environment,
        node_public_key: hex::encode(server.brain_credentials().invitation_public_key()),
        node: super::node::current_node_capabilities(server.primary_provider().is_some()),
    }))
}

pub(super) async fn attach_named_brain(
    State(server): State<Arc<AgentServer>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(request): Json<AttachNamedBrainRequest>,
) -> Result<Json<AttachNamedBrainResponse>, Response> {
    let claims = authorize_named_brain(
        &server,
        &headers,
        &name,
        crate::brain::BrainCredentialScope::BrainAttach,
    )?;
    claims
        .require_participant(&request.subject, request.role)
        .map_err(|error| brain_auth_error(StatusCode::FORBIDDEN, error.to_string()))?;
    if claims.attachment_id.is_some() || claims.connection_id.is_some() {
        return Err(brain_auth_error(
            StatusCode::FORBIDDEN,
            "an attachment-bound credential cannot create another attachment",
        ));
    }
    let attachment = crate::server::BrainLifecycleService::from_server(&server)
        .attach(&name, &request.subject, request.role, request.attachment_id)
        .map_err(brain_state_conflict)?;
    let connection_id = attachment
        .connection_id
        .expect("new remote Brain attachment has a pending connection");
    let (token, bound_claims) = match server.brain_credentials().bind_attachment(
        &claims,
        attachment.attachment_id,
        connection_id,
        unix_epoch_millis(),
    ) {
        Ok(bound) => bound,
        Err(error) => {
            let _ = crate::server::BrainLifecycleService::from_server(&server).detach(
                &name,
                attachment.attachment_id,
                connection_id,
            );
            return Err(AppError(error).into_response());
        }
    };
    Ok(Json(AttachNamedBrainResponse {
        attachment,
        token,
        claims: bound_claims,
    }))
}

pub(super) async fn archive_named_brain(
    State(server): State<Arc<AgentServer>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<ArchiveNamedBrainResponse>, Response> {
    let claims = authorize_named_brain(
        &server,
        &headers,
        &name,
        crate::brain::BrainCredentialScope::EnvironmentAdmin,
    )?;
    require_unbound_administrative_credential(&claims)?;
    let execution_lock = server
        .brain_store()
        .execution_lock(&name)
        .map_err(|error| AppError(error).into_response())?;
    let _turn = execution_lock.lock_owned().await;
    let archived_to = server
        .brain_store()
        .archive(&name)
        .map_err(|error| AppError(error).into_response())?;
    Ok(Json(ArchiveNamedBrainResponse {
        name,
        archived_to: archived_to.map(|path| path.display().to_string()),
    }))
}

pub(super) async fn get_named_brain_selection(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<crate::brain::BrainProviderSelection>, Response> {
    check_brain_bootstrap_access(&server, addr, &headers).await?;
    server
        .brain_store()
        .provider_selection(&name)
        .map(Json)
        .map_err(|error| AppError(error).into_response())
}

pub(super) async fn put_named_brain_selection(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(selection): Json<crate::brain::BrainProviderSelection>,
) -> Result<Json<crate::brain::BrainProviderSelection>, Response> {
    check_brain_bootstrap_access(&server, addr, &headers).await?;
    if !provider_selection_is_valid(&selection) {
        return Err(AppError(anyhow::anyhow!(
            "provider/model selection contains an invalid name, model, or thinking level"
        ))
        .into_response());
    }
    let selection_lock = server
        .brain_store()
        .execution_lock(&name)
        .map_err(|error| AppError(error).into_response())?;
    let _selection_write = selection_lock.lock_owned().await;
    server
        .brain_store()
        .set_provider_selection(&name, selection)
        .map(Json)
        .map_err(|error| AppError(error).into_response())
}

fn provider_selection_is_valid(selection: &crate::brain::BrainProviderSelection) -> bool {
    let valid_line = |value: Option<&str>, max_len: usize| {
        value.is_none_or(|value| {
            !value.trim().is_empty()
                && value == value.trim()
                && value.len() <= max_len
                && !value.chars().any(char::is_control)
        })
    };
    let valid_effort = selection.reasoning_effort.as_deref().is_none_or(|value| {
        matches!(
            value,
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
        )
    });
    valid_line(selection.provider.as_deref(), 128)
        && valid_line(selection.model.as_deref(), 256)
        && valid_line(selection.reasoning_effort.as_deref(), 32)
        && valid_effort
}

#[cfg(test)]
mod selection_tests {
    use super::provider_selection_is_valid;
    use crate::brain::BrainProviderSelection;

    #[test]
    fn persisted_selection_rejects_terminal_controls_and_untrimmed_values() {
        for model in ["gpt-5\u{1b}[2J", " gpt-5", "gpt-5\t"] {
            let selection = BrainProviderSelection {
                provider: Some("work".into()),
                model: Some(model.into()),
                reasoning_effort: Some("high".into()),
                provider_inherited: false,
            };
            assert!(
                !provider_selection_is_valid(&selection),
                "unsafe model identity must not enter persisted status text: {model:?}"
            );
        }
    }

    #[test]
    fn persisted_selection_accepts_secret_free_profile_identity() {
        assert!(provider_selection_is_valid(&BrainProviderSelection {
            provider: Some("chatgpt-work".into()),
            model: Some("gpt-5.6-sol".into()),
            reasoning_effort: Some("xhigh".into()),
            provider_inherited: false,
        }));
    }
}
