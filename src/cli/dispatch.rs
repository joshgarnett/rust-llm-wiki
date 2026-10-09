use super::arguments::*;
use crate::{
    app::*,
    catalog::{
        Catalog, CatalogDiagnostic, CatalogGraphValidator, CatalogOptions, DocumentRow,
        ReaderSnapshot, SnapshotVerification,
        query::QuerySnapshot,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    changes::ChangeEngine,
    config::{self, PreferenceOptions},
    domain::*,
    jobs::{Capability, JobLedgerApi},
    output::{Envelope, ErrorOutput, Metadata},
    records::parse_note,
    retrieval,
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    vault::{VaultFs, WriterPermit},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::Path,
    time::Duration,
};

pub const COMMANDS: &[&str] = &[
    "capabilities",
    "skill export",
    "schema",
    "init",
    "read",
    "page put",
    "page init",
    "page batch",
    "page rename",
    "source add",
    "source refresh",
    "source refresh-batch",
    "source withdraw",
    "source import prepare",
    "source import run",
    "source import resume",
    "source import status",
    "evidence revalidate",
    "index sync",
    "index rebuild",
    "embeddings check",
    "embeddings sync",
    "search",
    "context",
    "graph extract",
    "graph import",
    "graph resolve",
    "graph decide",
    "graph review",
    "graph query",
    "graph neighbors",
    "check",
    "doctor",
    "research plan",
    "research run",
    "research resume",
    "research import",
    "research status",
    "research report",
    "research maintenance",
    "storage inventory",
    "storage plan",
    "storage cleanup",
    "changes show",
    "changes apply",
    "changes abort",
    "changes rollback",
    "changes resolve",
    "jobs amend",
    "jobs status",
    "jobs diagnostics inspect",
    "jobs diagnostics prune",
    "recover",
    "migrate",
];
pub const SCHEMAS: &[&str] = &[
    "context-selection",
    "output",
    "record",
    "page",
    "page-batch",
    "page-source-refs",
    "source-refresh-batch",
    "stream",
    "extraction",
    "extraction-packet",
    "extraction-state",
    "graph-resolution",
    "graph-resolution-receipt",
    "run",
    "run-event",
    "usage-receipt",
    "entity-decisions",
    "entity-decision-receipt",
    "graph-review",
    "graph-review-receipt",
    "research-packet",
    "research-submission",
];
fn usage(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::Usage, message)
}
fn value<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))
}
fn failure(command: &str, error: WikiError) -> Envelope {
    let mut envelope = Envelope::failure(command, &error.code.to_string(), error.message.clone());
    envelope.meta.network_used = error.network_used;
    envelope.meta.wiki_id = error
        .details
        .get("wiki_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(change) = error.details.get("change") {
        envelope.data = json!({"change":change});
        envelope.meta.partial = true;
    }
    if error.details.get("partial_export").and_then(Value::as_bool) == Some(true) {
        envelope.meta.partial = true;
    }
    if command == "check" && error.details.get("complete").and_then(Value::as_bool) == Some(false) {
        envelope.meta.partial = true;
    }
    envelope.error = Some(error_output(error));
    envelope
}
pub fn execute(args: &Arguments) -> (Envelope, u8) {
    match execute_inner(args) {
        Ok(envelope) => {
            let exit = if envelope.ok { 0 } else { 9 };
            (envelope, exit)
        }
        Err(mut error) => {
            // Retain known local identity even when an operation fails after binding.
            let local_preparation = matches!(
                &args.command,
                Command::Source { command: SourceCommand::Import(arguments) }
                    if !arguments.command.requires_vault()
            );
            if !local_preparation
                && let Ok(cwd) = std::env::current_dir()
                && let Ok(root) =
                    crate::vault::discovery::resolve(args.wiki.as_deref(), &cwd, false)
                && let Ok(binding) = ChangeEngine::new(VaultFs::new(root))
            {
                if !error.details.is_object() {
                    error.details = json!({"context":error.details});
                }
                error.details["wiki_id"] = binding.vault_id().as_str().into();
            }
            let exit = error.exit_code();
            (failure(args.command.name(), error), exit)
        }
    }
}
fn operation_options(args: &Arguments, offline: bool, timeout: u64) -> OperationOptions {
    OperationOptions {
        dry_run: args.dry_run,
        stage_only: args.stage,
        offline,
        lock_timeout_ms: timeout,
    }
}
fn execute_inner(args: &Arguments) -> Result<Envelope> {
    let command = args.command.name();
    if args.output_format() == OutputFormat::Jsonl && !args.command.streaming() {
        return Err(usage(
            "JSONL is supported for index sync/rebuild, recover, changes apply, source add/refresh, research run/resume, and doctor --probe",
        ));
    }
    match &args.command {
        Command::Source {
            command: SourceCommand::Import(arguments),
        } if !arguments.command.requires_vault() => {
            if args.stage {
                return Err(usage(
                    "source import prepare cannot be staged; it prepares a standalone manifest",
                ));
            }
            let super::source_import::SourceImportCommand::Prepare { input_list, output } =
                &arguments.command
            else {
                unreachable!("only manifest preparation does not require a vault");
            };
            return Ok(Envelope::success(
                command,
                value(crate::app::prepare_source_import(
                    input_list,
                    output,
                    args.dry_run,
                )?)?,
            ));
        }
        Command::Capabilities => {
            return Ok(Envelope::success(
                command,
                json!({"version":env!("CARGO_PKG_VERSION"),"commands":COMMANDS,"schemas":SCHEMAS,"network":true,"read":{"start_without_end":true,"omitted_end":"body_eof","coordinates":"utf8_body_bytes","explicit_ranges":"strict","byte_limit":"returned_text","dry_run":"unresolved_request_only"},"page_source_refs":{"commands":["page init","page put"],"flag":"--source-refs","schema":"page-source-refs","max_input_bytes":65536,"max_references":16,"citation_kinds":["source"],"links":"relative_to_resolved_page_path","prose_support_verified":false,"dry_run":"request_validation_only"},"search_modes":["literal","lexical","semantic","hybrid"],"selected_search":{"flag":"--verify-selected","layout":"normalized","modes":["literal","lexical","semantic","hybrid"],"no_sync_compatible":true,"scope":"displayed document dependencies","global_membership_verified":false,"dry_run":"request validation only","budget":{"max_bytes":67108864,"max_files":4096,"max_entries":16384,"max_elapsed_ms":2000}},"selected_neighbors":{"command":"graph neighbors","layout":"normalized","root_kind":"entity","default_verification":true,"explicit_verification_flag":"--verify-selected","no_sync":"cached_uncited","scope":"selected_graph_neighbors","global_membership_verified":false,"current_only":true,"navigation":false,"cursor":false,"dry_run":"request validation only","limits":{"depth":2,"incident_per_seed":16,"assertions":128,"candidates":80,"hits":50,"support":2,"contrary":1},"budget":{"max_bytes":67108864,"max_files":4096,"max_entries":16384,"max_elapsed_ms":2000}},"graph_seed_modes":["lexical","semantic"],"extraction_executors":["agent","api"],"research_executor":"agent-handoff","jsonl_commands":["index sync","index rebuild","recover","changes apply","source add","source refresh","research run","research resume","research import","doctor --probe"]}),
            ));
        }
        Command::Schema { name } => {
            let schema = match name.as_str() {
                "context-selection" => include_str!("../../schemas/context-selection-v1.json"),
                "output" => include_str!("../../schemas/output-v1.json"),
                "research-packet" => include_str!("../../schemas/research-packet-v1.json"),
                "research-submission" => include_str!("../../schemas/research-submission-v1.json"),
                "record" | "page" => include_str!("../../schemas/record-v1.json"),
                "page-batch" => include_str!("../../schemas/page-batch-v1.json"),
                "page-source-refs" => include_str!("../../schemas/page-source-refs-v1.json"),
                "source-refresh-batch" => {
                    include_str!("../../schemas/source-refresh-batch-v1.json")
                }
                "stream" => include_str!("../../schemas/stream-v1.json"),
                "extraction" => include_str!("../../schemas/extraction-v1.json"),
                "extraction-packet" => include_str!("../../schemas/extraction-packet-v1.json"),
                "extraction-state" => include_str!("../../schemas/extraction-state-v1.json"),
                "graph-resolution" => include_str!("../../schemas/graph-resolution-v1.json"),
                "graph-review" => include_str!("../../schemas/graph-review-v1.json"),
                "graph-review-receipt" => {
                    include_str!("../../schemas/graph-review-receipt-v1.json")
                }
                "entity-decisions" => include_str!("../../schemas/entity-decisions-v1.json"),
                "entity-decision-receipt" => {
                    include_str!("../../schemas/entity-decision-receipt-v1.json")
                }
                "run" => include_str!("../../schemas/run-v1.json"),
                "run-event" => include_str!("../../schemas/run-event-v1.json"),
                "usage-receipt" => include_str!("../../schemas/usage-receipt-v1.json"),
                "graph-resolution-receipt" => {
                    include_str!("../../schemas/graph-resolution-receipt-v1.json")
                }
                _ => {
                    return Err(WikiError::new(
                        ErrorCode::CapabilityUnavailable,
                        format!("unknown schema: {name}"),
                    ));
                }
            };
            return Ok(Envelope::success(
                command,
                serde_json::from_str(schema)
                    .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?,
            ));
        }
        Command::Skill {
            command: SkillCommand::Export { target, output },
        } => {
            if args.stage {
                return Err(usage("skill export cannot be staged"));
            }
            return Ok(Envelope::success(
                command,
                super::skill_export::export_skill(target, output, args.dry_run)?,
            ));
        }
        Command::Init { path, title } => {
            if args.stage {
                return Err(usage("initialization cannot be staged"));
            }
            if let Some(explicit) = &args.wiki {
                let cwd = std::env::current_dir()
                    .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
                if cwd.join(explicit) != cwd.join(path) {
                    return Err(usage("init PATH and --wiki must select the same directory"));
                }
            }
            let outcome = crate::app::offline::init(
                path,
                title,
                operation_options(args, args.offline, args.lock_timeout_ms.unwrap_or(5000)),
            )?;
            let mut envelope = Envelope::success(command, value(&outcome)?);
            envelope.meta.wiki_id = Some(outcome.id.to_string());
            return Ok(envelope);
        }
        _ => {}
    }
    let cwd =
        std::env::current_dir().map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
    let root = crate::vault::discovery::resolve(args.wiki.as_deref(), &cwd, false)?;
    let fs = VaultFs::new(root);
    let binding = ChangeEngine::new(fs.clone())?;
    let read_limit = match &args.command {
        Command::Read { max_bytes, .. } => *max_bytes,
        _ => None,
    };
    let preferences = config::local::resolve(
        &fs,
        binding.vault_id(),
        &PreferenceOptions {
            offline: args.offline,
            read_max_bytes: read_limit,
            lock_timeout_ms: args.lock_timeout_ms,
            profile: args.profile.clone(),
        },
        args.preferences.as_deref(),
    )?;
    let app = OfflineApp::new(
        fs,
        operation_options(args, preferences.offline, preferences.lock_timeout_ms),
    )?;
    let mut envelope = Envelope::success(command, Value::Null);
    envelope.meta.wiki_id = Some(app.vault_id().to_string());
    envelope.warnings = preferences.warnings;
    match &args.command {
        Command::Read {
            selector,
            start,
            end,
            no_sync,
            ..
        } => {
            let (range, from) = match (start, end) {
                (Some(start), Some(end)) => (
                    Some(ByteSpan::new(*start, *end).map_err(|e| usage(e.message))?),
                    None,
                ),
                (Some(start), None) => (None, Some(*start)),
                (None, None) => (None, None),
                (None, Some(_)) => return Err(usage("--end requires --start")),
            };
            let request = ReadRequest {
                selector: selector.record_selector()?,
                range,
                max_bytes: preferences.read_max_bytes,
            };
            if args.dry_run {
                envelope.data = json!({
                    "dry_run": true,
                    "selector": {"id": selector.id, "path": selector.path},
                    "requested_range": match from {
                        Some(start) => json!({"start": start, "end": null}),
                        None => value(&request.range)?,
                    },
                    "max_bytes": request.max_bytes,
                    "mode": if *no_sync { "cached" } else { "verified" },
                    "body": null,
                    "source_citation": null,
                    "target_resolution_performed": false,
                    "utf8_range_validation_performed": false,
                    "verification_performed": false,
                });
                envelope.warnings.push("Dry-run plans the read request; target existence, identity uniqueness, UTF-8 range validity, body and freshness are unverified. Run without --dry-run to read bytes.".into());
                return Ok(envelope);
            }
            let catalog = Catalog::with_options(
                app.fs().clone(),
                app.vault_id().clone(),
                CatalogOptions {
                    busy_timeout_ms: app.options().lock_timeout_ms,
                    fault: None,
                },
            );
            let outcome = if catalog.operation_state()?.is_some() {
                catalog.guard_query()?;
                let reader = catalog.query_snapshot(QueryReadLimits::default())?;
                if !reader.normalized_layout() {
                    return Err(WikiError::new(
                        ErrorCode::IndexCorrupt,
                        "normalized read selected another catalog layout",
                    ));
                }
                let mut outcome = cached_query_read(&reader, request, from)?;
                if !*no_sync {
                    let read_proof_error = |mut error: WikiError| {
                        if error.code == ErrorCode::BudgetExceeded {
                            error.hint = Some("Selected read verification exceeded its bounded budget. Context supports --verification-* controls; read --no-sync returns explicitly unverified cached bytes when those are sufficient.".into());
                        }
                        error
                    };
                    let mut proof = retrieval::selected_documents::authenticate(
                        &catalog,
                        &reader,
                        std::slice::from_ref(&outcome.path),
                        &retrieval::VerificationBudget::default(),
                    )
                    .map_err(read_proof_error)?;
                    proof.recheck(&catalog, &reader).map_err(read_proof_error)?;
                    let document = proof.documents.get(&outcome.path).ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::FreshnessConflict,
                            "read path escaped selected proof",
                        )
                    })?;
                    outcome.source_citation = authenticated_read_citation(document, &outcome)?;
                    let verification = retrieval::indexed_documents::verification(&reader)?;
                    result_metadata(&mut envelope.meta, reader.snapshot(), &verification);
                    envelope.warnings.push("Read verifies selected document dependencies against this discovery generation; global membership and identity uniqueness are not verified. Use index sync to discover external edits.".into());
                }
                reader.verify_operations(&catalog)?;
                if *no_sync && !args.dry_run {
                    envelope.meta.index_generation = Some(reader.snapshot().generation);
                    envelope.meta.freshness = Some("index_snapshot".into());
                }
                outcome
            } else {
                let (_writer, reader) = reader(&app, *no_sync)?;
                snapshot_metadata(&mut envelope.meta, &reader);
                if *no_sync {
                    cached_legacy_read(&catalog, &reader, request, from)?
                } else {
                    let mut outcome = match from {
                        Some(start) => app.read_from(request, start)?,
                        None => app.read(request)?,
                    };
                    let document = reader
                        .projection()
                        .documents
                        .iter()
                        .find(|d| d.path == outcome.path && d.hash == outcome.hash)
                        .ok_or_else(|| {
                            WikiError::new(
                                ErrorCode::FreshnessConflict,
                                "read bytes changed after snapshot verification",
                            )
                        })?;
                    outcome.source_citation = authenticated_read_citation(document, &outcome)?;
                    outcome
                }
            };
            envelope.meta.partial = outcome.truncated;
            if let Some(range) = &outcome.continuation {
                let quoted_path = format!("'{}'", outcome.path.as_str().replace('\'', "'\\''"));
                let quoted_wiki = format!(
                    "'{}'",
                    app.fs()
                        .root()
                        .path()
                        .to_string_lossy()
                        .replace('\'', "'\\''")
                );
                let read_mode = if *no_sync { " --no-sync" } else { "" };
                envelope.warnings.push(format!("Read is truncated; continue with: lwiki --wiki {quoted_wiki} read --path {quoted_path}{read_mode} --start {} --end {} --max-bytes {}", range.start(), range.end(), preferences.read_max_bytes));
            }
            envelope.data = value(outcome)?;
        }
        Command::Page {
            command:
                PageCommand::Init {
                    file,
                    source_refs,
                    title,
                    id,
                    path,
                },
        } => {
            let refs = source_refs_input(source_refs.as_deref(), file)?;
            let count = refs.as_ref().map(PageSourceRefs::len);
            let body =
                String::from_utf8(input(file)?).map_err(|_| usage("page body must be UTF-8"))?;
            let outcome = if let Some(refs) = refs {
                app.page_initialize_with_source_refs(
                    path.clone(),
                    id.clone(),
                    title.clone(),
                    body,
                    refs,
                )?
            } else {
                app.page_initialize(path.clone(), id.clone(), title.clone(), body)?
            };
            page_mutation(&mut envelope, &app, outcome)?;
            source_refs_metadata(&mut envelope, count, args.dry_run);
        }
        Command::Page {
            command: PageCommand::Batch { file },
        } => {
            let request: PageBatchRequest = serde_json::from_slice(&input(file)?)
                .map_err(|_| usage("invalid page batch JSON; inspect lwiki schema page-batch"))?;
            page_mutation(&mut envelope, &app, app.page_batch(request)?)?;
        }
        Command::Storage(options) => {
            envelope.data = super::storage::execute(&options.command, &app)?;
            if let Some(warnings) = envelope.data.get("warnings").and_then(Value::as_array) {
                envelope
                    .warnings
                    .extend(warnings.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            envelope.meta.partial =
                envelope.data.get("complete").and_then(Value::as_bool) == Some(false);
        }
        Command::Page {
            command:
                PageCommand::Put {
                    file,
                    source_refs,
                    path,
                    if_match,
                },
        } => {
            let refs = source_refs_input(source_refs.as_deref(), file)?;
            let count = refs.as_ref().map(PageSourceRefs::len);
            let bytes = input(file)?;
            let normalized_preview = args.dry_run
                && path.is_none()
                && Catalog::new(app.fs().clone(), app.vault_id().clone())
                    .operation_state()?
                    .is_some();
            if normalized_preview {
                let note = parse_note(&bytes);
                let record = note
                    .canonical
                    .as_ref()
                    .filter(|r| r.kind() == RecordKind::Page)
                    .ok_or_else(|| crate::app::offline::page_envelope_error(&note))?;
                envelope.data = json!({
                    "plan":{"title":"Put page", "operations":[], "read_preconditions":[]},
                    "unresolved_target":{"record_id":record.id(), "if_match":if_match,
                        "byte_len":bytes.len(), "path":null},
                    "allocated_ids":{}, "change":null, "status":null, "snapshot":null
                });
                page_preview_metadata(&mut envelope, false);
            } else {
                let target = if let Some(path) = path {
                    path.clone()
                } else {
                    let note = parse_note(&bytes);
                    let record = note
                        .canonical
                        .as_ref()
                        .filter(|r| r.kind() == RecordKind::Page)
                        .ok_or_else(|| crate::app::offline::page_envelope_error(&note))?;
                    app.default_page_path(record.id())?
                };
                let outcome = if let Some(refs) = refs {
                    app.page_put_with_source_refs(target, bytes, if_match.clone(), refs)?
                } else {
                    app.page_put(target, bytes, if_match.clone())?
                };
                page_mutation(&mut envelope, &app, outcome)?;
            }
            source_refs_metadata(&mut envelope, count, args.dry_run);
        }
        Command::Page {
            command: PageCommand::Rename { id, to, if_match },
        } => {
            mutation(
                &mut envelope,
                app.page_rename(id.clone(), to.clone(), if_match.clone())?,
            )?;
            if args.dry_run
                && Catalog::new(app.fs().clone(), app.vault_id().clone())
                    .operation_state()?
                    .is_some()
            {
                envelope.data["dry_run"] = true.into();
                envelope.data["plan_complete"] = false.into();
                envelope.data["reused"] = Value::Null;
                envelope.data["request"] = json!({"id":id,"to":to,"if_match":if_match});
                envelope.data["validation"] = json!({"target_resolution_checked":false,"author_hash_checked":false,"destination_availability_checked":false,"incoming_links_checked":false,"read_dependencies_checked":false,"portable_collisions_checked":false});
                envelope.warnings.push("Page rename preview validates the request only. Target identity, author hash, destination, incoming links and dependencies remain unchecked. Staging or applying performs admission checks.".into());
            }
        }
        Command::Source {
            command:
                SourceCommand::Add {
                    file,
                    title,
                    media_type,
                },
        } => source_mutation(
            &mut envelope,
            &app,
            app.source_add(capture(file, title.as_deref(), media_type.clone())?)?,
            json!({"operation":"add", "file":file, "title":title, "media_type":media_type}),
        )?,
        Command::Source {
            command:
                SourceCommand::Refresh {
                    id,
                    file,
                    title,
                    media_type,
                },
        } => source_mutation(
            &mut envelope,
            &app,
            app.source_refresh_with_title(
                id.clone(),
                capture(file, title.as_deref(), media_type.clone())?,
                title.as_deref(),
            )?,
            json!({"operation":"refresh", "source_id":id, "file":file, "title":title, "media_type":media_type}),
        )?,
        Command::Source {
            command: SourceCommand::Withdraw { id, reason },
        } => source_mutation(
            &mut envelope,
            &app,
            app.source_withdraw(id.clone(), reason)?,
            json!({"operation":"withdraw", "source_id":id, "reason":reason}),
        )?,
        Command::Source {
            command: SourceCommand::RefreshBatch { file },
        } => {
            let outcome = app.source_refresh_batch_file(file)?;
            if outcome.items.iter().any(|item| {
                item.capture_state == Some(crate::sources::SourceCaptureState::Unsupported)
            }) {
                envelope.warnings.push("Some refreshed Sources retain original bytes without extracted text; their result items report unsupported capture_state.".into());
            }
            envelope.meta.index_generation = outcome
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.generation);
            if outcome.dry_run {
                envelope.warnings.push("Source batch preview verifies request bounds and external input hashes. Selected Source identities, author hashes, heads, revision reuse and dependent publication remain unresolved.".into());
            }
            envelope.data = value(outcome)?;
        }
        Command::Source {
            command: SourceCommand::Import(arguments),
        } => {
            envelope.data = super::source_import::execute(&arguments.command, &app)?;
            envelope.meta.partial =
                envelope.data["completed"] == false && envelope.data["preview"] != true;
        }
        Command::Evidence {
            command:
                EvidenceCommand::Revalidate {
                    id,
                    to_revision,
                    if_match,
                },
        } => mutation(
            &mut envelope,
            app.evidence_revalidate(id.clone(), to_revision.clone(), if_match.clone())?,
        )?,
        Command::Index { command } => {
            let outcome = match command {
                IndexCommand::Rebuild { normalized: true } => app.index_rebuild_normalized()?,
                IndexCommand::Rebuild { normalized: false } => app.index_sync(true)?,
                IndexCommand::Sync => app.index_sync(false)?,
            };
            if let Some(report) = &outcome.report {
                envelope.meta.index_generation = Some(report.snapshot.generation);
                if report.vector_cache_lost {
                    envelope.warnings.push("explicit rebuild discarded vector cache; regeneration requires an explicit embeddings sync".into());
                }
                if report.vector_loss_unknown {
                    envelope
                        .warnings
                        .push("previous vector cache state is unknown".into());
                }
            }
            envelope.data = value(outcome)?;
        }
        Command::Embeddings(options) => {
            use super::embeddings::EmbeddingCommand;
            let (settings, remote, probe, sync) = match &options.command {
                EmbeddingCommand::Check(o) => (o.settings.settings(), &o.remote, o.probe, false),
                EmbeddingCommand::Sync(o) => (o.settings.settings(), &o.remote, false, true),
            };
            remote.limits()?;
            if args.stage {
                return Err(usage("embedding cache operations cannot be staged"));
            }
            let runtime = if !args.dry_run
                && !app.options().offline
                && (sync || probe || args.profile.is_some())
            {
                Some(remote.runtime(&app, args.profile.as_deref(), Capability::Embed)?)
            } else {
                None
            };
            let embedding = runtime.as_ref().map(embedding_runtime);
            if args.dry_run {
                settings.validate()?;
                envelope.data = json!({"settings":settings,"dry_run":true,"probe":probe,
                    "sync":sync,"cache_state_unknown":true,"network_used":false});
                envelope
                    .warnings
                    .push("embedding coverage is unknown during dry-run".into());
                return Ok(envelope);
            }
            let mut report = with_network_activity(
                runtime.as_ref().map(|r| &r.dispatcher),
                (|| {
                    if sync && !args.dry_run {
                        if app.options().offline {
                            return app.embeddings_sync_cached(&settings);
                        }
                        app.embeddings_sync(
                            &settings,
                            embedding.as_ref().ok_or_else(|| {
                                usage("embeddings sync requires a trusted provider profile")
                            })?,
                        )
                    } else {
                        app.embeddings_check(&settings, embedding.as_ref(), probe && !args.dry_run)
                    }
                })(),
            )?;
            report.network_used = runtime
                .as_ref()
                .is_some_and(|r| r.dispatcher.network_used());
            envelope.meta.network_used = report.network_used;
            envelope.meta.partial = report.coverage.missing_units > 0;
            envelope.warnings.extend(report.warnings.iter().cloned());
            envelope.data = value(report)?;
        }
        Command::Search(command) => {
            let search = &command.search;
            let plan = retrieval::lexical::validate_plan(&search.query, &search.plan())?;
            if command.verify_selected {
                if !matches!(
                    plan.mode,
                    retrieval::SearchMode::Literal
                        | retrieval::SearchMode::Lexical
                        | retrieval::SearchMode::Semantic
                        | retrieval::SearchMode::Hybrid
                ) || search.graph.is_some()
                {
                    return Err(WikiError::new(
                        ErrorCode::CapabilityUnavailable,
                        "--verify-selected supports normalized literal, lexical, semantic and hybrid search without graph expansion",
                    ));
                }
                if args.dry_run {
                    envelope.data = retrieval::selected_search::preview(&search.query, &plan)?;
                    envelope.warnings.push("Selected search preview validates the request only; index access, admission and evidence verification are unperformed.".into());
                } else {
                    let catalog = Catalog::with_options(
                        app.fs().clone(),
                        app.vault_id().clone(),
                        CatalogOptions {
                            busy_timeout_ms: app.options().lock_timeout_ms,
                            fault: None,
                        },
                    );
                    let hits = if matches!(plan.mode, retrieval::SearchMode::Literal | retrieval::SearchMode::Lexical) {
                        retrieval::selected_search::search(&catalog, &search.query, &plan,
                            &retrieval::VerificationBudget::default())
                    } else {
                        search.remote.limits()?;
                        with_embedding_runtime(&app, args, &search.remote, search.lexical_fallback,
                            |runtime, fallback| app.semantic_search_selected(&search.query,
                                &plan, runtime, fallback))
                    }.map_err(|mut error| {
                        if matches!(error.code, ErrorCode::FreshnessConflict | ErrorCode::BudgetExceeded) {
                            error.hint = Some("Inspect selected source changes and run index sync when appropriate; reduce --limit or use plain search for explicitly unverified cached discovery.".into());
                        }
                        error
                    })?;
                    result_metadata(&mut envelope.meta, &hits.snapshot, &hits.verification);
                    envelope.meta.network_used = hits.network_used;
                    envelope.meta.partial = hits.truncated;
                    envelope.warnings.extend(hits.warnings.iter().cloned());
                    envelope.data = value(hits)?;
                }
                return Ok(envelope);
            }
            if matches!(
                plan.mode,
                retrieval::SearchMode::Semantic | retrieval::SearchMode::Hybrid
            ) {
                search.remote.limits()?;
            }
            if search.graph.is_some() && plan.mode != retrieval::SearchMode::Hybrid {
                return Err(usage("--graph entities requires hybrid search"));
            }
            let catalog = Catalog::with_options(
                app.fs().clone(),
                app.vault_id().clone(),
                CatalogOptions {
                    busy_timeout_ms: app.options().lock_timeout_ms,
                    fault: None,
                },
            );
            if catalog.operation_state()?.is_some() {
                if search.graph.is_some() {
                    return Err(WikiError::new(
                        ErrorCode::CapabilityUnavailable,
                        "normalized general search supports literal, lexical, semantic and hybrid documents without graph expansion; discovery uses the published index, and index sync discovers external edits",
                    ));
                }
                if args.dry_run && plan.mode != retrieval::SearchMode::Lexical {
                    envelope.data = retrieval::selected_search::preview(&search.query, &plan)?;
                    envelope.warnings.push("Search preview validates the request only; index access, cache availability and evidence verification are unperformed.".into());
                    return Ok(envelope);
                }
                catalog.guard_query()?;
                let hits = if matches!(
                    plan.mode,
                    retrieval::SearchMode::Literal | retrieval::SearchMode::Lexical
                ) {
                    let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
                    if !reader.normalized_layout() {
                        return Err(WikiError::new(
                            ErrorCode::IndexCorrupt,
                            "normalized search selected another catalog layout",
                        ));
                    }
                    let hits = retrieval::lexical::search_catalog(&reader, &search.query, &plan)?;
                    reader.verify_operations(&catalog)?;
                    hits
                } else {
                    with_embedding_runtime(
                        &app,
                        args,
                        &search.remote,
                        search.lexical_fallback,
                        |runtime, fallback| {
                            app.semantic_search(
                                &search.query,
                                &plan,
                                runtime,
                                search.no_sync,
                                fallback,
                                None,
                            )
                        },
                    )?
                };
                result_metadata(&mut envelope.meta, &hits.snapshot, &hits.verification);
                envelope.meta.network_used = hits.network_used;
                envelope.meta.partial = hits.truncated;
                envelope.warnings.extend(hits.warnings.iter().cloned());
                envelope.data = value(hits)?;
                if args.dry_run {
                    envelope
                        .warnings
                        .push("dry-run results reflect the published cache".into());
                }
            } else if args.dry_run {
                // Even a read-only WAL open can create sidecars. Use no SQLite path.
                let projection = crate::catalog::scan::scan(app.fs(), app.vault_id())?;
                envelope.data = json!({"query":search.query,"plan":plan,"dry_run":true,"cache_state_unknown":true,"canonical_document_count":projection.documents.len(),"hits":null});
                envelope
                    .warnings
                    .push("search results and index freshness are unknown during dry-run".into());
            } else {
                let hits = if matches!(
                    plan.mode,
                    retrieval::SearchMode::Semantic | retrieval::SearchMode::Hybrid
                ) {
                    let graph = search.graph.is_some().then(|| crate::graph::GraphPlan {
                        filters: plan.filters.clone(),
                        ..Default::default()
                    });
                    with_embedding_runtime(
                        &app,
                        args,
                        &search.remote,
                        search.lexical_fallback,
                        |runtime, fallback| {
                            app.semantic_search(
                                &search.query,
                                &plan,
                                runtime,
                                search.no_sync,
                                fallback,
                                graph.as_ref(),
                            )
                        },
                    )?
                } else {
                    if search.graph.is_some() {
                        return Err(usage("--graph requires hybrid search"));
                    }
                    let (_writer, reader) = reader(&app, search.no_sync)?;
                    retrieval::search(&reader, &search.query, &plan)?
                };
                require_legacy_query_still_selected(&catalog)?;
                result_metadata(&mut envelope.meta, &hits.snapshot, &hits.verification);
                envelope.meta.network_used = hits.network_used;
                envelope.meta.partial = hits.truncated;
                envelope.warnings.extend(hits.warnings.iter().cloned());
                envelope.data = value(hits)?;
            }
        }
        Command::Graph {
            command: GraphCommand::Extract(options),
        } => {
            let request = options.request(&app)?;
            if matches!(options.executor, super::extraction::Executor::Api) {
                options.remote.limits()?;
                if options.max_output_tokens == 0 {
                    return Err(usage("max-output-tokens must be positive"));
                }
            }
            if matches!(options.executor, super::extraction::Executor::Agent) || args.dry_run {
                let outcome = app.graph_extract_agent(&request)?;
                envelope.meta.partial = outcome.coverage.omitted_source_bytes > 0;
                if args.dry_run {
                    envelope
                        .warnings
                        .push("packet preview is unpersisted and cannot be imported".into());
                    if matches!(options.executor, super::extraction::Executor::Api) {
                        envelope.warnings.push(format!("API preview did not check private service dimensions, including the effective max_output_tokens cap (default 4096); requested {}. No credentials were resolved.", options.max_output_tokens));
                    }
                }
                envelope.data = value(outcome)?;
            } else {
                let runtime =
                    options
                        .remote
                        .runtime(&app, args.profile.as_deref(), Capability::Generate)?;
                let run_id = options.run.clone().map(Ok).unwrap_or_else(|| {
                    app.default_api_extraction_run_id(
                        &request,
                        &runtime.service,
                        options.max_output_tokens,
                    )
                })?;
                let request = crate::graph::api_extract::ApiExtractionRequest {
                    requested_limits: runtime.requested_limits,
                    export: request,
                    run_id,
                    created_at_utc_ms: runtime.created_at_utc_ms,
                    deadline_utc_ms: runtime.deadline_utc_ms,
                    limits: runtime.limits,
                    max_output_tokens: options.max_output_tokens,
                    new_extraction: options.new_extraction,
                };
                let outcome = with_network_activity(
                    Some(&runtime.dispatcher),
                    app.graph_extract_api(
                        &request,
                        &runtime.service,
                        &runtime.dispatcher,
                        runtime.job_options,
                    ),
                )?;
                envelope.meta.network_used = runtime.dispatcher.network_used();
                envelope.meta.partial = outcome.coverage.omitted_source_bytes > 0;
                envelope.warnings.extend(outcome.warnings.clone());
                envelope.data = value(outcome)?;
            }
        }
        Command::Graph {
            command: GraphCommand::Import(options),
        } => {
            envelope.data = app.graph_import(
                &super::extraction::response_input(&options.file)?,
                options.new_extraction,
            )?;
        }
        Command::Graph {
            command: GraphCommand::Resolve(options),
        } => {
            envelope.data = app.graph_resolve(&super::extraction::bounded_json_input(
                &options.file,
                "mention resolution",
            )?)?;
        }
        Command::Graph {
            command: GraphCommand::Decide(options),
        } => {
            envelope.data = app.graph_decide(&super::extraction::bounded_json_input(
                &options.file,
                "entity decisions",
            )?)?;
        }
        Command::Graph {
            command: GraphCommand::Review(options),
        } => {
            envelope.data = app.graph_review(&super::extraction::bounded_json_input(
                &options.file,
                "graph review",
            )?)?;
        }
        Command::Graph { command } => {
            use crate::graph::query;
            let (options, neighbors) = match command {
                GraphCommand::Query { options, .. } => (options, false),
                GraphCommand::Neighbors { options, .. } => (options, true),
                _ => unreachable!("extraction handled above"),
            };
            let mut plan = query::validate_plan(&options.plan(neighbors))?;
            if options.verify_selected && !neighbors {
                return Err(usage(
                    "--verify-selected supports normalized named Entity neighbors only",
                ));
            }
            if plan.seed_mode == crate::graph::GraphSeedMode::Semantic && !neighbors {
                options.remote.limits()?;
            }
            if let GraphCommand::Query { query, .. } = command
                && plan.seed_mode == crate::graph::GraphSeedMode::Lexical
            {
                retrieval::lexical::lexical_expression(query)?;
            }
            if args.dry_run {
                if neighbors {
                    plan.include_navigation = options.navigation;
                }
                envelope.data = json!({"plan":plan,"dry_run":true,"cache_state_unknown":true,
                    "layout_unknown":true,"target_resolution_performed":false,
                    "canonical_scan_performed":false,"verification_performed":false,
                    "legacy_default_navigation":neighbors,"results":null});
                envelope
                    .warnings
                    .push("Dry-run validates the graph request; target existence, layout capabilities, results and freshness remain unknown. Normalized named neighbors verify selected dependencies by default; --no-sync is cached unless --verify-selected is explicit.".into());
            } else {
                let catalog = Catalog::with_options(
                    app.fs().clone(),
                    app.vault_id().clone(),
                    CatalogOptions {
                        busy_timeout_ms: app.options().lock_timeout_ms,
                        fault: None,
                    },
                );
                let normalized = catalog.operation_state()?.is_some();
                let result = if normalized {
                    let GraphCommand::Neighbors { id, .. } = command else {
                        return Err(WikiError::new(
                            ErrorCode::CapabilityUnavailable,
                            "normalized graph lookup supports named Entity neighbors; discover an Entity ID with lexical search first",
                        ));
                    };
                    plan.include_navigation = options.navigation;
                    crate::graph::indexed_neighbors::neighbors(
                        &catalog,
                        id,
                        &plan,
                        &retrieval::VerificationBudget::default(),
                        !options.no_sync || options.verify_selected,
                    )?
                } else if options.verify_selected {
                    return Err(WikiError::new(
                        ErrorCode::CapabilityUnavailable,
                        "--verify-selected requires normalized named neighbors; legacy graph lookup uses its existing snapshot verification",
                    ));
                } else if plan.seed_mode == crate::graph::GraphSeedMode::Semantic && !neighbors {
                    let GraphCommand::Query { query, .. } = command else {
                        unreachable!()
                    };
                    with_embedding_runtime(
                        &app,
                        args,
                        &options.remote,
                        options.lexical_fallback,
                        |runtime, fallback| {
                            app.semantic_graph(query, &plan, runtime, options.no_sync, fallback)
                        },
                    )?
                } else {
                    let (_writer, reader) = reader(&app, options.no_sync)?;
                    match command {
                        GraphCommand::Query { query, .. } => query::query(&reader, query, &plan)?,
                        GraphCommand::Neighbors { id, .. } => query::neighbors(&reader, id, &plan)?,
                        _ => unreachable!("extraction handled above"),
                    }
                };
                result_metadata(&mut envelope.meta, &result.snapshot, &result.verification);
                envelope.meta.network_used = result.network_used;
                envelope.meta.partial = result.truncated;
                envelope.warnings.extend(result.warnings.iter().cloned());
                envelope.data = value(result)?;
            }
        }
        Command::Context(context) => {
            let catalog = Catalog::with_options(
                app.fs().clone(),
                app.vault_id().clone(),
                CatalogOptions {
                    busy_timeout_ms: app.options().lock_timeout_ms,
                    fault: None,
                },
            );
            let normalized_selected = catalog.operation_state()?.is_some();
            let mut request = context.request();
            if context.scope.is_none() && normalized_selected {
                request.scope = retrieval::ContextScope::IndexedDocuments;
            }
            let request = retrieval::context::validate_request(&context.search.query, &request)?;
            if request.scope == retrieval::ContextScope::IndexedEvidence
                && (context.prepare_selection || context.selection.is_some())
            {
                return Err(usage(
                    "indexed-evidence does not support host selection; use lexical indexed-documents",
                ));
            }
            let selection = context.selection_action()?;
            retrieval::context::validate_selection_action(&request, &selection)?;
            if matches!(
                request.documents.mode,
                retrieval::SearchMode::Semantic | retrieval::SearchMode::Hybrid
            ) || request
                .graph
                .as_ref()
                .is_some_and(|g| g.seed_mode == crate::graph::GraphSeedMode::Semantic)
            {
                context.search.remote.limits()?;
            }
            if context.search.no_sync
                && !matches!(
                    request.scope,
                    retrieval::ContextScope::Snapshot
                        | retrieval::ContextScope::IndexedEvidence
                        | retrieval::ContextScope::IndexedDocuments
                )
            {
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "--no-sync requires snapshot, indexed-evidence or indexed-documents scope",
                ));
            }
            let request = retrieval::context::validate_request(&context.search.query, &request)?;
            let cached_documents = request.scope == retrieval::ContextScope::Snapshot
                && request.target == retrieval::ContextTarget::Documents
                && request.documents.mode == retrieval::SearchMode::Lexical
                && request.graph.is_none();
            // Refuse before preparing embeddings or acquiring a writer for a
            // mode that cannot consume the selected normalized catalog.
            if normalized_selected
                && !matches!(
                    request.scope,
                    retrieval::ContextScope::IndexedEvidence
                        | retrieval::ContextScope::IndexedDocuments
                )
                && !cached_documents
            {
                return Err(WikiError::new(
                    ErrorCode::CapabilityUnavailable,
                    "normalized context supports literal, lexical, semantic and hybrid indexed-documents, plus lexical indexed-evidence and snapshot; omit --scope for selected document verification. Strict current/historical context is not yet available on normalized vaults; check performs a separate full audit",
                ));
            }
            let cached_dry_run =
                args.dry_run && context.search.no_sync && cached_documents && normalized_selected;
            if args.dry_run && !cached_dry_run {
                envelope.data = json!({"query": context.search.query, "request": request, "dry_run": true, "cache_state_unknown": true, "context": null});
                envelope
                    .warnings
                    .push("context and freshness are unknown during dry-run".into());
            } else {
                let result = if matches!(
                    request.documents.mode,
                    retrieval::SearchMode::Semantic | retrieval::SearchMode::Hybrid
                ) || request
                    .graph
                    .as_ref()
                    .is_some_and(|g| g.seed_mode == crate::graph::GraphSeedMode::Semantic)
                {
                    with_embedding_runtime(
                        &app,
                        args,
                        &context.search.remote,
                        context.search.lexical_fallback,
                        |runtime, fallback| {
                            app.semantic_context_with_selection(
                                &context.search.query,
                                &request,
                                runtime,
                                context.search.no_sync,
                                fallback,
                                &selection,
                            )
                        },
                    )?
                } else {
                    let writer = if matches!(
                        request.scope,
                        retrieval::ContextScope::Snapshot
                            | retrieval::ContextScope::IndexedEvidence
                            | retrieval::ContextScope::IndexedDocuments
                    ) {
                        None
                    } else {
                        Some(WriterPermit::acquire(
                            app.fs().root(),
                            Duration::from_millis(app.options().lock_timeout_ms),
                        )?)
                    };
                    retrieval::verification::context_with_options(
                        &catalog,
                        writer.as_ref(),
                        &context.search.query,
                        &request,
                        &retrieval::ContextOptions {
                            selection: selection.clone(),
                            ..Default::default()
                        },
                    )?
                };
                envelope.meta.network_used = result.network_used;
                envelope.meta.index_generation = Some(result.snapshot().generation);
                match result.verification() {
                    SnapshotVerification::IndexSnapshot => {
                        envelope.meta.freshness = Some("index_snapshot".into())
                    }
                    SnapshotVerification::IndexedEvidence { verified_at, .. } => {
                        envelope.meta.freshness = Some("indexed_evidence".into());
                        envelope.meta.verified_at = Some(verified_at.clone());
                    }
                    SnapshotVerification::VerifiedSnapshot { verified_at } => {
                        envelope.meta.freshness = Some("verified_snapshot".into());
                        envelope.meta.verified_at = Some(verified_at.clone());
                    }
                }
                envelope.meta.partial = result.truncated();
                envelope.warnings.extend(result.warnings().iter().cloned());
                envelope.data = value(result)?;
                if cached_dry_run {
                    envelope
                        .warnings
                        .push("dry-run results reflect the published cache".into());
                }
            }
        }
        Command::Check => {
            let outcome = app.check()?;
            let count = outcome.error_count;
            // A successful preview intentionally skips checking; it is not
            // truncated output or retained partial work.
            envelope.meta.partial = !outcome.complete && !args.dry_run;
            envelope.data = value(outcome)?;
            if count != 0 {
                envelope.ok = false;
                envelope.error = Some(error_output(WikiError::invalid(format!(
                    "{count} knowledge diagnostics"
                ))));
            }
        }
        Command::Research(arguments) => {
            let result = super::research::execute(&arguments.command, &app)?;
            envelope.data = result.data;
            envelope.meta.network_used = result.network_used;
            envelope.meta.partial = result.partial;
            envelope.warnings.extend(result.warnings);
        }
        Command::Doctor {
            probe,
            role,
            remote,
            extraction_schema,
        } => {
            if *extraction_schema && !matches!(role, super::remote::ProbeRole::Generate) {
                return Err(usage(
                    "--extraction-schema requires --probe --role generate",
                ));
            }
            if *probe && preferences.offline && !args.dry_run {
                return Err(WikiError::new(
                    ErrorCode::OfflineUnavailable,
                    "offline execution cannot probe a provider",
                ));
            }
            let mut doctor = app.doctor()?;
            if *probe {
                let service_role = role.service_role();
                remote.limits()?;
                if args.dry_run {
                    envelope.data = json!({"doctor":doctor,"probe":{"dry_run":true,"role":service_role,"network_used":false,"remote_work":"unknown"}});
                } else {
                    let runtime = remote.runtime(
                        &app,
                        preferences.profile.as_deref(),
                        service_role.capability(),
                    )?;
                    let result = if *extraction_schema {
                        app.probe_extraction_schema(&runtime)?
                    } else {
                        app.probe_provider(&runtime, service_role)?
                    };
                    doctor.provider_probe_performed = result.network_used;
                    envelope.meta.network_used = result.network_used;
                    envelope.data = json!({"doctor":doctor,"probe":result});
                }
            } else {
                envelope.data = value(doctor)?;
            }
        }
        Command::Changes { command } => match command {
            ChangesCommand::Resolve { id, mode, file } => {
                if args.stage {
                    return Err(usage("conflict resolution cannot be staged"));
                }
                let engine = ChangeEngine::new(app.fs().clone())?;
                if args.dry_run {
                    let inspected = engine.inspect(id)?;
                    envelope.data =
                        value(engine.resolution_plan(&inspected.prepared, mode.mode())?)?;
                } else {
                    let file = file.as_ref().ok_or_else(|| usage("inspect with changes resolve ID --dry-run --mode resume|abandon, then pass the data object with --file REQUEST.json"))?;
                    let request: crate::changes::ConflictResolutionRequest =
                        crate::changes::prepare::strict_json(
                            &super::extraction::bounded_json_input(
                                file,
                                "conflict resolution request",
                            )?,
                        )?;
                    if &request.change.change_id != id {
                        return Err(usage("resolution request change ID differs from command"));
                    }
                    let writer = WriterPermit::acquire(
                        app.fs().root(),
                        Duration::from_millis(app.options().lock_timeout_ms),
                    )?;
                    let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
                    envelope.data = value(engine.resolve(
                        &writer,
                        &request,
                        &CatalogGraphValidator,
                        &catalog,
                    )?)?;
                }
            }
            ChangesCommand::Show { id, operation } => {
                if let Some(operation) = operation {
                    envelope.data = value(app.changes_payload(id.clone(), *operation)?)?;
                } else {
                    let details = app.changes_show(id.clone())?;
                    envelope.meta.partial = !details.omitted_payloads.is_empty();
                    if !details.unavailable_payloads.is_empty() {
                        envelope.warnings.push("Some retained payloads are unavailable; history remains inspectable, but these operations cannot be used for undo without restoring a full-vault backup.".into());
                    }
                    envelope.data = value(details)?;
                }
            }
            ChangesCommand::Apply { id } => {
                mutation(&mut envelope, app.changes_apply(id.clone())?)?
            }
            ChangesCommand::Abort { id } => {
                mutation(&mut envelope, app.changes_abort(id.clone())?)?
            }
            ChangesCommand::Rollback { id } => {
                mutation(&mut envelope, app.changes_rollback(id.clone())?)?
            }
        },
        Command::Jobs {
            command:
                JobsCommand::Amend {
                    run,
                    reason,
                    remote,
                },
        } => {
            if args.stage {
                return Err(usage("job limit amendments cannot be staged"));
            }
            envelope.data = value(app.amend_provider_job_overrides(
                run,
                &remote.requested_limits()?,
                reason,
            )?)?;
        }
        Command::Jobs {
            command: JobsCommand::Status { run },
        } => {
            let options = crate::app::remote::native_job_options(
                crate::jobs::ExecutionPolicy {
                    offline: true,
                    dry_run: args.dry_run,
                    retry_uncertain: false,
                },
                app.options().lock_timeout_ms,
            );
            let ledger = crate::jobs::JobLedger::new(
                app.fs().clone(),
                app.vault_id().clone(),
                run.clone(),
                options,
            )?;
            envelope.data = value(ledger.inspect()?)?;
        }
        Command::Jobs {
            command: JobsCommand::Diagnostics { command },
        } => {
            if args.stage {
                return Err(usage("diagnostic operations cannot be staged"));
            }
            let (run, attempt_id, kind) = match command {
                DiagnosticsCommand::Inspect {
                    run, attempt, kind, ..
                }
                | DiagnosticsCommand::Prune { run, attempt, kind } => (run, attempt, kind.kind()),
            };
            let options = crate::app::remote::native_job_options(
                crate::jobs::ExecutionPolicy {
                    offline: true,
                    dry_run: args.dry_run,
                    retry_uncertain: false,
                },
                app.options().lock_timeout_ms,
            );
            let ledger = crate::jobs::JobLedger::new(
                app.fs().clone(),
                app.vault_id().clone(),
                run.clone(),
                options,
            )?;
            let inspection = ledger.inspect()?;
            let recorded = inspection
                .attempts
                .iter()
                .find(|item| &item.attempt.attempt_id == attempt_id)
                .ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::RecordNotFound,
                        "attempt does not belong to retained job",
                    )
                })?;
            envelope.data = match command {
                DiagnosticsCommand::Inspect { raw, .. } => {
                    value(ledger.inspect_diagnostic(&recorded.attempt, kind, *raw)?)?
                }
                DiagnosticsCommand::Prune { .. } if args.dry_run => {
                    json!({"dry_run":true,"protected":recorded.phase != crate::jobs::AttemptPhase::Settled || recorded.billing == crate::jobs::BillingDisposition::UnknownReserved,"diagnostic":ledger.inspect_diagnostic(&recorded.attempt,kind,false)?})
                }
                DiagnosticsCommand::Prune { .. } => {
                    json!({"pruned":ledger.prune_diagnostic(&recorded.attempt,kind)?})
                }
            };
        }
        Command::Recover => envelope.data = value(app.recover()?)?,
        Command::Migrate {
            selector,
            if_match,
            to_schema,
        } => mutation(
            &mut envelope,
            app.migrate(selector.record_selector()?, if_match.clone(), to_schema)?,
        )?,
        Command::Capabilities
        | Command::Schema { .. }
        | Command::Init { .. }
        | Command::Skill { .. } => unreachable!(),
    }
    Ok(envelope)
}
fn error_output(error: WikiError) -> ErrorOutput {
    ErrorOutput {
        code: error.code.to_string(),
        message: error.message,
        retryable: error.retryable,
        hint: error.hint,
        details: error.details,
    }
}
fn mutation(envelope: &mut Envelope, outcome: MutationOutcome) -> Result<()> {
    envelope.meta.index_generation = outcome.snapshot.as_ref().map(|s| s.generation);
    let capture = outcome.source_capture;
    envelope.data = value(outcome)?;
    if let Some(capture) = capture {
        envelope.data["extraction_status"] = capture.extraction_status().into();
        envelope.data["citable"] = capture.citable().into();
        match capture {
            crate::sources::SourceCaptureState::Unsupported => envelope.warnings.push("Original bytes captured only; text extraction is unsupported for this format or invalid UTF-8. The content cannot be searched, embedded or cited. Supply extracted UTF-8 .txt/.md text when needed.".into()),
            crate::sources::SourceCaptureState::Empty => envelope.warnings.push("Empty source captured; no text is available to search, embed or cite.".into()),
            crate::sources::SourceCaptureState::Complete => {},
        }
    }
    Ok(())
}
fn page_mutation(
    envelope: &mut Envelope,
    app: &OfflineApp,
    outcome: MutationOutcome,
) -> Result<()> {
    mutation(envelope, outcome)?;
    if app.options().dry_run
        && Catalog::new(app.fs().clone(), app.vault_id().clone())
            .operation_state()?
            .is_some()
    {
        page_preview_metadata(envelope, true);
    }
    Ok(())
}
fn source_mutation(
    envelope: &mut Envelope,
    app: &OfflineApp,
    outcome: MutationOutcome,
    request: Value,
) -> Result<()> {
    mutation(envelope, outcome)?;
    if app.options().dry_run
        && Catalog::new(app.fs().clone(), app.vault_id().clone())
            .operation_state()?
            .is_some()
    {
        envelope.data["dry_run"] = true.into();
        envelope.data["plan_complete"] = false.into();
        envelope.data["reused"] = Value::Null;
        envelope.data["request"] = request;
        envelope.data["validation"] = json!({
            "explicit_file_guards_checked":false,
            "target_resolution_checked":false,
            "indexed_admission_checked":false,
            "read_dependencies_checked":false,
            "portable_collisions_checked":false,
            "identities_reserved":false
        });
        envelope.warnings.push("Source preview validates the supplied request only. Existing targets, revision reuse, identities, dependencies and portable collisions remain unchecked. Proposed add identities are not reserved. Staging or applying performs admission checks.".into());
    }
    Ok(())
}
fn page_preview_metadata(envelope: &mut Envelope, explicit_targets: bool) {
    envelope.data["dry_run"] = true.into();
    envelope.data["plan_complete"] = false.into();
    envelope.data["reused"] = Value::Null;
    envelope.data["validation"] = json!({
        "explicit_file_guards_checked":explicit_targets,
        "target_resolution_checked":explicit_targets,
        "indexed_admission_checked":false,
        "read_dependencies_checked":false,
        "portable_collisions_checked":false
    });
    envelope.warnings.push("Page preview leaves indexed identities, affected records, read dependencies and portable path collisions unchecked. Omitted destinations remain unresolved; --path allows checking that file's author guard. Staging or applying performs admission checks.".into());
}
fn reader(app: &OfflineApp, no_sync: bool) -> Result<(Option<WriterPermit>, ReaderSnapshot)> {
    let catalog = Catalog::with_options(
        app.fs().clone(),
        app.vault_id().clone(),
        CatalogOptions {
            busy_timeout_ms: app.options().lock_timeout_ms,
            fault: None,
        },
    );
    if no_sync {
        return Ok((None, catalog.index_snapshot()?));
    }
    let writer = WriterPermit::acquire(
        app.fs().root(),
        Duration::from_millis(app.options().lock_timeout_ms),
    )?;
    match catalog.verified_snapshot(Some(&writer)) {
        Ok(snapshot) => Ok((Some(writer), snapshot)),
        Err(error) if error.code == ErrorCode::RecoveryRequired => {
            ChangeEngine::new(app.fs().clone())?.recover(
                &writer,
                &CatalogGraphValidator,
                &catalog,
            )?;
            let snapshot = catalog.verified_snapshot(Some(&writer))?;
            Ok((Some(writer), snapshot))
        }
        Err(error) => Err(error),
    }
}
fn snapshot_metadata(meta: &mut Metadata, reader: &ReaderSnapshot) {
    meta.index_generation = Some(reader.snapshot().generation);
    match reader.verification() {
        SnapshotVerification::IndexSnapshot => meta.freshness = Some("index_snapshot".into()),
        SnapshotVerification::IndexedEvidence { verified_at, .. } => {
            meta.freshness = Some("indexed_evidence".into());
            meta.verified_at = Some(verified_at.clone());
        }
        SnapshotVerification::VerifiedSnapshot { verified_at } => {
            meta.freshness = Some("verified_snapshot".into());
            meta.verified_at = Some(verified_at.clone());
        }
    }
}
fn cached_legacy_read(
    catalog: &Catalog,
    reader: &ReaderSnapshot,
    request: ReadRequest,
    from: Option<u64>,
) -> Result<ReadOutcome> {
    let outcome = cached_read(reader, request, from)?;
    require_legacy_query_still_selected(catalog)?;
    Ok(outcome)
}
fn require_legacy_query_still_selected(catalog: &Catalog) -> Result<()> {
    if catalog.operation_state()?.is_some() {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "normalized authority activated while a legacy reader was held",
        ));
    }
    Ok(())
}
fn cached_read(
    reader: &ReaderSnapshot,
    request: ReadRequest,
    from: Option<u64>,
) -> Result<ReadOutcome> {
    let projection = reader.projection();
    let path = match &request.selector {
        RecordSelector::Path(path) => path.clone(),
        RecordSelector::Id(id) => {
            if projection.diagnostics.iter().any(|d| {
                d.record_id.as_ref() == Some(id) && d.code == ErrorCode::ReferenceAmbiguous
            }) {
                return Err(WikiError::new(
                    ErrorCode::ReferenceAmbiguous,
                    "duplicate ID in index snapshot",
                ));
            }
            if let Some(row) = projection.records.get(id) {
                row.path.clone()
            } else {
                let candidates: Vec<_> = projection
                    .documents
                    .iter()
                    .filter(|d| {
                        d.owner_revision.is_none()
                            && parse_note(d.raw_text.as_bytes())
                                .fields
                                .as_ref()
                                .and_then(|f| f.get("wiki_id"))
                                .and_then(Value::as_str)
                                == Some(id.as_str())
                    })
                    .collect();
                match candidates.as_slice() {
                    [document] => document.path.clone(),
                    [] => {
                        return Err(WikiError::new(
                            ErrorCode::RecordNotFound,
                            "ID absent from index snapshot",
                        ));
                    }
                    _ => {
                        return Err(WikiError::new(
                            ErrorCode::ReferenceAmbiguous,
                            "duplicate ID in index snapshot",
                        ));
                    }
                }
            }
        }
    };
    let document = projection
        .documents
        .iter()
        .find(|d| d.path == path)
        .ok_or_else(|| {
            WikiError::new(ErrorCode::RecordNotFound, "path absent from index snapshot")
        })?;
    format_cached_read(
        document,
        document
            .record_id
            .as_ref()
            .and_then(|id| projection.records.get(id))
            .map(|r| r.record.clone()),
        projection
            .diagnostics
            .iter()
            .filter(|d| d.path == path)
            .cloned()
            .collect(),
        request,
        from,
    )
}

