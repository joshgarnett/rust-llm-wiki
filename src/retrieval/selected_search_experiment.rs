//! Finite paired probe and grouped tests against the shipping selected-search coordinator.
use super::{
    ExcerptLabel, HitSet, QueryPlan, SearchHit, SearchMode,
    context_types::VerificationBudget,
    lexical, selected_documents,
    selected_search::{
        SCOPE_WARNING, Stats, bind_hit_for_test as bind_hit, measured_search as selected, preview,
        search,
    },
};
use crate::{
    catalog::{Catalog, query_types::QueryReadLimits},
    domain::*,
};
use std::time::Instant;
fn serialization(error: serde_json::Error) -> WikiError {
    WikiError::new(ErrorCode::Internal, error.to_string())
}

fn cached(catalog: &Catalog, query: &str, plan: &QueryPlan) -> (Result<HitSet>, Stats) {
    let start = Instant::now();
    let mut stats = Stats::default();
    let result = (|| {
        catalog.guard_query()?;
        let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
        let result = lexical::search_catalog(&reader, query, plan).and_then(|hits| {
            reader.verify_operations(catalog)?;
            serde_json::to_vec(&hits).map_err(serialization)?;
            Ok(hits)
        });
        stats.catalog_rows = reader.usage().rows;
        stats.catalog_bytes = reader.usage().bytes;
        result
    })();
    stats.elapsed_ns = start.elapsed().as_nanos();
    (result, stats)
}

