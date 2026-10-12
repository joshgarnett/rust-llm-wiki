//! Optional preparation shares the normal dispatcher and one command allowance.
use super::{
    Arguments,
    remote::{RemoteArguments, RemoteContext},
};
use crate::{
    app::{
        OfflineApp, OperationOptions,
        embeddings::{
            AutomaticPreparationOutcome, AutomaticPreparationState, EmbeddingRuntime,
            PreparationLimits, PreparationTrigger,
        },
    },
    config::{self, DefaultWiki},
    domain::*,
    output::Envelope,
};
use serde_json::{Value, json};

pub(super) fn consent_label(consent: &Value) -> &'static str {
    if consent == "matching" {
        "allowed for this wiki and model"
    } else if consent.get("binding_changed").is_some() {
        "needs permission for the changed wiki, destination or model"
    } else {
        "off"
    }
}

pub(super) fn preview(
    enabled: bool,
    trigger: PreparationTrigger,
    document: &config::LocalConfigDocument,
) -> Value {
    let mut limits = PreparationLimits::for_trigger(&trigger);
    let context = RemoteContext {
        profile: None,
        wiki_id: None,
        document,
        allow_ambient_provider: false,
    };
    let remote = RemoteArguments::default().effective(&context);
    limits.requests = limits.requests.min(remote.max_requests.unwrap_or(60));
    limits.deadline_ms = limits.deadline_ms.min(
        remote
            .deadline_ms
            .unwrap_or(crate::jobs::DEFAULT_DEADLINE_MS),
    );
    limits.request_bytes = limits
        .request_bytes
        .min(remote.max_request_bytes.unwrap_or(limits.request_bytes));
    limits.response_bytes = limits
        .response_bytes
        .min(remote.max_response_bytes.unwrap_or(limits.response_bytes));
    json!({"state":"skipped","reason":"preview","enabled":enabled,
        "planned_limits":limits,"deadline_scope":"starting_additional_work",
        "network_used":false,"credentials_resolved":false,"inputs":null,"cache_state":"unknown"})
}

fn skipped(reason: &str) -> Value {
    json!({"state":"skipped","reason":reason,"observations":{"network_used":false},"error":null,"report":null})
}
fn unavailable(error: WikiError) -> Value {
    if error.code == ErrorCode::ProfileUntrusted {
        return skipped("permission_absent");
    }
    // Config parser details can contain private references. Public secondary
    // status carries only the code and a plain next step until runtime exists.
    json!({"state":"blocked","reason":"automatic_permission_unavailable",
        "error":{"code":error.code,"message":"Automatic similarity search is unavailable. Run lwiki config show to review the selected model and permission."},
        "observations":{"network_used":false},"report":null})
}
pub(super) fn runtime(remote: &crate::app::remote::RemoteRuntime) -> EmbeddingRuntime<'_> {
    EmbeddingRuntime::new(
        &remote.service,
        &remote.dispatcher,
        remote.job_options.clone(),
        remote.limits.clone(),
        remote.created_at_utc_ms,
        remote.deadline_utc_ms,
        remote.requested_limits.clone(),
    )
}

