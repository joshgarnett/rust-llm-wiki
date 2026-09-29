use super::arguments::*;
use crate::{
    app::*,
    catalog::{
        Catalog, CatalogGraphValidator, CatalogOptions, ReaderSnapshot, SnapshotVerification,
    },
    changes::ChangeEngine,
    config::{self, PreferenceOptions},
    domain::*,
    jobs::Capability,
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
    "page rename",
    "source add",
    "source refresh",
    "source withdraw",
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
    "research status",
    "research report",
    "changes show",
    "changes apply",
    "changes abort",
    "changes rollback",
    "recover",
    "migrate",
];
pub const SCHEMAS: &[&str] = &[
    "output",
    "record",
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
    "research-frontier",
    "research-gaps",
    "research-synthesis",
    "research-run-plan",
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
    if let Some(outcome) = error.details.get("research_outcome") {
        envelope.data = outcome.clone();
        envelope.meta.partial = true;
        envelope.warnings = outcome
            .get("report")
            .and_then(|report| report.get("warnings"))
            .and_then(Value::as_array)
            .map_or(vec![], |warnings| {
                warnings
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            });
    }
    if let Some(change) = error.details.get("change") {
        envelope.data = json!({"change":change});
        envelope.meta.partial = true;
    }
    if error.details.get("partial_export").and_then(Value::as_bool) == Some(true) {
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
        Err(error) => {
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
        Command::Capabilities => {
            return Ok(Envelope::success(
                command,
                json!({"version":env!("CARGO_PKG_VERSION"),"commands":COMMANDS,"schemas":SCHEMAS,"network":true,"search_modes":["literal","lexical","semantic","hybrid"],"graph_seed_modes":["lexical","semantic"],"extraction_executors":["agent","api"],"jsonl_commands":["index sync","index rebuild","recover","changes apply","source add","source refresh","research run","research resume","doctor --probe"]}),
            ));
        }
        Command::Schema { name } => {
            let schema = match name.as_str() {
                "output" => include_str!("../../schemas/output-v1.json"),
                "record" => include_str!("../../schemas/record-v1.json"),
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
                "research-frontier" => include_str!("../../schemas/research-frontier-v1.json"),
                "research-gaps" => include_str!("../../schemas/research-gaps-v1.json"),
                "research-synthesis" => include_str!("../../schemas/research-synthesis-v1.json"),
                "research-run-plan" => include_str!("../../schemas/research-run-plan-v1.json"),
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
                operation_options(args, args.offline, args.lock_timeout_ms.unwrap_or(1000)),
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
            let range = match (start, end) {
                (Some(start), Some(end)) => {
                    Some(ByteSpan::new(*start, *end).map_err(|e| usage(e.message))?)
                }
                (None, None) => None,
                _ => return Err(usage("--start and --end must be supplied together")),
            };
            let request = ReadRequest {
                selector: selector.record_selector()?,
                range,
                max_bytes: preferences.read_max_bytes,
            };
            let outcome = if args.dry_run {
                app.read(request)?
            } else {
                let (_writer, reader) = reader(&app, *no_sync)?;
                snapshot_metadata(&mut envelope.meta, &reader);
                if *no_sync {
                    cached_read(&reader, request)?
                } else {
                    let outcome = app.read(request)?;
                    if !reader
                        .projection()
                        .documents
                        .iter()
                        .any(|d| d.path == outcome.path && d.hash == outcome.hash)
                    {
                        return Err(WikiError::new(
                            ErrorCode::FreshnessConflict,
                            "read bytes changed after snapshot verification",
                        ));
                    }
                    outcome
                }
            };
            envelope.meta.partial = outcome.truncated;
            envelope.data = value(outcome)?;
            if args.dry_run {
                envelope
                    .warnings
                    .push("cache state and index freshness are unknown during dry-run".into());
            }
        }
        Command::Page {
            command:
                PageCommand::Put {
                    file,
                    path,
                    if_match,
                },
        } => {
            let bytes = input(file)?;
            let target = if let Some(path) = path {
                path.clone()
            } else {
                let note = parse_note(&bytes);
                let record = note
                    .canonical
                    .as_ref()
                    .filter(|r| r.kind() == RecordKind::Page)
                    .ok_or_else(|| WikiError::invalid("page put requires a valid page envelope"))?;
                let projection = crate::catalog::scan::scan(app.fs(), app.vault_id())?;
                projection
                    .records
                    .get(record.id())
                    .map(|row| row.path.clone())
                    .unwrap_or(VaultRelativePath::new(format!("pages/{}.md", record.id()))?)
            };
            mutation(
                &mut envelope,
                app.page_put(target, bytes, if_match.clone())?,
            )?;
        }
        Command::Page {
            command: PageCommand::Rename { id, to, if_match },
        } => mutation(
            &mut envelope,
            app.page_rename(id.clone(), to.clone(), if_match.clone())?,
        )?,
        Command::Source {
            command:
                SourceCommand::Add {
                    file,
                    title,
                    media_type,
                },
        } => mutation(
            &mut envelope,
            app.source_add(capture(file, title.as_deref(), media_type.clone())?)?,
        )?,
        Command::Source {
            command:
                SourceCommand::Refresh {
                    id,
                    file,
                    title,
                    media_type,
                },
        } => mutation(
            &mut envelope,
            app.source_refresh(
                id.clone(),
                capture(file, title.as_deref(), media_type.clone())?,
            )?,
        )?,
        Command::Source {
            command: SourceCommand::Withdraw { id, reason },
        } => mutation(&mut envelope, app.source_withdraw(id.clone(), reason)?)?,
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
            let outcome = app.index_sync(matches!(command, IndexCommand::Rebuild))?;
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
                && (sync || probe || !app.options().offline && args.profile.is_some())
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
        Command::Search(search) => {
            let plan = retrieval::lexical::validate_plan(&search.query, &search.plan())?;
            if matches!(
                plan.mode,
                retrieval::SearchMode::Semantic | retrieval::SearchMode::Hybrid
            ) {
                search.remote.limits()?;
            }
            if search.graph.is_some() && plan.mode != retrieval::SearchMode::Hybrid {
                return Err(usage("--graph entities requires hybrid search"));
            }
            if args.dry_run {
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
            let plan = query::validate_plan(&options.plan(neighbors))?;
            if plan.seed_mode == crate::graph::GraphSeedMode::Semantic && !neighbors {
                options.remote.limits()?;
            }
            if let GraphCommand::Query { query, .. } = command
                && plan.seed_mode == crate::graph::GraphSeedMode::Lexical
            {
                retrieval::lexical::lexical_expression(query)?;
            }
            if args.dry_run {
                let projection = crate::catalog::scan::scan(app.fs(), app.vault_id())?;
                if let GraphCommand::Neighbors { id, .. } = command {
                    if projection.diagnostics.iter().any(|d| {
                        d.record_id.as_ref() == Some(id) && d.code == ErrorCode::ReferenceAmbiguous
                    }) {
                        return Err(WikiError::new(
                            ErrorCode::ReferenceAmbiguous,
                            "neighbor ID is ambiguous",
                        ));
                    }
                    if !projection.records.contains_key(id) {
                        return Err(WikiError::new(
                            ErrorCode::RecordNotFound,
                            "neighbor ID is not present",
                        ));
                    }
                }
                envelope.data = json!({"plan":plan,"dry_run":true,"cache_state_unknown":true,"canonical_record_count":projection.records.len(),"results":null});
                envelope
                    .warnings
                    .push("graph results and index freshness are unknown during dry-run".into());
            } else {
                let result = if plan.seed_mode == crate::graph::GraphSeedMode::Semantic
                    && !neighbors
                {
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
            let request = context.request();
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
            if context.search.no_sync && request.scope != retrieval::ContextScope::Snapshot {
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "unverified context requires --scope snapshot",
                ));
            }
            let request = retrieval::context::validate_request(&context.search.query, &request)?;
            if args.dry_run {
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
                            app.semantic_context(
                                &context.search.query,
                                &request,
                                runtime,
                                context.search.no_sync,
                                fallback,
                            )
                        },
                    )?
                } else {
                    let catalog = Catalog::with_options(
                        app.fs().clone(),
                        app.vault_id().clone(),
                        CatalogOptions {
                            busy_timeout_ms: app.options().lock_timeout_ms,
                            fault: None,
                        },
                    );
                    let writer = if request.scope == retrieval::ContextScope::Snapshot {
                        None
                    } else {
                        Some(WriterPermit::acquire(
                            app.fs().root(),
                            Duration::from_millis(app.options().lock_timeout_ms),
                        )?)
                    };
                    retrieval::verification::context(
                        &catalog,
                        writer.as_ref(),
                        &context.search.query,
                        &request,
                    )?
                };
                envelope.meta.network_used = result.network_used;
                envelope.meta.index_generation = Some(result.snapshot().generation);
                match result.verification() {
                    SnapshotVerification::IndexSnapshot => {
                        envelope.meta.freshness = Some("index_snapshot".into())
                    }
                    SnapshotVerification::VerifiedSnapshot { verified_at } => {
                        envelope.meta.freshness = Some("verified_snapshot".into());
                        envelope.meta.verified_at = Some(verified_at.clone());
                    }
                }
                envelope.meta.partial = result.truncated();
                envelope.warnings.extend(result.warnings().iter().cloned());
                envelope.data = value(result)?;
            }
        }
        Command::Check => {
            let outcome = app.check()?;
            let count = outcome.error_count;
            envelope.data = value(outcome)?;
            if count != 0 {
                envelope.ok = false;
                envelope.error = Some(error_output(WikiError::invalid(format!(
                    "{count} knowledge diagnostics"
                ))));
            }
        }
        Command::Research(arguments) => {
            let result =
                super::research::execute(&arguments.command, &app, preferences.profile.as_deref())?;
            envelope.data = result.data;
            envelope.meta.network_used = result.network_used;
            envelope.meta.partial = result.partial;
            envelope.warnings.extend(result.warnings);
        }
        Command::Doctor {
            probe,
            role,
            remote,
        } => {
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
                    let result = app.probe_provider(&runtime, service_role)?;
                    doctor.provider_probe_performed = result.network_used;
                    envelope.meta.network_used = result.network_used;
                    envelope.data = json!({"doctor":doctor,"probe":result});
                }
            } else {
                envelope.data = value(doctor)?;
            }
        }
        Command::Changes { command } => match command {
            ChangesCommand::Show { id, operation } => {
                if let Some(operation) = operation {
                    envelope.data = value(app.changes_payload(id.clone(), *operation)?)?;
                } else {
                    let details = app.changes_show(id.clone())?;
                    envelope.meta.partial = !details.omitted_payloads.is_empty();
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
    envelope.data = value(outcome)?;
    Ok(())
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
        SnapshotVerification::VerifiedSnapshot { verified_at } => {
            meta.freshness = Some("verified_snapshot".into());
            meta.verified_at = Some(verified_at.clone());
        }
    }
}
fn cached_read(reader: &ReaderSnapshot, request: ReadRequest) -> Result<ReadOutcome> {
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
    let note = parse_note(document.raw_text.as_bytes());
    let canonical = document.owner_revision.is_none();
    let body = if canonical {
        std::str::from_utf8(note.body())
            .map_err(|_| WikiError::invalid("cached body is not UTF-8"))?
    } else {
        &document.raw_text
    };
    let range = request
        .range
        .unwrap_or(ByteSpan::new(0, body.len() as u64)?);
    range.slice(body).map_err(|e| usage(e.message))?;
    let start = range.start() as usize;
    let wanted_end = range.end() as usize;
    let mut end = wanted_end.min(start.saturating_add(request.max_bytes));
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    Ok(ReadOutcome {
        path: path.clone(),
        hash: document.hash.clone(),
        record: document
            .record_id
            .as_ref()
            .and_then(|id| projection.records.get(id))
            .map(|r| r.record.clone()),
        metadata: if canonical { note.fields.clone() } else { None },
        diagnostics: projection
            .diagnostics
            .iter()
            .filter(|d| d.path == path)
            .cloned()
            .collect(),
        body: body[start..end].to_owned(),
        range: ByteSpan::new(start as u64, end as u64)?,
        truncated: end < wanted_end,
    })
}
fn input(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if path == Path::new("-") {
        io::stdin()
            .lock()
            .take(MAX_INPUT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
    } else {
        File::open(path)
            .map_err(|e| WikiError::new(ErrorCode::Internal, format!("open input: {e}")))?
            .take(MAX_INPUT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
    }
    .map_err(|e| WikiError::new(ErrorCode::Internal, format!("read input: {e}")))?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(usage("input exceeds 16 MiB"));
    }
    Ok(bytes)
}
fn capture(path: &Path, title: Option<&str>, media_type: Option<String>) -> Result<CaptureRequest> {
    let original = input(path)?;
    let origin = path
        .to_str()
        .ok_or_else(|| usage("source input path must be UTF-8"))?
        .to_owned();
    let text_type = path == Path::new("-")
        || path.extension().is_none()
        || path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "txt"
                    | "md"
                    | "markdown"
                    | "csv"
                    | "tsv"
                    | "json"
                    | "yaml"
                    | "yml"
                    | "rs"
                    | "toml"
                    | "log"
            )
        });
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
            }
            if let Some(change) = envelope
                .data
                .get("change")
                .and_then(|c| c.get("change_id"))
                .and_then(Value::as_str)
            {
                writeln!(
                    output,
                    "Retained change: {change}\nInspect with: lwiki changes show {change}"
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
        OutputFormat::Human if envelope.command == "read" => write!(
            output,
            "{}",
            envelope.data["body"].as_str().unwrap_or_default()
        ),
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
                        hit["path"].as_str().unwrap_or_default(),
                        hit["title"].as_str().unwrap_or_default(),
                        hit["eligibility"].as_str().unwrap_or_default(),
                        hit["excerpt"]["text"].as_str().unwrap_or_default()
                    )?;
                }
                if hits.is_empty() {
                    writeln!(output, "No matches.")?;
                }
                if envelope.meta.partial {
                    writeln!(
                        output,
                        "Results are truncated; use the continuation cursor for another page."
                    )?;
                }
                for warning in &envelope.warnings {
                    writeln!(output, "Warning: {warning}")?;
                }
                Ok(())
            } else {
                serde_json::to_writer_pretty(&mut *output, &envelope.data)?;
                writeln!(output)
            }
        }
        OutputFormat::Human => {
            serde_json::to_writer_pretty(&mut *output, &envelope.data)?;
            writeln!(output)
        }
    }
}

fn embedding_runtime(
    runtime: &crate::app::remote::RemoteRuntime,
) -> crate::app::embeddings::EmbeddingRuntime<'_> {
    crate::app::embeddings::EmbeddingRuntime {
        service: &runtime.service,
        dispatcher: &runtime.dispatcher,
        job_options: runtime.job_options.clone(),
        limits: runtime.limits.clone(),
        deadline_ms: u64::try_from(runtime.deadline_utc_ms - runtime.created_at_utc_ms)
            .unwrap_or(0),
    }
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
