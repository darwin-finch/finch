use super::*;

pub(super) async fn check_brain_bootstrap_access(
    _server: &AgentServer,
    addr: SocketAddr,
    _headers: &HeaderMap,
) -> Result<(), Response> {
    if is_local_brain_bootstrap(addr) {
        return Ok(());
    }
    Err((
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "local Brain bootstrap access required"})),
    )
        .into_response())
}

pub(super) async fn has_brain_bootstrap_access(
    _server: &AgentServer,
    addr: SocketAddr,
    _headers: &HeaderMap,
) -> bool {
    is_local_brain_bootstrap(addr)
}

pub(super) async fn issue_named_brain_credential(
    State(server): State<Arc<AgentServer>>,
    restricted: Option<axum::Extension<RestrictedBrainListener>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(request): Json<IssueNamedBrainCredentialRequest>,
) -> Result<Json<IssueNamedBrainCredentialResponse>, Response> {
    if request.role == crate::brain::store::AttachmentRole::Runner {
        return Err(brain_auth_error(
            StatusCode::BAD_REQUEST,
            "runner authority cannot be minted as a participant credential",
        ));
    }
    let snapshot = server
        .brain_store()
        .snapshot(&name)
        .map_err(|error| AppError(error).into_response())?;
    let now_ms = unix_epoch_millis();
    let delegator =
        if restricted.is_none() && has_brain_bootstrap_access(&server, addr, &headers).await {
            None
        } else {
            let token = bearer_token(&headers).ok_or_else(|| {
                brain_auth_error(
                    StatusCode::UNAUTHORIZED,
                    "Brain bootstrap password or delegating credential required",
                )
            })?;
            let claims = server
                .brain_credentials()
                .verify(token, now_ms)
                .map_err(|error| brain_auth_error(StatusCode::UNAUTHORIZED, error.to_string()))?;
            claims
                .require_audience(
                    snapshot.brain_id,
                    &name,
                    snapshot.environment.generation,
                    crate::brain::credential::BrainCredentialScope::BrainControl,
                )
                .map_err(|error| brain_auth_error(StatusCode::FORBIDDEN, error.to_string()))?;
            require_unbound_administrative_credential(&claims)?;
            Some(claims)
        };
    let ttl_ms = request
        .ttl_ms
        .unwrap_or(DEFAULT_BRAIN_CREDENTIAL_TTL_MS)
        .min(MAX_BRAIN_CREDENTIAL_TTL_MS);
    let scopes = request
        .scopes
        .unwrap_or_else(|| crate::brain::credential::default_participant_scopes(request.role));
    let permitted = crate::brain::credential::permitted_participant_scopes(request.role);
    if !scopes.is_subset(&permitted) {
        return Err(brain_auth_error(
            StatusCode::FORBIDDEN,
            "requested Brain credential scopes exceed this participant role",
        ));
    }
    let delegation_chain = if let Some(delegator) = &delegator {
        delegator
            .attenuate(&scopes, ttl_ms, now_ms)
            .map_err(|error| brain_auth_error(StatusCode::FORBIDDEN, error.to_string()))?
    } else {
        Vec::new()
    };
    let token = server
        .brain_credentials()
        .issue(
            crate::brain::credential::BrainCredentialRequest {
                issuer: snapshot.environment.machine.clone(),
                subject: request.subject,
                brain_id: snapshot.brain_id,
                brain: name,
                environment_generation: snapshot.environment.generation,
                role: request.role,
                scopes,
                delegation_chain,
                ttl_ms,
            },
            now_ms,
        )
        .map_err(|error| AppError(error).into_response())?;
    let claims = server
        .brain_credentials()
        .verify(&token, now_ms)
        .map_err(|error| AppError(error).into_response())?;
    Ok(Json(IssueNamedBrainCredentialResponse { token, claims }))
}

pub(super) async fn issue_named_brain_invitation(
    State(server): State<Arc<AgentServer>>,
    restricted: Option<axum::Extension<RestrictedBrainListener>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(request): Json<IssueNamedBrainInvitationRequest>,
) -> Result<Json<IssueNamedBrainInvitationResponse>, Response> {
    if request.role == crate::brain::store::AttachmentRole::Runner {
        return Err(brain_auth_error(
            StatusCode::BAD_REQUEST,
            "runner authority cannot be delegated through a Brain invitation",
        ));
    }
    let snapshot = server
        .brain_store()
        .snapshot(&name)
        .map_err(|error| AppError(error).into_response())?;
    let now_ms = unix_epoch_millis();
    let delegator =
        if restricted.is_none() && has_brain_bootstrap_access(&server, addr, &headers).await {
            None
        } else {
            let token = bearer_token(&headers).ok_or_else(|| {
                brain_auth_error(
                    StatusCode::UNAUTHORIZED,
                    "Brain bootstrap password or controlling credential required",
                )
            })?;
            let claims = server
                .brain_credentials()
                .verify(token, now_ms)
                .map_err(|error| brain_auth_error(StatusCode::UNAUTHORIZED, error.to_string()))?;
            claims
                .require_audience(
                    snapshot.brain_id,
                    &name,
                    snapshot.environment.generation,
                    crate::brain::credential::BrainCredentialScope::BrainControl,
                )
                .map_err(|error| brain_auth_error(StatusCode::FORBIDDEN, error.to_string()))?;
            require_unbound_administrative_credential(&claims)?;
            Some(claims)
        };
    let ttl_ms = request
        .ttl_ms
        .unwrap_or(DEFAULT_BRAIN_INVITATION_TTL_MS)
        .min(MAX_BRAIN_INVITATION_TTL_MS);
    let scopes = request
        .scopes
        .unwrap_or_else(|| crate::brain::credential::default_participant_scopes(request.role));
    if !scopes.is_subset(&crate::brain::credential::permitted_participant_scopes(
        request.role,
    )) || !scopes.contains(&crate::brain::credential::BrainCredentialScope::BrainAttach)
    {
        return Err(brain_auth_error(
            StatusCode::FORBIDDEN,
            "requested Brain invitation scopes are invalid for this participant role",
        ));
    }
    let delegation_chain = if let Some(delegator) = &delegator {
        delegator
            .attenuate(&scopes, ttl_ms, now_ms)
            .map_err(|error| brain_auth_error(StatusCode::FORBIDDEN, error.to_string()))?
    } else {
        Vec::new()
    };
    let (invitation, claims) = server
        .brain_credentials()
        .issue_invitation(
            crate::brain::credential::BrainInvitationRequest {
                issuer: snapshot.environment.machine.clone(),
                brain_id: snapshot.brain_id,
                brain: name,
                environment_generation: snapshot.environment.generation,
                role: request.role,
                scopes,
                delegation_chain,
                ttl_ms,
            },
            now_ms,
        )
        .map_err(|error| AppError(error).into_response())?;
    Ok(Json(IssueNamedBrainInvitationResponse {
        invitation,
        claims,
    }))
}