/// Reads one published cached document. No claim about current canonical bytes
/// or corpus-wide freshness follows from this explicitly no-sync operation.
fn cached_query_read(
    reader: &QuerySnapshot,
    request: ReadRequest,
    from: Option<u64>,
) -> Result<ReadOutcome> {
    let corrupt = |message| WikiError::new(ErrorCode::IndexCorrupt, message);
    let claim = match &request.selector {
        RecordSelector::Id(id) => {
            let claim = reader.unique_identity_claim(id)?;
            if claim.is_none() {
                if reader.record(id)?.is_some() {
                    return Err(corrupt("adopted cached record has no identity claim"));
                }
                return Err(WikiError::new(
                    ErrorCode::RecordNotFound,
                    "ID absent from index snapshot",
                ));
            }
            claim
        }
        RecordSelector::Path(_) => None,
    };
    let path = claim
        .as_ref()
        .map(|c| c.path.clone())
        .unwrap_or_else(|| match &request.selector {
            RecordSelector::Path(path) => path.clone(),
            RecordSelector::Id(_) => unreachable!("ID was resolved above"),
        });
    let document = reader.document(&path)?.ok_or_else(|| {
        if claim.is_some() {
            corrupt("cached identity claim has no document")
        } else {
            WikiError::new(ErrorCode::RecordNotFound, "path absent from index snapshot")
        }
    })?;
    let diagnostics = reader.diagnostics(&std::collections::BTreeSet::from([path.clone()]))?;
    let note = parse_note(document.raw_text.as_bytes());
    // The existing projection represents non-UTF8 notes as empty cached text
    // plus their original hash and parse diagnostic; keep that representation.
    let unreadable_note = document.owner_revision.is_none()
        && document.record_id.is_none()
        && document.source_id.is_none()
        && document.raw_text.is_empty()
        && diagnostics.iter().any(|d| {
            d.code == ErrorCode::RecordInvalid
                && d.details.get("message").and_then(Value::as_str) == Some("note is not UTF-8")
        });
    if note.source_hash != document.hash && !unreadable_note {
        return Err(corrupt("cached document text differs from its stored hash"));
    }
    if let Some(claim) = &claim {
        if claim.path != document.path
            || claim.hash != document.hash
            || document.owner_revision.is_some()
        {
            return Err(corrupt("cached identity claim differs from its document"));
        }
        if note
            .fields
            .as_ref()
            .and_then(|f| f.get("wiki_id"))
            .and_then(Value::as_str)
            != Some(claim.id.as_str())
        {
            // Conservative reservations include isolated IDs in malformed YAML;
            // those do not make an otherwise unreadable ID selectable.
            if document.record_id.is_some() {
                return Err(corrupt("adopted cached document has another identity"));
            }
            return Err(WikiError::new(
                ErrorCode::RecordNotFound,
                "ID absent from index snapshot",
            ));
        }
    }
    let record = if let Some(id) = &document.record_id {
        let row = reader
            .record(id)?
            .ok_or_else(|| corrupt("cached document has no adopted record"))?;
        let identity = match &claim {
            Some(claim) => claim.clone(),
            None => reader
                .unique_identity_claim(id)?
                .ok_or_else(|| corrupt("adopted cached record has no identity claim"))?,
        };
        if identity.id != *id
            || identity.path != path
            || identity.hash != document.hash
            || identity.kind != Some(row.record.kind())
            || row.path != path
            || row.hash != document.hash
            || row.record.id() != id
            || document.kind != Some(row.record.kind())
            || document.owner_revision.is_some()
            || note.canonical.as_ref() != Some(&row.record)
        {
            return Err(corrupt(
                "cached record, document and identity claim disagree",
            ));
        }
        Some(row.record)
    } else {
        if let Some(claim) = &claim {
            if reader.record(&claim.id)?.is_some() {
                return Err(corrupt("adopted cached record is absent from its document"));
            }
        }
        None
    };
    format_cached_read(&document, record, diagnostics, request, from)
}