/// The fallback closure performs ordinary local lookup. It runs while the same
/// automatic runtime is still retained, before the final accounting refresh.
pub(super) fn lookup<T>(
    app: &OfflineApp,
    context: &RemoteContext<'_>,
    remote: &RemoteArguments,
    query: &str,
    no_sync: bool,
    semantic: impl FnOnce(&EmbeddingRuntime<'_>) -> Result<T>,
    local: impl FnOnce() -> Result<T>,
) -> Result<(T, Value)> {
    if app.options().offline || app.options().dry_run || no_sync {
        let reason = if app.options().offline {
            "offline"
        } else if no_sync {
            "no_sync"
        } else {
            "dry_run"
        };
        return local().map(|result| (result, skipped(reason)));
    }
    let remote_runtime = match remote.automatic_runtime(app, context) {
        Ok(runtime) => runtime,
        Err(error) => return local().map(|result| (result, unavailable(error))),
    };
    let settings = super::embeddings::EmbeddingSettingsArguments::default()
        .settings(&context.document.embeddings);
    let scoped = match runtime(&remote_runtime).for_automatic_operation(
        PreparationTrigger::Lookup {
            query: query.into(),
        },
        &settings,
    ) {
        Ok(scoped) => scoped,
        Err(error) => return local().map(|result| (result, unavailable(error))),
    };
    let mut outcome = app.prepare_automatic_embeddings(&settings, &scoped);
    let prepared_inputs = outcome.observations.generated_inputs;
    let result = if matches!(
        outcome.state,
        AutomaticPreparationState::Ready | AutomaticPreparationState::Partial
    ) {
        match semantic(&scoped) {
            Ok(result) => Ok(result),
            Err(error) => {
                outcome.error = Some(error);
                outcome.state = AutomaticPreparationState::Pending;
                local()
            }
        }
    } else {
        local()
    };
    outcome.observations = app.refresh_automatic_accounting(&scoped);
    let status = prepared_status(outcome, &remote_runtime.service, prepared_inputs);
    match result {
        Ok(result) => Ok((result, status)),
        Err(mut error) => {
            error.network_used |= remote_runtime.dispatcher.network_used();
            if !error.details.is_object() {
                error.details = json!({"context":error.details});
            }
            error.details["automatic_embeddings"] = status;
            Err(error)
        }
    }
}

pub(super) fn prepare(
    app: &OfflineApp,
    context: &RemoteContext<'_>,
    trigger: PreparationTrigger,
) -> Value {
    if app.options().offline || app.options().dry_run || app.options().stage_only {
        return skipped(if app.options().offline {
            "offline"
        } else if app.options().dry_run {
            "dry_run"
        } else {
            "staged"
        });
    }
    let remote = match RemoteArguments::default().automatic_runtime(app, context) {
        Ok(remote) => remote,
        Err(error) => return unavailable(error),
    };
    let settings = super::embeddings::EmbeddingSettingsArguments::default()
        .settings(&context.document.embeddings);
    match runtime(&remote).for_automatic_operation(trigger, &settings) {
        Ok(scoped) => {
            let outcome = app.prepare_automatic_embeddings(&settings, &scoped);
            let prepared_inputs = outcome.observations.generated_inputs;
            prepared_status(outcome, &remote.service, prepared_inputs)
        }
        Err(error) => unavailable(error),
    }
}

// Freeze the corpus count before query embedding. The invocation observation
// includes query requests, which must not be described as newly prepared notes.
fn prepared_status(
    outcome: AutomaticPreparationOutcome,
    service: &crate::config::providers::TrustedService,
    prepared_inputs: usize,
) -> Value {
    let mut status = serde_json::to_value(outcome).expect("serializable automatic outcome");
    let host = url::Url::parse(&service.service().url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned));
    status["prepared_inputs"] = json!(prepared_inputs);
    status["provider"] = json!({"host":host,"model":service.summary().model});
    status
}

pub(super) fn attach(envelope: &mut Envelope, status: Value, lexical: bool) {
    envelope.meta.network_used |= status["observations"]["network_used"]
        .as_bool()
        .unwrap_or(false);
    if lexical {
        envelope.data["automatic_lookup"] =
            json!({"requested_mode":"hybrid","returned_mode":"lexical"});
        envelope.warnings.push(
            "Similarity search is unavailable for this lookup; results use local text matching."
                .into(),
        );
    }
    envelope.data["automatic_embeddings"] = status;
}

