//! Generation-scoped discovery with closed, selected-source byte verification.
use super::{
    context::{self, Packet, PackingInput},
    context_selection::{self, SelectionDocument},
    context_types::*,
    lexical,
    types::*,
    verification::{Meter, seal},
};
use crate::{
    catalog::{
        Catalog, CatalogDiagnostic, DocumentRow, RecordRow, SnapshotVerification,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    domain::*,
    records::parse_note,
    sources::selected::{SelectedSourceBinding, verify_captured_source},
    vault::ExpectedState,
};
use rusqlite::{Connection, Row};
use std::collections::{BTreeMap, BTreeSet};

const MAX_CONTROL_BYTES: usize = 1024 * 1024;

fn conflict(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::FreshnessConflict, message)
}

/// Only authenticated selected objects are available to shared packing. There is
/// no CatalogProjection, no complete-registry claim, and no cache fallback.
struct SelectedCatalog<'a> {
    reader: &'a dyn QueryCatalog,
    records: BTreeMap<RecordId, RecordRow>,
    documents: BTreeMap<VaultRelativePath, DocumentRow>,
    fingerprint: Blake3Hash,
}

impl QueryCatalog for SelectedCatalog<'_> {
    fn publication_id(&self) -> Option<&str> {
        self.reader.publication_id()
    }
    fn normalized_layout(&self) -> bool {
        self.reader.normalized_layout()
    }
    fn connection(&self) -> &Connection {
        self.reader.connection()
    }
    fn snapshot(&self) -> &ReadSnapshot {
        self.reader.snapshot()
    }
    fn vault_id(&self) -> &RecordId {
        self.reader.vault_id()
    }
    fn verification(&self) -> &SnapshotVerification {
        self.reader.verification()
    }
    fn record(&self, id: &RecordId) -> Result<Option<RecordRow>> {
        Ok(self.records.get(id).cloned())
    }
    fn document(&self, path: &VaultRelativePath) -> Result<Option<DocumentRow>> {
        Ok(self.documents.get(path).cloned())
    }
    fn diagnostics(&self, _: &BTreeSet<VaultRelativePath>) -> Result<Vec<CatalogDiagnostic>> {
        Err(conflict(
            "selected evidence cannot read uncaptured diagnostics",
        ))
    }
    fn dependency_fingerprint(&self) -> Result<Blake3Hash> {
        Ok(self.fingerprint.clone())
    }
    fn query_scope(&self) -> &'static str {
        "indexed_evidence"
    }
    fn decode_document(&self, _: &Row<'_>, _: usize) -> Result<DocumentRow> {
        Err(conflict(
            "selected evidence cannot decode additional cache documents",
        ))
    }
}

fn capture_file(
    catalog: &Catalog,
    path: &VaultRelativePath,
    captured: &mut BTreeMap<VaultRelativePath, Vec<u8>>,
    meter: &mut Meter,
) -> Result<()> {
    if !captured.contains_key(path) {
        let bytes = meter
            .read(catalog, path)?
            .ok_or_else(|| conflict(format!("selected file disappeared: {path}")))?;
        captured.insert(path.clone(), bytes);
    }
    meter.check()
}

fn canonical(bytes: &[u8]) -> Result<CanonicalRecord> {
    if bytes.len() > MAX_CONTROL_BYTES {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "selected control record exceeds byte ceiling",
        ));
    }
    parse_note(bytes)
        .canonical
        .ok_or_else(|| conflict("selected control record is not canonical"))
}

fn authenticate_record(row: &RecordRow, bytes: &[u8], kind: RecordKind) -> Result<()> {
    let record = canonical(bytes)?;
    if record != row.record
        || record.kind() != kind
        || Blake3Hash::digest(bytes) != row.hash
        || row.authored_status.as_deref() != record.string("wiki_status")
        || row.eligibility != Eligibility::Current
        || row.identity_eligibility.is_some()
        || row.description_eligibility.is_some()
        || row.disputed
        || !row.reasons.is_empty()
    {
        return Err(conflict(
            "selected cached record metadata disagrees with canonical source state",
        ));
    }
    Ok(())
}

fn payload_path(revision: &RecordRow, field: &str) -> Result<VaultRelativePath> {
    let parent = revision
        .path
        .as_str()
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .ok_or_else(|| conflict("selected revision lacks parent directory"))?;
    let relative = revision
        .record
        .string(field)
        .ok_or_else(|| conflict(format!("selected revision lacks {field}")))?;
    VaultRelativePath::new(format!("{parent}/{relative}"))
}