fn format_cached_read(
    document: &DocumentRow,
    record: Option<CanonicalRecord>,
    diagnostics: Vec<CatalogDiagnostic>,
    request: ReadRequest,
    from: Option<u64>,
) -> Result<ReadOutcome> {
    let note = parse_note(document.raw_text.as_bytes());
    let canonical = document.owner_revision.is_none();
    let body = if canonical {
        std::str::from_utf8(note.body())
            .map_err(|_| WikiError::invalid("cached body is not UTF-8"))?
    } else {
        &document.raw_text
    };
    let range = crate::app::offline::requested_read_range(body, request.range, from)?;
    let start = range.start() as usize;
    let wanted_end = range.end() as usize;
    let end = crate::app::offline::bounded_utf8_end(body, start, wanted_end, request.max_bytes)?;
    Ok(ReadOutcome {
        continuation: if end < wanted_end {
            Some(ByteSpan::new(end as u64, wanted_end as u64)?)
        } else {
            None
        },
        path: document.path.clone(),
        hash: document.hash.clone(),
        record,
        metadata: if canonical { note.fields.clone() } else { None },
        diagnostics,
        body: body[start..end].to_owned(),
        range: ByteSpan::new(start as u64, end as u64)?,
        truncated: end < wanted_end,
        source_citation: None,
    })
}
fn authenticated_read_citation(
    document: &DocumentRow,
    outcome: &ReadOutcome,
) -> Result<Option<ReadSourceCitation>> {
    let (Some(source), Some(revision)) = (&document.source_id, &document.owner_revision) else {
        return Ok(None);
    };
    if document.path != outcome.path
        || document.hash != outcome.hash
        || outcome.range.slice(&document.raw_text)? != outcome.body
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "read range differs from authenticated source bytes",
        ));
    }
    if outcome.range.is_empty()
        || !matches!(
            document.eligibility,
            Eligibility::Current | Eligibility::Historical | Eligibility::Withdrawn
        )
    {
        return Ok(None);
    }
    Ok(Some(ReadSourceCitation {
        citation: CitationRef::Source(SourceSpanRef {
            source_id: source.clone(),
            source_revision: revision.clone(),
            span: outcome.range,
            quote_hash: Blake3Hash::digest(outcome.body.as_bytes()),
        }),
        eligibility: document.eligibility,
    }))
}
fn input(path: &Path) -> Result<Vec<u8>> {
    input_bounded(path, MAX_INPUT_BYTES, "input exceeds 16 MiB")
}
fn input_bounded(path: &Path, max_bytes: usize, message: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if path == Path::new("-") {
        io::stdin()
            .lock()
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
    } else {
        File::open(path)
            .map_err(|e| input_error(path, e))?
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
    }
    .map_err(|e| input_error(path, e))?;
    if bytes.len() > max_bytes {
        return Err(usage(message));
    }
    Ok(bytes)
}
fn source_refs_input(path: Option<&Path>, body: &Path) -> Result<Option<PageSourceRefs>> {
    let Some(path) = path else { return Ok(None) };
    if path == Path::new("-") && body == Path::new("-") {
        return Err(usage(
            "Page body and SourceRefs cannot both use standard input",
        ));
    }
    let bytes = input_bounded(
        path,
        crate::app::page_citations::MAX_SOURCE_REFS_INPUT_BYTES,
        "Page SourceRefs input exceeds 64 KiB",
    )?;
    PageSourceRefs::from_json_slice(&bytes).map(Some)
}
fn source_refs_metadata(envelope: &mut Envelope, count: Option<usize>, dry_run: bool) {
    if let Some(count) = count {
        envelope.data["source_citations"] = json!({
            "reference_count":count,
            "verification_performed":!dry_run,
            "links_rendered":!dry_run,
            "state_scope":"verified_for_guarded_page_proposal",
            "prose_support_verified":false
        });
        if dry_run {
            envelope.warnings.push("Source citation preview parses the request only; Source/Revision paths, quote hashes, eligibility and generated links are unverified. Staging or applying performs verification.".into());
        }
    }
}
fn input_error(path: &Path, error: io::Error) -> WikiError {
    let mut failure = WikiError::new(
        ErrorCode::Usage,
        format!("cannot read input {}: {error}", path.display()),
    );
    failure.details = serde_json::json!({"next_action": "Check that the input path names a readable file, or use - to read standard input."});
    failure
}
fn capture(path: &Path, title: Option<&str>, media_type: Option<String>) -> Result<CaptureRequest> {
    let original = input(path)?;
    let origin = path
        .to_str()
        .ok_or_else(|| usage("source input path must be UTF-8"))?
        .to_owned();
    let text_type =
        path == Path::new("-") || crate::sources::local_text::local_text_candidate(path);
    let extraction = if text_type && std::str::from_utf8(&original).is_ok() {
        ExtractionInput::Utf8Preserve
    } else {
        ExtractionInput::Unsupported {
            extractor: "unsupported-local-format-v1".into(),
            fingerprint: Blake3Hash::digest(b"unsupported-local-format-v1"),
        }
    };
    Ok(CaptureRequest {
        title: title
            .unwrap_or_else(|| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Captured source")
            })
            .into(),
        origin_kind: if path == Path::new("-") {
            SourceOrigin::AgentReport
        } else {
            SourceOrigin::LocalFile
        },
        origin,
        original,
        extraction,
        media_type,
    })
}
pub fn present(
    envelope: &Envelope,
    format: OutputFormat,
    output: &mut impl Write,
) -> io::Result<()> {
    present_with_wiki(envelope, format, output, None)
}