/// Only completed public writes supply owners. Proposed operations, reused saves
/// and operational receipts cannot schedule another automatic operation.
pub(super) fn committed_owners(app: &OfflineApp, data: &Value) -> Vec<VaultRelativePath> {
    if data["status"] != "committed" || data["reused"] == true || data["dry_run"] == true {
        return Vec::new();
    }
    let mut owners = std::collections::BTreeSet::new();
    if let Some(operations) = data["plan"]["operations"].as_array() {
        for operation in operations {
            if operation["before"] == operation["after"] {
                continue;
            }
            let Some(path) = operation["path"]
                .as_str()
                .and_then(|path| VaultRelativePath::new(path).ok())
            else {
                continue;
            };
            if path.as_str().starts_with(".wiki/")
                || path.as_str().starts_with("runs/")
                || path.as_str().starts_with("changes/")
                || path.as_str().starts_with("research/")
            {
                continue;
            }
            let bytes = match crate::changes::prepare::read_bounded(
                app.fs(),
                &path,
                crate::app::MAX_INPUT_BYTES,
            ) {
                Ok(Some(bytes)) => bytes,
                _ => continue,
            };
            let parsed = crate::records::parse_note(&bytes);
            if parsed.canonical.as_ref().is_some_and(|record| {
                matches!(
                    record.kind(),
                    RecordKind::Source
                        | RecordKind::Page
                        | RecordKind::Entity
                        | RecordKind::Assertion
                        | RecordKind::Evidence
                )
            }) || parsed.canonical.is_none()
                && matches!(
                    std::path::Path::new(path.as_str())
                        .extension()
                        .and_then(|s| s.to_str()),
                    Some("md" | "txt")
                )
            {
                owners.insert(path);
            }
        }
    }
    owners.into_iter().collect()
}

pub(super) fn after_setup(
    args: &Arguments,
    wiki: &DefaultWiki,
    profile: Option<&str>,
    configuration: Option<&std::path::Path>,
    envelope: &mut Envelope,
) {
    let result = (|| -> Result<Value> {
        let loaded = config::local::load_user_config(
            configuration.or(args.preferences.as_deref()),
            args.no_user_config,
        )?;
        let root = crate::vault::VaultRoot::explicit(&wiki.path)?;
        let fs = crate::vault::VaultFs::new(root);
        let preferences = config::local::resolve_loaded(
            &fs,
            &wiki.wiki_id,
            &config::PreferenceOptions {
                offline: args.offline,
                profile: profile.map(str::to_owned).or_else(|| args.profile.clone()),
                lock_timeout_ms: args.lock_timeout_ms,
                ..Default::default()
            },
            &loaded,
        )?;
        let app = OfflineApp::new(
            fs,
            OperationOptions {
                offline: preferences.offline,
                dry_run: args.dry_run,
                stage_only: false,
                lock_timeout_ms: preferences.lock_timeout_ms,
            },
        )?;
        let context = RemoteContext {
            profile: preferences.profile.as_deref(),
            wiki_id: Some(&wiki.wiki_id),
            document: loaded.document(),
            allow_ambient_provider: loaded.summary().origin
                != config::local::UserConfigOrigin::Disabled,
        };
        Ok(prepare(&app, &context, PreparationTrigger::Setup))
    })();
    attach(envelope, result.unwrap_or_else(unavailable), false);
}

pub(super) fn present(status: &Value, output: &mut impl std::io::Write) -> std::io::Result<()> {
    if !status.is_object() {
        return Ok(());
    }
    let message = match status["state"].as_str() {
        Some("ready") => "Similarity search is ready.",
        Some("partial") => {
            "Similarity search is partly prepared. Local text matching remains available."
        }
        Some("pending") => {
            "Similarity search preparation is pending. Local text matching remains available."
        }
        Some("failed") => {
            "Similarity search preparation failed. Your saved notes remain available."
        }
        Some("blocked") => "Automatic similarity search needs attention. Run lwiki config show.",
        _ => return Ok(()),
    };
    if let Some(count) = status["prepared_inputs"]
        .as_u64()
        .filter(|count| *count > 0)
    {
        write!(output, "Prepared {count} text chunks")?;
        if let Some(host) = status["provider"]["host"].as_str() {
            write!(
                output,
                " via {}",
                super::dispatch::terminal_text(host, false)
            )?;
        }
        write!(output, ". ")?;
    }
    writeln!(output, "{message}")
}