fn authenticate_document(
    document: &DocumentRow,
    source: &RecordRow,
    revision: &RecordRow,
    content: &[u8],
) -> Result<()> {
    let raw =
        std::str::from_utf8(content).map_err(|_| conflict("selected content is not UTF-8"))?;
    let (headings, body) = crate::catalog::scan::normalized_markdown(raw);
    if document.path != payload_path(revision, "wiki_content_path")?
        || document.hash != Blake3Hash::digest(content)
        || document.record_id.is_some()
        || document.kind.is_some()
        || document.title != revision.record.title()
        || !document.aliases.is_empty()
        || !document.tags.is_empty()
        || document.headings != headings
        || document.body != body
        || document.raw_text != raw
        || document.source_id.as_ref() != Some(source.record.id())
        || document.owner_revision.as_ref() != Some(revision.record.id())
        || document.eligibility != Eligibility::Current
        || !document.reasons.is_empty()
    {
        return Err(conflict(
            "selected cached document disagrees with canonical source content",
        ));
    }
    Ok(())
}

fn locator(vault_id: &RecordId, document: &DocumentRow) -> Result<DocumentLocator> {
    Ok(DocumentLocator {
        record: Some(RecordRef {
            vault_id: vault_id.clone(),
            record_id: document
                .owner_revision
                .clone()
                .ok_or_else(|| conflict("source owner missing"))?,
            expected_kind: RecordKind::Revision,
        }),
        path: document.path.clone(),
        observed_hash: document.hash.clone(),
    })
}

fn authenticate_hit(
    hit: &SearchHit,
    document: &DocumentRow,
    vault_id: &RecordId,
    request: &ContextRequest,
) -> Result<()> {
    if hit.locator != locator(vault_id, document)?
        || hit.title != document.title
        || hit.kind.is_some()
        || hit.authored_status.is_some()
        || hit.identity_eligibility.is_some()
        || hit.source_id != document.source_id
        || hit.owner_revision != document.owner_revision
        || hit.eligibility != Eligibility::Current
        || (!request.documents.filters.source_ids.is_empty()
            && !document
                .source_id
                .as_ref()
                .is_some_and(|id| request.documents.filters.source_ids.contains(id)))
        || request
            .documents
            .filters
            .path_prefix
            .as_ref()
            .is_some_and(|prefix| !document.path.as_str().starts_with(prefix))
    {
        return Err(conflict(
            "selected hit identity, metadata or filters disagree with verified owner",
        ));
    }
    for excerpt in std::iter::once(&hit.excerpt).chain(&hit.secondary_excerpts) {
        if excerpt.span.len() > request.documents.limits.excerpt_bytes as u64
            || excerpt.span.slice(&document.raw_text)? != excerpt.text
        {
            return Err(conflict(
                "selected hit excerpt disagrees with verified source bytes",
            ));
        }
    }
    Ok(())
}

fn passage(
    document: &DocumentRow,
    vault_id: &RecordId,
    span: ByteSpan,
    rank: usize,
) -> Result<ContextPassage> {
    let text = span.slice(&document.raw_text)?.to_owned();
    if span.is_empty() {
        return Err(conflict("selected passage has an empty span"));
    }
    Ok(ContextPassage {
        locator: locator(vault_id, document)?,
        citations: vec![CitationRef::Source(SourceSpanRef {
            source_id: document
                .source_id
                .clone()
                .ok_or_else(|| conflict("selected source ID missing"))?,
            source_revision: document
                .owner_revision
                .clone()
                .ok_or_else(|| conflict("selected revision ID missing"))?,
            span,
            quote_hash: Blake3Hash::digest(text.as_bytes()),
        })],
        text,
        span,
        label: ExcerptLabel::CapturedSource,
        eligibility: Eligibility::Current,
        contributors: vec![],
        rank_contributions: vec![RankContribution {
            channel: "direct_document_owner".into(),
            rank,
            score: None,
        }],
        support_group: Some(document.hash.clone()),
    })
}