pub(super) async fn revoke_delegated_named_brain_credential(
    State(server): State<Arc<AgentServer>>,
    headers: HeaderMap,
    Path((name, credential_id)): Path<(String, uuid::Uuid)>,
    Json(request): Json<RevokeDelegatedNamedBrainCredentialRequest>,
) -> Result<StatusCode, Response> {
    let delegator = authorize_named_brain(
        &server,
        &headers,
        &name,
        crate::brain::credential::BrainCredentialScope::BrainControl,
    )?;
    require_unbound_administrative_credential(&delegator)?;
    let now_ms = unix_epoch_millis();
    let (descendant_id, brain_id, brain, generation, delegation_chain) = match request {
        RevokeDelegatedNamedBrainCredentialRequest {
            credential: Some(credential),
            invitation: None,
        } => {
            let claims = server
                .brain_credentials()
                .verify(&credential, now_ms)
                .map_err(|error| brain_auth_error(StatusCode::UNAUTHORIZED, error.to_string()))?;
            (
                claims.credential_id,
                claims.brain_id,
                claims.brain,
                claims.environment_generation,
                claims.delegation_chain,
            )
        }
        RevokeDelegatedNamedBrainCredentialRequest {
            credential: None,
            invitation: Some(invitation),
        } => {
            let claims = server
                .brain_credentials()
                .verify_invitation_descendant_proof(&invitation, now_ms)
                .map_err(|error| brain_auth_error(StatusCode::UNAUTHORIZED, error.to_string()))?;
            (
                claims.invitation_id,
                claims.brain_id,
                claims.brain,
                claims.environment_generation,
                claims.delegation_chain,
            )
        }
        _ => {
            return Err(brain_auth_error(
                StatusCode::BAD_REQUEST,
                "supply exactly one credential or invitation descendant proof",
            ));
        }
    };
    if descendant_id != credential_id
        || brain_id != delegator.brain_id
        || brain != delegator.brain
        || generation != delegator.environment_generation
        || !delegation_chain.contains(&delegator.credential_id)
    {
        return Err(brain_auth_error(
            StatusCode::FORBIDDEN,
            "a controlling credential may revoke only its own descendants",
        ));
    }
    server
        .brain_credentials()
        .revoke(credential_id)
        .map_err(|error| AppError(error).into_response())?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn redeem_named_brain_invitation(
    State(server): State<Arc<AgentServer>>,
    Json(request): Json<RedeemNamedBrainInvitationRequest>,
) -> Result<Json<IssueNamedBrainCredentialResponse>, Response> {
    let now_ms = unix_epoch_millis();
    let invitation = server
        .brain_credentials()
        .inspect_invitation(&request.invitation, now_ms)
        .map_err(|error| brain_auth_error(StatusCode::UNAUTHORIZED, error.to_string()))?;
    let snapshot = server
        .brain_store()
        .snapshot(&invitation.brain)
        .map_err(|error| AppError(error).into_response())?;
    if invitation.brain_id != snapshot.brain_id
        || invitation.environment_generation != snapshot.environment.generation
    {
        return Err(brain_auth_error(
            StatusCode::CONFLICT,
            "Brain invitation audience is no longer current",
        ));
    }
    let (token, claims) = server
        .brain_credentials()
        .redeem_invitation(&request.invitation, &request.subject, now_ms)
        .map_err(|error| brain_auth_error(StatusCode::UNAUTHORIZED, error.to_string()))?;
    Ok(Json(IssueNamedBrainCredentialResponse { token, claims }))
}

pub(super) async fn revoke_named_brain_credential(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(credential_id): Path<uuid::Uuid>,
) -> Result<StatusCode, Response> {
    check_brain_bootstrap_access(&server, addr, &headers).await?;
    server
        .brain_credentials()
        .revoke(credential_id)
        .map_err(|error| AppError(error).into_response())?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn show_brain_password(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !addr.ip().is_loopback() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(Json(serde_json::json!({
        "password": server.brain_password().await
    })))
}

pub(super) async fn change_brain_password(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(request): Json<ChangeBrainPassword>,
) -> Result<StatusCode, Response> {
    if !addr.ip().is_loopback() {
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    if request.password.trim().len() < 12 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "brain password must be at least 12 characters"})),
        )
            .into_response());
    }
    let mut config =
        crate::config::load_config().map_err(|error| AppError(error).into_response())?;
    config.server.brain_password = request.password.clone();
    config
        .save()
        .map_err(|error| AppError(error).into_response())?;
    server.set_brain_password(request.password).await;
    Ok(StatusCode::NO_CONTENT)
}