/// Pure, bounded first-attempt estimates. Cache work remains unknown.
pub(super) fn sync_payload_preview(
    app: &OfflineApp,
    remote: &RemoteArguments,
    context: &RemoteContext<'_>,
    settings: &crate::retrieval::spaces::EmbeddingSettings,
) -> Result<Value> {
    use crate::{
        catalog::{
            Catalog,
            query_types::{QueryCatalog, QueryReadLimits},
        },
        retrieval::{
            indexed_units::{self, UnitBudget, UnitLimits},
            render,
            spaces::SpaceSpec,
        },
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let effective = remote.effective(context);
    let profile = context.profile.ok_or_else(|| {
        WikiError::new(
            ErrorCode::ProfileUntrusted,
            "choose an embedding model to preview its request",
        )
    })?;
    let path = effective
        .providers_config
        .as_ref()
        .map(|path| Ok(path.clone()))
        .unwrap_or_else(|| {
            if context.allow_ambient_provider {
                crate::app::remote::default_provider_path()
            } else {
                Err(WikiError::new(
                    ErrorCode::ProfileUntrusted,
                    "choose private provider settings to preview a request",
                ))
            }
        })?;
    let providers = config::providers::ProviderConfig::load(&path)?;
    let service = providers
        .authorize(
            app.fs(),
            app.vault_id(),
            profile,
            crate::jobs::Capability::Embed,
        )
        .or_else(|_| providers.authorize_automatic_embeddings(app.fs(), app.vault_id(), profile))?;
    let spec = SpaceSpec::from_service(&service, settings.clone())?;
    let space = spec.id()?;
    let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
    let mut inputs = Vec::new();
    let mut input_bytes = 0usize;
    let mut complete = true;
    let mut reason = None;
    fn append(
        inputs: &mut Vec<crate::providers::types::EmbeddingInput>,
        bytes: &mut usize,
        unit: render::RenderedUnit,
        deadline: std::time::Instant,
    ) -> bool {
        if std::time::Instant::now() >= deadline
            || inputs.len() >= 128
            || bytes.saturating_add(unit.utf8.len()) > 1024 * 1024
        {
            return false;
        }
        *bytes += unit.utf8.len();
        inputs.push(unit.input());
        true
    }
    let layout;
    if catalog.operation_state()?.is_some() {
        layout = "normalized";
        let reader = match catalog.current_query_snapshot(QueryReadLimits {
            max_elapsed_ms: 5000,
            max_bytes: 64 * 1024 * 1024,
            ..Default::default()
        })? {
            crate::catalog::transient_read::CurrentQueryAttempt::Ready(reader) => *reader,
            crate::catalog::transient_read::CurrentQueryAttempt::BoundedUnavailable(error) => {
                return Err(error);
            }
        };
        let budget = UnitBudget::with_deadline(
            UnitLimits {
                max_render_bytes: 2 * 1024 * 1024,
                max_units: 256,
                max_owner_render_bytes: 1024 * 1024,
                ..Default::default()
            },
            deadline,
        )?;
        let paths = reader.embedding_document_paths(None, 128)?;
        if paths.len() == 128 {
            complete = false;
            reason = Some("preview_owner_limit");
        }
        for path in paths {
            let document = reader.document(&path)?.ok_or_else(|| {
                WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "preview note changed during rendering",
                )
            })?;
            let units = indexed_units::render_owner(
                &document,
                settings,
                Blake3Hash::digest(b"detached-embedding-preview"),
                &budget,
            )?;
            let mut stop = false;
            for unit in units {
                if !append(&mut inputs, &mut input_bytes, unit, deadline) {
                    complete = false;
                    reason = Some("preview_render_limit");
                    stop = true;
                    break;
                }
            }
            if stop {
                break;
            }
        }
        reader.verify_operations(&catalog)?;
    } else {
        layout = "legacy";
        let budget = crate::catalog::scan::ReadScanBudget::with_deadline(
            crate::catalog::scan::ReadScanLimits {
                max_elapsed_ms: 5000,
                ..Default::default()
            },
            deadline,
        )?;
        let reader = catalog.read_only_snapshot_with_budget(&budget)?;
        for unit in render::corpus_iter(&reader, settings)? {
            if !append(&mut inputs, &mut input_bytes, unit?, deadline) {
                complete = false;
                reason = Some("preview_render_limit");
                break;
            }
        }
        catalog.recheck_read_snapshot(&reader, &budget)?;
    }
    let limits = effective.limits()?;
    let now = crate::app::remote::native_job_options(
        crate::jobs::ExecutionPolicy {
            dry_run: true,
            offline: true,
            retry_uncertain: false,
        },
        0,
    )
    .clock
    .read()?
    .utc_ms;
    let end = now
        .checked_add(
            i64::try_from(
                effective
                    .deadline_ms
                    .unwrap_or(crate::jobs::DEFAULT_DEADLINE_MS),
            )
            .map_err(|_| WikiError::new(ErrorCode::Usage, "preview deadline overflows"))?,
        )
        .ok_or_else(|| WikiError::new(ErrorCode::Usage, "preview deadline overflows"))?;
    let batch_items = usize::from(service.service().max_batch_items.unwrap_or(32)).min(128);
    let mut batches = Vec::new();
    let mut totals = crate::jobs::budgets::zero();
    let mut offset = 0;
    while offset < inputs.len() {
        if std::time::Instant::now() >= deadline {
            complete = false;
            reason = Some("preview_deadline");
            break;
        }
        let mut take = batch_items.min(inputs.len() - offset);
        let (bound, quoted) = loop {
            let result = crate::providers::preview_embedding_request(
                &service,
                &inputs[offset..offset + take],
                spec.dimensions,
                &space,
            )
            .and_then(|mut bound| {
                bound.timeout_ms = bound.timeout_ms.min((end - now) as u64);
                bound.bounds_fingerprint = crate::jobs::budgets::bound_fingerprint(&bound)?;
                let quote = crate::jobs::budgets::quote_bound(&bound, &limits, now, end)?;
                crate::jobs::budgets::fits(
                    &crate::jobs::budgets::zero(),
                    &totals,
                    &quote,
                    &limits,
                )?;
                Ok((bound, quote))
            });
            match result {
                Ok(pair) => break pair,
                Err(error) if take > 1 && error.code == ErrorCode::BudgetExceeded => {
                    take /= 2;
                }
                Err(error) => {
                    return Ok(
                        json!({"state":"partial","layout":layout,"counts_complete":false,
                        "reason":"preview_request_limit","limitation":{"code":error.code,"message":error.message},
                        "eligible_rendered_inputs":inputs.len(),"rendered_utf8_bytes":input_bytes,"batches":batches,
                        "estimated_first_attempt_allowance":totals,"actual_pending_inputs":null,
                        "cache_state":"unknown","network_used":false,"credentials_resolved":false}),
                    );
                }
            }
        };
        crate::jobs::budgets::add(&mut totals, &quoted)?;
        batches.push(json!({"inputs":inputs[offset..offset + take].iter().map(|input|
            json!({"input_hash":input.input_hash,"utf8_bytes":input.utf8.len()})).collect::<Vec<_>>(),
            "request_bytes":bound.request_bytes,"maximum_response_bytes":bound.response_bytes,"wire_hash":bound.wire_hash}));
        offset += take;
    }
    Ok(
        json!({"state":if complete {"complete"} else {"partial"},"layout":layout,"counts_complete":complete,
        "reason":reason,"eligible_rendered_inputs":inputs.len(),"rendered_utf8_bytes":input_bytes,"batches":batches,
        "estimated_first_attempt_allowance":totals,"actual_pending_inputs":null,"cache_state":"unknown",
        "network_used":false,"credentials_resolved":false}),
    )
}

#[cfg(test)]
#[path = "automatic_embeddings_tests.rs"]
mod tests;