fn audit_passages(
    draft: &context::ContextDraft,
    selected: &SelectedCatalog<'_>,
    max_excerpt_bytes: usize,
) -> Result<()> {
    if !draft.bundles.is_empty() || draft.selection_packet.is_some() {
        return Err(conflict(
            "indexed source context contains an unsupported evidence domain",
        ));
    }
    for p in &draft.passages {
        let document = selected
            .documents
            .get(&p.locator.path)
            .ok_or_else(|| conflict("final passage owner was not verified"))?;
        if p.locator != locator(selected.vault_id(), document)?
            || p.span.is_empty()
            || p.span.slice(&document.raw_text)? != p.text
            || p.text.len() > max_excerpt_bytes
            || p.label != ExcerptLabel::CapturedSource
            || p.eligibility != Eligibility::Current
            || !p.contributors.is_empty()
            || p.support_group.as_ref() != Some(&document.hash)
            || p.citations.is_empty()
        {
            return Err(conflict(
                "final merged passage disagrees with verified source content",
            ));
        }
        let mut owner_cited = false;
        for citation in &p.citations {
            let CitationRef::Source(reference) = citation else {
                return Err(conflict(
                    "indexed source context contains an assertion citation",
                ));
            };
            let cited = selected
                .documents
                .values()
                .find(|d| {
                    d.source_id.as_ref() == Some(&reference.source_id)
                        && d.owner_revision.as_ref() == Some(&reference.source_revision)
                })
                .ok_or_else(|| conflict("final citation owner was not verified"))?;
            if reference.span != p.span
                || reference.span.slice(&cited.raw_text)? != p.text
                || reference.quote_hash != Blake3Hash::digest(p.text.as_bytes())
                || cited.hash != document.hash
            {
                return Err(conflict(
                    "final merged citation disagrees with verified source span",
                ));
            }
            owner_cited |= cited.path == document.path;
        }
        if !owner_cited {
            return Err(conflict(
                "final passage does not cite its own source revision",
            ));
        }
    }
    Ok(())
}

