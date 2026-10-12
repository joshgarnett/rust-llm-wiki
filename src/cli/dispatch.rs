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
    "setup",
    "config show",
    "config set",
    "skill export",
    "skill install",
    "skill status",
    "skill update",
    "skill remove",
    "schema",
    "init",
    "wiki show",
    "wiki default set",
    "wiki default clear",
    "read",
    "inspect",
    "edit",
    "page put",
    "page init",
    "page batch",
    "page rename",
    "source add",
    "source capture",
    "source read",
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
    "relations prepare",
    "relations run",
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
    "context-original-selection",
    "context-original-selection-auto",
    "context-selection",
    "output",
    "output-v1",
    "literal-edit",
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
fn has_retained_writes(state: &Value) -> bool {
    state["applied"] == true
        || state["changed"] == true
        || ["retained_paths", "created", "published_paths"]
            .iter()
            .any(|key| {
                state[*key]
                    .as_array()
                    .is_some_and(|paths| !paths.is_empty())
            })
}
fn failure(command: &str, error: WikiError) -> Envelope {
    let mut envelope = Envelope::failure(command, &error.code.to_string(), error.message.clone());
    envelope.meta.network_used = error.network_used;
    envelope.meta.selection = error.details.get("selection").cloned();
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
    if has_retained_writes(&error.details["initialization"])
        || has_retained_writes(&error.details["installation"])
        || error.details["init"]["created"] == true
    {
        envelope.meta.partial = true;
    }
    if command == "check" && error.details.get("complete").and_then(Value::as_bool) == Some(false) {
        envelope.meta.partial = true;
    }
    if matches!(command, "index sync" | "index rebuild") {
        if !envelope.data.is_object() {
            envelope.data = json!({});
        }
        envelope.data["completion"] = json!("failed");
        envelope.data["health"] = json!({"schema_version":"lwiki.health.v1","discovery_complete":null,"structured_eligibility_complete":null,"membership_freshness":"unknown"});
    }
    envelope.error = Some(error_output(error));
    envelope
}
pub fn execute(args: &Arguments) -> (Envelope, u8) {
    crate::maintenance_diagnostic::begin_command();
    let _diagnostic_unwind = crate::maintenance_diagnostic::Unwind;
    let (result, metrics) = match crate::maintenance_parallel::command_scope(|| {
        let result = execute_inner(args);
        Ok((result, crate::maintenance_parallel::metrics()))
    }) {
        Ok(value) => value,
        Err(error) => (Err(error), Default::default()),
    };
    let (mut envelope, exit) = match result {
        Ok(envelope) => {
            let exit = if envelope.ok { 0 } else { 9 };
            crate::maintenance_diagnostic::command_result(exit == 0);
            (envelope, exit)
        }
        Err(error) => {
            let exit = error.exit_code();
            (failure(args.command.name(), error), exit)
        }
    };
    if metrics.submitted != 0 || metrics.owner_activation_validations != 0 {
        envelope.meta.maintenance =
            Some(serde_json::to_value(metrics).expect("maintenance metrics serialize"));
    }
    crate::maintenance_diagnostic::command_result(exit == 0);
    (envelope, exit)
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
    match args.command.name() {
        "search" => crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::Search,
            || execute_inner_diagnostic_body(args),
        ),
        "read" => crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::Read,
            || execute_inner_diagnostic_body(args),
        ),
        _ => execute_inner_diagnostic_body(args),
    }
}
fn execute_inner_diagnostic_body(args: &Arguments) -> Result<Envelope> {
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
        Command::Setup(setup) => return super::setup::execute(args, setup),
        Command::Config {
            command: ConfigCommand::Show,
        } => return super::config_show::execute(args),
        Command::Config {
            command: ConfigCommand::Set(settings),
        } => return super::config_set::execute(args, settings),
        Command::Capabilities => {
            return Ok(Envelope::success(
                command,
                json!({"version":env!("CARGO_PKG_VERSION"),"commands":COMMANDS,"schemas":SCHEMAS,"network":true,"original_source_selection":{"prepare_flag":"--prepare-original-selection","path_flag":"--selection-original-path","reply_flag":"--selection","schema":"context-original-selection","version":"lwiki.context-original-selection.v1","discovery":{"flag":"--discover-originals","schema":"context-original-selection-auto","version":"lwiki.context-original-selection.v2","max_candidates":80,"max_hits":10,"admission":"whole_authenticated_originals_with_explicit_omissions","replay":"repeat_same_discovery_flags"},"layout":"normalized","mode":"lexical","scope":"indexed-documents","historical":"explicit_include_historical","max_paths":16,"max_raw_bytes":98304,"max_indexed_payload_bytes":524288,"max_selector_input_bytes":130048,"transport_reserved_bytes":1024,"max_reply_bytes":4096,"max_ranges":16,"max_ranges_per_owner":4,"global_membership_verified":false,"model_called":false},"read":{"start_without_end":true,"omitted_end":"body_eof","coordinates":"utf8_body_bytes","explicit_ranges":"strict","byte_limit":"returned_text","dry_run":"unresolved_request_only"},"page_source_refs":{"commands":["page init","page put"],"flag":"--source-refs","schema":"page-source-refs","max_input_bytes":65536,"max_references":16,"citation_kinds":["source"],"links":"relative_to_resolved_page_path","prose_support_verified":false,"dry_run":"request_validation_only"},"search_modes":["literal","lexical","semantic","hybrid"],"selected_search":{"flag":"--verify-selected","layout":"normalized","modes":["literal","lexical","semantic","hybrid"],"no_sync_compatible":true,"scope":"displayed document dependencies","global_membership_verified":false,"dry_run":"request validation only","budget":{"max_bytes":67108864,"max_files":4096,"max_entries":16384,"max_elapsed_ms":2000}},"selected_neighbors":{"command":"graph neighbors","layout":"normalized","root_kind":"entity","default_verification":true,"explicit_verification_flag":"--verify-selected","no_sync":"cached_uncited","scope":"selected_graph_neighbors","global_membership_verified":false,"current_only":true,"navigation":false,"cursor":false,"dry_run":"request validation only","limits":{"depth":2,"incident_per_seed":16,"assertions":128,"candidates":80,"hits":50,"support":2,"contrary":1},"budget":{"max_bytes":67108864,"max_files":4096,"max_entries":16384,"max_elapsed_ms":2000}},"graph_seed_modes":["lexical","semantic"],"extraction_executors":["agent","api"],"research_executor":"agent-handoff","jsonl_commands":["index sync","index rebuild","recover","changes apply","source add","source refresh","research run","research resume","research import","doctor --probe"]}),
            ));
        }
        Command::Schema { name } => {
            let schema = match name.as_str() {
                "context-original-selection" => {
                    include_str!("../../schemas/context-original-selection-v1.json")
                }
                "context-original-selection-auto" => {
                    include_str!("../../schemas/context-original-selection-v2.json")
                }
                "context-selection" => include_str!("../../schemas/context-selection-v1.json"),
                "output" => include_str!("../../schemas/output-v2.json"),
                "output-v1" => include_str!("../../schemas/output-v1.json"),
                "literal-edit" => include_str!("../../schemas/literal-edit-v1.json"),
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
        Command::Skill { command: skill } if !matches!(skill, SkillCommand::Export { .. }) => {
            if args.stage {
                return Err(usage(
                    "agent installation cannot be staged; use --dry-run to preview",
                ));
            }
            let (options, action) = match skill {
                SkillCommand::Install(options) => (options, "install"),
                SkillCommand::Status(options) => (options, "status"),
                SkillCommand::Update(options) => (options, "update"),
                SkillCommand::Remove(options) => (options, "remove"),
                SkillCommand::Export { .. } => unreachable!(),
            };
            let root = match &options.directory {
                Some(path) => path.clone(),
                None if options.scope == "project" => std::env::current_dir()
                    .map_err(|error| usage(format!("cannot resolve project folder: {error}")))?,
                None => std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .ok_or_else(|| {
                        usage("HOME is unavailable; use --directory to choose the agent's home folder")
                    })?,
            };
            let report = super::skill_install::operate(
                action,
                &options.target,
                &root,
                &options.scope,
                args.dry_run,
            )?;
            if !report["failure"].is_null() {
                let mut error: WikiError = serde_json::from_value(report["failure"].clone())
                    .map_err(|_| {
                        WikiError::new(
                            ErrorCode::Internal,
                            "invalid agent installation failure report",
                        )
                    })?;
                error.details = json!({"installation":report});
                return Err(error);
            }
            return Ok(Envelope::success(command, report));
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
        Command::Wiki {
            command: WikiCommand::Default {
                command: default_command,
            },
        } => {
            if args.stage {
                return Err(usage("default selection cannot be staged"));
            }
            let local =
                config::local::load_user_config(args.preferences.as_deref(), args.no_user_config)?;
            let proposed = match default_command {
                DefaultCommand::Set { path } => {
                    Some(crate::vault::discovery::default_from_path(path)?)
                }
                DefaultCommand::Clear => None,
            };
            if let (Some(expected), Some(target)) = (&args.expect_wiki_id, &proposed)
                && expected != &target.wiki_id
            {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "default target differs from --expect-wiki-id",
                ));
            }
            let report = config::local::update_default(
                &local,
                proposed.clone(),
                args.dry_run,
                args.lock_timeout_ms.unwrap_or(5000),
            )?;
            let mut envelope = Envelope::success(command, value(report)?);
            if let Some(target) = proposed {
                envelope.meta.wiki_id = Some(target.wiki_id.to_string());
                envelope.meta.selection = Some(
                    json!({"schema_version":"lwiki.selection.v1","root":target.path,"wiki_id":target.wiki_id,"origin":"explicit","requested_path":target.path,"configuration":local.summary(),"preference_origins":{}}),
                );
            }
            return Ok(envelope);
        }
        Command::Init {
            path,
            title,
            existing,
            set_default,
        } => {
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
            if args.expect_wiki_id.is_some() {
                return Err(usage(
                    "init allocates a new identity; --expect-wiki-id applies to existing wikis",
                ));
            }
            let local =
                config::local::load_user_config(args.preferences.as_deref(), args.no_user_config)?;
            if *set_default {
                config::local::require_write_target(&local)?;
            }
            let outcome = crate::app::offline::init_with_existing(
                path,
                title,
                *existing,
                operation_options(args, args.offline, args.lock_timeout_ms.unwrap_or(5000)),
            )?;
            let mut envelope = Envelope::success(command, value(&outcome)?);
            envelope.meta.wiki_id = Some(outcome.id.to_string());
            envelope.meta.selection = Some(
                json!({"schema_version":"lwiki.selection.v1", "root":outcome.path,"wiki_id":outcome.id,"origin":"initialization","requested_path":path,"configuration":local.summary(),"preference_origins":{}}),
            );
            if *set_default {
                let proposed = config::DefaultWiki {
                    path: std::path::PathBuf::from(&outcome.path),
                    wiki_id: outcome.id.clone(),
                };
                match config::local::update_default(
                    &local,
                    Some(proposed),
                    args.dry_run,
                    args.lock_timeout_ms.unwrap_or(5000),
                ) {
                    Ok(report) => envelope.data["default_registration"] = value(report)?,
                    Err(mut error) => {
                        let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
                        let preferences = local
                            .path()
                            .map(|path| {
                                format!(" --preferences {}", quote(&path.to_string_lossy()))
                            })
                            .unwrap_or_default();
                        let retry = format!(
                            "lwiki{preferences} wiki default set {}",
                            quote(&outcome.path)
                        );
                        error.details = json!({"init":outcome,"wiki_id":outcome.id,"selection":envelope.meta.selection,"default_registered":false,"configuration":local.summary(),"registration_error":error.details,"retry":retry});
                        return Err(error);
                    }
                }
            }
            return Ok(envelope);
        }
        _ => {}
    }
    let cwd =
        std::env::current_dir().map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
    let local = config::local::load_user_config(args.preferences.as_deref(), args.no_user_config)?;
    if let Command::Inspect {
        path,
        max_bytes,
        start,
        end,
    } = &args.command
    {
        if args.stage {
            return Err(usage("physical inspection cannot be staged"));
        }
        let (root, identity, origin, requested) = if let Some(explicit) = &args.wiki {
            let root = crate::vault::VaultRoot::for_initialization(explicit)?;
            let identity = crate::vault::discovery::identify(&root).ok();
            (root, identity, "explicit", explicit.clone())
        } else {
            let selected = crate::vault::discovery::select(None, &cwd, local.default_wiki())?;
            let origin = match selected.origin {
                crate::vault::discovery::SelectionOrigin::Explicit => "explicit",
                crate::vault::discovery::SelectionOrigin::Ancestor => "ancestor",
                crate::vault::discovery::SelectionOrigin::Default => "default",
            };
            (
                selected.root,
                Some(selected.wiki_id),
                origin,
                selected.requested_path,
            )
        };
        if args
            .expect_wiki_id
            .as_ref()
            .is_some_and(|expected| Some(expected) != identity.as_ref())
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "physical inspection could not verify --expect-wiki-id",
            ));
        }
        let mut envelope = Envelope::success(command, Value::Null);
        envelope.meta.wiki_id = identity.as_ref().map(ToString::to_string);
        envelope.meta.selection = Some(
            json!({"schema_version":"lwiki.selection.v1","root":root.path(),"wiki_id":identity,"origin":origin,"requested_path":requested,"configuration":local.summary(),"preference_origins":{}}),
        );
        if args.dry_run {
            envelope.data = json!({"dry_run":true,"path":path,"max_bytes":max_bytes,"start":start,"end":end,"target_bytes_inspected":false});
            return Ok(envelope);
        }
        let range = match (start, end) {
            (Some(start), Some(end)) => Some(ByteSpan::new(*start, *end)?),
            (None, None) => None,
            _ => return Err(usage("inspect --start and --end must be supplied together")),
        };
        let inspected = crate::app::literal::inspect_literal(
            &VaultFs::new(root),
            crate::app::literal::LiteralInspectRequest {
                path: path.clone(),
                range,
                max_bytes: *max_bytes,
            },
        )?;
        envelope.meta.partial = inspected.truncated;
        envelope.meta.freshness = Some("physical_bytes".into());
        envelope.data = value(inspected)?;
        envelope.warnings.push("Physical bytes only; this does not prove committed state, canonical identity, current facts or citation authority.".into());
        return Ok(envelope);
    }
    let selected =
        crate::vault::discovery::select(args.wiki.as_deref(), &cwd, local.default_wiki())?;
    let error_selection = json!({"schema_version":"lwiki.selection.v1","root":selected.root.path(),"wiki_id":selected.wiki_id,"origin":selected.origin,"requested_path":selected.requested_path,"configuration":local.summary(),"preference_origins":{}});
    let result = execute_selected(args, &local, &selected);
    result.map_err(|mut error| {
        if !error.details.is_object() {
            error.details = json!({"context":error.details});
        }
        error.details["wiki_id"] = selected.wiki_id.as_str().into();
        error.details["selection"] = error_selection;
        error
    })
}
fn execute_selected(
    args: &Arguments,
    local: &config::local::LoadedUserConfig,
    selected: &crate::vault::discovery::SelectedWiki,
) -> Result<Envelope> {
    let command = args.command.name();
    crate::vault::discovery::expect_wiki_id(selected, args.expect_wiki_id.as_ref())?;
    let selected_root = selected.root.path().to_path_buf();
    let fs = VaultFs::new(selected.root.clone());
    let binding = ChangeEngine::new(fs.clone())?;
    let read_limit = match &args.command {
        Command::Read { max_bytes, .. } => *max_bytes,
        Command::Source {
            command: SourceCommand::Read { max_bytes, .. },
        } => *max_bytes,
        _ => None,
    };
    let preferences = config::local::resolve_loaded(
        &fs,
        binding.vault_id(),
        &PreferenceOptions {
            offline: args.offline,
            read_max_bytes: read_limit,
            lock_timeout_ms: args.lock_timeout_ms,
            profile: args.profile.clone(),
        },
        local,
    )?;
    let app = OfflineApp::new(
        fs,
        operation_options(args, preferences.offline, preferences.lock_timeout_ms),
    )?;
    let remote_context = super::remote::RemoteContext {
        profile: preferences.profile.as_deref(),
        wiki_id: Some(&selected.wiki_id),
        document: local.document(),
        allow_ambient_provider: local.summary().origin != config::local::UserConfigOrigin::Disabled,
    };
    if app.vault_id() != &selected.wiki_id || binding.vault_id() != &selected.wiki_id {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "wiki identity changed during selection; retry after inspecting WIKI.md",
        ));
    }
    let mut envelope = Envelope::success(command, Value::Null);
    // Reuse legacy reader health; reopening its cache can require WAL writes or
    // discard a successful in-memory reconstruction after cache loss.
    let mut legacy_read_health = None;
    let mut current_read_health = None;
    envelope.meta.selection = Some(
        json!({"schema_version":"lwiki.selection.v1","root":selected_root,"wiki_id":selected.wiki_id,"origin":selected.origin,"requested_path":selected.requested_path,"configuration":local.summary(),"preference_origins":preferences.origins}),
    );
    envelope.meta.wiki_id = Some(app.vault_id().to_string());
    envelope.warnings = preferences.warnings;
    match &args.command {
        Command::Wiki {
            command: WikiCommand::Show,
        } => {
            envelope.data = envelope.meta.selection.clone().expect("selection bound");
            envelope.data["title"] = json!(super::human_status::wiki_title(&app)?);
        }
        Command::Edit {
            path,
            patch,
            if_match,
        } => {
            let patch = crate::changes::prepare::strict_json(
                &super::extraction::bounded_json_input(patch, "literal edit patch")?,
            )?;
            mutation(
                &mut envelope,
                app.edit_literal(crate::app::literal::LiteralEditRequest {
                    path: path.clone(),
                    if_match: if_match.clone(),
                    patch,
                })?,
            )?;
        }
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
            if request.max_bytes == 0 || request.max_bytes > MAX_INPUT_BYTES {
                return Err(usage("read limit must be 1..=16 MiB"));
            }
            if args.dry_run {
                envelope.data = json!({
                    "dry_run": true,
                    "selector": {"id": selector.id, "path": selector.path},
                    "requested_range": match from {
                        Some(start) => json!({"start": start, "end": null}),
                        None => value(request.range)?,
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
                let reader = if *no_sync {
                    catalog.query_snapshot(QueryReadLimits::default())?
                } else {
                    normalized_current_reader(&catalog, &mut envelope)?
                };
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
                            error.hint = Some(if reader.is_current_observation() {
                                "Checking this note reached a size or time limit. Use inspect --path for a known file when physical bytes are sufficient.".into()
                            } else {
                                "Selected read verification exceeded its bounded budget. Context supports --verification-* controls; read --no-sync returns explicitly unverified cached bytes when those are sufficient.".into()
                            });
                        }
                        error
                    };
                    let requested_budget = retrieval::VerificationBudget::default();
                    let proof_budget = if reader.is_current_observation() {
                        retrieval::verification::current_proof_budget(&reader, &requested_budget)?
                    } else {
                        requested_budget
                    };
                    let mut proof = retrieval::selected_documents::authenticate(
                        &catalog,
                        &reader,
                        std::slice::from_ref(&outcome.path),
                        &proof_budget,
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
                    let verification = if reader.is_current_observation() {
                        retrieval::verification::verify_current_observation(
                            &catalog, &reader, &proof,
                        )?
                    } else {
                        retrieval::indexed_documents::verification(&reader)?
                    };
                    result_metadata(&mut envelope.meta, reader.snapshot(), &verification);
                    envelope.meta.index_generation = reader.persistent_generation();
                    if !reader.is_current_observation() {
                        envelope.warnings.push("Read verifies selected document dependencies against this discovery generation; global membership and identity uniqueness are not verified. Use index sync to discover external edits.".into());
                    }
                }
                if reader.is_current_observation() {
                    current_read_health = current_observation_health(&reader, &mut envelope.meta);
                } else {
                    legacy_read_health = Some(Some(reader.known_health()?));
                    reader.verify_operations(&catalog)?;
                }
                if *no_sync && !args.dry_run {
                    envelope.meta.index_generation = Some(reader.snapshot().generation);
                    envelope.meta.freshness = Some("index_snapshot".into());
                }
                outcome
            } else {
                let (reader, read_budget) = reader(&app, *no_sync)?;
                snapshot_metadata(&mut envelope.meta, &reader);
                legacy_read_health = Some(held_legacy_read_health(&reader, &mut envelope));
                if *no_sync {
                    cached_legacy_read(&catalog, &reader, request, from)?
                } else {
                    let mut outcome = cached_read(&reader, request, from)?;
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
                    read_budget.check()?;
                    let bytes = crate::vault::fs::read_regular_bounded(
                        app.fs().root(),
                        &outcome.path,
                        false,
                        read_budget.remaining().0,
                        "verified read",
                    )?
                    .ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::FreshnessConflict,
                            "selected read path disappeared after snapshot verification",
                        )
                    })?;
                    if Blake3Hash::digest(&bytes) != outcome.hash {
                        return Err(WikiError::new(
                            ErrorCode::FreshnessConflict,
                            "selected read bytes changed after snapshot verification",
                        ));
                    }
                    let note = parse_note(&bytes);
                    let body = if document.owner_revision.is_none() {
                        note.body()
                    } else {
                        bytes.as_slice()
                    };
                    std::str::from_utf8(body)
                        .map_err(|_| WikiError::invalid("requested body is not UTF-8"))?;
                    // Includes the Source/head/ownership dependencies used to
                    // derive this document, plus complete canonical membership.
                    read_budget.charge_observation(bytes.len(), 1, 0)?;
                    catalog.recheck_read_snapshot(&reader, &read_budget)?;
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
            command: SourceCommand::Capture { paths, files_from },
        } => {
            let outcome = app.capture_sources(crate::app::SourceCaptureRequest {
                paths: paths.clone(),
                files_from: files_from.clone(),
            })?;
            let owners = outcome.changed_owners.clone();
            envelope.meta.partial = outcome.partial;
            envelope.ok = !outcome.partial;
            if outcome.partial {
                envelope.error = Some(error_output(WikiError::new(
                    ErrorCode::ContentConflict,
                    "Some files could not be captured. Successful captures were saved; check the listed files and repeat the command.",
                )));
            }
            envelope.data = value(outcome)?;
            if !args.dry_run && !owners.is_empty() {
                let automatic = super::automatic_embeddings::prepare(
                    &app,
                    &remote_context,
                    crate::app::embeddings::PreparationTrigger::AfterWrite { owners },
                );
                super::automatic_embeddings::attach(&mut envelope, automatic, false);
            }
        }
        Command::Source {
            command:
                SourceCommand::Read {
                    id,
                    revision,
                    start,
                    end,
                    ..
                },
        } => {
            if args.stage {
                return Err(usage(
                    "source read does not create a change; remove --stage",
                ));
            }
            let (range, from) = match (start, end) {
                (Some(start), Some(end)) => (Some(ByteSpan::new(*start, *end)?), None),
                (Some(start), None) => (None, Some(*start)),
                (None, None) => (None, None),
                _ => return Err(usage("--end requires --start")),
            };
            if args.dry_run {
                envelope.data = json!({"dry_run":true,"source_id":id,"revision_id":revision,
                    "start":start,"end":end,"max_bytes":preferences.read_max_bytes,
                    "text_read":false,"citation":null});
                return Ok(envelope);
            }
            let outcome = app.read_source(crate::app::SourceReadRequest {
                source_id: id.clone(),
                revision_id: revision.clone(),
                range,
                from,
                max_bytes: preferences.read_max_bytes,
            })?;
            result_metadata(&mut envelope.meta, &outcome.snapshot, &outcome.verification);
            envelope.meta.index_generation = outcome.persistent_generation;
            envelope.meta.partial = outcome.read.truncated;
            if let Some(reason) = &outcome.current_observation_unavailable {
                envelope.warnings.push(format!(
                    "Using the saved source list after checking this source: {reason}"
                ));
            }
            if let Some(next) = outcome.read.continuation {
                envelope.warnings.push(format!("More text is available. Continue with source read {id} --revision {} --start {} --max-bytes {}.", outcome.revision_id, next.start(), preferences.read_max_bytes));
            }
            envelope.data = value(outcome)?;
            if envelope.meta.index_generation.is_none() {
                envelope.data["snapshot"]["generation"] = Value::Null;
            }
            return Ok(envelope);
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
                    path,
                    input_if_match,
                    file,
                    title,
                    media_type,
                },
        } => {
            let request = match input_if_match {
                Some(hash) => crate::sources::path_refresh::guarded_capture_file(
                    file,
                    title.as_deref(),
                    media_type.clone(),
                    hash,
                )?,
                None => capture(file, title.as_deref(), media_type.clone())?,
            };
            let (outcome, selection_scope) = match (id, path) {
                (Some(id), None) => (
                    app.source_refresh_with_title(id.clone(), request, title.as_deref())?,
                    None,
                ),
                (None, Some(path)) => {
                    let (outcome, scope) = app.source_refresh_path_with_scope(
                        path.clone(),
                        request,
                        title.as_deref(),
                    )?;
                    (outcome, Some(scope))
                }
                _ => return Err(usage("source refresh requires one ID or --path")),
            };
            source_mutation(
                &mut envelope,
                &app,
                outcome,
                json!({"operation":"refresh", "source_id":id, "path":path,"file":file,"input_if_match":input_if_match,"title":title,"media_type":media_type}),
            )?;
            if let Some(scope) = selection_scope {
                if scope.selection == "published_index_and_selected_dependencies" {
                    envelope.warnings.push("Source selection used the published index and verified the selected dependencies. Current global membership is unknown; run index sync to discover external duplicates or other editor changes.".into());
                }
                envelope.data["selection_verification"] = value(scope)?;
            }
        }
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
            let completed = outcome.report.is_some();
            let preview_inspected = outcome
                .maintenance
                .as_ref()
                .and_then(|metadata| metadata["canonical_scan_performed"].as_bool())
                .unwrap_or(true);
            envelope.data = value(outcome)?;
            let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
            if completed {
                let health =
                    catalog
                        .published_health(Default::default())
                        .map_err(|mut error| {
                            error.details =
                                json!({"sync_outcome":envelope.data,"health_error":error.details});
                            error
                        })?;
                if Some(health.index_generation) != envelope.meta.index_generation {
                    return Err(WikiError::new(
                        ErrorCode::FreshnessConflict,
                        "published index changed before sync health reporting; inspect doctor --diagnostics",
                    ));
                }
                super::health::attach_sync(&mut envelope, &health, true)?;
            } else {
                super::health::attach_preview(&mut envelope, preview_inspected);
            }
        }
        Command::Relations(options) => {
            use super::source_relations::SourceRelationsCommand;
            if args.stage {
                return Err(usage(
                    "relation preparation uses its own resumable Run and cannot be staged",
                ));
            }
            let remote = match &options.command {
                SourceRelationsCommand::Prepare(o) => &o.remote,
                SourceRelationsCommand::Run(o) => &o.remote,
            };
            remote.limits()?;
            let deadline = std::time::Instant::now()
                .checked_add(Duration::from_millis(
                    remote
                        .effective(&remote_context)
                        .deadline_ms
                        .unwrap_or(900_000),
                ))
                .ok_or_else(|| usage("relation deadline exceeds clock range"))?;
            let runtime = remote.runtime(&app, &remote_context, Capability::Generate)?;
            match &options.command {
                SourceRelationsCommand::Prepare(o) => {
                    let prepared = app.prepare_source_relations(
                        &o.source_ids,
                        &runtime,
                        o.max_output_tokens,
                        o.quote,
                        deadline,
                    )?;
                    let bytes = prepared.manifest;
                    let hash = Blake3Hash::digest(&bytes);
                    let text = String::from_utf8(bytes).map_err(|_| {
                        WikiError::new(ErrorCode::Internal, "manifest encoding differs")
                    })?;
                    envelope.data = json!({"manifest_json":text,"manifest_hash":hash,
                    "sources":o.source_ids.len(),"provider_calls":0,"persisted":false});
                    if let Some(quote) = prepared.quote {
                        envelope.data["quote"] = value(quote)?;
                        envelope.warnings.push("Quotes describe one fresh attempt per Source and do not reserve money or grant send authority. A serial lifetime cap can stop acquisition before every Source completes.".into());
                    }
                    envelope.warnings.push("Save manifest_json verbatim and supply that file with the same --run identity for every resume. Preparation does not send provider requests.".into());
                    if o.quote {
                        crate::app::source_relation_quote::check_response_size(
                            &envelope, deadline,
                        )?;
                    }
                }
                SourceRelationsCommand::Run(o) => {
                    let bytes = o.manifest_bytes(deadline)?;
                    let report = with_network_activity(
                        Some(&runtime.dispatcher),
                        app.run_source_relation_preparation(
                            &bytes,
                            &o.run,
                            &runtime,
                            deadline,
                            o.max_tasks,
                        ),
                    )?;
                    envelope.meta.network_used = runtime.dispatcher.network_used();
                    envelope.meta.partial = report.pending > 0 || report.failed > 0;
                    envelope.data = value(report)?;
                }
            }
        }
        Command::Embeddings(options) => {
            use super::embeddings::EmbeddingCommand;
            let (settings, remote, probe, sync) = match &options.command {
                EmbeddingCommand::Check(o) => (
                    o.settings.settings(&local.document().embeddings),
                    &o.remote,
                    o.probe,
                    false,
                ),
                EmbeddingCommand::Sync(o) => (
                    o.settings.settings(&local.document().embeddings),
                    &o.remote,
                    false,
                    true,
                ),
            };
            remote.limits()?;
            if args.stage {
                return Err(usage("embedding cache operations cannot be staged"));
            }
            let runtime = if !args.dry_run
                && !app.options().offline
                && (sync || probe || remote_context.profile.is_some())
            {
                Some(remote.runtime(&app, &remote_context, Capability::Embed)?)
            } else {
                None
            };
            let embedding = runtime.as_ref().map(embedding_runtime);
            if args.dry_run {
                settings.validate()?;
                envelope.data = json!({"settings":settings,"dry_run":true,"probe":probe,
                "sync":sync,"cache_state_unknown":true,"network_used":false});
                super::human_status::attach_provider_summary(
                    &mut envelope,
                    remote,
                    &remote_context,
                );
                envelope
                    .warnings
                    .push("embedding coverage is unknown during dry-run".into());
                if sync {
                    envelope.data["payload_preview"] =
                        match super::automatic_embeddings::sync_payload_preview(
                            &app,
                            remote,
                            &remote_context,
                            &settings,
                        ) {
                            Ok(preview) => preview,
                            Err(error) => json!({"state":"unavailable","reason":error.code,
                            "message":"The bounded request preview is unavailable. Review the selected model with lwiki config show.",
                            "counts_complete":false,"actual_pending_inputs":null,"cache_state":"unknown",
                            "network_used":false,"credentials_resolved":false}),
                        };
                }
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
            if search.mode.is_none() && !args.dry_run {
                let mut lexical = false;
                let (hits, automatic) = super::automatic_embeddings::lookup(
                    &app,
                    &remote_context,
                    &search.remote,
                    &search.query,
                    search.no_sync,
                    |runtime| {
                        if command.verify_selected {
                            app.semantic_search_selected(&search.query, &plan, Some(runtime), false)
                        } else {
                            let graph = search.graph.is_some().then(|| crate::graph::GraphPlan {
                                filters: plan.filters.clone(),
                                ..Default::default()
                            });
                            app.semantic_search(
                                &search.query,
                                &plan,
                                Some(runtime),
                                search.no_sync,
                                false,
                                graph.as_ref(),
                            )
                        }
                    },
                    || {
                        lexical = true;
                        automatic_local_search(
                            &app,
                            command,
                            &plan,
                            &mut envelope,
                            &mut legacy_read_health,
                            &mut current_read_health,
                        )
                    },
                )?;
                result_metadata(&mut envelope.meta, &hits.snapshot, &hits.verification);
                envelope.meta.network_used = hits.network_used;
                envelope.meta.partial = hits.truncated;
                envelope.warnings.extend(hits.warnings.iter().cloned());
                envelope.data = value(hits)?;
                super::automatic_embeddings::attach(&mut envelope, automatic, lexical);
                finish_read_health(
                    args,
                    &app,
                    &mut envelope,
                    legacy_read_health,
                    current_read_health,
                )?;
                return Ok(envelope);
            }
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
                    if matches!(
                        plan.mode,
                        retrieval::SearchMode::Semantic | retrieval::SearchMode::Hybrid
                    ) {
                        super::human_status::attach_provider_summary(
                            &mut envelope,
                            &search.remote,
                            &remote_context,
                        );
                    }
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
                    let hits = if plan.mode == retrieval::SearchMode::Lexical {
                    retrieval::selected_search::search_ordinary_lexical(&catalog, &search.query, &plan,
                        &retrieval::VerificationBudget::default())
                } else if plan.mode == retrieval::SearchMode::Literal {
                    retrieval::selected_search::search(&catalog, &search.query, &plan,
                        &retrieval::VerificationBudget::default())
                } else {
                    search.remote.limits()?;
                    with_embedding_runtime(&app, &remote_context, &search.remote, search.lexical_fallback,
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
                attach_read_health(args, &app, &mut envelope)?;
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
                    if matches!(
                        plan.mode,
                        retrieval::SearchMode::Semantic | retrieval::SearchMode::Hybrid
                    ) {
                        super::human_status::attach_provider_summary(
                            &mut envelope,
                            &search.remote,
                            &remote_context,
                        );
                    }
                    envelope.warnings.push("Search preview validates the request only; index access, cache availability and evidence verification are unperformed.".into());
                    return Ok(envelope);
                }
                catalog.guard_query()?;
                let hits = if matches!(
                    plan.mode,
                    retrieval::SearchMode::Literal | retrieval::SearchMode::Lexical
                ) {
                    let reader = if !args.dry_run && !search.no_sync {
                        normalized_current_reader(&catalog, &mut envelope)?
                    } else {
                        catalog.cached_query_snapshot(QueryReadLimits::default())?
                    };
                    if !reader.normalized_layout() {
                        return Err(WikiError::new(
                            ErrorCode::IndexCorrupt,
                            "normalized search selected another catalog layout",
                        ));
                    }
                    let hits = if reader.is_current_observation() {
                        retrieval::selected_search::search_current_observation(
                            &catalog,
                            &reader,
                            &search.query,
                            &plan,
                            &retrieval::VerificationBudget::default(),
                        )?
                    } else if plan.mode == retrieval::SearchMode::Lexical {
                        retrieval::lexical::search_ordinary_lexical_catalog(
                            &reader,
                            &search.query,
                            &plan,
                        )?
                    } else {
                        retrieval::lexical::search_catalog(&reader, &search.query, &plan)?
                    };
                    if reader.is_current_observation() {
                        current_read_health =
                            current_observation_health(&reader, &mut envelope.meta);
                    } else {
                        legacy_read_health = Some(Some(reader.known_health()?));
                        reader.verify_operations(&catalog)?;
                    }
                    hits
                } else {
                    with_embedding_runtime(
                        &app,
                        &remote_context,
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
                if current_read_health.is_some() {
                    envelope.meta.index_generation = None;
                }
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
                if matches!(
                    plan.mode,
                    retrieval::SearchMode::Semantic | retrieval::SearchMode::Hybrid
                ) {
                    super::human_status::attach_provider_summary(
                        &mut envelope,
                        &search.remote,
                        &remote_context,
                    );
                }
                envelope
                    .warnings
                    .push("search results and index freshness are unknown during dry-run".into());
            } else {
                let mut local_search_generation = None;
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
                        &remote_context,
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
                    let (reader, read_budget) = reader(&app, search.no_sync)?;
                    legacy_read_health = Some(held_legacy_read_health(&reader, &mut envelope));
                    let hits = if plan.mode == retrieval::SearchMode::Lexical {
                        retrieval::lexical::search_ordinary_lexical(&reader, &search.query, &plan)?
                    } else {
                        retrieval::search(&reader, &search.query, &plan)?
                    };
                    if !search.no_sync {
                        catalog.recheck_read_snapshot(&reader, &read_budget)?;
                    }
                    local_search_generation = Some(reader.persistent_generation());
                    hits
                };
                require_legacy_query_still_selected(&catalog)?;
                result_metadata(&mut envelope.meta, &hits.snapshot, &hits.verification);
                if let Some(generation) = local_search_generation {
                    envelope.meta.index_generation = generation;
                }
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
                        envelope.warnings.push(format!("API preview did not authorize provider work or check whether the requested output allowance {} fits the service cap. No credentials were resolved.", options.max_output_tokens));
                    }
                }
                envelope.data = value(outcome)?;
                if args.dry_run && matches!(options.executor, super::extraction::Executor::Api) {
                    super::human_status::attach_provider_summary(
                        &mut envelope,
                        &options.remote,
                        &remote_context,
                    );
                }
            } else {
                let runtime =
                    options
                        .remote
                        .runtime(&app, &remote_context, Capability::Generate)?;
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
                if plan.seed_mode == crate::graph::GraphSeedMode::Semantic {
                    super::human_status::attach_provider_summary(
                        &mut envelope,
                        &options.remote,
                        &remote_context,
                    );
                }
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
                let mut local_graph_generation = None;
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
                        &remote_context,
                        &options.remote,
                        options.lexical_fallback,
                        |runtime, fallback| {
                            app.semantic_graph(query, &plan, runtime, options.no_sync, fallback)
                        },
                    )?
                } else {
                    let (reader, read_budget) = reader(&app, options.no_sync)?;
                    let result = match command {
                        GraphCommand::Query { query, .. } => query::query(&reader, query, &plan)?,
                        GraphCommand::Neighbors { id, .. } => query::neighbors(&reader, id, &plan)?,
                        _ => unreachable!("extraction handled above"),
                    };
                    if !options.no_sync {
                        catalog.recheck_read_snapshot(&reader, &read_budget)?;
                    }
                    local_graph_generation = Some(reader.persistent_generation());
                    result
                };
                result_metadata(&mut envelope.meta, &result.snapshot, &result.verification);
                if let Some(generation) = local_graph_generation {
                    envelope.meta.index_generation = generation;
                }
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
            let automatic_default = context.search.mode.is_none()
                && request.target == retrieval::ContextTarget::Documents
                && request.scope != retrieval::ContextScope::IndexedEvidence
                && !request.documents.filters.include_historical
                && !context.prepare_selection
                && !context.prepare_original_selection
                && context.selection.is_none()
                && !context.discover_originals
                && context.selection_original_paths.is_empty()
                && context.selection_input_max_bytes.is_none()
                && !context.experimental_source_relations
                && !context.experimental_hybrid_lexical_evidence;
            if context.search.mode.is_none() && !automatic_default {
                request.documents.mode = retrieval::SearchMode::Lexical;
            }
            if automatic_default && context.search.no_sync && context.scope.is_none() {
                request.scope = if normalized_selected {
                    retrieval::ContextScope::IndexedDocuments
                } else {
                    retrieval::ContextScope::Snapshot
                };
            }
            let current_local_documents = normalized_selected
                && request.target == retrieval::ContextTarget::Documents
                && request.graph.is_none()
                && matches!(
                    request.documents.mode,
                    retrieval::SearchMode::Literal | retrieval::SearchMode::Lexical
                )
                && !request.documents.filters.include_historical
                && !context.search.no_sync
                && !context.prepare_selection
                && !context.prepare_original_selection
                && context.selection.is_none()
                && !context.discover_originals
                && context.selection_original_paths.is_empty()
                && context.selection_input_max_bytes.is_none()
                && !context.experimental_source_relations
                && !context.experimental_hybrid_lexical_evidence;
            if context.scope.is_none() && normalized_selected {
                request.scope = if current_local_documents {
                    retrieval::ContextScope::Current
                } else {
                    retrieval::ContextScope::IndexedDocuments
                };
            }
            let request = retrieval::context::validate_request(&context.search.query, &request)?;
            if request.scope == retrieval::ContextScope::IndexedEvidence
                && (context.prepare_selection
                    || context.prepare_original_selection
                    || context.selection.is_some())
            {
                return Err(usage(
                    "indexed-evidence does not support host selection; use lexical indexed-documents",
                ));
            }
            let selection = context.selection_action()?;
            retrieval::context::validate_selection_action(&request, &selection)?;
            retrieval::context::validate_experimental_evidence(
                &request,
                &selection,
                context.experimental_hybrid_lexical_evidence,
            )?;
            retrieval::context::validate_source_relations(
                &request,
                &retrieval::ContextOptions {
                    experimental_source_relations: context.experimental_source_relations,
                    selection: selection.clone(),
                    ..Default::default()
                },
            )?;
            if context.experimental_hybrid_lexical_evidence && context.search.lexical_fallback {
                return Err(usage(
                    "experimental hybrid lexical evidence cannot use lexical discovery fallback",
                ));
            }
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
            if automatic_default && !args.dry_run {
                let mut lexical = false;
                let options = retrieval::ContextOptions {
                    selection: selection.clone(),
                    ..Default::default()
                };
                let (result, automatic) = super::automatic_embeddings::lookup(
                    &app,
                    &remote_context,
                    &context.search.remote,
                    &context.search.query,
                    context.search.no_sync,
                    |runtime| {
                        app.semantic_context_with_options(
                            &context.search.query,
                            &request,
                            Some(runtime),
                            context.search.no_sync,
                            false,
                            &options,
                        )
                    },
                    || {
                        lexical = true;
                        automatic_local_context(
                            &app,
                            context,
                            &request,
                            &options,
                            &mut envelope,
                            &mut legacy_read_health,
                            &mut current_read_health,
                        )
                    },
                )?;
                result_metadata(&mut envelope.meta, result.snapshot(), result.verification());
                envelope.meta.network_used = result.network_used;
                envelope.meta.partial = result.truncated();
                envelope.warnings.extend(result.warnings().iter().cloned());
                envelope.data = value(result)?;
                super::automatic_embeddings::attach(&mut envelope, automatic, lexical);
                finish_read_health(
                    args,
                    &app,
                    &mut envelope,
                    legacy_read_health,
                    current_read_health,
                )?;
                return Ok(envelope);
            }
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
                && !(current_local_documents && request.scope == retrieval::ContextScope::Current)
            {
                return Err(WikiError::new(
                    ErrorCode::CapabilityUnavailable,
                    "normalized current context supports local literal/lexical documents without graph or host selection; use indexed-documents for existing selected/semantic routes, or indexed-evidence for captured sources",
                ));
            }
            let cached_dry_run =
                args.dry_run && context.search.no_sync && cached_documents && normalized_selected;
            if args.dry_run && !cached_dry_run {
                envelope.data = json!({"query": context.search.query, "request": request, "dry_run": true, "cache_state_unknown": true, "context": null});
                if matches!(
                    request.documents.mode,
                    retrieval::SearchMode::Semantic | retrieval::SearchMode::Hybrid
                ) || request
                    .graph
                    .as_ref()
                    .is_some_and(|graph| graph.seed_mode == crate::graph::GraphSeedMode::Semantic)
                {
                    super::human_status::attach_provider_summary(
                        &mut envelope,
                        &context.search.remote,
                        &remote_context,
                    );
                }
                envelope
                    .warnings
                    .push("context and freshness are unknown during dry-run".into());
            } else {
                let mut local_generation = None;
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
                        &remote_context,
                        &context.search.remote,
                        context.search.lexical_fallback,
                        |runtime, fallback| {
                            app.semantic_context_with_options(
                                &context.search.query,
                                &request,
                                runtime,
                                context.search.no_sync,
                                fallback,
                                &retrieval::ContextOptions {
                                    selection: selection.clone(),
                                    experimental_hybrid_lexical_evidence: context
                                        .experimental_hybrid_lexical_evidence,
                                    ..Default::default()
                                },
                            )
                        },
                    )?
                } else {
                    let options = retrieval::ContextOptions {
                        selection: selection.clone(),
                        experimental_source_relations: context.experimental_source_relations,
                        ..Default::default()
                    };
                    if current_local_documents && request.scope == retrieval::ContextScope::Current
                    {
                        let mut scoped_request = request.clone();
                        let reader = match catalog.current_query_snapshot(QueryReadLimits::default())? {
                            crate::catalog::transient_read::CurrentQueryAttempt::Ready(reader) => *reader,
                            crate::catalog::transient_read::CurrentQueryAttempt::BoundedUnavailable(reason)
                                if context.scope.is_none() => {
                                    scoped_request.scope = retrieval::ContextScope::IndexedDocuments;
                                    published_after_observation_bound(&catalog, reason, &mut envelope)?
                                }
                            crate::catalog::transient_read::CurrentQueryAttempt::BoundedUnavailable(reason) => return Err(reason),
                        };
                        let result = retrieval::indexed_documents::context_with_reader(
                            &catalog,
                            &reader,
                            &context.search.query,
                            &scoped_request,
                            &options,
                        )?;
                        local_generation = Some(reader.persistent_generation());
                        if reader.is_current_observation() {
                            current_read_health =
                                current_observation_health(&reader, &mut envelope.meta);
                        } else {
                            legacy_read_health = Some(Some(reader.known_health()?));
                            reader.verify_operations(&catalog)?;
                        }
                        result
                    } else if !normalized_selected
                        && !matches!(
                            request.scope,
                            retrieval::ContextScope::IndexedEvidence
                                | retrieval::ContextScope::IndexedDocuments
                        )
                    {
                        let (reader, read_budget) =
                            reader(&app, request.scope == retrieval::ContextScope::Snapshot)?;
                        legacy_read_health = Some(held_legacy_read_health(&reader, &mut envelope));
                        local_generation = Some(reader.persistent_generation());
                        retrieval::verification::context_with_reader(
                            &catalog,
                            &reader,
                            &read_budget,
                            &context.search.query,
                            &request,
                            &options,
                        )?
                    } else {
                        retrieval::verification::context_with_options(
                            &catalog,
                            None,
                            &context.search.query,
                            &request,
                            &options,
                        )?
                    }
                };
                if context.experimental_source_relations {
                    envelope.meta.evidence_strategy = Some("experimental_source_relations".into());
                }
                if context.experimental_hybrid_lexical_evidence {
                    envelope.meta.evidence_strategy =
                        Some("experimental_hybrid_owner_lexical_evidence".into());
                }
                envelope.meta.network_used = result.network_used;
                envelope.meta.index_generation = Some(result.snapshot().generation);
                if let Some(generation) = local_generation {
                    envelope.meta.index_generation = generation;
                }
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
            diagnostics,
            diagnostic_limit,
            diagnostic_cursor,
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
            let mut provider = super::human_status::provider_summary(remote, &remote_context);
            if provider["embedding"].is_object() {
                provider["embedding"]["automatic_embedding_consent"] =
                    super::human_status::automatic_consent(&app, remote, &remote_context);
            }
            let mut doctor = app.doctor()?;
            if *probe {
                let service_role = role.service_role();
                remote.limits()?;
                if args.dry_run {
                    envelope.data = json!({"doctor":doctor,"probe":{"dry_run":true,"role":service_role,"network_used":false,"remote_work":"unknown"}});
                } else {
                    let result = (|| {
                        let runtime =
                            remote.runtime(&app, &remote_context, service_role.capability())?;
                        if *extraction_schema {
                            app.probe_extraction_schema(&runtime)
                        } else {
                            app.probe_provider(&runtime, service_role)
                        }
                    })()
                    .map_err(|mut error: WikiError| {
                        if !error.details.is_object() {
                            error.details = json!({"cause":error.details});
                        }
                        error.details["provider"] = provider.clone();
                        error.details["probe_role"] = json!(service_role);
                        super::probe_diagnostic::decorate(
                            &mut error,
                            args,
                            local,
                            selected,
                            remote,
                            &remote_context,
                            *extraction_schema,
                        );
                        error
                    })?;
                    doctor.provider_probe_performed = result.network_used;
                    envelope.meta.network_used = result.network_used;
                    envelope.data = json!({"doctor":doctor,"probe":result});
                }
            } else {
                envelope.data = value(doctor)?;
            }
            envelope.data["provider"] = provider;
            if *diagnostics {
                if args.dry_run {
                    super::health::attach_preview(&mut envelope, false);
                } else {
                    let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
                    let health = catalog.published_health(
                        crate::catalog::health::DiagnosticPageRequest {
                            limit: diagnostic_limit.unwrap_or(50),
                            cursor: diagnostic_cursor.clone(),
                            max_bytes: 65_536,
                        },
                    )?;
                    super::health::attach_sync(&mut envelope, &health, false)?;
                }
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
            ChangesCommand::Show {
                id,
                operation,
                summary,
            } => {
                if *summary {
                    envelope.data = value(app.changes_summary(id.clone())?)?;
                    envelope.warnings.push("Metadata summary only: retained payload availability and integrity, and current target freshness are not checked. This does not establish apply readiness or undo availability. Full show or --operation verifies retained bytes separately; apply still enforces its guards.".into());
                } else if let Some(operation) = operation {
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
                let outcome = app.changes_apply(id.clone())?;
                let preview = app.options().dry_run
                    && !outcome.reused
                    && Catalog::new(app.fs().clone(), app.vault_id().clone())
                        .operation_state()?
                        .is_some();
                mutation(&mut envelope, outcome)?;
                if preview {
                    envelope.data["dry_run"] = true.into();
                    envelope.data["plan_complete"] = false.into();
                    envelope.data["validation"] = json!({
                        "retained_intent_checked": true,
                        "retained_file_guards_checked": true,
                        "indexed_admission_checked": false
                    });
                    envelope.warnings.push("Apply preview checks retained intent and file guards without opening the index. Index layout, ownership and publication readiness remain unchecked; applying performs those checks.".into());
                }
            }
            ChangesCommand::Abort { id } => {
                mutation(&mut envelope, app.changes_abort(id.clone())?)?
            }
            ChangesCommand::Rollback { id } => {
                mutation(&mut envelope, app.changes_rollback(id.clone())?)?;
                if app.options().dry_run
                    && Catalog::new(app.fs().clone(), app.vault_id().clone())
                        .operation_state()?
                        .is_some()
                {
                    page_preview_metadata(&mut envelope, true);
                }
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
            let retained_currency = if remote.max_cost.is_some() && remote.currency.is_none() {
                let ledger = crate::jobs::JobLedger::new(
                    app.fs().clone(),
                    app.vault_id().clone(),
                    run.clone(),
                    crate::app::remote::native_job_options(
                        crate::jobs::ExecutionPolicy {
                            offline: true,
                            dry_run: args.dry_run,
                            retry_uncertain: false,
                        },
                        app.options().lock_timeout_ms,
                    ),
                )?;
                ledger
                    .inspect()?
                    .effective_limits
                    .max_cost
                    .map(|cost| cost.currency().as_str().to_owned())
            } else {
                None
            };
            let requested = remote.requested_limits_with_currency(
                retained_currency
                    .as_deref()
                    .or(local.document().remote.currency.as_deref()),
            )?;
            envelope.data = value(app.amend_provider_job_overrides(run, &requested, reason)?)?;
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
        | Command::Setup(_)
        | Command::Config { .. }
        | Command::Schema { .. }
        | Command::Init { .. }
        | Command::Inspect { .. }
        | Command::Wiki {
            command: WikiCommand::Default { .. },
        }
        | Command::Skill { .. } => unreachable!(),
    }
    if !args.dry_run && !args.stage && envelope.ok {
        let owners = super::automatic_embeddings::committed_owners(&app, &envelope.data);
        if !owners.is_empty() {
            let automatic = super::automatic_embeddings::prepare(
                &app,
                &remote_context,
                crate::app::embeddings::PreparationTrigger::AfterWrite { owners },
            );
            super::automatic_embeddings::attach(&mut envelope, automatic, false);
        }
    }
    if args.dry_run {
        let search = match &args.command {
            Command::Search(command) => Some(&command.search),
            Command::Context(context) => Some(&context.search),
            _ => None,
        };
        if let Some(search) = search.filter(|search| search.mode.is_none()) {
            envelope.data["automatic_embeddings"] = super::automatic_embeddings::preview(
                false,
                crate::app::embeddings::PreparationTrigger::Lookup {
                    query: search.query.clone(),
                },
                local.document(),
            );
        }
    }
    finish_read_health(
        args,
        &app,
        &mut envelope,
        legacy_read_health,
        current_read_health,
    )?;
    Ok(envelope)
}

fn finish_read_health(
    args: &Arguments,
    app: &OfflineApp,
    envelope: &mut Envelope,
    legacy_read_health: Option<Option<crate::catalog::health::KnownHealth>>,
    current_read_health: Option<Value>,
) -> Result<()> {
    if let Some(health) = current_read_health {
        // Result metadata may have copied the transient SQL generation after
        // the reader's health was captured. It is never a published generation.
        envelope.meta.index_generation = None;
        // The private generation zero binds SQL/cursors, never publication.
        if envelope.data.get("snapshot").is_some() {
            envelope.data["snapshot"]["generation"] = Value::Null;
        }
        envelope.data["health"] = health;
    } else if let Some(health) = legacy_read_health {
        if let Some(health) = health {
            super::health::attach_known_read(envelope, &health)?;
        } else if envelope.data.get("snapshot").is_some() {
            envelope.data["snapshot"]["generation"] = Value::Null;
            envelope.meta.index_generation = None;
        }
    } else {
        attach_read_health(args, app, envelope)?;
    }
    Ok(())
}

fn automatic_local_search(
    app: &OfflineApp,
    command: &SearchCommandArguments,
    requested: &retrieval::QueryPlan,
    envelope: &mut Envelope,
    legacy_health: &mut Option<Option<crate::catalog::health::KnownHealth>>,
    current_health: &mut Option<Value>,
) -> Result<retrieval::HitSet> {
    let search = &command.search;
    let mut plan = requested.clone();
    plan.mode = retrieval::SearchMode::Lexical;
    let catalog = Catalog::with_options(
        app.fs().clone(),
        app.vault_id().clone(),
        CatalogOptions {
            busy_timeout_ms: app.options().lock_timeout_ms,
            fault: None,
        },
    );
    if command.verify_selected {
        return retrieval::selected_search::search_ordinary_lexical(
            &catalog,
            &search.query,
            &plan,
            &retrieval::VerificationBudget::default(),
        );
    }
    if catalog.operation_state()?.is_some() {
        catalog.guard_query()?;
        let reader = if search.no_sync {
            catalog.cached_query_snapshot(QueryReadLimits::default())?
        } else {
            normalized_current_reader(&catalog, envelope)?
        };
        let hits = if reader.is_current_observation() {
            retrieval::selected_search::search_current_observation(
                &catalog,
                &reader,
                &search.query,
                &plan,
                &retrieval::VerificationBudget::default(),
            )?
        } else {
            retrieval::lexical::search_ordinary_lexical_catalog(&reader, &search.query, &plan)?
        };
        if reader.is_current_observation() {
            *current_health = current_observation_health(&reader, &mut envelope.meta);
        } else {
            *legacy_health = Some(Some(reader.known_health()?));
            reader.verify_operations(&catalog)?;
        }
        Ok(hits)
    } else {
        let (reader, budget) = reader(app, search.no_sync)?;
        *legacy_health = Some(held_legacy_read_health(&reader, envelope));
        let hits = retrieval::lexical::search_ordinary_lexical(&reader, &search.query, &plan)?;
        if !search.no_sync {
            catalog.recheck_read_snapshot(&reader, &budget)?;
        }
        require_legacy_query_still_selected(&catalog)?;
        Ok(hits)
    }
}

fn automatic_local_context(
    app: &OfflineApp,
    context: &super::context::ContextArguments,
    requested: &retrieval::ContextRequest,
    options: &retrieval::ContextOptions,
    envelope: &mut Envelope,
    legacy_health: &mut Option<Option<crate::catalog::health::KnownHealth>>,
    current_health: &mut Option<Value>,
) -> Result<retrieval::ContextResult> {
    let catalog = Catalog::with_options(
        app.fs().clone(),
        app.vault_id().clone(),
        CatalogOptions {
            busy_timeout_ms: app.options().lock_timeout_ms,
            fault: None,
        },
    );
    let normalized = catalog.operation_state()?.is_some();
    let mut request = requested.clone();
    request.documents.mode = retrieval::SearchMode::Lexical;
    if normalized && context.scope.is_none() && !context.search.no_sync {
        request.scope = retrieval::ContextScope::Current;
    }
    let request = retrieval::context::validate_request(&context.search.query, &request)?;
    if normalized && request.scope == retrieval::ContextScope::Current {
        let reader = match catalog.current_query_snapshot(QueryReadLimits::default())? {
            crate::catalog::transient_read::CurrentQueryAttempt::Ready(reader) => *reader,
            crate::catalog::transient_read::CurrentQueryAttempt::BoundedUnavailable(reason)
                if context.scope.is_none() =>
            {
                let mut indexed = request.clone();
                indexed.scope = retrieval::ContextScope::IndexedDocuments;
                let reader = published_after_observation_bound(&catalog, reason, envelope)?;
                let result = retrieval::indexed_documents::context_with_reader(
                    &catalog,
                    &reader,
                    &context.search.query,
                    &indexed,
                    options,
                )?;
                *legacy_health = Some(Some(reader.known_health()?));
                reader.verify_operations(&catalog)?;
                return Ok(result);
            }
            crate::catalog::transient_read::CurrentQueryAttempt::BoundedUnavailable(reason) => {
                return Err(reason);
            }
        };
        let result = retrieval::indexed_documents::context_with_reader(
            &catalog,
            &reader,
            &context.search.query,
            &request,
            options,
        )?;
        *current_health = current_observation_health(&reader, &mut envelope.meta);
        return Ok(result);
    }
    if !normalized
        && !matches!(
            request.scope,
            retrieval::ContextScope::IndexedDocuments | retrieval::ContextScope::IndexedEvidence
        )
    {
        let (reader, budget) = reader(app, request.scope == retrieval::ContextScope::Snapshot)?;
        *legacy_health = Some(held_legacy_read_health(&reader, envelope));
        retrieval::verification::context_with_reader(
            &catalog,
            &reader,
            &budget,
            &context.search.query,
            &request,
            options,
        )
    } else {
        retrieval::verification::context_with_options(
            &catalog,
            None,
            &context.search.query,
            &request,
            options,
        )
    }
}

/// Only admitted observation exhaustion permits published discovery fallback.
fn normalized_current_reader(catalog: &Catalog, envelope: &mut Envelope) -> Result<QuerySnapshot> {
    match catalog.current_query_snapshot(QueryReadLimits::default())? {
        crate::catalog::transient_read::CurrentQueryAttempt::Ready(reader) => Ok(*reader),
        crate::catalog::transient_read::CurrentQueryAttempt::BoundedUnavailable(reason) => {
            published_after_observation_bound(catalog, reason, envelope)
        }
    }
}

fn published_after_observation_bound(
    catalog: &Catalog,
    reason: WikiError,
    envelope: &mut Envelope,
) -> Result<QuerySnapshot> {
    let reader = catalog
        .published_read_only_query_snapshot(QueryReadLimits::default())
        .map_err(|mut error| {
            if !error.details.is_object() {
                error.details = json!({"context":error.details});
            }
            error.details["current_observation_unavailable"] = json!(reason);
            error
        })?;
    envelope.warnings.push("Checking current notes reached a size or time limit. Results use the saved index and may miss new or edited notes. Run lwiki index sync in a session with write access to update it.".into());
    Ok(reader)
}

/// This contains no query or filesystem read after the complete final proof.
fn current_observation_health(reader: &QuerySnapshot, meta: &mut Metadata) -> Option<Value> {
    reader.current_observation().map(|observation| {
        meta.index_generation = None;
        meta.read_view = Some(json!({
            "kind":"current_observation", "fingerprint":observation.fingerprint,
            "observed_at":observation.observed_at, "published_generation":null,
        }));
        json!({
            "schema_version":"lwiki.health.v1", "observation":"current_observation",
            "index_generation":null, "known_issues":observation.known_issues,
            "current_health":"observed", "global_membership_freshness":"current_observation",
            "selected_verification":"verified_snapshot", "structured_eligibility_complete":null,
            "read_work": {
                "canonical":observation.usage(), "construction":observation.build_stats,
                "construction_rows":observation.construction_rows,
                "sql_progress_callbacks":observation.sql_progress_callbacks(),
            },
        })
    })
}

fn held_legacy_read_health(
    reader: &ReaderSnapshot,
    envelope: &mut Envelope,
) -> Option<crate::catalog::health::KnownHealth> {
    let has_issues = !reader.projection().diagnostics.is_empty();
    if let Some(index_generation) = reader.persistent_generation() {
        Some(crate::catalog::health::KnownHealth {
            index_generation,
            has_issues,
        })
    } else {
        if has_issues {
            let paths: std::collections::BTreeSet<_> = reader
                .projection()
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.path.as_str())
                .collect();
            let shown = paths.iter().take(3).copied().collect::<Vec<_>>().join(", ");
            let more = if paths.len() > 3 {
                format!(" and {} more", paths.len() - 3)
            } else {
                String::new()
            };
            envelope.warnings.push(format!(
                "Some notes need attention: {shown}{more}. Run check for details."
            ));
        }
        None
    }
}

