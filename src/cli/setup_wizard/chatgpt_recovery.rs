//! The ChatGPT credential setup ceremony and its recovery loop.
//!
//! Self-contained: every one of the wizard's `crate::oauth` references and every one of its
//! `crate::cli::chatgpt_auth` references is in this file. Nothing else in the wizard talks to
//! either.

use super::*;

/// Secret-free recovery state shown after a ChatGPT setup ceremony terminates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChatGptSetupRecovery {
    pub(super) invocation: SetupInvocation,
    pub(super) credential_ref: String,
    pub(super) cause: ChatGptSetupFailureCause,
    pub(super) summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatGptSetupFailureCause {
    Cancelled,
    Expired,
    Denied,
    StartDisabledOrUnsupported,
    ProviderRejected,
    PollContract,
    TokenExchangeRejected,
    TokenExchangeContract,
    IdentityVerification,
    ClientBinding,
    AccountEntitlement,
    Persistence,
    ProtocolOrOther,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChatGptSetupRecoveryAction {
    RetrySignIn,
    ChangeNamedCredential(String),
    RemoveProvider,
    CancelSetup,
}

pub(super) trait ChatGptSetupRecoveryEditor {
    fn choose(&mut self, recovery: &ChatGptSetupRecovery) -> Result<ChatGptSetupRecoveryAction>;
}

pub(super) struct TerminalChatGptSetupRecoveryEditor;

pub(super) const MAX_CHATGPT_EDITOR_INPUT_ATTEMPTS: usize = 4;

pub(super) fn choose_chatgpt_setup_recovery_with_io(
    recovery: &ChatGptSetupRecovery,
    input: &mut impl std::io::BufRead,
    output: &mut impl std::io::Write,
) -> Result<ChatGptSetupRecoveryAction> {
    writeln!(output, "\nChatGPT provider/account editor")?;
    writeln!(output, "Credential: {}", recovery.credential_ref)?;
    writeln!(output, "{}", recovery.summary)?;
    for _ in 0..MAX_CHATGPT_EDITOR_INPUT_ATTEMPTS {
        writeln!(output, "  1. Retry sign-in")?;
        writeln!(output, "  2. Change named credential")?;
        writeln!(output, "  3. Remove provider")?;
        writeln!(output, "  4. Cancel setup")?;
        write!(output, "Choose an action [1-4]: ")?;
        output.flush()?;
        let mut choice = String::new();
        if input.read_line(&mut choice)? == 0 {
            return Ok(ChatGptSetupRecoveryAction::CancelSetup);
        }
        match choice.trim() {
            "1" => return Ok(ChatGptSetupRecoveryAction::RetrySignIn),
            "2" => {
                write!(output, "Named credential (for example chatgpt:work): ")?;
                output.flush()?;
                let mut reference = String::new();
                if input.read_line(&mut reference)? == 0 {
                    return Ok(ChatGptSetupRecoveryAction::CancelSetup);
                }
                let reference = reference.trim().to_string();
                if crate::oauth::validate_reference(&reference).is_ok() {
                    return Ok(ChatGptSetupRecoveryAction::ChangeNamedCredential(reference));
                }
                writeln!(
                    output,
                    "Named credential is invalid; choose an action again."
                )?;
            }
            "3" => return Ok(ChatGptSetupRecoveryAction::RemoveProvider),
            "4" => return Ok(ChatGptSetupRecoveryAction::CancelSetup),
            _ => writeln!(output, "Invalid selection; choose 1, 2, 3, or 4.")?,
        }
    }
    writeln!(
        output,
        "Too many invalid selections; setup was cancelled without saving."
    )?;
    Ok(ChatGptSetupRecoveryAction::CancelSetup)
}

impl ChatGptSetupRecoveryEditor for TerminalChatGptSetupRecoveryEditor {
    fn choose(&mut self, recovery: &ChatGptSetupRecovery) -> Result<ChatGptSetupRecoveryAction> {
        tracing::debug!(
            invocation = ?recovery.invocation,
            cause = ?recovery.cause,
            "ChatGPT setup entered secret-free recovery"
        );
        let stdin = io::stdin();
        let mut input = stdin.lock();
        let stderr = io::stderr();
        let mut output = stderr.lock();
        choose_chatgpt_setup_recovery_with_io(recovery, &mut input, &mut output)
    }
}

pub(super) enum ChatGptSetupAttempt {
    Ready {
        config: crate::config::Config,
        compensations: Vec<crate::cli::chatgpt_auth::ChatGptCompensationHandle>,
    },
    Recoverable(ChatGptSetupRecovery),
}

/// Validate and save setup through the shared first-run/command/REPL boundary.
///
/// Unsupported legacy ChatGPT subscription profiles are rejected before any
/// provider, network, or process boundary is reached.
pub async fn validate_and_apply_for(
    invocation: SetupInvocation,
    result: &SetupResult,
) -> Result<SetupApplyOutcome> {
    tracing::debug!(?invocation, "Starting shared setup commit ceremony");
    if result
        .providers
        .iter()
        .any(|provider| matches!(provider, ProviderEntry::LegacyChatgptSubscription { .. }))
    {
        anyhow::bail!(
            "Legacy chatgpt_subscription profiles are unsupported because Finch no longer launches Codex app-server. Remove that profile and configure OpenAI Platform with an API key or another supported provider"
        );
    }
    if chatgpt_setup_references(result).is_empty() {
        apply_and_save(result)?;
        return Ok(SetupApplyOutcome::Saved);
    }
    let service = crate::cli::chatgpt_auth::ChatGptAuthService::production()?;
    let mut editor = TerminalChatGptSetupRecoveryEditor;
    let Some((config, committed_result, compensations)) =
        run_chatgpt_setup_recovery_loop(invocation, result, &service, &mut editor).await?
    else {
        return Ok(SetupApplyOutcome::Cancelled);
    };
    save_chatgpt_setup_config(&config, &compensations, &service, |config| config.save())?;
    if let Some(prompt) = committed_result.custom_system_prompt.as_deref() {
        crate::config::Persona::save_system_prompt_override(
            &committed_result.default_persona,
            prompt,
        )?;
    }
    Ok(SetupApplyOutcome::Saved)
}

pub(super) fn save_chatgpt_setup_config<A, F>(
    config: &crate::config::Config,
    compensations: &[crate::cli::chatgpt_auth::ChatGptCompensationHandle],
    authenticator: &A,
    save: F,
) -> Result<()>
where
    A: crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator,
    F: FnOnce(&crate::config::Config) -> Result<()>,
{
    if let Err(error) = save(config) {
        let failures = compensate_chatgpt_setup(authenticator, compensations);
        if !failures.is_empty() {
            anyhow::bail!("ChatGPT setup configuration could not be saved or rolled back safely. Run `finch auth status chatgpt` before trying again");
        }
        return Err(error).context(
            "ChatGPT setup configuration was not saved; newly issued credentials were rolled back",
        );
    }
    Ok(())
}

pub(super) const MAX_CHATGPT_SETUP_ATTEMPTS: usize = 8;

pub(super) async fn run_chatgpt_setup_recovery_loop<A, E>(
    invocation: SetupInvocation,
    result: &SetupResult,
    authenticator: &A,
    editor: &mut E,
) -> Result<
    Option<(
        crate::config::Config,
        SetupResult,
        Vec<crate::cli::chatgpt_auth::ChatGptCompensationHandle>,
    )>,
>
where
    A: crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator,
    E: ChatGptSetupRecoveryEditor,
{
    let mut working = result.clone();
    let mut auth_attempts = 0;
    loop {
        let references = chatgpt_setup_references(&working);
        if references.is_empty() {
            return Ok(Some((
                config_from_setup_result(&working),
                working,
                Vec::new(),
            )));
        }
        if auth_attempts == MAX_CHATGPT_SETUP_ATTEMPTS {
            anyhow::bail!(
                "ChatGPT setup reached the retry limit; setup was not saved and no authorization remains pending"
            );
        }
        auth_attempts += 1;

        let cancel = tokio_util::sync::CancellationToken::new();
        let signal_cancel = cancel.clone();
        let signal = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal_cancel.cancel();
            }
        });
        let attempt =
            prepare_chatgpt_setup_attempt(invocation, &working, &references, authenticator, cancel)
                .await;
        signal.abort();
        let _ = signal.await;

        match attempt? {
            ChatGptSetupAttempt::Ready {
                config,
                compensations,
            } => return Ok(Some((config, working, compensations))),
            ChatGptSetupAttempt::Recoverable(recovery) => match editor.choose(&recovery)? {
                ChatGptSetupRecoveryAction::RetrySignIn => {}
                ChatGptSetupRecoveryAction::ChangeNamedCredential(replacement) => {
                    replace_chatgpt_setup_reference(
                        &mut working,
                        &recovery.credential_ref,
                        &replacement,
                    )?;
                }
                ChatGptSetupRecoveryAction::RemoveProvider => {
                    remove_chatgpt_setup_provider(&mut working, &recovery.credential_ref);
                }
                ChatGptSetupRecoveryAction::CancelSetup => return Ok(None),
            },
        }
    }
}

