use super::arguments::*;
use crate::{
    app::*,
    catalog::{
        Catalog, CatalogGraphValidator, CatalogOptions, ReaderSnapshot, SnapshotVerification,
    },
    changes::ChangeEngine,
    config::{self, PreferenceOptions},
    domain::*,
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
    "search",
    "check",
    "doctor",
    "changes show",
    "changes apply",
    "changes abort",
    "changes rollback",
    "recover",
    "migrate",
];
fn usage(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::Usage, message)
}
fn value<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))
}
fn failure(command: &str, error: WikiError) -> Envelope {
    let mut envelope = Envelope::failure(command, &error.code.to_string(), error.message.clone());
    if let Some(change) = error.details.get("change") {
        envelope.data = json!({"change":change});
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
            "JSONL is supported for index sync/rebuild, recover, changes apply, and source add/refresh",
        ));
    }
    match &args.command {
        Command::Capabilities => {
            return Ok(Envelope::success(
                command,
                json!({"version":env!("CARGO_PKG_VERSION"),"commands":COMMANDS,"schemas":["output","record","stream"],"network":false,"search_modes":["literal","lexical"],"jsonl_commands":["index sync","index rebuild","recover","changes apply","source add","source refresh"]}),
            ));
        }
        Command::Schema { name } => {
            let schema = match name.as_str() {
                "output" => include_str!("../../schemas/output-v1.json"),
                "record" => include_str!("../../schemas/record-v1.json"),
                "stream" => include_str!("../../schemas/stream-v1.json"),
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
        Command::Search(search) => {
            let plan = retrieval::lexical::validate_plan(&search.query, &search.plan())?;
            if args.dry_run {
                // Even a read-only WAL open can create sidecars. Use no SQLite path.
                let projection = crate::catalog::scan::scan(app.fs(), app.vault_id())?;
                envelope.data = json!({"query":search.query,"plan":plan,"dry_run":true,"cache_state_unknown":true,"canonical_document_count":projection.documents.len(),"hits":null});
                envelope
                    .warnings
                    .push("search results and index freshness are unknown during dry-run".into());
            } else {
                let (_writer, reader) = reader(&app, search.no_sync)?;
                let hits = retrieval::search(&reader, &search.query, &plan)?;
                snapshot_metadata(&mut envelope.meta, &reader);
                envelope.meta.partial = hits.truncated;
                envelope.warnings.extend(hits.warnings.iter().cloned());
                envelope.data = value(hits)?;
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
        Command::Doctor { probe } => {
            if *probe {
                return Err(WikiError::new(
                    if preferences.offline {
                        ErrorCode::OfflineUnavailable
                    } else {
                        ErrorCode::CapabilityUnavailable
                    },
                    "provider probing is not implemented; local doctor remains available without --probe",
                ));
            }
            envelope.data = value(app.doctor()?)?;
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
        Command::Capabilities | Command::Schema { .. } | Command::Init { .. } => unreachable!(),
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