fn attach_read_health(args: &Arguments, app: &OfflineApp, envelope: &mut Envelope) -> Result<()> {
    if !args.dry_run
        && matches!(
            &args.command,
            Command::Read { .. } | Command::Search(_) | Command::Context(_)
        )
    {
        let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
        let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
        let health = reader.known_health()?;
        if envelope
            .meta
            .index_generation
            .is_some_and(|generation| generation != health.index_generation)
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "index generation changed before health reporting; retry selected read",
            ));
        }
        reader.verify_operations(&catalog)?;
        super::health::attach_known_read(envelope, &health)?;
    }
    Ok(())
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
    if let Some(source) = envelope.data["allocated_ids"]["source"]
        .as_str()
        .map(str::to_owned)
    {
        let managed = format!("sources/{source}/source.md");
        let input = request.get("file").cloned().unwrap_or(Value::Null);
        let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
        let root = app.fs().root().path().to_string_lossy();
        envelope.data["capture_paths"] = json!({
            "editable_input":input,
            "managed_source":managed,
            "immutable_revisions":format!("sources/{source}/revisions"),
            "sync_recaptures_input":false,
            "refresh_template":format!("lwiki --wiki {} --expect-wiki-id {} source refresh --path {} --file {} --input-if-match HASH", quote(&root), quote(app.vault_id().as_str()), quote(&managed), quote(input.as_str().unwrap_or("INPUT")))
        });
    }
    if app.options().dry_run
        && Catalog::new(app.fs().clone(), app.vault_id().clone())
            .operation_state()?
            .is_some()
    {
        envelope.data["dry_run"] = true.into();
        envelope.data["plan_complete"] = false.into();
        envelope.data["reused"] = Value::Null;
        let input_guard_checked = request
            .get("input_if_match")
            .is_some_and(|value| !value.is_null());
        envelope.data["request"] = request;
        envelope.data["validation"] = json!({
            "explicit_file_guards_checked":input_guard_checked,
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
fn reader(
    app: &OfflineApp,
    no_sync: bool,
) -> Result<(ReaderSnapshot, crate::catalog::scan::ReadScanBudget)> {
    let catalog = Catalog::with_options(
        app.fs().clone(),
        app.vault_id().clone(),
        CatalogOptions {
            busy_timeout_ms: app.options().lock_timeout_ms,
            fault: None,
        },
    );
    let budget =
        crate::catalog::scan::ReadScanBudget::new(crate::catalog::scan::ReadScanLimits::default())?;
    let reader = if no_sync {
        catalog.index_snapshot()?
    } else {
        catalog.read_only_snapshot_with_budget(&budget)?
    };
    Ok((reader, budget))
}
fn snapshot_metadata(meta: &mut Metadata, reader: &ReaderSnapshot) {
    meta.index_generation = reader.persistent_generation();
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
        if let Some(claim) = &claim
            && reader.record(&claim.id)?.is_some()
        {
            return Err(corrupt("adopted cached record is absent from its document"));
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
    let _diagnostic_finish = crate::maintenance_diagnostic::Finish;
    crate::maintenance_diagnostic::observe(crate::maintenance_diagnostic::Phase::Output, || {
        if matches!(format, OutputFormat::Human)
            && !(envelope.ok
                && matches!(
                    envelope.command.as_str(),
                    "read" | "source read" | "context" | "inspect"
                ))
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
    })
}

pub(super) fn terminal_text(text: &str, multiline: bool) -> String {
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
    if format == OutputFormat::Human
        && let Some(status) = envelope.data.get("automatic_embeddings")
    {
        super::automatic_embeddings::present(status, output)?;
    }
    if format == OutputFormat::Human
        && !matches!(
            envelope.command.as_str(),
            "read" | "source read" | "context" | "inspect" | "wiki show"
        )
        && let Some(selection) = &envelope.meta.selection
    {
        writeln!(
            output,
            "Wiki: {} ({}, selected by {})",
            selection["root"].as_str().unwrap_or("unknown"),
            selection["wiki_id"].as_str().unwrap_or("unknown"),
            selection["origin"].as_str().unwrap_or("unknown")
        )?;
    }
    if format == OutputFormat::Human
        && !matches!(
            envelope.command.as_str(),
            "read" | "source read" | "context" | "inspect"
        )
        && let Some(health) = envelope.data.get("health")
    {
        if let Some(completion) = envelope.data["completion"].as_str() {
            let message = match completion {
                "completed" => "The index update finished.",
                "completed_with_issues" => "The index update finished. Some files need attention.",
                "failed" => "The index update did not finish.",
                _ => "The index update status is unavailable.",
            };
            writeln!(output, "{message}")?;
        }
        if let Some(known) = health["known_issues"].as_bool() {
            if known {
                writeln!(
                    output,
                    "Some indexed files need attention. Run {command_prefix} doctor --diagnostics to see them."
                )?;
            }
            if health["membership_freshness"] == "unknown" {
                writeln!(
                    output,
                    "These results use the saved index. Changes in other files may not be included."
                )?;
            }
        }
        if let Some(diagnostics) = health["diagnostics"].as_array() {
            for diagnostic in diagnostics {
                writeln!(
                    output,
                    "{}: {} {}",
                    diagnostic["path"].as_str().unwrap_or_default(),
                    diagnostic["code"].as_str().unwrap_or_default(),
                    diagnostic["message"].as_str().unwrap_or_default()
                )?;
            }
        }
        if let Some(cursor) = health["diagnostic_continuation"].as_str() {
            writeln!(
                output,
                "More diagnostics: {command_prefix} doctor --diagnostics --diagnostic-cursor '{}'",
                cursor.replace('\'', "'\\''")
            )?;
        }
    }
    match format {
        OutputFormat::Json => {
            serde_json::to_writer(&mut *output, envelope)?;
            writeln!(output)
        }
        OutputFormat::Jsonl => {
            serde_json::to_writer(&mut *output, envelope)?;
            writeln!(output)
        }
        OutputFormat::Human
            if envelope.ok
                && envelope.command == "inspect"
                && envelope.data["bytes"].is_array() =>
        {
            let bytes: Vec<u8> =
                serde_json::from_value(envelope.data["bytes"].clone()).map_err(io::Error::other)?;
            output.write_all(&bytes)
        }
        OutputFormat::Human if envelope.command == "setup" => {
            super::setup::present_report(envelope, output)
        }
        OutputFormat::Human if envelope.command == "config set" => {
            super::config_set::present(envelope, output)
        }
        OutputFormat::Human if envelope.ok && envelope.command == "config show" => {
            super::config_show::present(envelope, output)
        }
        OutputFormat::Human
            if envelope.ok
                && matches!(
                    envelope.command.as_str(),
                    "skill install" | "skill status" | "skill update" | "skill remove"
                ) =>
        {
            let data = &envelope.data;
            if let Some(message) = data["message"].as_str() {
                writeln!(output, "{message}")?;
            }
            if let Some(destination) = data["destination"].as_str() {
                writeln!(output, "Agent instructions: {destination}")?;
            }
            if data["dry_run"] == true {
                writeln!(output, "Preview only. No files were changed.")?;
                if let Some(operations) = data["operations"].as_array() {
                    for operation in operations {
                        if let (Some(action), Some(path)) =
                            (operation["operation"].as_str(), operation["path"].as_str())
                        {
                            writeln!(output, "  {action}: {path}")?;
                        }
                    }
                }
            } else if data["changed"] == true {
                writeln!(output, "Restart your agent to use this change.")?;
            }
            if data["shared_link_retained"] == true {
                writeln!(
                    output,
                    "The instructions are still used by another installed agent."
                )?;
            }
            Ok(())
        }
        OutputFormat::Human if envelope.ok && envelope.command == "wiki show" => {
            super::human_status::present_wiki(envelope, output)
        }
        OutputFormat::Human if envelope.command == "doctor" => {
            super::human_status::present_doctor(envelope, output, &command_prefix)
        }
        OutputFormat::Human if envelope.command == "source capture" => {
            super::source_capture::present_capture(envelope, output)
        }
        OutputFormat::Human if envelope.ok && envelope.command == "source read" => {
            super::source_capture::present_read(envelope, output)
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
                } else if error.code == "CONTENT_CONFLICT"
                    && !envelope.command.starts_with("skill ")
                    && envelope.command != "init"
                {
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
        OutputFormat::Human if envelope.data["inspection"] == "metadata_summary" => {
            let summary = &envelope.data;
            let change = summary["prepared"]["change_id"]
                .as_str()
                .unwrap_or_default();
            writeln!(
                output,
                "Change {change}: {}",
                summary["status"].as_str().unwrap_or_default()
            )?;
            writeln!(
                output,
                "Title: {}",
                terminal_text(summary["title"].as_str().unwrap_or_default(), false)
            )?;
            writeln!(
                output,
                "Manifest: {}",
                summary["prepared"]["manifest_hash"]
                    .as_str()
                    .unwrap_or_default()
            )?;
            writeln!(
                output,
                "Note status (diagnostic): {}",
                terminal_text(summary["note_status"].as_str().unwrap_or_default(), false)
            )?;
            writeln!(output, "Operations: {}", summary["operation_count"])?;
            for operation in summary["operations"].as_array().unwrap() {
                writeln!(
                    output,
                    "  [{}] {} {} ({})",
                    operation["operation"],
                    operation["kind"].as_str().unwrap_or_default(),
                    terminal_text(operation["target"].as_str().unwrap_or_default(), false),
                    operation["role"].as_str().unwrap_or_default()
                )?;
                writeln!(
                    output,
                    "    Expected before: {}; proposed after: {}",
                    operation["before"], operation["after"]
                )?;
                writeln!(
                    output,
                    "    Retained before: {}; retained proposed: {}",
                    operation["before_payload"], operation["after_payload"]
                )?;
                writeln!(
                    output,
                    "    Apply after operations: {}",
                    operation["apply_after"]
                )?;
            }
            writeln!(output, "Read guards (current state not checked):")?;
            for guard in summary["read_preconditions"].as_array().unwrap() {
                writeln!(
                    output,
                    "  {}: {}",
                    terminal_text(guard["path"].as_str().unwrap_or_default(), false),
                    guard["expected"]
                )?;
            }
            writeln!(
                output,
                "Manifest binding and recorded status verified. Payload availability, payload integrity and current target freshness: not checked."
            )?;
            for warning in &envelope.warnings {
                writeln!(output, "{}", terminal_text(warning, false))?;
            }
            if summary["operation_count"].as_u64().unwrap_or_default() > 0 {
                writeln!(
                    output,
                    "Inspect exact bytes with: {command_prefix} changes show {change} --operation 0"
                )?;
            }
            Ok(())
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
            if let Some(paths) = envelope.data.get("capture_paths") {
                writeln!(
                    output,
                    "Editable input: {}",
                    paths["editable_input"]
                        .as_str()
                        .unwrap_or("standard input (no editable file)")
                )?;
                writeln!(
                    output,
                    "Managed Source: {}",
                    paths["managed_source"].as_str().unwrap_or_default()
                )?;
                writeln!(
                    output,
                    "Snapshots are immutable. After editing the input, explicitly refresh; sync does not recapture it."
                )?;
                writeln!(
                    output,
                    "Refresh with the new input hash: {}",
                    paths["refresh_template"].as_str().unwrap_or_default()
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
                        "Inspect with: {command_prefix} changes show {change} --summary\nApply with: {command_prefix} changes apply {change}"
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
    for (key, label) in [
        ("initialization", "Wiki setup"),
        ("installation", "Agent installation"),
    ] {
        let state = &details[key];
        if has_retained_writes(state) {
            writeln!(output, "{label} stopped after changing files.")?;
            if state["durable"] == false {
                writeln!(
                    output,
                    "lwiki could not confirm that these changes were fully saved."
                )?;
            }
            let mut shown = std::collections::BTreeSet::new();
            for key in ["retained_paths", "created", "published_paths"] {
                if let Some(paths) = state[key].as_array() {
                    for path in paths.iter().filter_map(Value::as_str) {
                        if shown.insert(path) {
                            writeln!(output, "Reported path: {}", terminal_text(path, false))?;
                        }
                    }
                }
            }
            if let Some(retry) = state["pending_operation"]["retry_command"].as_str() {
                writeln!(output, "Retry: {}", terminal_text(retry, false))?;
            }
        }
    }
    if details["init"]["created"] == true {
        writeln!(
            output,
            "Wiki creation succeeded at {}. Default registration failed; the wiki is retained.",
            details["init"]["path"].as_str().unwrap_or_default()
        )?;
        if let Some(retry) = details["retry"].as_str() {
            writeln!(output, "Retry: {retry}")?;
        }
    }
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
    fn interrupted_setup_and_installation_report_retained_writes() {
        for (command, field) in [
            ("init", "initialization"),
            ("skill install", "installation"),
        ] {
            let mut error = WikiError::new(ErrorCode::ContentConflict, "publication interrupted");
            error.details = json!({field:{"applied":true,"durable":false,
                "retained_paths":["/chosen folder/kept\nname"]}});
            let envelope = failure(command, error);
            assert!(envelope.meta.partial);
            let mut bytes = Vec::new();
            present(&envelope, OutputFormat::Human, &mut bytes).unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert!(text.contains("Reported path: /chosen folder/kept\\nname"));
            assert!(!text.contains("changes resolve"));
        }
        let mut error = WikiError::new(ErrorCode::ConfigInvalid, "default registration failed");
        error.details = json!({"init":{"created":true,"path":"/retained wiki"}});
        assert!(failure("init", error).meta.partial);
        assert!(
            !failure(
                "init",
                WikiError::new(ErrorCode::Usage, "preflight refused")
            )
            .meta
            .partial
        );
    }

    #[test]
    fn interrupted_link_publication_is_partial_before_the_operation_finishes() {
        let mut error = WikiError::new(ErrorCode::ContentConflict, "second link changed");
        error.details = json!({"installation":{"applied":false,"durable":false,
            "changed":true,"created":[],"published_paths":["/project/.agents/skills/llm-wiki"],
            "pending_operation":{"retry_command":"lwiki skill update --target codex --scope project --directory '/project'"}}});
        let envelope = failure("skill update", error);
        assert!(envelope.meta.partial);
        let mut bytes = Vec::new();
        present(&envelope, OutputFormat::Human, &mut bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("stopped after changing files"));
        assert!(text.contains("Reported path: /project/.agents/skills/llm-wiki"));
        assert!(text.contains("--scope project --directory '/project'"));
        assert!(text.contains("could not confirm"));
    }

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
    context: &super::remote::RemoteContext<'_>,
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
            if !app.options().offline && context.profile.is_some() {
                let runtime = remote.runtime(app, context, Capability::Embed)?;
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
