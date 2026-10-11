//! Ignored causal reference; original candidates and exact selected-proof packing.
//! No shipping selector, new evidence, provider, or interpretation of approval.
use super::{
    context, context_types::*, excerpts::SourceMap, indexed_documents, lexical, segment,
    selected_documents, types::*,
};
use crate::{
    catalog::{
        Catalog, DocumentRow,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    changes::ChangeEngine,
    domain::*,
    vault::{VaultFs, VaultRoot},
};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const MIB: usize = 1024 * 1024;
const READ_LIMIT: usize = 512 * MIB;
const OUTPUT_LIMIT: usize = 24 * MIB;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    vault: PathBuf,
    tasks: PathBuf,
    outputdir: PathBuf,
}
#[derive(Deserialize)]
struct Task {
    id: String,
    query: String,
}
#[derive(Deserialize)]
struct Tasks {
    tasks: Vec<Task>,
}
#[derive(Default)]
struct Work {
    total: usize,
    categories: BTreeMap<String, usize>,
}
fn checked<T, E: std::fmt::Display>(result: std::result::Result<T, E>) -> Result<T> {
    result.map_err(|error| WikiError::invalid(error.to_string()))
}
fn charge(work: &mut Work, category: &str, bytes: usize) -> Result<()> {
    work.total = work
        .total
        .checked_add(bytes)
        .ok_or_else(|| WikiError::invalid("read count overflow"))?;
    *work.categories.entry(category.into()).or_default() += bytes;
    if work.total > READ_LIMIT {
        return Err(WikiError::invalid("512MiB combined logical read bound"));
    }
    Ok(())
}
fn read(path: &Path, work: &mut Work) -> Result<Value> {
    if path.is_symlink() {
        return Err(WikiError::invalid("experiment input symlink"));
    }
    let size = checked(fs::metadata(path))?.len() as usize;
    charge(work, "metadata_and_frozen_reads", size)?;
    checked(serde_json::from_slice(&checked(fs::read(path))?))
}
fn save(path: &Path, value: &Value, output: &mut usize) -> Result<()> {
    let bytes = checked(serde_json::to_vec(value))?;
    if bytes.len() > 2 * MIB || *output + bytes.len() > OUTPUT_LIMIT {
        return Err(WikiError::invalid("compact case/whole output bound"));
    }
    let mut file = checked(OpenOptions::new().write(true).create_new(true).open(path))?;
    checked(file.write_all(&bytes))?;
    *output += bytes.len();
    Ok(())
}
fn deadline_check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        Err(WikiError::invalid("reference owning-process deadline"))
    } else {
        Ok(())
    }
}
fn strip_timestamps(value: &mut Value) {
    if let Some(fields) = value.get_mut("verification").and_then(Value::as_object_mut) {
        fields.remove("verified_at");
    }
}
struct Heading {
    start: usize,
    level: usize,
    text: String,
}
fn heading_inventory(raw: &str, body: usize, observed_end: usize) -> Vec<Heading> {
    let mut end = raw.len().min(body.saturating_add(MIB)).min(observed_end);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    let mut headings = Vec::new();
    let mut active = None;
    for (event, range) in Parser::new(&raw[body..end]).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                active = Some(Heading {
                    start: range.start + body,
                    level: level as usize,
                    text: String::new(),
                })
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(heading) = &mut active {
                    heading.text.push_str(&text);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(heading) = active.take() {
                    headings.push(heading);
                    if headings.len() == 4096 {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    headings
}
fn breadcrumbs(headings: &[Heading], start: usize) -> String {
    let mut ancestry = Vec::new();
    for heading in headings.iter().take_while(|heading| heading.start <= start) {
        segment::push_heading(&mut ancestry, heading.level, &heading.text);
    }
    segment::labels(&ancestry).join("\n")
}
fn overlaps(left: ByteSpan, right: ByteSpan) -> bool {
    left.start() < right.end() && right.start() < left.end()
}
fn rows<'a>(trace: &'a [Value], stage: &str) -> Result<&'a Vec<Value>> {
    trace
        .iter()
        .find(|entry| entry["stage"] == stage)
        .and_then(|entry| entry["rows"].as_array())
        .ok_or_else(|| WikiError::invalid(format!("missing trace stage {stage}")))
}
fn hit_field<T: serde::de::DeserializeOwned>(row: &Value, name: &str) -> Result<T> {
    checked(serde_json::from_value(row[name].clone()))
}
fn decode_hit(row: &Value) -> Result<SearchHit> {
    let hit = SearchHit {
        locator: hit_field(row, "locator")?,
        title: hit_field(row, "title")?,
        kind: hit_field(row, "kind")?,
        authored_status: hit_field(row, "authored_status")?,
        eligibility: hit_field(row, "eligibility")?,
        identity_eligibility: hit_field(row, "identity_eligibility")?,
        excerpt: hit_field(row, "excerpt")?,
        secondary_excerpts: hit_field(row, "secondary_excerpts")?,
        reasons: hit_field(row, "reasons")?,
        rank_contributions: hit_field(row, "rank_contributions")?,
        source_id: hit_field(row, "source_id")?,
        owner_revision: hit_field(row, "owner_revision")?,
    };
    if checked(serde_json::to_value(&hit))? != *row {
        return Err(WikiError::invalid("exact traced hit roundtrip differs"));
    }
    Ok(hit)
}
struct Core {
    owner: usize,
    span: ByteSpan,
    features: Value,
    score: Option<f64>,
}

// Task labels and frozen packets are not parameters to this ranker.
fn reference(
    catalog: &Catalog,
    request: &ContextRequest,
    query: &str,
    trace: &[Value],
    work: &mut Work,
    deadline: Instant,
    report: &mut Value,
) -> Result<()> {
    let hits = rows(trace, "document_hits")?
        .iter()
        .map(decode_hit)
        .collect::<Result<Vec<_>>>()?;
    if hits.len() > 5 {
        return Err(WikiError::invalid("discovered owner ceiling"));
    }
    let reader = catalog.cached_query_snapshot(QueryReadLimits {
        max_elapsed_ms: 2000,
        ..Default::default()
    })?;
    if checked(serde_json::to_value(reader.snapshot()))? != report["native"]["snapshot"] {
        return Err(WikiError::invalid(
            "reference snapshot differs from native discovery",
        ));
    }
    let paths = hits
        .iter()
        .map(|hit| hit.locator.path.clone())
        .collect::<Vec<_>>();
    let mut proof =
        selected_documents::authenticate(catalog, &reader, &paths, &request.verification_budget)?;
    charge(work, "reference_catalog_decoded", reader.usage().bytes)?;
    let (proof_bytes, _, _) = proof.meter().work();
    charge(work, "reference_authentication", proof_bytes)?;
    let selected = indexed_documents::SelectedCatalog {
        reader: &reader,
        proof: &proof,
    };
    let mut owners = Vec::<DocumentRow>::new();
    let mut headings = Vec::new();
    for (owner_index, hit) in hits.iter().enumerate() {
        deadline_check(deadline)?;
        let owner = selected
            .document(&hit.locator.path)?
            .ok_or_else(|| WikiError::invalid("unauthenticated owner"))?;
        if owner.hash != hit.locator.observed_hash {
            return Err(WikiError::invalid("owner hash changed"));
        }
        let body = if owner.owner_revision.is_some() {
            0
        } else {
            crate::records::parse_note(owner.raw_text.as_bytes()).body_start
        };
        let observed_end = rows(trace, "source_blocks")?
            .iter()
            .find(|row| row["owner_index"].as_u64() == Some(owner_index as u64))
            .and_then(|row| row["scanned_end"].as_u64())
            .map_or(body, |end| end as usize);
        charge(
            work,
            "reference_heading_prefix",
            observed_end.saturating_sub(body),
        )?;
        headings.push(heading_inventory(&owner.raw_text, body, observed_end));
        owners.push(owner);
    }
    report["owners"] = json!(
        owners
            .iter()
            .map(|owner| json!({"path":owner.path,"hash":owner.hash,"title":owner.title}))
            .collect::<Vec<_>>()
    );
    let connection = checked(Connection::open_in_memory())?;
    connection.progress_handler(1000, Some(move || Instant::now() >= deadline));
    checked(connection.execute_batch("CREATE VIRTUAL TABLE cores USING fts5(title,headings,body,tokenize='unicode61 remove_diacritics 2')"))?;
    let mut cores = Vec::<Core>::new();
    let mut unique = BTreeSet::new();
    let index_start = Instant::now();
    for owner_row in rows(trace, "source_candidates_generated")? {
        let owner = owner_row["owner_index"]
            .as_u64()
            .ok_or_else(|| WikiError::invalid("owner index"))? as usize;
        let document = owners
            .get(owner)
            .ok_or_else(|| WikiError::invalid("candidate owner outside discovery"))?;
        let candidates = owner_row["candidates"]
            .as_array()
            .ok_or_else(|| WikiError::invalid("generated candidates"))?;
        for candidate in candidates {
            deadline_check(deadline)?;
            let span: ByteSpan = checked(serde_json::from_value(candidate["span"].clone()))?;
            if candidate["clipped"] != false
                || span.len() > 1024
                || span.is_empty()
                || !unique.insert((owner, span.start(), span.end()))
            {
                continue;
            }
            let raw = span.slice(&document.raw_text)?;
            charge(work, "reference_core_scan", raw.len())?;
            let body = SourceMap::markdown(raw, 0).text;
            if body.trim().is_empty() {
                continue;
            }
            let heading = breadcrumbs(&headings[owner], span.start() as usize);
            charge(
                work,
                "reference_fts_features",
                document.title.len() + heading.len() + body.len(),
            )?;
            let features = json!({"title":document.title,"headings":heading,"body":body});
            checked(connection.execute(
                "INSERT INTO cores(rowid,title,headings,body) VALUES(?1,?2,?3,?4)",
                params![
                    (cores.len() + 1) as i64,
                    &document.title,
                    features["headings"].as_str().unwrap(),
                    features["body"].as_str().unwrap()
                ],
            ))?;
            cores.push(Core {
                owner,
                span,
                features,
                score: None,
            });
        }
    }
    let expression = lexical::lexical_expression(query)?;
    report["reference_index_seconds"] = json!(index_start.elapsed().as_secs_f64());
    let rank_start = Instant::now();
    let mut statement =
        checked(connection.prepare("SELECT rowid,bm25(cores) FROM cores WHERE cores MATCH ?1"))?;
    let ranked = checked(statement.query_map([expression], |row| {
        Ok(((row.get::<_, i64>(0)? - 1) as usize, row.get::<_, f64>(1)?))
    }))?;
    let mut order = Vec::new();
    for row in ranked {
        let (index, score) = checked(row)?;
        cores[index].score = Some(score);
        order.push(index);
    }
    order.sort_by(|&a, &b| {
        cores[a]
            .score
            .unwrap()
            .total_cmp(&cores[b].score.unwrap())
            .then(
                owners[cores[a].owner]
                    .path
                    .cmp(&owners[cores[b].owner].path),
            )
            .then(cores[a].span.start().cmp(&cores[b].span.start()))
            .then(cores[a].span.end().cmp(&cores[b].span.end()))
    });
    report["reference_rank_seconds"] = json!(rank_start.elapsed().as_secs_f64());
    let pack_start = Instant::now();
    let mut retained = vec![0usize; owners.len()];
    let mut accepted = Vec::<ContextPassage>::new();
    let mut accepted_spans = Vec::<(usize, ByteSpan)>::new();
    let mut text = context::render_documents_for_test(&selected, request, &[])?;
    let mut decisions = BTreeMap::new();
    for (rank, &index) in order.iter().enumerate() {
        deadline_check(deadline)?;
        let core = &cores[index];
        let reason = if retained[core.owner] == 32 {
            "owner32_cap"
        } else {
            retained[core.owner] += 1;
            if accepted_spans
                .iter()
                .any(|&(owner, span)| owner == core.owner && overlaps(span, core.span))
            {
                "actual_overlap"
            } else if let Some(passage) = context::passage_for_span_for_test(
                &selected,
                request,
                &hits[core.owner],
                core.span,
                core.owner + 1,
            )? {
                match context::admit_document_for_test(&selected, request, &accepted, &passage)? {
                    Ok((passages, rendered)) => {
                        accepted = passages;
                        text = rendered;
                        accepted_spans.push((core.owner, core.span));
                        "accepted"
                    }
                    Err(reason) => reason,
                }
            } else {
                "ineligible_passage"
            }
        };
        decisions.insert(
            index,
            json!({"rank":rank+1,"retained_by_owner32":reason!="owner32_cap","admission":reason}),
        );
    }
    report["rankings"] = json!(
        cores
            .iter()
            .enumerate()
            .map(|(index, core)| json!({
                    "owner_index":core.owner,"path":owners[core.owner].path,"span":core.span,
            "score":core.score,"features":{
                "title":core.features["title"], "headings":core.features["headings"],
                "body_hash":Blake3Hash::digest(core.features["body"].as_str().unwrap().as_bytes()),
                "body_bytes":core.features["body"].as_str().unwrap().len(),
            },"decision":decisions.get(&index),
                }))
            .collect::<Vec<_>>()
    );
    report["reference"] = json!({"verified":false,"text":text,"passages":accepted,"usage":{
        "rendered_bytes":text.len(),"estimated_tokens":text.len().div_ceil(4)},
        "eligible_cores":cores.len(),"matching_cores":order.len(),"owner_retained_counts":retained,
        "qualification":"TEST-ONLY fixed contextual BM25 reference; not a native public shipping result"});
    report["reference_pack_seconds"] = json!(pack_start.elapsed().as_secs_f64());
    let before_recheck = proof.meter().work().0;
    proof.recheck(catalog, &reader)?;
    charge(
        work,
        "reference_recheck",
        proof.meter().work().0 - before_recheck,
    )?;
    deadline_check(deadline)?;
    report["reference"]["verified"] = json!(true);
    Ok(())
}

#[test]
#[ignore = "one frozen source/protocol-admitted diagnostic; no automatic advancement"]
fn frozen_contextual_passage_reference() {
    run().unwrap();
}
fn run() -> Result<()> {
    let began = Instant::now();
    let mut work = Work::default();
    let config_path = checked(std::env::var("LWIKI_CONTEXTUAL_REFERENCE_CONFIG"))?;
    let config: Config = checked(serde_json::from_value(read(
        Path::new(&config_path),
        &mut work,
    )?))?;
    let repo = checked(fs::canonicalize(env!("CARGO_MANIFEST_DIR")))?;
    let artifact = repo.join(".artifacts/native-structural-evidence-001");
    let fixed_tasks=repo.join(".artifacts/normalized-semantic-lifecycle-001/public-lifecycle-attribution-001/critic/public-tasks.json");
    if config.vault != artifact.join("paired-001/candidate")
        || config.tasks != fixed_tasks
        || config.outputdir.exists()
        || !config.outputdir.starts_with(&artifact)
    {
        return Err(WikiError::invalid(
            "fixed candidate/tasks and fresh owned output required",
        ));
    }
    let tasks: Tasks = checked(serde_json::from_value(read(&config.tasks, &mut work)?))?;
    if tasks.tasks.len() != 12 {
        return Err(WikiError::invalid("original twelve tasks required"));
    }
    let frozen_summary = read(&artifact.join("paired-002/summary.json"), &mut work)?;
    let commands = frozen_summary["commands"]
        .as_array()
        .ok_or_else(|| WikiError::invalid("frozen command receipt"))?;
    let fs_handle = VaultFs::new(VaultRoot::explicit(&config.vault)?);
    let engine = ChangeEngine::new(fs_handle.clone())?;
    charge(
        &mut work,
        "experiment_vault_identity",
        checked(fs::metadata(config.vault.join("WIKI.md")))?.len() as usize,
    )?;
    let catalog = Catalog::new(fs_handle, engine.vault_id().clone());
    let request = ContextRequest {
        scope: ContextScope::IndexedDocuments,
        documents: QueryPlan {
            filters: SearchFilters {
                path_prefix: Some("sources/".into()),
                ..Default::default()
            },
            limits: SearchLimits {
                hits: 5,
                candidates: 80,
                excerpt_bytes: 1024,
            },
            ..Default::default()
        },
        budget: ContextBudget {
            max_bytes: 6000,
            max_tokens: 1500,
            ..Default::default()
        },
        verification_budget: VerificationBudget {
            max_entries: 65536,
            ..Default::default()
        },
        ..Default::default()
    };
    checked(fs::create_dir(&config.outputdir))?;
    let mut output = 0usize;
    let mut cases = Vec::new();
    let planned = tasks
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    for task in tasks.tasks {
        if began.elapsed() >= Duration::from_secs(60) {
            break;
        }
        if !task.id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(WikiError::invalid("unsafe output task label"));
        }
        let started = Instant::now();
        let deadline = (started + Duration::from_secs(5)).min(began + Duration::from_secs(60));
        let before = work.total;
        let before_categories = work.categories.clone();
        let mut report = json!({"task_id":task.id,"query":task.query,"classification":"DEVELOPMENT causal reference"});
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
            let native_start = Instant::now();
            let (native, trace) = context::with_source_selection_trace(|| {
                indexed_documents::context(
                    &catalog,
                    &task.query,
                    &request,
                    &ContextOptions::default(),
                )
            });
            report["native_seconds"] = json!(native_start.elapsed().as_secs_f64());
            let native = native?;
            charge(
                &mut work,
                "native_catalog_decoded",
                match native.verification() {
                    crate::catalog::SnapshotVerification::IndexedEvidence {
                        catalog_bytes_decoded,
                        ..
                    } => *catalog_bytes_decoded,
                    _ => return Err(WikiError::invalid("native selected verification expected")),
                },
            )?;
            charge(
                &mut work,
                "native_canonical_verification",
                native.usage().verification_bytes,
            )?;
            let native_value = checked(serde_json::to_value(&native))?;
            report["native"] = native_value.clone();
            report["trace"] = json!(trace);
            let label = format!("{}-context-candidate", task.id);
            let (index, row) = commands
                .iter()
                .enumerate()
                .find(|(_, row)| row["label"] == label)
                .ok_or_else(|| WikiError::invalid("frozen candidate packet absent"))?;
            let frozen = read(
                &artifact.join(format!(
                    "paired-002/commands/{:02}-{}/stdout",
                    index + 1,
                    row["label"].as_str().unwrap()
                )),
                &mut work,
            )?;
            let mut actual = native_value;
            let mut expected = frozen["data"].clone();
            strip_timestamps(&mut actual);
            strip_timestamps(&mut expected);
            report["native_replay_matches_frozen"] = json!(actual == expected);
            if actual != expected {
                return Err(WikiError::invalid(
                    "unmodified native replay changed frozen packet",
                ));
            }
            deadline_check(deadline)?;
            let reference_start = Instant::now();
            reference(
                &catalog,
                &request,
                &task.query,
                &trace,
                &mut work,
                deadline,
                &mut report,
            )?;
            report["reference_seconds"] = json!(reference_start.elapsed().as_secs_f64());
            Ok(())
        }))
        .unwrap_or_else(|_| {
            Err(WikiError::invalid(
                "diagnostic invariant/trace bound panicked; partial case retained",
            ))
        });
        report["elapsed_seconds"] = json!(started.elapsed().as_secs_f64());
        report["combined_logical_bytes"] = json!(work.total - before);
        report["logical_reads"] = json!(
            work.categories
                .iter()
                .map(|(key, value)| (
                    key.clone(),
                    value - before_categories.get(key).copied().unwrap_or(0)
                ))
                .collect::<BTreeMap<_, _>>()
        );
        report["error"] = json!(outcome.as_ref().err().map(ToString::to_string));
        // Reserve terminal accounting and preserve a small failure record if
        // a complete case cannot fit the fixed output allowance.
        let saved =
            if output + checked(serde_json::to_vec(&report))?.len() <= OUTPUT_LIMIT - 64 * 1024 {
                save(
                    &config.outputdir.join(format!("{}.json", task.id)),
                    &report,
                    &mut output,
                )
            } else {
                Err(WikiError::invalid(
                    "whole output bound; terminal reservation",
                ))
            };
        if let Err(error) = saved {
            save(
                &config.outputdir.join(format!("{}.json", task.id)),
                &json!({"task_id":task.id,
                "error":error.to_string(),"partial":true,"elapsed_seconds":report["elapsed_seconds"]}),
                &mut output,
            )?;
            cases.push(
                json!({"task_id":task.id,"ok":false,"status":"FAILED","error":error.to_string()}),
            );
            break;
        }
        cases.push(json!({"task_id":task.id,"ok":outcome.is_ok(),"elapsed_seconds":report["elapsed_seconds"]}));
        if outcome.is_err() {
            break;
        }
    }
    let complete = cases.len() == 12
        && cases.iter().all(|case| case["ok"] == true)
        && began.elapsed() < Duration::from_secs(60);
    for id in planned.iter().skip(cases.len()) {
        cases.push(json!({"task_id":id,"ok":false,"status":"UNRUN"}));
    }
    save(
        &config.outputdir.join("summary.json"),
        &json!({"cases":cases,"planned_tasks":12,
        "complete":complete,"elapsed_seconds":began.elapsed().as_secs_f64(),"logical_reads":work.categories,
        "combined_logical_bytes":work.total,"case_output_bytes":output,"physical_io_bytes":null,
        "qualification":"TEST-ONLY reference diagnostic; no native quality score or automatic advancement"}),
        &mut output,
    )?;
    if complete {
        Ok(())
    } else {
        Err(WikiError::invalid(
            "reference has failed/unfinished cases; partial outputs retained",
        ))
    }
}

#[test]
fn breadcrumbs_are_original_ancestor_labels_and_utf8_spans_remain_exact() {
    let raw = "# Root\n\n## Café\n\n安全 target\n\n## Unrelated\n\nother\n";
    let headings = heading_inventory(raw, 0, raw.len());
    let start = raw.find("安全").unwrap();
    assert_eq!(breadcrumbs(&headings, start), "Root\nCafé");
    let span = ByteSpan::new(start as u64, (start + "安全".len()) as u64).unwrap();
    assert_eq!(span.slice(raw).unwrap(), "安全");
    assert!(!overlaps(
        span,
        ByteSpan::new(span.end(), span.end() + 1).unwrap()
    ));
    assert!(overlaps(
        span,
        ByteSpan::new(span.start(), span.end()).unwrap()
    ));
}