pub(super) fn context(
    catalog: &Catalog,
    query: &str,
    request: &ContextRequest,
    options: &ContextOptions,
) -> Result<ContextResult> {
    let mut meter = Meter::new(&request.verification_budget);
    let request = context::validate_request(query, request)?;
    context::validate_selection_action(&request, &options.selection)?;
    if request.scope != ContextScope::IndexedEvidence {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "indexed coordinator requires indexed-evidence scope",
        ));
    }
    meter.check()?;
    catalog.guard_query()?;
    meter.check()?;
    let reader = catalog.query_snapshot(QueryReadLimits {
        max_elapsed_ms: meter.remaining_ms(),
        ..Default::default()
    })?;
    let mut captured = BTreeMap::new();
    let wiki_path = VaultRelativePath::new("WIKI.md")?;
    capture_file(catalog, &wiki_path, &mut captured, &mut meter)?;
    let wiki = canonical(&captured[&wiki_path])?;
    if wiki.kind() != RecordKind::Vault || wiki.id() != catalog.vault_id() {
        return Err(conflict("selected vault marker identity changed"));
    }
    meter.check()?;
    let hits = lexical::search_indexed_sources(&reader, query, &request.documents)?;
    meter.check()?;
    if hits.hits.len() > request.documents.limits.hits || hits.snapshot != *reader.snapshot() {
        return Err(conflict(
            "indexed candidates exceed bounds or differ from pinned generation",
        ));
    }
    let mut selected = SelectedCatalog {
        reader: &reader,
        records: BTreeMap::new(),
        documents: BTreeMap::new(),
        fingerprint: reader.dependency_fingerprint()?,
    };
    for hit in &hits.hits {
        meter.check()?;
        let document = reader
            .document(&hit.locator.path)?
            .ok_or_else(|| conflict("selected indexed document disappeared"))?;
        let source_id = document
            .source_id
            .as_ref()
            .ok_or_else(|| conflict("selected document has no source ID"))?;
        let revision_id = document
            .owner_revision
            .as_ref()
            .ok_or_else(|| conflict("selected document has no revision ID"))?;
        let source = match selected.records.get(source_id) {
            Some(row) => row.clone(),
            None => reader
                .record(source_id)?
                .ok_or_else(|| conflict("selected source record missing"))?,
        };
        let revision = match selected.records.get(revision_id) {
            Some(row) => row.clone(),
            None => reader
                .record(revision_id)?
                .ok_or_else(|| conflict("selected revision record missing"))?,
        };
        capture_file(catalog, &source.path, &mut captured, &mut meter)?;
        capture_file(catalog, &revision.path, &mut captured, &mut meter)?;
        authenticate_record(&source, &captured[&source.path], RecordKind::Source)?;
        authenticate_record(&revision, &captured[&revision.path], RecordKind::Revision)?;
        let original_path = payload_path(&revision, "wiki_original_path")?;
        let content_path = payload_path(&revision, "wiki_content_path")?;
        capture_file(catalog, &original_path, &mut captured, &mut meter)?;
        capture_file(catalog, &content_path, &mut captured, &mut meter)?;
        let binding = SelectedSourceBinding {
            vault_id: catalog.vault_id().clone(),
            source_id: source_id.clone(),
            revision_id: revision_id.clone(),
            source_path: source.path.clone(),
            revision_path: revision.path.clone(),
            content_path: document.path.clone(),
            source_hash: source.hash.clone(),
            revision_hash: revision.hash.clone(),
            content_hash: document.hash.clone(),
        };
        let paths: BTreeSet<_> = [
            &wiki_path,
            &source.path,
            &revision.path,
            &original_path,
            &content_path,
        ]
        .into_iter()
        .collect();
        // Check the pure verifier's allocation ceilings before constructing its
        // owned input, not after cloning a potentially oversized original.
        let mut source_bytes = 0usize;
        for path in &paths {
            let bytes = &captured[*path];
            source_bytes = source_bytes.checked_add(bytes.len()).ok_or_else(|| {
                WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "selected source capture size overflow",
                )
            })?;
            if bytes.len() > 64 * 1024 * 1024 || source_bytes > 128 * 1024 * 1024 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "selected source capture exceeds allocation ceiling",
                ));
            }
        }
        let input: BTreeMap<_, _> = paths
            .into_iter()
            .map(|path| (path.clone(), captured[path].clone()))
            .collect();
        let proof = verify_captured_source(catalog.fs(), &binding, &input)?;
        for dep in &proof.dependencies {
            if captured
                .get(&dep.path)
                .is_none_or(|bytes| dep.expected != ExpectedState::Hash(Blake3Hash::digest(bytes)))
            {
                return Err(conflict("selected source proof escaped captured bytes"));
            }
        }
        authenticate_document(&document, &source, &revision, &proof.content)?;
        authenticate_hit(hit, &document, catalog.vault_id(), &request)?;
        selected.records.insert(source_id.clone(), source);
        selected.records.insert(revision_id.clone(), revision);
        selected.documents.insert(document.path.clone(), document);
        meter.check()?;
    }
    let states: BTreeMap<_, _> = captured
        .iter()
        .map(|(path, bytes)| (path.clone(), ExpectedState::Hash(Blake3Hash::digest(bytes))))
        .collect();
    let selected_commitment = if let Some(publication) = reader.publication_id() {
        serde_json::to_vec(&(
            "lwiki-indexed-evidence-selected-dependencies-v1",
            catalog.vault_id(),
            publication,
            reader.snapshot(),
            &states,
        ))
    } else {
        serde_json::to_vec(&(
            "lwiki-indexed-evidence-selected-dependencies-v1",
            catalog.vault_id(),
            reader.snapshot(),
            &states,
        ))
    };
    selected.fingerprint = Blake3Hash::digest(
        selected_commitment
            .map_err(|error| WikiError::new(ErrorCode::Internal, error.to_string()))?,
    );
    let anchors: Vec<_> = hits
        .hits
        .iter()
        .map(|hit| {
            std::iter::once(&hit.excerpt)
                .chain(hit.secondary_excerpts.iter().take(1))
                .map(|excerpt| excerpt.span)
                .collect::<Vec<_>>()
        })
        .collect();
    let documents: Vec<_> = hits
        .hits
        .iter()
        .enumerate()
        .map(|(index, hit)| SelectionDocument {
            owner_index: index,
            document: &selected.documents[&hit.locator.path],
            seed_spans: &anchors[index],
        })
        .collect();
    let selection = context_selection::select_candidates_with_semantics(
        &selected,
        query,
        &documents,
        request.documents.limits.excerpt_bytes,
        &[],
    )?;
    meter.check()?;
    let mut omissions = Vec::new();
    for omission in selection.omissions {
        let hit = &hits.hits[omission.owner_index];
        omissions.push(ContextOmission {
            record_id: hit.owner_revision.clone(),
            path: Some(hit.locator.path.clone()),
            reason: omission.reason.into(),
            count: 1,
        });
    }
    if hits.omitted_candidates > 0 {
        omissions.push(ContextOmission {
            record_id: None,
            path: None,
            reason: "indexed_source_candidate_cap; count_is_lower_bound".into(),
            count: hits.omitted_candidates,
        });
    }
    if hits.next_cursor.is_some() {
        omissions.push(ContextOmission {
            record_id: None,
            path: None,
            reason: "indexed_source_results_continue_on_next_page".into(),
            count: 1,
        });
    }
    let mut packets = Vec::new();
    for candidate in selection.candidates {
        let hit = &hits.hits[candidate.owner_index];
        let document = &selected.documents[&hit.locator.path];
        let rank = candidate.owner_index + 1;
        let p = passage(document, catalog.vault_id(), candidate.span, rank)?;
        packets.push(Packet {
            key: format!(
                "document:{}:{:020}:{:020}",
                p.locator.path,
                p.span.start(),
                p.span.end()
            ),
            passages: vec![p],
            bundle: None,
            navigation: None,
            score: 1.0 / (60.0 + rank as f64),
            selection: Some(candidate),
            selection_ordinal: None,
            unit_score: None,
            unit_origin: None,
            fallback: None,
            unit_clipped: false,
        });
    }
    let mut warnings = hits.warnings.clone();
    warnings.push(format!("source-aware context inspected {} bytes in {} blocks; query overlap guides passage selection, not answer completeness", selection.scanned_bytes, selection.scanned_blocks));
    warnings.push(format!("discovery domain: captured-source payloads only; {} candidate owners and {} selected owners; pages, graph records and other vault content were not searched; omissions outside this domain are not counted", hits.candidate_count, hits.hits.len()));
    warnings.push("eligibility and discovery refer to the published generation; selected source heads and complete original/content bytes were checked, without proving global identity uniqueness, membership, cache completeness or unselected dependency freshness".into());
    if reader.normalized_layout() {
        warnings.push("SQL VM and elapsed limits are cooperative and decoded-row ceilings are not a hard process-memory limit".into());
    } else {
        warnings.push("pending-apply safety guards still inspect retained change history outside canonical proof byte/file counters; SQL VM and elapsed limits are cooperative and decoded-row ceilings are not a hard process-memory limit".into());
    }
    let signals = ContextSelectionSignals::default();
    let draft = context::pack(
        &selected,
        &request,
        PackingInput {
            packets,
            omissions,
            term_weights: selection.term_weights,
            selection_warnings: warnings,
            source_aware: true,
            query: Some(query),
            signals: &signals,
            selection_action: &options.selection,
            hits: &hits,
            graph: None,
            dependency_fingerprint: selected.fingerprint.clone(),
            evidence_sets: None,
        },
    )?;
    meter.check()?;
    audit_passages(&draft, &selected, request.documents.limits.excerpt_bytes)?;
    if let Some(fault) = &options.fault {
        fault.check(ContextCheckpoint::BeforeFinalVerification { attempt: 0 })?;
    }
    meter.check()?;
    reader.verify_operations(catalog)?;
    meter.check()?;
    for (path, expected) in &states {
        let bytes = meter
            .read(catalog, path)?
            .ok_or_else(|| conflict(format!("selected dependency disappeared: {path}")))?;
        if *expected != ExpectedState::Hash(Blake3Hash::digest(&bytes)) {
            return Err(conflict(format!(
                "selected dependency changed before emission: {path}"
            )));
        }
    }
    meter.check()?;
    reader.verify_operations(catalog)?;
    meter.check()?;
    let usage = reader.usage();
    let verification = SnapshotVerification::IndexedEvidence {
        verified_at: crate::sources::revision::timestamp()?,
        discovery_generation: reader.snapshot().generation,
        evidence_domain: "captured_sources".into(),
        pending_operation_at_start: reader.pending_operation_at_start(),
        global_membership_verified: false,
        catalog_rows_decoded: usage.rows,
        catalog_bytes_decoded: usage.bytes,
    };
    meter.check()?;
    Ok(seal(draft, verification, &meter))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::revision::{common, record_bytes};
    use serde_json::json;

    #[test]
    fn selected_wrapper_preserves_layout_and_physical_cursor_binding() {
        struct Reader {
            connection: Connection,
            snapshot: ReadSnapshot,
            vault: RecordId,
            publication: Option<&'static str>,
            normalized: bool,
        }
        impl QueryCatalog for Reader {
            fn publication_id(&self) -> Option<&str> {
                self.publication
            }
            fn normalized_layout(&self) -> bool {
                self.normalized
            }
            fn connection(&self) -> &Connection {
                &self.connection
            }
            fn snapshot(&self) -> &ReadSnapshot {
                &self.snapshot
            }
            fn vault_id(&self) -> &RecordId {
                &self.vault
            }
            fn verification(&self) -> &SnapshotVerification {
                &SnapshotVerification::IndexSnapshot
            }
            fn record(&self, _: &RecordId) -> Result<Option<RecordRow>> {
                panic!("unused selected reader access")
            }
            fn document(&self, _: &VaultRelativePath) -> Result<Option<DocumentRow>> {
                panic!("unused selected reader access")
            }
            fn diagnostics(
                &self,
                _: &BTreeSet<VaultRelativePath>,
            ) -> Result<Vec<CatalogDiagnostic>> {
                panic!("unused selected reader access")
            }
            fn dependency_fingerprint(&self) -> Result<Blake3Hash> {
                panic!("unused selected reader access")
            }
            fn query_scope(&self) -> &'static str {
                "indexed_evidence"
            }
            fn decode_document(&self, _: &Row<'_>, _: usize) -> Result<DocumentRow> {
                panic!("unused selected reader access")
            }
        }
        let reader = |publication: Option<&'static str>, normalized| Reader {
            connection: Connection::open_in_memory().unwrap(),
            snapshot: match publication {
                Some(file_id) => ReadSnapshot::published(
                    1,
                    Blake3Hash::digest("parser"),
                    file_id.to_owned(),
                    Blake3Hash::digest("publication"),
                )
                .unwrap(),
                None => ReadSnapshot::canonical(
                    1,
                    Blake3Hash::digest("parser"),
                    Blake3Hash::digest("manifest"),
                ),
            },
            vault: RecordId::new("vault_wrapper").unwrap(),
            publication,
            normalized,
        };
        fn wrap(reader: &dyn QueryCatalog) -> SelectedCatalog<'_> {
            SelectedCatalog {
                reader,
                records: BTreeMap::new(),
                documents: BTreeMap::new(),
                fingerprint: Blake3Hash::digest("selected"),
            }
        }
        let old = reader(Some("00000000000000000000000000000001"), true);
        let selected = wrap(&old as &dyn QueryCatalog);
        assert!(selected.normalized_layout());
        assert_eq!(selected.publication_id(), old.publication_id());
        let fingerprint = Blake3Hash::digest("query");
        let cursor = super::super::cursor::encode(&selected, fingerprint.clone(), 1).unwrap();
        assert_eq!(
            super::super::cursor::offset(&selected, &fingerprint, Some(&cursor), 2).unwrap(),
            1
        );
        let new = reader(Some("00000000000000000000000000000002"), true);
        let replacement = wrap(&new as &dyn QueryCatalog);
        assert_eq!(
            replacement.snapshot().generation,
            selected.snapshot().generation
        );
        assert_ne!(replacement.snapshot(), selected.snapshot());
        assert_eq!(
            super::super::cursor::offset(&replacement, &fingerprint, Some(&cursor), 2)
                .unwrap_err()
                .code,
            ErrorCode::CursorStale
        );
        let legacy = reader(None, false);
        let legacy = wrap(&legacy as &dyn QueryCatalog);
        assert!(!legacy.normalized_layout());
        assert_eq!(legacy.publication_id(), None);
    }

    fn record_row(record: CanonicalRecord, path: &str) -> (RecordRow, Vec<u8>) {
        let bytes = record_bytes(record.clone(), b"").unwrap();
        (
            RecordRow {
                authored_status: record.string("wiki_status").map(str::to_owned),
                record,
                path: VaultRelativePath::new(path).unwrap(),
                hash: Blake3Hash::digest(&bytes),
                eligibility: Eligibility::Current,
                reasons: vec![],
                identity_eligibility: None,
                description_eligibility: None,
                disputed: false,
                dependencies: vec![],
            },
            bytes,
        )
    }

    fn fixture() -> (RecordRow, RecordRow, DocumentRow, Vec<u8>) {
        let source_id = RecordId::generate(RecordKind::Source).unwrap();
        let revision_id = RecordId::generate(RecordKind::Revision).unwrap();
        let content = "# Fixture\n\nCafé evidence includes exact source bytes.\n";
        let mut fields = common(&source_id, RecordKind::Source, "Source title");
        fields.extend(BTreeMap::from([
            ("wiki_status".into(), json!("active")),
            ("wiki_origin_kind".into(), json!("local-file")),
            ("wiki_origin".into(), json!("fixture.txt")),
            ("wiki_current_revision".into(), json!(revision_id)),
            ("wiki_revisions".into(), json!([revision_id])),
        ]));
        let (source, bytes) = record_row(
            CanonicalRecord::new(fields).unwrap(),
            "sources/fixture/source.md",
        );
        let mut fields = common(&revision_id, RecordKind::Revision, "Revision title");
        fields.extend(BTreeMap::from([
            ("wiki_source_id".into(), json!(source_id)),
            ("wiki_captured_at".into(), json!("2026-10-03T00:00:00Z")),
            ("wiki_original_path".into(), json!("original.bin")),
            (
                "wiki_original_hash".into(),
                json!(Blake3Hash::digest(content)),
            ),
            ("wiki_extractor".into(), json!("fixture")),
            (
                "wiki_extractor_fingerprint".into(),
                json!(Blake3Hash::digest(b"fixture")),
            ),
            ("wiki_extraction_status".into(), json!("complete")),
            ("wiki_content_path".into(), json!("content.md")),
            (
                "wiki_content_hash".into(),
                json!(Blake3Hash::digest(content)),
            ),
        ]));
        let (revision, _) = record_row(
            CanonicalRecord::new(fields).unwrap(),
            "sources/fixture/revisions/one/revision.md",
        );
        let (headings, body) = crate::catalog::scan::normalized_markdown(content);
        let document = DocumentRow {
            path: payload_path(&revision, "wiki_content_path").unwrap(),
            hash: Blake3Hash::digest(content),
            record_id: None,
            kind: None,
            title: revision.record.title().into(),
            aliases: vec![],
            headings,
            tags: vec![],
            body,
            raw_text: content.into(),
            source_id: Some(source_id),
            owner_revision: Some(revision_id),
            eligibility: Eligibility::Current,
            reasons: vec![],
        };
        (source, revision, document, bytes)
    }

    #[test]
    fn normalized_source_proof_survives_unrelated_operations_and_rejects_selected_edit() {
        use crate::{
            catalog::{
                file_types::{BuildIdentity, CatalogSelection},
                normalized_build::{BuildLimits, NormalizedBuilder},
                scan, selector,
            },
            changes::{PreparedChange, operation_authority},
            sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
            vault::{VaultFs, VaultRoot, WriterPermit},
        };
        use std::{
            sync::{
                Arc,
                atomic::{AtomicUsize, Ordering},
            },
            time::Duration,
        };
        struct RevisionFault {
            fs: VaultFs,
            vault: RecordId,
            calls: AtomicUsize,
            selected_edit: Option<std::path::PathBuf>,
        }
        impl ContextFault for RevisionFault {
            fn check(&self, checkpoint: ContextCheckpoint) -> Result<()> {
                assert_eq!(
                    checkpoint,
                    ContextCheckpoint::BeforeFinalVerification { attempt: 0 }
                );
                assert_eq!(self.calls.fetch_add(1, Ordering::SeqCst), 0);
                let writer = WriterPermit::acquire(self.fs.root(), Duration::from_secs(1))?;
                let idle = operation_authority::load(
                    &self.fs,
                    &self.vault,
                    operation_authority::Presence::Required,
                )?
                .unwrap();
                let change = PreparedChange {
                    change_id: RecordId::new("change_final_hook")?,
                    manifest_hash: Blake3Hash::digest("final hook operation"),
                };
                let intended = operation_authority::Publication {
                    file_id: idle.publication().file_id.clone(),
                    epoch: idle.publication().epoch + 1,
                };
                let active =
                    operation_authority::begin(&self.fs, &writer, &idle, change.clone(), intended)?;
                // This operation changed no canonical bytes, so cancel is
                // justified and unrelated revision changes do not block reads.
                operation_authority::cancel(&self.fs, &writer, &active, &change)?;
                if let Some(path) = &self.selected_edit {
                    let mut bytes = std::fs::read(path).unwrap();
                    bytes.extend_from_slice(b"\nexternal selected-content edit\n");
                    std::fs::write(path, bytes).unwrap();
                }
                Ok(())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: vault_final_hook\nwiki_kind: vault\ntitle: Final hook fixture\n---\nFixture\n").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let vault = RecordId::new("vault_final_hook").unwrap();
        let body = "# Source proof\n\nsourceproofneedle: Café evidence preserves captured bytes.\n";
        let capture = SourceStore::new(fs.clone())
            .plan_capture(CaptureRequest {
                title: "Source proof fixture".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "fixture.md".into(),
                original: body.as_bytes().to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/markdown".into()),
            })
            .unwrap();
        let content_path = temp.path().join(format!(
            "sources/{}/revisions/{}/content.md",
            capture.source_id, capture.revision_id
        ));
        // Populate a disposable canonical fixture from the real capture planner.
        // This exercises selected proof, rather than claiming changeset replay.
        for operation in capture.draft.unwrap().operations {
            let target = temp.path().join(operation.target.as_str());
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, operation.proposed.unwrap()).unwrap();
        }
        {
            let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
            let identity = BuildIdentity {
                selection: CatalogSelection::new(vault.clone(), 1).unwrap(),
                origin: None,
                vector_cache_lost: false,
                vector_loss_unknown: false,
            };
            selector::prepare(&fs, &writer, &identity.selection).unwrap();
            let mut builder =
                NormalizedBuilder::begin(&fs, &writer, identity, BuildLimits::default()).unwrap();
            let input = scan::scan_input(&fs, &vault).unwrap();
            let projection = scan::project_with_sink(&fs, &input, false, &mut builder).unwrap();
            let completed = builder.finish(&projection).unwrap();
            selector::publish(
                &fs,
                &writer,
                &completed.identity.selection,
                Duration::from_secs(1),
            )
            .unwrap();
        }
        let catalog = Catalog::new(fs.clone(), vault.clone());
        let mut request = ContextRequest {
            scope: ContextScope::IndexedEvidence,
            ..Default::default()
        };
        request.documents.filters.source_ids = vec![capture.source_id];
        let baseline = context(
            &catalog,
            "sourceproofneedle",
            &request,
            &ContextOptions::default(),
        )
        .unwrap();
        assert!(!baseline.passages().is_empty());
        // A query that starts during an unrelated active operation remains
        // available and reports that precise initial observation.
        {
            let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
            let idle = catalog.operation_state().unwrap().unwrap();
            let change = PreparedChange {
                change_id: RecordId::new("change_unrelated_active").unwrap(),
                manifest_hash: Blake3Hash::digest("unrelated active"),
            };
            let intended = operation_authority::Publication {
                file_id: idle.publication().file_id.clone(),
                epoch: idle.publication().epoch + 1,
            };
            let active =
                operation_authority::begin(&fs, &writer, &idle, change.clone(), intended).unwrap();
            let result = context(
                &catalog,
                "sourceproofneedle",
                &request,
                &ContextOptions::default(),
            )
            .unwrap();
            assert_eq!(result.passages(), baseline.passages());
            let SnapshotVerification::IndexedEvidence {
                pending_operation_at_start,
                ..
            } = result.verification()
            else {
                panic!("indexed proof expected");
            };
            assert_eq!(pending_operation_at_start.as_ref(), Some(&change.change_id));
            operation_authority::cancel(&fs, &writer, &active, &change).unwrap();
        }
        let fault = Arc::new(RevisionFault {
            fs: fs.clone(),
            vault: vault.clone(),
            calls: AtomicUsize::new(0),
            selected_edit: None,
        });
        let options = ContextOptions {
            fault: Some(fault.clone()),
            ..Default::default()
        };
        let result = context(&catalog, "sourceproofneedle", &request, &options).unwrap();
        assert_eq!(result.passages(), baseline.passages());
        let SnapshotVerification::IndexedEvidence {
            pending_operation_at_start,
            ..
        } = result.verification()
        else {
            panic!("indexed proof expected");
        };
        assert_eq!(pending_operation_at_start, &None);
        assert_eq!(fault.calls.load(Ordering::SeqCst), 1);

        let fault = Arc::new(RevisionFault {
            fs,
            vault,
            calls: AtomicUsize::new(0),
            selected_edit: Some(content_path),
        });
        let options = ContextOptions {
            fault: Some(fault.clone()),
            ..Default::default()
        };
        assert_eq!(
            context(&catalog, "sourceproofneedle", &request, &options)
                .unwrap_err()
                .code,
            ErrorCode::FreshnessConflict
        );
        assert_eq!(fault.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cached_payload_fields_require_canonical_content_and_revision_title() {
        let (source, revision, document, _) = fixture();
        let content = document.raw_text.as_bytes();
        authenticate_document(&document, &source, &revision, content).unwrap();
        for case in 0..7 {
            let mut poisoned = document.clone();
            match case {
                0 => poisoned.title = source.record.title().into(),
                1 => poisoned.body.push_str("invented fact"),
                2 => poisoned.raw_text.push_str("invented fact"),
                3 => poisoned.headings.push_str("invented heading"),
                4 => poisoned.tags.push("invented tag".into()),
                5 => poisoned.source_id = Some(RecordId::generate(RecordKind::Source).unwrap()),
                6 => poisoned.kind = Some(RecordKind::Source),
                _ => unreachable!(),
            }
            assert!(
                authenticate_document(&poisoned, &source, &revision, content).is_err(),
                "case {case}"
            );
        }
    }

    #[test]
    fn matching_hash_does_not_authorize_poisoned_cached_record_fields_or_status() {
        let (source, _, _, bytes) = fixture();
        authenticate_record(&source, &bytes, RecordKind::Source).unwrap();
        let mut poisoned = source.clone();
        let mut fields = poisoned.record.fields().clone();
        fields.insert("title".into(), json!("Invented title"));
        poisoned.record = CanonicalRecord::new(fields).unwrap();
        assert!(authenticate_record(&poisoned, &bytes, RecordKind::Source).is_err());
        let mut poisoned = source;
        poisoned.authored_status = Some("withdrawn".into());
        assert!(authenticate_record(&poisoned, &bytes, RecordKind::Source).is_err());
    }

    #[test]
    fn generated_passage_cites_exact_utf8_bytes_and_rejects_split_boundaries() {
        let (_, _, document, _) = fixture();
        let vault = RecordId::generate(RecordKind::Vault).unwrap();
        let start = document.raw_text.find("Café").unwrap();
        let span = ByteSpan::new(start as u64, (start + "Café".len()) as u64).unwrap();
        let result = passage(&document, &vault, span, 1).unwrap();
        assert_eq!(result.text, "Café");
        let CitationRef::Source(reference) = &result.citations[0] else {
            panic!("source citation")
        };
        assert_eq!(reference.quote_hash, Blake3Hash::digest("Café"));
        assert_eq!(reference.span, span);
        let split = ByteSpan::new(start as u64, (start + 4) as u64).unwrap();
        assert!(passage(&document, &vault, split, 1).is_err());
    }
}