// Equality discards only declared proof additions; cached warnings and all discovery fields survive.
fn discovery(mut hits: HitSet) -> HitSet {
    hits.verification = crate::catalog::SnapshotVerification::IndexSnapshot;
    hits.dependency_fingerprint = Blake3Hash::digest(b"paired-discovery");
    hits.warnings.retain(|warning| warning != SCOPE_WARNING);
    for hit in &mut hits.hits {
        for excerpt in std::iter::once(&mut hit.excerpt).chain(&mut hit.secondary_excerpts) {
            excerpt.citation = None;
            excerpt.label = ExcerptLabel::NoteText;
        }
    }
    hits
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeConfig {
    vault: std::path::PathBuf,
    requests: std::path::PathBuf,
    output: std::path::PathBuf,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeRequest {
    task_id: String,
    query: String,
    plan: QueryPlan,
    original_command: serde_json::Value,
}

#[test]
#[ignore = "explicit frozen config; root serializes native measurement"]
fn paired_selected_search_probe() {
    use std::io::Write;
    let config: ProbeConfig = serde_json::from_str(
        &std::env::var("LWIKI_VERIFIED_SEARCH_CONFIG").expect("explicit JSON config required"),
    )
    .unwrap();
    let requests: Vec<ProbeRequest> =
        serde_json::from_slice(&std::fs::read(&config.requests).unwrap()).unwrap();
    assert_eq!(requests.len(), 20);
    assert!(
        requests
            .windows(2)
            .all(|pair| pair[0].task_id < pair[1].task_id)
    );
    for request in &requests {
        assert_eq!(request.plan.mode, SearchMode::Lexical);
        assert_eq!(
            (
                request.plan.limits.candidates,
                request.plan.limits.hits,
                request.plan.limits.excerpt_bytes
            ),
            (80, 5, 1024)
        );
        assert!(request.plan.cursor.is_none());
    }
    let handle =
        crate::vault::VaultFs::new(crate::vault::VaultRoot::explicit(&config.vault).unwrap());
    let marker = crate::records::parse_note(&std::fs::read(config.vault.join("WIKI.md")).unwrap());
    let catalog = Catalog::new(handle, marker.canonical.unwrap().id().clone());
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config.output)
        .unwrap();
    let mut pinned = None;
    let mut errors = 0;
    let mut mismatches = 0;
    let mut observations = 0;
    let mut times = [Vec::new(), Vec::new()];
    let mut round_times: Vec<[Vec<u128>; 2]> = (0..3).map(|_| [Vec::new(), Vec::new()]).collect();
    for round in 0..3 {
        for request in &requests {
            let mut pair = [None, None];
            for arm in if round == 1 { [1, 0] } else { [0, 1] } {
                let (result, stats) = if arm == 0 {
                    cached(&catalog, &request.query, &request.plan)
                } else {
                    selected(
                        &catalog,
                        &request.query,
                        &request.plan,
                        &VerificationBudget::default(),
                        || Ok(()),
                    )
                };
                times[arm].push(stats.elapsed_ns);
                round_times[round][arm].push(stats.elapsed_ns);
                let value = match &result {
                    Ok(hits) => {
                        let changed = pinned
                            .as_ref()
                            .is_some_and(|snapshot| snapshot != &hits.snapshot);
                        if changed {
                            mismatches += 1;
                        }
                        if pinned.is_none() {
                            pinned = Some(hits.snapshot.clone());
                        }
                        pair[arm] = Some(discovery(hits.clone()));
                        serde_json::json!({"ok":true,"result":hits,"publication_changed":changed})
                    }
                    Err(error) => {
                        errors += 1;
                        serde_json::json!({"ok":false,"error":error})
                    }
                };
                serde_json::to_writer(&mut output, &serde_json::json!({"type":"observation","round":round+1,"task_id":request.task_id,"arm":if arm==0 {"D"} else {"V"},"original_command":request.original_command,"stats":stats,"outcome":value})).unwrap();
                writeln!(output).unwrap();
                observations += 1;
            }
            if let [Some(base), Some(verified)] = pair {
                if base != verified {
                    mismatches += 1;
                }
            }
        }
    }
    let distributions = times.map(|mut values| {
        let first = values[0]; values.sort_unstable();
        serde_json::json!({"count":values.len(),"first_ns":first,"p95_ns":values[(values.len()*95).div_ceil(100)-1],"max_ns":values.last()})
    });
    serde_json::to_writer(&mut output, &serde_json::json!({"type":"summary","observations":observations,"errors":errors,"mismatches":mismatches,"snapshot":pinned,"pooled_timings":distributions,"per_round_raw_ns":round_times,"peak_rss":"unavailable in this helper; root owns measurement","proof_counters_on_authentication_error":"unavailable, never assumed zero"})).unwrap();
    writeln!(output).unwrap();
    output.sync_all().unwrap();
    assert_eq!(
        (observations, errors, mismatches),
        (120, 0, 0),
        "all observations retained before gate assertion"
    );
}

mod tests {
    use super::*;
    use crate::{
        changes::ChangeDraft,
        sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
        vault::{VaultFs, VaultRoot, WriterPermit},
    };
    use serde_json::json;
    use std::{collections::BTreeMap, fs, time::Duration};

    const QUERY: &str = "searchproofmarker";
    const BODY: &str = "searchproofmarker: Café 東京 dispatch needs the violet permit.\n";
    fn path(value: &str) -> VaultRelativePath {
        VaultRelativePath::new(value).unwrap()
    }
    fn id(value: &str) -> RecordId {
        RecordId::new(value).unwrap()
    }
    fn request(body: &[u8]) -> CaptureRequest {
        CaptureRequest {
            title: "Captured dispatch".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture.txt".into(),
            original: body.to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: Some("text/plain".into()),
        }
    }
    fn plan() -> QueryPlan {
        QueryPlan {
            limits: super::super::SearchLimits {
                hits: 5,
                candidates: 80,
                excerpt_bytes: 1024,
            },
            ..Default::default()
        }
    }
    struct Fixture {
        _temp: tempfile::TempDir,
        catalog: Catalog,
        source: RecordId,
        revision: RecordId,
        content: VaultRelativePath,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_search_proof\nwiki_kind: vault\ntitle: Search proof\n---\n").unwrap();
            fs::write(
                temp.path().join("authored.md"),
                format!("# Authored café\n\n{QUERY}: uncited authored setup.\n"),
            )
            .unwrap();
            let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
            let capture = SourceStore::new(handle.clone())
                .plan_capture(request(BODY.as_bytes()))
                .unwrap();
            let source = capture.source_id.clone();
            let revision = capture.revision_id.clone();
            let content = path(&format!("sources/{source}/revisions/{revision}/content.md"));
            let catalog = Catalog::new(handle, id("vault_search_proof"));
            let fixture = Self {
                _temp: temp,
                catalog,
                source,
                revision,
                content,
            };
            fixture.seed(capture.draft.unwrap());
            let writer =
                WriterPermit::acquire(fixture.catalog.fs().root(), Duration::from_secs(1)).unwrap();
            fixture.catalog.rebuild_normalized(&writer).unwrap();
            drop(writer);
            fixture
        }
        fn seed(&self, draft: ChangeDraft) {
            for operation in draft.operations {
                let target = self
                    .catalog
                    .fs()
                    .root()
                    .path()
                    .join(operation.target.as_str());
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                fs::write(target, operation.proposed.unwrap()).unwrap();
            }
        }
        fn sync(&self) {
            let writer =
                WriterPermit::acquire(self.catalog.fs().root(), Duration::from_secs(1)).unwrap();
            self.catalog.sync_normalized(&writer).unwrap();
        }
        fn payload_plan(&self) -> QueryPlan {
            let mut plan = plan();
            plan.filters.path_prefix = Some(self.content.as_str().into());
            plan
        }
        fn edit_source(&self) {
            let withdrawal = SourceStore::new(self.catalog.fs().clone())
                .plan_withdraw(&self.source, "External fixture policy change")
                .unwrap();
            self.seed(withdrawal.draft.unwrap());
        }
        fn add_identity(&self, name: &str, dependencies: &str) -> VaultRelativePath {
            let target = path(&format!("{name}.md"));
            fs::write(
                self.catalog.fs().root().path().join(target.as_str()),
                format!("---\nwiki_schema: '1'\nwiki_id: {name}\nwiki_kind: entity\ntitle: {QUERY} {name}\nwiki_status: active\nwiki_entity_type: component\n{dependencies}---\n"),
            ).unwrap();
            target
        }
    }
    fn paired(fixture: &Fixture, query: &str, plan: &QueryPlan) -> HitSet {
        let baseline = cached(&fixture.catalog, query, plan).0.unwrap();
        let verified = search(
            &fixture.catalog,
            query,
            plan,
            &VerificationBudget::default(),
        )
        .unwrap();
        assert_eq!(discovery(baseline), discovery(verified.clone()));
        verified
    }
    fn assert_citation(fixture: &Fixture, hit: &SearchHit) {
        let CitationRef::Source(reference) = hit.excerpt.citation.as_ref().unwrap() else {
            panic!("source citation required")
        };
        assert_eq!(reference.source_id, hit.source_id.clone().unwrap());
        assert_eq!(
            reference.source_revision,
            hit.owner_revision.clone().unwrap()
        );
        assert_eq!(reference.span, hit.excerpt.span);
        let bytes = fs::read(
            fixture
                .catalog
                .fs()
                .root()
                .path()
                .join(hit.locator.path.as_str()),
        )
        .unwrap();
        let quote = &bytes[reference.span.start() as usize..reference.span.end() as usize];
        assert_eq!(quote, hit.excerpt.text.as_bytes());
        assert_eq!(Blake3Hash::digest(quote), reference.quote_hash);
        let app = crate::app::OfflineApp::new(
            fixture.catalog.fs().clone(),
            crate::app::OperationOptions {
                offline: true,
                ..Default::default()
            },
        )
        .unwrap();
        let read = app
            .read(crate::app::ReadRequest {
                selector: crate::app::RecordSelector::Path(hit.locator.path.clone()),
                range: Some(reference.span),
                max_bytes: 1024,
            })
            .unwrap();
        assert_eq!(read.body, hit.excerpt.text);
        SourceStore::new(fixture.catalog.fs().clone())
            .view()
            .unwrap()
            .verify(
                hit.excerpt.citation.as_ref().unwrap(),
                crate::sources::CitationScope::Historical,
            )
            .unwrap();
    }

    #[test]
    fn mixed_page_preserves_discovery_and_exact_captured_navigation() {
        let fixture = Fixture::new();
        let verified = paired(&fixture, QUERY, &plan());
        assert_eq!(verified.hits.len(), 2);
        for hit in &verified.hits {
            if hit.source_id.is_some() {
                assert_citation(&fixture, hit);
                assert_eq!(hit.eligibility, Eligibility::Current);
            } else {
                assert_eq!(hit.excerpt.label, ExcerptLabel::NoteText);
                assert!(hit.excerpt.citation.is_none());
            }
        }
        assert!(matches!(
            verified.verification,
            crate::catalog::SnapshotVerification::IndexedEvidence {
                global_membership_verified: false,
                ..
            }
        ));
        let identity = paired(&fixture, fixture.source.as_str(), &plan());
        assert!(
            identity
                .hits
                .iter()
                .all(|hit| hit.owner_revision.is_none() && hit.excerpt.citation.is_none())
        );
        assert_eq!(verified.warnings.last().unwrap(), SCOPE_WARNING);
    }

    #[test]
    fn current_entity_identity_preserves_mixed_and_pure_discovery_uncited() {
        let fixture = Fixture::new();
        fixture.add_identity("entity_identity", "");
        fixture.add_identity(
            "entity_invalid_description",
            "wiki_depends_on_ids: [assertion_absent]\n",
        );
        fixture.sync();
        let verified = paired(&fixture, QUERY, &plan());
        assert_eq!(verified.hits.len(), 4);
        let identities = verified
            .hits
            .iter()
            .filter(|hit| hit.kind == Some(RecordKind::Entity))
            .collect::<Vec<_>>();
        assert_eq!(identities.len(), 2);
        assert!(
            identities
                .iter()
                .any(|hit| hit.eligibility == Eligibility::Invalid)
        );
        assert!(
            identities
                .iter()
                .any(|hit| hit.eligibility == Eligibility::Unsupported)
        );
        for hit in identities {
            assert_eq!(hit.identity_eligibility, Some(Eligibility::Current));
            assert!(
                hit.reasons
                    .contains(&super::super::RetrievalReason::Identity)
            );
            assert!(hit.excerpt.text.is_empty() && hit.excerpt.span.is_empty());
            assert!(hit.excerpt.matched_spans.is_empty());
            assert!(hit.excerpt.citation.is_none() && hit.secondary_excerpts.is_empty());
            assert!(hit.source_id.is_none() && hit.owner_revision.is_none());
        }
        for hit in &verified.hits {
            if hit.source_id.is_some() {
                assert_citation(&fixture, hit);
            }
        }
        let pure = paired(&fixture, "entity_identity", &plan());
        assert_eq!(pure.hits.len(), 1);
        assert_eq!(pure.hits[0].kind, Some(RecordKind::Entity));
        let mut historical = plan();
        historical.filters.include_historical = true;
        let body_channel = cached(&fixture.catalog, "entity_identity", &historical)
            .0
            .unwrap();
        assert_eq!(body_channel.hits.len(), 1);
        assert!(
            !body_channel.hits[0]
                .reasons
                .contains(&super::super::RetrievalReason::Identity)
        );
        assert!(
            search(
                &fixture.catalog,
                "entity_identity",
                &historical,
                &VerificationBudget::default()
            )
            .is_err()
        );
        // Pagination and cursor contents remain those of ordinary discovery.
        let mut page = plan();
        page.limits.hits = 2;
        let first = paired(&fixture, QUERY, &page);
        page.cursor = first.next_cursor;
        assert!(page.cursor.is_some());
        assert_eq!(paired(&fixture, QUERY, &page).hits.len(), 2);
    }

    #[test]
    fn identity_exception_rejects_body_match_secondary_citation_and_forged_provenance() {
        let fixture = Fixture::new();
        let entity_path = fixture.add_identity("entity_identity", "");
        let target = fixture
            .catalog
            .fs()
            .root()
            .path()
            .join(entity_path.as_str());
        let mut raw = fs::read_to_string(&target).unwrap();
        raw.push_str(&format!("{QUERY}: unsupported description body.\n"));
        fs::write(target, raw).unwrap();
        fixture.sync();
        let reader = fixture
            .catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let hit = lexical::search_catalog(&reader, "entity_identity", &plan())
            .unwrap()
            .hits
            .remove(0);
        let mut proof = selected_documents::authenticate(
            &fixture.catalog,
            &reader,
            &[entity_path.clone()],
            &VerificationBudget::default(),
        )
        .unwrap();
        let raw = &proof.documents[&entity_path].raw_text;
        let body_start = raw.find(&format!("{QUERY}: unsupported")).unwrap();
        let body_text = raw[body_start..].to_owned();
        let body_span = ByteSpan::new(body_start as u64, raw.len() as u64).unwrap();
        for variant in 0..12 {
            let mut bad = hit.clone();
            match variant {
                0 => bad
                    .reasons
                    .retain(|reason| *reason != super::super::RetrievalReason::Identity),
                1 => {
                    bad.excerpt.text = body_text.clone();
                    bad.excerpt.span = body_span;
                }
                2 => bad.excerpt.matched_spans.push(bad.excerpt.span),
                3 => {
                    let mut excerpt = bad.excerpt.clone();
                    excerpt.text = body_text.clone();
                    excerpt.span = body_span;
                    bad.secondary_excerpts.push(excerpt);
                }
                4 => {
                    bad.excerpt.citation = Some(CitationRef::Source(SourceSpanRef {
                        source_id: fixture.source.clone(),
                        source_revision: fixture.revision.clone(),
                        span: bad.excerpt.span,
                        quote_hash: Blake3Hash::digest(b""),
                    }))
                }
                5 => bad.source_id = Some(fixture.source.clone()),
                6 => bad.owner_revision = Some(fixture.revision.clone()),
                7 => bad.identity_eligibility = Some(Eligibility::Historical),
                8 => bad.kind = Some(RecordKind::Page),
                9 => {
                    bad.excerpt.span = body_span;
                    bad.excerpt.text.clear();
                }
                10 => bad.secondary_excerpts.push(bad.excerpt.clone()),
                _ => bad.excerpt.label = ExcerptLabel::CapturedSource,
            }
            assert!(
                bind_hit(&mut bad, &proof, fixture.catalog.vault_id()).is_err(),
                "variant {variant}"
            );
        }
        // Even mutually matching cached metadata cannot substitute for a proven
        // Current Entity record. Poison only this disposable internal proof.
        proof
            .records
            .get_mut(&id("entity_identity"))
            .unwrap()
            .identity_eligibility = Some(Eligibility::Historical);
        let mut not_current = hit.clone();
        not_current.identity_eligibility = Some(Eligibility::Historical);
        assert_eq!(
            bind_hit(&mut not_current, &proof, fixture.catalog.vault_id())
                .unwrap_err()
                .code,
            ErrorCode::CapabilityUnavailable
        );
        proof
            .records
            .get_mut(&id("entity_identity"))
            .unwrap()
            .identity_eligibility = Some(Eligibility::Current);
        proof.documents.get_mut(&entity_path).unwrap().source_id = Some(fixture.source.clone());
        let mut associated = hit.clone();
        associated.source_id = Some(fixture.source.clone());
        assert_eq!(
            bind_hit(&mut associated, &proof, fixture.catalog.vault_id())
                .unwrap_err()
                .code,
            ErrorCode::CapabilityUnavailable
        );
        proof.documents.get_mut(&entity_path).unwrap().source_id = None;
        let source_record = crate::records::parse_note(
            &fs::read(
                fixture
                    .catalog
                    .fs()
                    .root()
                    .path()
                    .join(format!("sources/{}/source.md", fixture.source)),
            )
            .unwrap(),
        )
        .canonical
        .unwrap();
        let row = proof.records.get_mut(&id("entity_identity")).unwrap();
        row.record = source_record;
        let mut forged = hit.clone();
        forged.locator.record = Some(RecordRef {
            vault_id: fixture.catalog.vault_id().clone(),
            record_id: row.record.id().clone(),
            expected_kind: RecordKind::Source,
        });
        assert_eq!(
            bind_hit(&mut forged, &proof, fixture.catalog.vault_id())
                .unwrap_err()
                .code,
            ErrorCode::CapabilityUnavailable
        );
    }

    #[test]
    fn selected_identity_edit_and_malformed_entity_still_refuse() {
        let fixture = Fixture::new();
        let entity_path = fixture.add_identity("entity_identity", "");
        fixture.sync();
        let mut fired = false;
        let (result, _) = selected(
            &fixture.catalog,
            "entity_identity",
            &plan(),
            &VerificationBudget::default(),
            || {
                fired = true;
                let target = fixture
                    .catalog
                    .fs()
                    .root()
                    .path()
                    .join(entity_path.as_str());
                let raw = fs::read_to_string(&target).unwrap();
                fs::write(
                    target,
                    raw.replace("wiki_status: active", "wiki_status: superseded"),
                )
                .unwrap();
                Ok(())
            },
        );
        assert!(fired);
        assert_eq!(result.unwrap_err().code, ErrorCode::FreshnessConflict);
        fs::write(fixture.catalog.fs().root().path().join("malformed-entity.md"), format!("---\nwiki_schema: unsupported\nwiki_id: entity_malformed\nwiki_kind: entity\ntitle: {QUERY} malformed entity\nwiki_status: active\nwiki_entity_type: component\n---\n{QUERY} malformed body.\n")).unwrap();
        fixture.sync();
        let mut malformed = plan();
        malformed.filters.path_prefix = Some("malformed-entity.md".into());
        let baseline = cached(&fixture.catalog, QUERY, &malformed).0.unwrap();
        assert_eq!(baseline.hits.len(), 1);
        assert!(baseline.hits[0].identity_eligibility.is_none());
        assert!(
            search(
                &fixture.catalog,
                QUERY,
                &malformed,
                &VerificationBudget::default()
            )
            .is_err()
        );
    }

    #[test]
    fn historical_refresh_and_withdrawal_keep_exact_old_bytes_and_states() {
        let fixture = Fixture::new();
        let old = fs::read(
            fixture
                .catalog
                .fs()
                .root()
                .path()
                .join(fixture.content.as_str()),
        )
        .unwrap();
        let refreshed = SourceStore::new(fixture.catalog.fs().clone())
            .plan_refresh(
                &fixture.source,
                request(BODY.replace("violet", "amber").as_bytes()),
            )
            .unwrap();
        fixture.seed(refreshed.draft.unwrap());
        fixture.sync();
        let mut plan = plan();
        plan.filters.source_ids = vec![fixture.source.clone()];
        let current = paired(&fixture, QUERY, &plan);
        assert_eq!(current.hits.len(), 1);
        assert_eq!(current.hits[0].owner_revision, Some(refreshed.revision_id));
        plan.filters.include_historical = true;
        let history = paired(&fixture, QUERY, &plan);
        assert_eq!(history.hits.len(), 2);
        let historical = history
            .hits
            .iter()
            .find(|hit| hit.owner_revision.as_ref() == Some(&fixture.revision))
            .unwrap();
        assert_eq!(historical.eligibility, Eligibility::Historical);
        for hit in &history.hits {
            assert_citation(&fixture, hit);
        }
        let withdrawal = SourceStore::new(fixture.catalog.fs().clone())
            .plan_withdraw(&fixture.source, "Fixture withdrawal")
            .unwrap();
        fixture.seed(withdrawal.draft.unwrap());
        fixture.sync();
        plan.filters.include_historical = false;
        assert!(paired(&fixture, QUERY, &plan).hits.is_empty());
        plan.filters.include_historical = true;
        let withdrawn = paired(&fixture, QUERY, &plan);
        assert_eq!(withdrawn.hits.len(), 2);
        for hit in &withdrawn.hits {
            assert_eq!(hit.eligibility, Eligibility::Withdrawn);
            assert_citation(&fixture, hit);
        }
        assert_eq!(
            old,
            fs::read(
                fixture
                    .catalog
                    .fs()
                    .root()
                    .path()
                    .join(fixture.content.as_str())
            )
            .unwrap()
        );
    }

    #[test]
    fn source_path_filters_pagination_and_publication_cursor_remain_unchanged() {
        let fixture = Fixture::new();
        let selected = paired(&fixture, QUERY, &fixture.payload_plan());
        assert_eq!(selected.hits.len(), 1);
        assert_eq!(selected.hits[0].locator.path, fixture.content);
        let mut filtered = plan();
        filtered.filters.source_ids = vec![fixture.source.clone()];
        assert_eq!(paired(&fixture, QUERY, &filtered).hits.len(), 1);
        let mut page = plan();
        page.limits.hits = 1;
        let first = paired(&fixture, QUERY, &page);
        page.cursor = first.next_cursor.clone();
        assert!(page.cursor.is_some());
        let second = paired(&fixture, QUERY, &page);
        assert_ne!(first.hits[0].locator.path, second.hits[0].locator.path);
        fs::write(
            fixture.catalog.fs().root().path().join("unrelated.md"),
            "New orchard inventory.\n",
        )
        .unwrap();
        fixture.sync();
        assert!(
            search(
                &fixture.catalog,
                QUERY,
                &page,
                &VerificationBudget::default()
            )
            .is_err()
        );
    }

    #[test]
    fn absent_and_empty_spans_never_invent_citations() {
        let fixture = Fixture::new();
        assert!(
            paired(&fixture, "absentuniquetoken", &plan())
                .hits
                .is_empty()
        );
        let empty = SourceStore::new(fixture.catalog.fs().clone())
            .plan_capture(request(b""))
            .unwrap();
        let empty_path = format!(
            "sources/{}/revisions/{}/content.md",
            empty.source_id, empty.revision_id
        );
        fixture.seed(empty.draft.unwrap());
        fixture.sync();
        let mut plan = plan();
        plan.filters.path_prefix = Some(empty_path);
        let hits = paired(&fixture, "Captured dispatch", &plan);
        assert_eq!(hits.hits.len(), 1);
        assert!(hits.hits[0].excerpt.span.is_empty());
        assert!(hits.hits[0].excerpt.citation.is_none());
    }

    #[test]
    fn selected_same_size_same_mtime_payload_edit_refuses_before_authentication() {
        let fixture = Fixture::new();
        let file = fixture
            .catalog
            .fs()
            .root()
            .path()
            .join(fixture.content.as_str());
        let before = fs::metadata(&file).unwrap();
        fs::write(&file, BODY.replace("violet", "orange")).unwrap();
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(before.modified().unwrap()))
            .unwrap();
        assert_eq!(fs::metadata(&file).unwrap().len(), before.len());
        assert_eq!(
            fs::metadata(&file).unwrap().modified().unwrap(),
            before.modified().unwrap()
        );
        assert_eq!(
            search(
                &fixture.catalog,
                QUERY,
                &fixture.payload_plan(),
                &VerificationBudget::default()
            )
            .unwrap_err()
            .code,
            ErrorCode::FreshnessConflict
        );
        assert!(
            cached(&fixture.catalog, QUERY, &fixture.payload_plan())
                .0
                .unwrap()
                .hits[0]
                .excerpt
                .citation
                .is_none()
        );
    }

    #[test]
    fn selected_policy_and_actual_post_authentication_edits_refuse() {
        let fixture = Fixture::new();
        let mut fired = false;
        let (result, stats) = selected(
            &fixture.catalog,
            QUERY,
            &fixture.payload_plan(),
            &VerificationBudget::default(),
            || {
                fired = true;
                fixture.edit_source();
                Ok(())
            },
        );
        assert!(fired);
        assert_eq!(result.unwrap_err().code, ErrorCode::FreshnessConflict);
        assert!(stats.proof_work.unwrap().1 > 0);
        assert_eq!(
            search(
                &fixture.catalog,
                QUERY,
                &fixture.payload_plan(),
                &VerificationBudget::default()
            )
            .unwrap_err()
            .code,
            ErrorCode::FreshnessConflict
        );
    }

    #[test]
    fn selected_read_disappearance_refuses_and_unrelated_edit_stays_bounded() {
        let fixture = Fixture::new();
        let (result, _) = selected(
            &fixture.catalog,
            QUERY,
            &fixture.payload_plan(),
            &VerificationBudget::default(),
            || {
                fs::write(
                    fixture.catalog.fs().root().path().join("unrelated.md"),
                    "External orchard details.",
                )
                .unwrap();
                Ok(())
            },
        );
        assert_citation(&fixture, &result.unwrap().hits[0]);
        let mut fired = false;
        let (result, stats) = selected(
            &fixture.catalog,
            QUERY,
            &fixture.payload_plan(),
            &VerificationBudget::default(),
            || {
                fired = true;
                fs::remove_file(
                    fixture
                        .catalog
                        .fs()
                        .root()
                        .path()
                        .join(fixture.content.as_str()),
                )
                .unwrap();
                Ok(())
            },
        );
        assert!(fired);
        assert_eq!(result.unwrap_err().code, ErrorCode::FreshnessConflict);
        assert!(stats.proof_work.is_some());
    }

    #[test]
    fn closed_metadata_text_and_match_span_adversaries_cannot_acquire_authority() {
        let fixture = Fixture::new();
        let reader = fixture
            .catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let hit = lexical::search_catalog(&reader, QUERY, &fixture.payload_plan())
            .unwrap()
            .hits
            .remove(0);
        let proof = selected_documents::authenticate(
            &fixture.catalog,
            &reader,
            &[fixture.content.clone()],
            &VerificationBudget::default(),
        )
        .unwrap();
        for variant in 0..13 {
            let mut bad = hit.clone();
            match variant {
                0 => bad.title.push('x'),
                1 => bad.locator.observed_hash = Blake3Hash::digest(b"bad"),
                2 => bad.locator.record.as_mut().unwrap().record_id = id("revision_wrong"),
                3 => bad.locator.record.as_mut().unwrap().expected_kind = RecordKind::Source,
                4 => bad.kind = Some(RecordKind::Page),
                5 => bad.authored_status = Some("reviewed".into()),
                6 => bad.identity_eligibility = Some(Eligibility::Current),
                7 => bad.eligibility = Eligibility::Historical,
                8 => bad.source_id = Some(id("source_wrong")),
                9 => bad.owner_revision = Some(id("revision_wrong")),
                10 => bad.excerpt.text.push('x'),
                11 => {
                    bad.excerpt.matched_spans =
                        vec![ByteSpan::new(0, BODY.len() as u64 + 1).unwrap()]
                }
                _ => {
                    bad.excerpt.span =
                        ByteSpan::new(0, BODY.find('é').unwrap() as u64 + 1).unwrap();
                    bad.excerpt.text.clear();
                }
            }
            assert_eq!(
                bind_hit(&mut bad, &proof, fixture.catalog.vault_id())
                    .unwrap_err()
                    .code,
                ErrorCode::FreshnessConflict,
                "variant {variant}"
            );
        }
        let mut secondary = hit.clone();
        let mut excerpt = hit.excerpt.clone();
        excerpt.text.push('x');
        secondary.secondary_excerpts.push(excerpt);
        assert!(bind_hit(&mut secondary, &proof, fixture.catalog.vault_id()).is_err());
    }

    #[test]
    fn invalid_unsupported_discovery_and_modes_fail_without_cached_fallback() {
        let fixture = Fixture::new();
        fs::write(fixture.catalog.fs().root().path().join("invalid.md"),format!("---\nwiki_schema: 'unsupported'\nwiki_id: page_invalid\nwiki_kind: page\ntitle: Invalid\n---\n{QUERY} malformed text.\n")).unwrap();
        fixture.sync();
        let mut plan = plan();
        plan.filters.path_prefix = Some("invalid.md".into());
        let baseline = cached(&fixture.catalog, QUERY, &plan).0.unwrap();
        assert_eq!(baseline.hits.len(), 1);
        assert_eq!(baseline.hits[0].eligibility, Eligibility::Invalid);
        assert!(
            search(
                &fixture.catalog,
                QUERY,
                &plan,
                &VerificationBudget::default()
            )
            .is_err()
        );
        let mut unsupported = request(b"binary");
        unsupported.extraction = ExtractionInput::Unsupported {
            extractor: "fixture".into(),
            fingerprint: Blake3Hash::digest(b"fixture"),
        };
        let capture = SourceStore::new(fixture.catalog.fs().clone())
            .plan_capture(unsupported)
            .unwrap();
        let revision = capture.revision_id.clone();
        fixture.seed(capture.draft.unwrap());
        fixture.sync();
        plan.filters.path_prefix = None;
        plan.filters.include_historical = true;
        let baseline = cached(&fixture.catalog, revision.as_str(), &plan)
            .0
            .unwrap();
        assert_eq!(baseline.hits.len(), 1);
        assert_eq!(baseline.hits[0].eligibility, Eligibility::Unsupported);
        let reader = fixture
            .catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let mut forged = baseline.hits[0].clone();
        let proof = selected_documents::authenticate(
            &fixture.catalog,
            &reader,
            &[forged.locator.path.clone()],
            &VerificationBudget::default(),
        )
        .unwrap();
        forged.reasons.push(super::super::RetrievalReason::Identity);
        forged.excerpt.text.clear();
        forged.excerpt.span =
            ByteSpan::new(forged.excerpt.span.start(), forged.excerpt.span.start()).unwrap();
        forged.excerpt.matched_spans.clear();
        assert_eq!(
            bind_hit(&mut forged, &proof, fixture.catalog.vault_id())
                .unwrap_err()
                .code,
            ErrorCode::CapabilityUnavailable
        );
        assert!(
            search(
                &fixture.catalog,
                revision.as_str(),
                &plan,
                &VerificationBudget::default()
            )
            .is_err()
        );
        for mode in [
            SearchMode::Literal,
            SearchMode::Semantic,
            SearchMode::Hybrid,
        ] {
            plan.mode = mode;
            assert_eq!(
                search(
                    &fixture.catalog,
                    QUERY,
                    &plan,
                    &VerificationBudget::default()
                )
                .unwrap_err()
                .code,
                ErrorCode::CapabilityUnavailable
            );
        }
    }

    #[test]
    fn proof_byte_file_entry_deadlines_refuse_and_preview_is_only_planning() {
        let fixture = Fixture::new();
        for budget in [
            VerificationBudget {
                max_bytes: 1,
                ..Default::default()
            },
            VerificationBudget {
                max_files: 1,
                ..Default::default()
            },
            VerificationBudget {
                max_entries: 1,
                ..Default::default()
            },
            VerificationBudget {
                max_elapsed_ms: 0,
                ..Default::default()
            },
        ] {
            assert_eq!(
                search(&fixture.catalog, QUERY, &fixture.payload_plan(), &budget)
                    .unwrap_err()
                    .code,
                ErrorCode::BudgetExceeded
            );
        }
        let mut fired = false;
        let (result, _) = selected(
            &fixture.catalog,
            QUERY,
            &fixture.payload_plan(),
            &VerificationBudget::default(),
            || {
                fired = true;
                std::thread::sleep(Duration::from_millis(2001));
                Ok(())
            },
        );
        assert!(fired);
        assert_eq!(result.unwrap_err().code, ErrorCode::BudgetExceeded);
        fn tree(
            root: &std::path::Path,
        ) -> BTreeMap<std::path::PathBuf, (Vec<u8>, std::time::SystemTime)> {
            fn visit(
                path: &std::path::Path,
                files: &mut BTreeMap<std::path::PathBuf, (Vec<u8>, std::time::SystemTime)>,
            ) {
                for entry in fs::read_dir(path).unwrap() {
                    let path = entry.unwrap().path();
                    let metadata = fs::metadata(&path).unwrap();
                    if metadata.is_dir() {
                        visit(&path, files);
                    } else {
                        files.insert(
                            path.clone(),
                            (fs::read(path).unwrap(), metadata.modified().unwrap()),
                        );
                    }
                }
            }
            let mut files = BTreeMap::new();
            visit(root, &mut files);
            files
        }
        let before = tree(fixture.catalog.fs().root().path());
        let value = preview(QUERY, &plan()).unwrap();
        assert_eq!(value["query"], QUERY);
        assert_eq!(value["plan"], serde_json::to_value(plan()).unwrap());
        assert_eq!(value["dry_run"], true);
        assert!(value["hits"].is_null());
        assert_eq!(value["cache_state_unknown"], true);
        assert_eq!(value["database_opened"], false);
        assert_eq!(value["admission_performed"], false);
        assert_eq!(value["target_resolution_performed"], false);
        assert_eq!(value["verification_performed"], false);
        assert_eq!(value["citations"], json!([]));
        assert_eq!(before, tree(fixture.catalog.fs().root().path()));
        assert_eq!(preview("", &plan()).unwrap_err().code, ErrorCode::Usage);
        let mut invalid = plan();
        invalid.limits.hits = 0;
        assert_eq!(preview(QUERY, &invalid).unwrap_err().code, ErrorCode::Usage);
    }
}