/// Terminal summaries retain the caller's explicit vault selection.
/// Read/context stdout remains exact, pipeable content authority.
pub fn present_with_wiki(
    envelope: &Envelope,
    format: OutputFormat,
    output: &mut impl Write,
    wiki: Option<&Path>,
) -> io::Result<()> {
    if matches!(format, OutputFormat::Human)
        && !(envelope.ok && matches!(envelope.command.as_str(), "read" | "context"))
    {
        let mut bytes = Vec::new();
        present_inner(envelope, format, &mut bytes, wiki)?;
        write!(
            output,
            "{}",
            terminal_text(&String::from_utf8_lossy(&bytes), true)
        )
    } else {
        present_inner(envelope, format, output, wiki)
    }
}

fn terminal_text(text: &str, multiline: bool) -> String {
    text.chars()
        .flat_map(|c| {
            if (c.is_control() && !(multiline && matches!(c, '\n' | '\t')))
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn present_inner(
    envelope: &Envelope,
    format: OutputFormat,
    output: &mut impl Write,
    wiki: Option<&Path>,
) -> io::Result<()> {
    let command_prefix = wiki.map_or_else(
        || "lwiki".to_owned(),
        |path| {
            format!(
                "lwiki --wiki '{}'",
                path.to_string_lossy().replace('\'', "'\\''")
            )
        },
    );
    match format {
        OutputFormat::Json => {
            serde_json::to_writer(&mut *output, envelope)?;
            writeln!(output)
        }
        OutputFormat::Jsonl => {
            serde_json::to_writer(&mut *output, envelope)?;
            writeln!(output)
        }
        OutputFormat::Human if !envelope.ok => {
            if let Some(error) = &envelope.error {
                writeln!(output, "{}: {}", error.code, error.message)?;
                if let Some(hint) = &error.hint {
                    writeln!(output, "{hint}")?;
                }
                human_error_guidance(output, &error.details)?;
                if envelope.command.starts_with("source import ") {
                    human_import_error_guidance(output, &error.details)?;
                } else if error.code == "CONTENT_CONFLICT" {
                    writeln!(
                        output,
                        "Inspect the current record and retained change before retrying. For an author edit, reconcile the current content and prepare a new change with its observed hash. If recovery reports a durable conflict, inspect changes resolve --help."
                    )?;
                }
            }
            if let Some(change) = envelope
                .data
                .get("change")
                .and_then(|c| c.get("change_id"))
                .and_then(Value::as_str)
            {
                writeln!(
                    output,
                    "Retained change: {change}\nInspect with: {command_prefix} changes show {change}"
                )?;
            }
            if let Some(diagnostics) = envelope.data.get("diagnostics").and_then(Value::as_array) {
                for diagnostic in diagnostics {
                    writeln!(
                        output,
                        "{}: {} {}",
                        diagnostic["path"].as_str().unwrap_or_default(),
                        diagnostic["code"].as_str().unwrap_or_default(),
                        diagnostic["details"]
                    )?;
                }
            }
            Ok(())
        }
        OutputFormat::Human
            if envelope.command == "doctor" && envelope.data["cache_state"].is_string() =>
        {
            let doctor = &envelope.data;
            writeln!(
                output,
                "Cache: {} / {}",
                doctor["cache_layout"].as_str().unwrap_or("unknown"),
                doctor["cache_state"].as_str().unwrap_or("unknown")
            )?;
            if let Some(compatible) = doctor["parser_compatible"].as_bool() {
                writeln!(output, "Parser compatible: {compatible}")?;
            }
            if let Some(note) = doctor["cache_note"].as_str() {
                writeln!(output, "{note}")?;
            }
            if let Some(message) = doctor["cache_error"]["message"].as_str() {
                writeln!(output, "Cache observation failed: {message}")?;
                if let Some(hint) = doctor["cache_error"]["hint"].as_str() {
                    writeln!(output, "{hint}")?;
                }
            }
            writeln!(
                output,
                "Operation status: {}",
                doctor["operation_state"].as_str().unwrap_or("not_checked")
            )?;
            if let Some(change) = doctor["active_change"].as_str() {
                writeln!(
                    output,
                    "Active change: {change}\nRecover with: {command_prefix} recover"
                )?;
            }
            writeln!(
                output,
                "Canonical, history and cache-integrity audits: not performed. Canonical freshness: unknown."
            )?;
            writeln!(output, "Canonical diagnostics: {command_prefix} check")
        }
        OutputFormat::Human if envelope.command == "check" => {
            if envelope.data["canonical_check_performed"] != true {
                writeln!(
                    output,
                    "Dry run: canonical and cache checks were not performed."
                )
            } else if envelope.data["cache_matches_canonical"] == true {
                writeln!(
                    output,
                    "Check complete: canonical documents and selected index agree; no knowledge diagnostics."
                )
            } else {
                writeln!(
                    output,
                    "Canonical check complete: no knowledge diagnostics. Cache integrity was not checked (legacy catalog)."
                )
            }
        }
        OutputFormat::Human if envelope.command == "read" && envelope.data["dry_run"] == true => {
            let selector = &envelope.data["selector"];
            let (kind, target) = if selector["id"].is_string() {
                ("ID", &selector["id"])
            } else {
                ("path", &selector["path"])
            };
            writeln!(
                output,
                "Would read {kind} {target} in {} mode, up to {} UTF-8 bytes.",
                envelope.data["mode"].as_str().unwrap_or_default(),
                envelope.data["max_bytes"]
            )?;
            if !envelope.data["requested_range"].is_null() {
                let range = &envelope.data["requested_range"];
                if range["end"].is_null() {
                    writeln!(
                        output,
                        "Requested byte range: {}..EOF (unresolved).",
                        range["start"]
                    )?;
                } else {
                    writeln!(
                        output,
                        "Requested byte range: {}..{}.",
                        range["start"], range["end"]
                    )?;
                }
            }
            writeln!(output, "Body and freshness are unknown during dry-run.")
        }
        OutputFormat::Human if envelope.command == "read" => write!(
            output,
            "{}",
            envelope.data["body"].as_str().unwrap_or_default()
        ),
        OutputFormat::Human
            if envelope.command == "context"
                && envelope.data["selection_packet"]["selector_input"].is_string() =>
        {
            write!(
                output,
                "{}",
                envelope.data["selection_packet"]["selector_input"]
                    .as_str()
                    .unwrap_or_default()
            )
        }
        OutputFormat::Human
            if envelope.command == "context" && envelope.data["text"].is_string() =>
        {
            write!(
                output,
                "{}",
                envelope.data["text"].as_str().unwrap_or_default()
            )
        }
        OutputFormat::Human
            if envelope.command == "graph extract" && envelope.data["ready_to_import"] == true =>
        {
            serde_json::to_writer_pretty(&mut *output, &envelope.data["packet"])?;
            writeln!(output)
        }
        OutputFormat::Human if envelope.command == "search" => {
            if let Some(hits) = envelope.data["hits"].as_array() {
                if let Some(freshness) = &envelope.meta.freshness {
                    writeln!(
                        output,
                        "Freshness: {freshness}{}",
                        envelope
                            .meta
                            .verified_at
                            .as_ref()
                            .map(|at| format!(" at {at}"))
                            .unwrap_or_else(|| " (unverified)".into())
                    )?;
                }
                for hit in hits {
                    writeln!(
                        output,
                        "{} — {} ({})\n{}\n",
                        terminal_text(hit["path"].as_str().unwrap_or_default(), false),
                        terminal_text(hit["title"].as_str().unwrap_or_default(), false),
                        hit["eligibility"].as_str().unwrap_or_default(),
                        hit["excerpt"]["text"].as_str().unwrap_or_default()
                    )?;
                    if hit["excerpt"]["citation"]["kind"] == "source" {
                        let reference = &hit["excerpt"]["citation"]["reference"];
                        writeln!(
                            output,
                            "Source: {} revision {} bytes {}..{}\nQuote hash: {}\n",
                            reference["source_id"].as_str().unwrap_or_default(),
                            reference["source_revision"].as_str().unwrap_or_default(),
                            reference["span"]["start"],
                            reference["span"]["end"],
                            reference["quote_hash"].as_str().unwrap_or_default()
                        )?;
                    }
                }
                if hits.is_empty() {
                    writeln!(output, "No matches.")?;
                }
                if envelope.meta.partial {
                    if let Some(cursor) = envelope.data["next_cursor"].as_str() {
                        writeln!(
                            output,
                            "More results: repeat this search with the same options and --cursor '{cursor}'"
                        )?;
                    } else {
                        writeln!(
                            output,
                            "Results are truncated by the candidate bound; increase --candidates or narrow the query. No continuation cursor is available."
                        )?;
                    }
                }
                Ok(())
            } else {
                serde_json::to_writer_pretty(&mut *output, &envelope.data)?;
                writeln!(output)
            }
        }
        OutputFormat::Human if envelope.command == "init" => {
            let action = if envelope.data["created"] == true {
                "Created wiki"
            } else {
                "Would create wiki"
            };
            writeln!(
                output,
                "{action}: {}",
                envelope.data["path"].as_str().unwrap_or_default()
            )?;
            writeln!(
                output,
                "Wiki ID: {}",
                envelope.data["id"].as_str().unwrap_or_default()
            )?;
            writeln!(
                output,
                "Next: add a local file with lwiki --wiki PATH source add FILE, then search its contents."
            )
        }
        OutputFormat::Human if envelope.data["plan"]["operations"].is_array() => {
            let status = envelope.data["status"].as_str().unwrap_or("preview");
            writeln!(output, "{}: {status}", envelope.command)?;
            if let Some(ids) = envelope.data["allocated_ids"].as_object() {
                for (kind, id) in ids {
                    if let Some(id) = id.as_str() {
                        writeln!(output, "{kind}: {id}")?;
                    }
                }
            }
            if let Some(id) = envelope.data["unresolved_target"]["record_id"].as_str() {
                writeln!(output, "Page: {id}; destination unresolved in preview")?;
            }
            if let Some(extraction) = envelope.data["extraction_status"].as_str() {
                writeln!(
                    output,
                    "Extraction: {extraction}; citable: {}",
                    envelope.data["citable"]
                )?;
            }
            for operation in envelope.data["plan"]["operations"].as_array().unwrap() {
                writeln!(
                    output,
                    "  {} ({} bytes)",
                    operation["path"].as_str().unwrap_or_default(),
                    operation["byte_len"]
                )?;
            }
            if let Some(change) = envelope.data["change"]["change_id"].as_str() {
                writeln!(output, "Change: {change}")?;
                if status == "prepared" {
                    writeln!(
                        output,
                        "Inspect with: {command_prefix} changes show {change}\nApply with: {command_prefix} changes apply {change}"
                    )?;
                }
            }
            if envelope.data["reused"] == true {
                writeln!(output, "Reused the existing change; no duplicate capture.")?;
            }
            Ok(())
        }
        OutputFormat::Human => {
            serde_json::to_writer_pretty(&mut *output, &envelope.data)?;
            writeln!(output)
        }
    }
}

fn human_error_guidance(output: &mut impl Write, details: &Value) -> io::Result<()> {
    if let Some(object) = details.as_object() {
        for (key, item) in object {
            if matches!(
                key.as_str(),
                "reason" | "failure_code" | "next_action" | "recovery_action"
            ) && let Some(text) = item.as_str()
            {
                writeln!(output, "{key}: {text}")?;
            }
            if key == "template"
                && let Some(template) = item.as_str()
            {
                writeln!(output, "Page template:\n{template}")?;
            }
            if item.is_object() {
                human_error_guidance(output, item)?;
            }
        }
    }
    Ok(())
}

fn human_import_error_guidance(output: &mut impl Write, details: &Value) -> io::Result<()> {
    if let Some(item) = details.get("import_item")
        && let (Some(ordinal), Some(path)) = (item["ordinal"].as_u64(), item["path"].as_str())
    {
        writeln!(
            output,
            "Required input {ordinal}: {}",
            terminal_text(path, false)
        )?;
    }
    if let Some(progress) = details.get("import") {
        if let (Some(key), Some(done), Some(total)) = (
            progress["key"].as_str(),
            progress["imported_items"].as_u64(),
            progress["total_items"].as_u64(),
        ) {
            writeln!(
                output,
                "Import '{}': {done}/{total} items committed.",
                terminal_text(key, false)
            )?;
        }
        if let Some(group) = progress["pending_group"].as_u64() {
            writeln!(output, "Pending group: {group}")?;
        }
        if let Some(change) = progress["pending_change"].as_str() {
            writeln!(output, "Pending change: {}", terminal_text(change, false))?;
        }
        if let Some(items) = progress["pending_items"].as_array() {
            for item in items.iter().take(8) {
                if let (Some(ordinal), Some(path)) =
                    (item["ordinal"].as_u64(), item["path"].as_str())
                {
                    writeln!(
                        output,
                        "Pending input {ordinal}: {}",
                        terminal_text(path, false)
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod import_error_presentation_tests {
    use super::*;

    #[test]
    fn plain_import_error_identifies_input_progress_and_continuation() {
        let mut error = WikiError::new(
            ErrorCode::ContentConflict,
            "import original type or length changed",
        );
        error.hint =
            Some("Restore the required input, then source import resume --key 'drift-key'".into());
        error.details = json!({
            "import_item":{"ordinal":4,"path":"/outside inputs/missing\nfile.txt"},
            "import":{"key":"drift-key","imported_items":4,"total_items":8,
                "pending_group":1,"pending_change":"change-example",
                "pending_items":[{"ordinal":4,"path":"/outside inputs/missing\nfile.txt"}]}
        });
        let envelope = failure("source import resume", error);
        let mut bytes = Vec::new();
        present(&envelope, OutputFormat::Human, &mut bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("Required input 4: /outside inputs/missing\\nfile.txt"));
        assert!(text.contains("Import 'drift-key': 4/8 items committed."));
        assert!(text.contains("Pending group: 1"));
        assert!(text.contains("Pending change: change-example"));
        assert!(text.contains("source import resume --key 'drift-key'"));
        assert!(!text.contains("author edit"));
        assert!(!text.contains("changes resolve"));
    }

    #[test]
    fn ordinary_content_conflict_keeps_existing_author_guidance() {
        let envelope = failure(
            "page put",
            WikiError::new(ErrorCode::ContentConflict, "author changed"),
        );
        let mut bytes = Vec::new();
        present(&envelope, OutputFormat::Human, &mut bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("author edit"));
        assert!(text.contains("changes resolve"));
        assert!(!text.contains("Pending input"));
    }
}

fn embedding_runtime(
    runtime: &crate::app::remote::RemoteRuntime,
) -> crate::app::embeddings::EmbeddingRuntime<'_> {
    crate::app::embeddings::EmbeddingRuntime::new(
        &runtime.service,
        &runtime.dispatcher,
        runtime.job_options.clone(),
        runtime.limits.clone(),
        runtime.created_at_utc_ms,
        runtime.deadline_utc_ms,
        runtime.requested_limits.clone(),
    )
}
// Fallback is allowed to return a local result after a remote failure. Keep
// invocation activity on that successful result, including its nested graph.
trait EmbeddingNetworkResult {
    fn record_network_activity(&mut self, used: bool);
}
impl EmbeddingNetworkResult for crate::retrieval::HitSet {
    fn record_network_activity(&mut self, used: bool) {
        self.network_used |= used;
        if let Some(graph) = &mut self.graph {
            graph.network_used |= used;
        }
    }
}
impl EmbeddingNetworkResult for crate::graph::GraphResult {
    fn record_network_activity(&mut self, used: bool) {
        self.network_used |= used;
    }
}
impl EmbeddingNetworkResult for crate::retrieval::ContextResult {
    fn record_network_activity(&mut self, used: bool) {
        self.network_used |= used;
    }
}
fn with_embedding_runtime<T: EmbeddingNetworkResult>(
    app: &OfflineApp,
    args: &Arguments,
    remote: &super::remote::RemoteArguments,
    fallback: bool,
    mut operation: impl FnMut(Option<&crate::app::embeddings::EmbeddingRuntime<'_>>, bool) -> Result<T>,
) -> Result<T> {
    match operation(None, false) {
        Ok(result) => Ok(result),
        Err(error)
            if matches!(
                error.code,
                ErrorCode::CapabilityUnavailable | ErrorCode::OfflineUnavailable
            ) =>
        {
            if !app.options().offline && args.profile.is_some() {
                let runtime = remote.runtime(app, args.profile.as_deref(), Capability::Embed)?;
                let embedding = embedding_runtime(&runtime);
                let mut result = with_network_activity(
                    Some(&runtime.dispatcher),
                    operation(Some(&embedding), fallback),
                )?;
                result.record_network_activity(runtime.dispatcher.network_used());
                Ok(result)
            } else if fallback {
                operation(None, true)
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

fn result_metadata(
    meta: &mut Metadata,
    snapshot: &crate::domain::ReadSnapshot,
    verification: &SnapshotVerification,
) {
    meta.index_generation = Some(snapshot.generation);
    match verification {
        SnapshotVerification::IndexSnapshot => meta.freshness = Some("index_snapshot".into()),
        SnapshotVerification::IndexedEvidence { verified_at, .. } => {
            meta.freshness = Some("indexed_evidence".into());
            meta.verified_at = Some(verified_at.clone());
        }
        SnapshotVerification::VerifiedSnapshot { verified_at } => {
            meta.freshness = Some("verified_snapshot".into());
            meta.verified_at = Some(verified_at.clone());
        }
    }
}

fn with_network_activity<T>(
    dispatcher: Option<&crate::providers::dispatcher::Dispatcher>,
    result: Result<T>,
) -> Result<T> {
    result.map_err(|mut error| {
        error.network_used |= dispatcher.is_some_and(|d| d.network_used());
        error
    })
}

#[cfg(test)]
#[path = "selected_search_tests.rs"]
mod selected_search_adapter_tests;

#[cfg(test)]
mod cached_read_tests {
    use super::*;

    #[test]
    fn cached_legacy_read_refuses_activation_after_reader_acquisition() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: '1'\nwiki_kind: vault\nwiki_id: vault_read_race\ntitle: Read race\n---\n").unwrap();
        std::fs::write(temp.path().join("page.md"), b"Before activation").unwrap();
        let fs = VaultFs::new(crate::vault::VaultRoot::explicit(temp.path()).unwrap());
        let vault = RecordId::new("vault_read_race").unwrap();
        let catalog = Catalog::new(fs.clone(), vault.clone());
        let writer = WriterPermit::acquire(fs.root(), Duration::ZERO).unwrap();
        catalog.sync(&writer).unwrap();
        let held = catalog.index_snapshot().unwrap();
        let request = || ReadRequest {
            selector: RecordSelector::Path(VaultRelativePath::new("page.md").unwrap()),
            range: None,
            max_bytes: 1024,
        };
        assert_eq!(
            cached_legacy_read(&catalog, &held, request(), None)
                .unwrap()
                .body,
            "Before activation"
        );
        // Authority becomes visible before selection publication during activation.
        // A held v1 reader must not escape through that transition window.
        crate::changes::operation_authority::activate(
            &fs,
            &writer,
            &vault,
            crate::changes::operation_authority::Publication {
                file_id: "a".repeat(32),
                epoch: 1,
            },
            crate::changes::operation_authority::Presence::LegacyMayBeAbsent,
        )
        .unwrap();
        assert_eq!(
            cached_legacy_read(&catalog, &held, request(), None)
                .unwrap_err()
                .code,
            ErrorCode::RecoveryRequired
        );
    }
}