pub(super) fn chatgpt_setup_references(result: &SetupResult) -> std::collections::BTreeSet<String> {
    result
        .providers
        .iter()
        .filter_map(|provider| match provider {
            ProviderEntry::Credentialed {
                provider: crate::config::CredentialProvider::ChatgptSubscription,
                credential,
                ..
            } => Some(credential.credential_ref.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn replace_chatgpt_setup_reference(
    result: &mut SetupResult,
    current: &str,
    replacement: &str,
) -> Result<()> {
    crate::oauth::validate_reference(replacement)?;
    let mut changed = false;
    for provider in &mut result.providers {
        let ProviderEntry::Credentialed {
            provider: crate::config::CredentialProvider::ChatgptSubscription,
            credential,
            ..
        } = provider
        else {
            continue;
        };
        if credential.credential_ref == current {
            credential.credential_ref = replacement.to_string();
            credential.account = None;
            changed = true;
        }
    }
    if !changed {
        anyhow::bail!("ChatGPT recovery credential no longer matches the edited provider graph");
    }
    Ok(())
}

pub(super) fn remove_chatgpt_setup_provider(result: &mut SetupResult, reference: &str) {
    result.providers.retain(|provider| {
        !matches!(
            provider,
            ProviderEntry::Credentialed {
                provider: crate::config::CredentialProvider::ChatgptSubscription,
                credential,
                ..
            } if credential.credential_ref == reference
        )
    });
}

#[cfg(test)]
pub(super) async fn prepare_chatgpt_setup_config<A>(
    result: &SetupResult,
    chatgpt_references: &std::collections::BTreeSet<String>,
    authenticator: &A,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<crate::config::Config>
where
    A: crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator,
{
    match prepare_chatgpt_setup_attempt(
        SetupInvocation::Command,
        result,
        chatgpt_references,
        authenticator,
        cancel,
    )
    .await?
    {
        ChatGptSetupAttempt::Ready { config, .. } => Ok(config),
        ChatGptSetupAttempt::Recoverable(recovery) => anyhow::bail!(recovery.summary),
    }
}

pub(super) async fn prepare_chatgpt_setup_attempt<A>(
    invocation: SetupInvocation,
    result: &SetupResult,
    chatgpt_references: &std::collections::BTreeSet<String>,
    authenticator: &A,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<ChatGptSetupAttempt>
where
    A: crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator,
{
    // Prove the entire secret-free graph is structurally valid before opening
    // an OAuth endpoint. Account identity remains deliberately unknown until a
    // signed token is returned.
    let mut preflight_credentials = result.credentials.clone();
    for reference in chatgpt_references {
        let existing = preflight_credentials
            .iter()
            .find(|credential| credential.name == *reference);
        if let Some(existing) = existing {
            if !is_exact_chatgpt_setup_credential(existing, reference) {
                anyhow::bail!(
                    "Stored named credential '{reference}' does not satisfy the exact ChatGPT setup authority contract"
                );
            }
            // Existing records are static configuration authority. Preserve
            // valid leases exactly. A refreshable access lease may be expired
            // here; the provider refreshes it when inference uses it.
            if is_reusable_chatgpt_setup_credential(existing) {
                continue;
            }
            preflight_credentials.retain(|credential| credential.name != *reference);
        }
        preflight_credentials.push(crate::config::ProviderCredential {
            name: reference.clone(),
            kind: crate::config::CredentialKind::OauthDevice,
            provider: crate::config::CredentialProvider::ChatgptSubscription,
            issuer: "openai-chatgpt".into(),
            audience: crate::config::AudienceBinding::standard(
                crate::config::EndpointFamily::ChatgptSubscription,
            ),
            tenant: None,
            project: None,
            account: None,
            scopes: crate::providers::chatgpt_oauth::chatgpt_required_scopes(),
            secret_ref: format!("oauth-store:{reference}"),
            lifecycle: crate::config::CredentialLifecycle::Active {
                expires_at: None,
                refreshable: true,
            },
            revocation: Default::default(),
        });
    }
    config_from_setup_result(result)
        .with_credentials(preflight_credentials)
        .validate()
        .context("Setup provider graph is invalid; ChatGPT login was not started")?;

    let mut credentials = result.credentials.clone();
    let mut compensations = Vec::new();
    for reference in chatgpt_references {
        if credentials
            .iter()
            .find(|credential| credential.name == *reference)
            .is_some_and(|credential| {
                is_exact_chatgpt_setup_credential(credential, reference)
                    && is_reusable_chatgpt_setup_credential(credential)
            })
        {
            // Preflight already proved this persisted record satisfies the
            // exact ChatGPT authority contract. Setup is not a provider-use
            // boundary, so it must neither refresh nor rewrite the lease.
            continue;
        }
        let ensured = match authenticator
            .ensure_named_credential(
                &reference,
                crate::cli::chatgpt_auth::DeviceLoginPresentation::default(),
                cancel.clone(),
            )
            .await
        {
            Ok(ensured) => ensured,
            Err(error) => {
                let failures = compensate_chatgpt_setup(authenticator, &compensations);
                if !failures.is_empty() {
                    anyhow::bail!("ChatGPT sign-in could not be rolled back safely. Setup was not saved. Run `finch auth status chatgpt` before trying again");
                }
                let cause = chatgpt_setup_failure_cause(&error);
                let summary = chatgpt_setup_failure_summary(cause);
                return Ok(ChatGptSetupAttempt::Recoverable(ChatGptSetupRecovery {
                    invocation,
                    credential_ref: reference.clone(),
                    cause,
                    summary,
                }));
            }
        };
        if let Some(compensation) = ensured.compensation {
            compensations.push(compensation);
        }
        credentials.retain(|existing| existing.name != *reference);
        credentials.push(ensured.credential);
    }
    let config = config_from_setup_result(result).with_credentials(credentials);
    if let Err(error) = config.validate() {
        let failures = compensate_chatgpt_setup(authenticator, &compensations);
        if !failures.is_empty() {
            anyhow::bail!("ChatGPT sign-in could not be rolled back safely. Setup was not saved. Run `finch auth status chatgpt` before trying again");
        }
        let cause = chatgpt_setup_failure_cause(&error);
        return Ok(ChatGptSetupAttempt::Recoverable(ChatGptSetupRecovery {
            invocation,
            credential_ref: chatgpt_references
                .iter()
                .next()
                .cloned()
                .unwrap_or_else(|| "chatgpt:default".into()),
            cause,
            summary: chatgpt_setup_failure_summary(cause),
        }));
    }
    Ok(ChatGptSetupAttempt::Ready {
        config,
        compensations,
    })
}

pub(super) fn is_exact_chatgpt_setup_credential(
    credential: &crate::config::ProviderCredential,
    reference: &str,
) -> bool {
    credential.kind == crate::config::CredentialKind::OauthDevice
        && credential.provider == crate::config::CredentialProvider::ChatgptSubscription
        && credential.issuer == "openai-chatgpt"
        && credential.audience
            == crate::config::AudienceBinding::standard(
                crate::config::EndpointFamily::ChatgptSubscription,
            )
        && credential.secret_ref == format!("oauth-store:{reference}")
        && credential
            .account
            .as_deref()
            .is_some_and(|account| !account.is_empty())
        && crate::providers::chatgpt_oauth::chatgpt_required_scopes().is_subset(&credential.scopes)
}

pub(super) fn is_reusable_chatgpt_setup_credential(
    credential: &crate::config::ProviderCredential,
) -> bool {
    matches!(
        &credential.lifecycle,
        crate::config::CredentialLifecycle::Active {
            expires_at,
            refreshable,
        } if *refreshable || expires_at.as_ref().is_none_or(|expiry| expiry > &Utc::now())
    )
}

pub(super) fn chatgpt_setup_failure_cause(error: &anyhow::Error) -> ChatGptSetupFailureCause {
    if let Some(terminal) = error.downcast_ref::<crate::oauth::OAuthDeviceAuthorizationError>() {
        return match terminal {
            crate::oauth::OAuthDeviceAuthorizationError::Cancelled => {
                ChatGptSetupFailureCause::Cancelled
            }
            crate::oauth::OAuthDeviceAuthorizationError::Expired => {
                ChatGptSetupFailureCause::Expired
            }
            crate::oauth::OAuthDeviceAuthorizationError::Denied => ChatGptSetupFailureCause::Denied,
        };
    }
    if let Some(endpoint) =
        error.downcast_ref::<crate::providers::chatgpt_oauth::ChatGptDeviceEndpointError>()
    {
        return match endpoint {
            crate::providers::chatgpt_oauth::ChatGptDeviceEndpointError::StartDisabledOrUnsupported => {
                ChatGptSetupFailureCause::StartDisabledOrUnsupported
            }
            crate::providers::chatgpt_oauth::ChatGptDeviceEndpointError::StartRejected(_)
            | crate::providers::chatgpt_oauth::ChatGptDeviceEndpointError::PollRejected(_) => {
                ChatGptSetupFailureCause::ProviderRejected
            }
        };
    }
    if let Some(stage) =
        error.downcast_ref::<crate::providers::chatgpt_oauth::ChatGptAuthStageError>()
    {
        return match stage {
            crate::providers::chatgpt_oauth::ChatGptAuthStageError::PollContract => {
                ChatGptSetupFailureCause::PollContract
            }
            crate::providers::chatgpt_oauth::ChatGptAuthStageError::TokenExchangeRejected(_) => {
                ChatGptSetupFailureCause::TokenExchangeRejected
            }
            crate::providers::chatgpt_oauth::ChatGptAuthStageError::TokenExchangeContract => {
                ChatGptSetupFailureCause::TokenExchangeContract
            }
            crate::providers::chatgpt_oauth::ChatGptAuthStageError::IdentityVerification => {
                ChatGptSetupFailureCause::IdentityVerification
            }
            crate::providers::chatgpt_oauth::ChatGptAuthStageError::ClientBinding => {
                ChatGptSetupFailureCause::ClientBinding
            }
            crate::providers::chatgpt_oauth::ChatGptAuthStageError::AccountEntitlement => {
                ChatGptSetupFailureCause::AccountEntitlement
            }
        };
    }
    if error
        .downcast_ref::<crate::oauth::OAuthCredentialPersistenceError>()
        .is_some()
    {
        return ChatGptSetupFailureCause::Persistence;
    }
    ChatGptSetupFailureCause::ProtocolOrOther
}

pub(super) fn chatgpt_setup_failure_summary(cause: ChatGptSetupFailureCause) -> String {
    match cause {
        ChatGptSetupFailureCause::Cancelled => "ChatGPT sign-in was cancelled. No credential was saved. You can retry, change the named credential, remove the provider, or cancel setup.",
        ChatGptSetupFailureCause::Expired => "ChatGPT sign-in expired. No credential was saved. If device login is not enabled for this account or workspace, enable device-code authorization in ChatGPT Settings > Security, then Retry sign-in for a fresh one-time code.",
        ChatGptSetupFailureCause::Denied => "ChatGPT sign-in was denied. No credential was saved. Choose Retry sign-in, change the named credential, remove the provider, or cancel setup.",
        ChatGptSetupFailureCause::StartDisabledOrUnsupported => "ChatGPT device authorization is disabled or unsupported for this account. Check ChatGPT Settings > Security, then choose Retry sign-in. No credential was saved.",
        ChatGptSetupFailureCause::ProviderRejected => "ChatGPT rejected the sign-in request. No credential was saved. Retry sign-in or choose another provider/account action.",
        ChatGptSetupFailureCause::PollContract => "ChatGPT approved the browser sign-in, but Finch could not read the completed device response. No credential was saved. Update Finch or retry after checking for a newer release.",
        ChatGptSetupFailureCause::TokenExchangeRejected => "ChatGPT approved the browser sign-in, but rejected the authorization-code exchange. No credential was saved. Retry sign-in for a fresh code; if it repeats, update Finch.",
        ChatGptSetupFailureCause::TokenExchangeContract => "ChatGPT approved the browser sign-in, but its token response was incompatible with this Finch version. No credential was saved. Update Finch before retrying.",
        ChatGptSetupFailureCause::IdentityVerification => "ChatGPT approved the browser sign-in, but Finch could not verify the signed identity response. No credential was saved. Check connectivity to auth.openai.com and retry; update Finch if it repeats.",
        ChatGptSetupFailureCause::ClientBinding => "ChatGPT approved the browser sign-in, but the signed issuer, public client, or token lifetime did not match Finch's pinned ChatGPT contract. No credential was saved. Update Finch before retrying.",
        ChatGptSetupFailureCause::AccountEntitlement => "ChatGPT approved the browser sign-in, but the signed response did not contain a usable ChatGPT account identifier. No credential was saved. Retry with the intended account; update Finch if it repeats.",
        ChatGptSetupFailureCause::Persistence => "ChatGPT sign-in was validated, but Finch could not save the named credential. No active credential was committed. Check local credential-store permissions before retrying.",
        ChatGptSetupFailureCause::ProtocolOrOther => "ChatGPT sign-in failed. No credential was saved. Retry sign-in or choose another provider/account action.",
    }
    .into()
}

pub(super) fn compensate_chatgpt_setup<A>(
    authenticator: &A,
    handles: &[crate::cli::chatgpt_auth::ChatGptCompensationHandle],
) -> Vec<String>
where
    A: crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator,
{
    let mut failed = Vec::new();
    for handle in handles.iter().rev() {
        if authenticator.compensate_with_tombstone(handle).is_err() {
            failed.push(handle.reference().to_string());
        }
    }
    failed
}
