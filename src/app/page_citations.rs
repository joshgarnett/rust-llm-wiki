//! Explicit, bounded Source citations rendered after a Page's destination is fixed.
use super::{MAX_INPUT_BYTES, MutationOutcome, OfflineApp, pages::page_initial_proposal};
use crate::{
    catalog::query_types::{QueryCatalog, QueryReadLimits},
    changes::{ChangeDraft, ProposedTarget, ReadDependency, ScanDocument, ValidationInput},
    domain::*,
    retrieval::{VerificationBudget, selected_documents},
    sources::{CitationScope, CitationState, SourceView},
    vault::ExpectedState,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::{collections::BTreeMap, time::Instant};

pub const MAX_SOURCE_REFS_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_SOURCE_REFS: usize = 16;
const BEGIN: &str = "<!-- lwiki:source-citations:v1 begin -->";
const END: &str = "<!-- lwiki:source-citations:v1 end -->";
const RESERVED: &str = "<!-- lwiki:source-citations:";

/// Only exact Source references are accepted; display paths and states are derived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PageSourceRefs {
    schema_version: String,
    citations: Vec<CitationRef>,
}
impl PageSourceRefs {
    pub(crate) fn citations(&self) -> &[CitationRef] {
        &self.citations
    }
    pub fn from_json_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_SOURCE_REFS_INPUT_BYTES {
            return Err(budget("source references exceed 64 KiB"));
        }
        serde_json::from_slice(bytes).map_err(|e| WikiError::invalid(e.to_string()))
    }
    pub fn len(&self) -> usize {
        self.citations.len()
    }
    pub fn is_empty(&self) -> bool {
        self.citations.is_empty()
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != "1" {
            return Err(WikiError::invalid(
                "source references require schema version 1",
            ));
        }
        if self.citations.len() > MAX_SOURCE_REFS
            || serde_json::to_vec(self)
                .map_err(|e| WikiError::invalid(e.to_string()))?
                .len()
                > MAX_SOURCE_REFS_INPUT_BYTES
        {
            return Err(budget("source references exceed 16 entries or 64 KiB"));
        }
        for citation in &self.citations {
            let reference = source_reference(citation)?;
            if reference.span.is_empty() {
                return Err(WikiError::invalid(
                    "source references require a nonempty span",
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn migration_source_refs(bytes: &[u8]) -> Result<Option<PageSourceRefs>> {
    let text = std::str::from_utf8(bytes).map_err(|_| WikiError::invalid("Page is not UTF-8"))?;
    let Some(range) = owned_block(text)? else {
        return Ok(None);
    };
    let normalized = text[range].replace("\r\n", "\n");
    let block = normalized.as_str();
    let encoded = if let Some(start) = block.find("```json\n") {
        let rest = &block[start + "```json\n".len()..];
        rest.split_once("\n```").map(|(json, _)| json)
    } else if let Some(start) = block.find("<!--\n{") {
        block[start + "<!--\n".len()..]
            .split_once("\n-->")
            .map(|(json, _)| json)
    } else {
        None
    }
    .ok_or_else(|| {
        WikiError::invalid("owned source citation block lacks supported exact references")
    })?;
    Ok(Some(PageSourceRefs::from_json_slice(encoded.as_bytes())?))
}

/// Source title, capture timestamp, navigation and citation state.
type CitationMigrationTarget = (
    String,
    String,
    crate::sources::presentation::SourceNavigation,
    CitationState,
);
type CitationMigrationResolver<'a> =
    dyn FnMut(&SourceSpanRef) -> Result<CitationMigrationTarget> + 'a;

pub(crate) fn migrate_source_citation_block(
    page: &VaultRelativePath,
    bytes: &[u8],
    target: &mut CitationMigrationResolver<'_>,
) -> Result<Vec<u8>> {
    let Some(refs) = migration_source_refs(bytes)? else {
        return Ok(bytes.to_vec());
    };
    let text = std::str::from_utf8(bytes).map_err(|_| WikiError::invalid("Page is not UTF-8"))?;
    let mut presentations = Vec::new();
    for citation in refs.citations() {
        let (source_title, captured_at, navigation, state) = target(source_reference(citation)?)?;
        presentations.push(CitationPresentation {
            source_title,
            content_path: navigation.current_path,
            capture_anchor: Some(navigation.capture_anchor),
            captured_at,
            state,
        });
    }
    replace_block(
        text,
        owned_block(text)?,
        &render_block(page, &refs, &presentations)?,
    )
}
impl<'de> Deserialize<'de> for PageSourceRefs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        // CitationRef and SourceSpanRef are intentionally compatible domain
        // types. This input surface has stricter nested-field rejection.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SpanWire {
            source_id: RecordId,
            source_revision: RevisionId,
            span: ByteSpan,
            quote_hash: Blake3Hash,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct CitationWire {
            kind: String,
            reference: SpanWire,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            schema_version: String,
            citations: Vec<CitationWire>,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.citations.len() > MAX_SOURCE_REFS {
            return Err(serde::de::Error::custom(
                "source references exceed 16 entries",
            ));
        }
        let mut citations = Vec::new();
        for citation in wire.citations {
            if citation.kind != "source" {
                return Err(serde::de::Error::custom(
                    "only source citations are supported",
                ));
            }
            let reference = CitationRef::Source(SourceSpanRef {
                source_id: citation.reference.source_id,
                source_revision: citation.reference.source_revision,
                span: citation.reference.span,
                quote_hash: citation.reference.quote_hash,
            });
            if !citations.contains(&reference) {
                citations.push(reference);
            }
        }
        let refs = Self {
            schema_version: wire.schema_version,
            citations,
        };
        refs.validate().map_err(serde::de::Error::custom)?;
        Ok(refs)
    }
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn integrity(message: &str) -> WikiError {
    WikiError::new(ErrorCode::SourceIntegrity, message)
}
fn source_reference(citation: &CitationRef) -> Result<&SourceSpanRef> {
    match citation {
        CitationRef::Source(reference) => Ok(reference),
        CitationRef::Assertion(_) => Err(WikiError::invalid("only source citations are supported")),
    }
}

struct CitationPresentation {
    source_title: String,
    content_path: VaultRelativePath,
    capture_anchor: Option<String>,
    captured_at: String,
    state: CitationState,
}

impl OfflineApp {
    fn plan_citation_presentations(
        &self,
        refs: &PageSourceRefs,
        presentations: &mut [CitationPresentation],
        guards: &mut Vec<ReadDependency>,
    ) -> Result<(
        Vec<crate::changes::ExpectedWrite>,
        Vec<crate::changes::indexed_refresh::IndexedSourcePresentationTarget>,
    )> {
        use crate::{
            records::{edit_note, parse_note},
            sources::presentation_planning,
        };
        use std::collections::BTreeSet;
        let mut requested = BTreeMap::<RecordId, BTreeSet<RecordId>>::new();
        for citation in &refs.citations {
            let reference = source_reference(citation)?;
            requested
                .entry(reference.source_id.clone())
                .or_default()
                .insert(reference.source_revision.clone());
        }
        let mut expected: BTreeMap<_, _> = guards
            .iter()
            .map(|guard| (guard.path.clone(), guard.expected.clone()))
            .collect();
        let mut operations = Vec::new();
        let mut sources = Vec::new();
        for (source_id, revisions) in requested {
            let path = VaultRelativePath::new(format!("sources/{source_id}/source.md"))?;
            let bytes = crate::changes::prepare::read_bounded(&self.fs, &path, 1024 * 1024)?
                .ok_or_else(|| integrity("citation Source is missing"))?;
            let note = parse_note(&bytes);
            if expected.get(&path) != Some(&ExpectedState::Hash(note.source_hash.clone())) {
                return Err(integrity(
                    "citation Source changed after exact citation verification",
                ));
            }
            let source = presentation_planning::proposed_source(&note, &BTreeMap::new())?;
            let planned = presentation_planning::plan_change(
                &source,
                note.body(),
                Some(&note),
                &[],
                &revisions,
                &mut |path, maximum| {
                    crate::vault::fs::read_regular_bounded(
                        self.fs.root(),
                        path,
                        false,
                        maximum,
                        "saved-note",
                    )
                },
            )?;
            for dependency in planned
                .dependencies
                .iter()
                .chain(&planned.plan.dependencies)
            {
                if expected
                    .insert(dependency.path.clone(), dependency.expected.clone())
                    .is_some_and(|prior| prior != dependency.expected)
                {
                    return Err(integrity(
                        "citation presentation dependency changed during planning",
                    ));
                }
            }
            for (citation, presentation) in refs.citations.iter().zip(presentations.iter_mut()) {
                let reference = source_reference(citation)?;
                if reference.source_id == source_id {
                    let navigation = planned
                        .plan
                        .navigation
                        .get(&reference.source_revision)
                        .ok_or_else(|| {
                            integrity("citation has no authenticated saved-copy target")
                        })?;
                    presentation.content_path = navigation.current_path.clone();
                    presentation.capture_anchor = Some(navigation.capture_anchor.clone());
                }
            }
            let bytes = edit_note(
                &note,
                &BTreeMap::from([(
                    "wiki_presentation".into(),
                    planned.plan.metadata.encode()?.into(),
                )]),
                None,
                &note.source_hash,
            )?;
            if bytes != note.raw {
                let after = planned
                    .plan
                    .writes
                    .iter()
                    .map(|operation| operation.target.clone())
                    .collect();
                operations.extend(planned.plan.writes);
                operations.push(crate::changes::ExpectedWrite {
                    target: path,
                    expected: ExpectedState::Hash(note.source_hash),
                    proposed: Some(bytes),
                    apply_after: after,
                });
                sources.push(
                    crate::changes::indexed_refresh::IndexedSourcePresentationTarget {
                        source_id,
                        revisions: revisions.into_iter().collect(),
                    },
                );
            } else if !planned.plan.writes.is_empty() {
                return Err(integrity(
                    "saved-copy writes changed without Source ownership update",
                ));
            }
        }
        *guards = expected
            .into_iter()
            .map(|(path, expected)| ReadDependency { path, expected })
            .collect();
        Ok((operations, sources))
    }
    pub fn page_initialize_with_source_refs(
        &self,
        path: Option<VaultRelativePath>,
        id: Option<RecordId>,
        title: String,
        body: String,
        refs: PageSourceRefs,
    ) -> Result<MutationOutcome> {
        let (path, id, bytes) = page_initial_proposal(path, id, title, body)?;
        let mut outcome = self.page_put_with_source_refs(path.clone(), bytes, None, refs)?;
        outcome.allocated_ids.insert(path.to_string(), id);
        Ok(outcome)
    }

    pub fn page_put_with_source_refs(
        &self,
        path: VaultRelativePath,
        bytes: Vec<u8>,
        if_match: Option<Blake3Hash>,
        refs: PageSourceRefs,
    ) -> Result<MutationOutcome> {
        refs.validate()?;
        // Keep the ordinary request/proposal preview, including author guards.
        // Preview never authenticates sources or modifies the generated block.
        if self.options.dry_run {
            return self.page_put(path, bytes, if_match);
        }
        let mut operation = self.plan_page_write(path.clone(), bytes, if_match)?;
        let proposal = operation.proposed.as_ref().expect("Page proposal");
        let text = std::str::from_utf8(proposal)
            .map_err(|_| WikiError::invalid("Page proposal is not UTF-8"))?;
        let owned = owned_block(text)?;
        let (mut presentations, mut read_preconditions) = if refs.is_empty() {
            (Vec::new(), Vec::new())
        } else if self.catalog().operation_state()?.is_some() {
            self.selected_page_citations(&refs)?
        } else {
            self.legacy_page_citations(&refs)?
        };
        let (mut presentation_operations, presentation_sources) =
            if crate::storage::layout::layout_kind(self.fs.root())?
                == crate::storage::layout::LayoutKind::ReadableSources
            {
                self.plan_citation_presentations(
                    &refs,
                    &mut presentations,
                    &mut read_preconditions,
                )?
            } else {
                (Vec::new(), Vec::new())
            };
        let generated = render_block(&path, &refs, &presentations)?;
        let rendered = replace_block(text, owned, &generated)?;
        let rendered_text = std::str::from_utf8(&rendered)
            .map_err(|_| WikiError::invalid("rendered Page is not UTF-8"))?;
        if owned_block(rendered_text)?.is_some() != !refs.is_empty() {
            return Err(WikiError::invalid(
                "generated source citations are not a navigable Markdown block",
            ));
        }
        operation.proposed = Some(rendered);
        operation.apply_after = presentation_operations
            .iter()
            .map(|op| op.target.clone())
            .collect();
        presentation_operations.push(operation);
        self.execute_cited_page_draft(
            ChangeDraft {
                title: "Put page".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: BTreeMap::new(),
                read_preconditions,
                operations: presentation_operations,
            },
            presentation_sources,
        )
    }

    fn selected_page_citations(
        &self,
        refs: &PageSourceRefs,
    ) -> Result<(Vec<CitationPresentation>, Vec<ReadDependency>)> {
        let catalog = self.catalog();
        catalog.guard_query()?;
        let reader = catalog.query_snapshot(QueryReadLimits::default())?;
        let mut paths = Vec::new();
        for citation in &refs.citations {
            let reference = source_reference(citation)?;
            let revision = reader.record(&reference.source_revision)?.ok_or_else(|| {
                WikiError::new(ErrorCode::RecordNotFound, "citation Revision is absent")
            })?;
            if revision.record.kind() != RecordKind::Revision {
                return Err(integrity("citation Revision has the wrong kind"));
            }
            let content = payload_path(&revision.path, &revision.record, "wiki_content_path")?;
            if !paths.contains(&content) {
                paths.push(content);
            }
        }
        let mut selected = selected_documents::authenticate(
            &catalog,
            &reader,
            &paths,
            &VerificationBudget::default(),
        )?;
        let mut presentations = Vec::new();
        for citation in &refs.citations {
            let reference = source_reference(citation)?;
            let source = selected
                .records
                .get(&reference.source_id)
                .ok_or_else(|| integrity("citation Source is outside the authenticated closure"))?;
            let revision = selected
                .records
                .get(&reference.source_revision)
                .ok_or_else(|| {
                    integrity("citation Revision is outside the authenticated closure")
                })?;
            if source.record.kind() != RecordKind::Source
                || revision.record.kind() != RecordKind::Revision
                || revision.record.string("wiki_source_id") != Some(reference.source_id.as_str())
                || !matches!(
                    source.eligibility,
                    Eligibility::Current | Eligibility::Withdrawn
                )
                || !matches!(
                    revision.eligibility,
                    Eligibility::Current | Eligibility::Historical | Eligibility::Withdrawn
                )
            {
                return Err(integrity("citation ownership or eligibility is invalid"));
            }
            let content_path = payload_path(&revision.path, &revision.record, "wiki_content_path")?;
            let document = selected.documents.get(&content_path).ok_or_else(|| {
                integrity("citation content is outside the authenticated closure")
            })?;
            if document.source_id.as_ref() != Some(&reference.source_id)
                || document.owner_revision.as_ref() != Some(&reference.source_revision)
            {
                return Err(integrity("citation content belongs to another owner"));
            }
            let quote = reference
                .span
                .slice(&document.raw_text)
                .map_err(|_| integrity("source span out of bounds or splits UTF-8"))?;
            if Blake3Hash::digest(quote) != reference.quote_hash {
                return Err(integrity("selected quote hash mismatch"));
            }
            let state = observed_state(&source.record, reference);
            presentations.push(CitationPresentation {
                source_title: source.record.title().to_owned(),
                content_path,
                capture_anchor: None,
                captured_at: revision
                    .record
                    .string("wiki_captured_at")
                    .unwrap_or_default()
                    .to_owned(),
                state,
            });
        }
        let guards = selected.read_preconditions();
        if guards.len() > 128 {
            return Err(budget(
                "Page citations exceed 128 publication read dependencies",
            ));
        }
        selected.recheck(&catalog, &reader)?;
        Ok((presentations, guards))
    }

    fn legacy_page_citations(
        &self,
        refs: &PageSourceRefs,
    ) -> Result<(Vec<CitationPresentation>, Vec<ReadDependency>)> {
        let limits = VerificationBudget::default();
        let start = Instant::now();
        let view = SourceView::from_fs_bounded(&self.fs, limits.max_bytes, limits.max_files)?;
        let mut notes = BTreeMap::new();
        let mut assets = BTreeMap::new();
        let mut total = 0usize;
        let mut presentations = Vec::new();
        // The closed selected input reuses the existing legacy SourceView proof;
        // it bounds all selected payload allocation before verification begins.
        for citation in &refs.citations {
            if start.elapsed().as_millis() >= u128::from(limits.max_elapsed_ms) {
                return Err(budget("elapsed source citation proof deadline exceeded"));
            }
            let reference = source_reference(citation)?;
            let (sp, sn) = view.resolve(&reference.source_id, RecordKind::Source, None)?;
            let source = sn.canonical.as_ref().expect("resolved Source");
            let head = RecordId::new(
                source
                    .string("wiki_current_revision")
                    .ok_or_else(|| integrity("Source lacks its current Revision"))?,
            )?;
            let (hp, hn) =
                view.resolve(&head, RecordKind::Revision, source.string("wiki_revision"))?;
            let (rp, rn) = view.resolve(&reference.source_revision, RecordKind::Revision, None)?;
            let revision = rn.canonical.as_ref().expect("resolved Revision");
            for (path, note) in [(sp, sn), (hp, hn), (rp, rn)] {
                if !notes.contains_key(path) {
                    total = add_proof_bytes(total, note.raw.len(), &limits)?;
                    notes.insert(
                        path.clone(),
                        ScanDocument {
                            path: path.clone(),
                            hash: note.source_hash.clone(),
                            bytes: note.raw.clone(),
                        },
                    );
                }
            }
            let content_path = payload_path(rp, revision, "wiki_content_path")?;
            for field in ["wiki_original_path", "wiki_content_path"] {
                let path = payload_path(rp, revision, field)?;
                if let std::collections::btree_map::Entry::Vacant(entry) = assets.entry(path) {
                    let remaining = limits.max_bytes.saturating_sub(total);
                    let bytes =
                        crate::changes::prepare::read_bounded(&self.fs, entry.key(), remaining)?
                            .ok_or_else(|| integrity("citation payload is missing"))?;
                    total = add_proof_bytes(total, bytes.len(), &limits)?;
                    entry.insert(bytes);
                }
            }
            presentations.push(CitationPresentation {
                source_title: source.title().to_owned(),
                content_path,
                capture_anchor: None,
                captured_at: revision
                    .string("wiki_captured_at")
                    .unwrap_or_default()
                    .to_owned(),
                state: observed_state(source, reference),
            });
        }
        let wiki = VaultRelativePath::new("WIKI.md")?;
        let wiki_bytes = crate::changes::prepare::read_bounded(
            &self.fs,
            &wiki,
            limits.max_bytes.saturating_sub(total),
        )?
        .ok_or_else(|| integrity("vault marker is missing"))?;
        add_proof_bytes(total, wiki_bytes.len(), &limits)?;
        let marker = crate::records::parse_note(&wiki_bytes);
        if !marker
            .canonical
            .as_ref()
            .is_some_and(|r| r.kind() == RecordKind::Vault && r.id() == &self.vault_id)
        {
            return Err(integrity("vault marker identity changed"));
        }
        let mut guards: BTreeMap<_, _> = notes
            .iter()
            .map(|(path, note)| (path.clone(), ExpectedState::Hash(note.hash.clone())))
            .collect();
        guards.insert(
            wiki.clone(),
            ExpectedState::Hash(Blake3Hash::digest(&wiki_bytes)),
        );
        for (path, bytes) in &assets {
            guards.insert(path.clone(), ExpectedState::Hash(Blake3Hash::digest(bytes)));
        }
        notes.insert(
            wiki.clone(),
            ScanDocument {
                path: wiki,
                hash: Blake3Hash::digest(&wiki_bytes),
                bytes: wiki_bytes,
            },
        );
        let input = ValidationInput {
            vault_id: self.vault_id.clone(),
            documents: notes.into_values().collect(),
            overlay: assets
                .into_iter()
                .map(|(path, bytes)| ProposedTarget {
                    path,
                    bytes: Some(bytes),
                })
                .collect(),
        };
        let closed = SourceView::from_closed_input(&self.fs, &input)?;
        for (citation, presentation) in refs.citations.iter().zip(&presentations) {
            let verified = closed.verify(citation, CitationScope::Historical)?;
            if verified.state != presentation.state
                || verified
                    .dependencies
                    .iter()
                    .any(|d| guards.get(&d.path) != Some(&d.expected))
            {
                return Err(integrity("citation verification escaped selected inputs"));
            }
            if start.elapsed().as_millis() >= u128::from(limits.max_elapsed_ms) {
                return Err(budget("elapsed source citation proof deadline exceeded"));
            }
        }
        if guards.len() > 128 {
            return Err(budget(
                "Page citations exceed 128 publication read dependencies",
            ));
        }
        Ok((
            presentations,
            guards
                .into_iter()
                .map(|(path, expected)| ReadDependency { path, expected })
                .collect(),
        ))
    }
}
fn add_proof_bytes(total: usize, bytes: usize, limits: &VerificationBudget) -> Result<usize> {
    total
        .checked_add(bytes)
        .filter(|n| *n <= limits.max_bytes)
        .ok_or_else(|| budget("source citation proof exceeds 64 MiB"))
}
fn payload_path(
    path: &VaultRelativePath,
    record: &CanonicalRecord,
    field: &str,
) -> Result<VaultRelativePath> {
    let parent = path
        .as_str()
        .rsplit_once('/')
        .map(|(p, _)| p)
        .ok_or_else(|| integrity("Revision directory is absent"))?;
    let name = record
        .string(field)
        .ok_or_else(|| integrity("Revision payload declaration is absent"))?;
    VaultRelativePath::new(format!("{parent}/{name}"))
}
fn observed_state(source: &CanonicalRecord, reference: &SourceSpanRef) -> CitationState {
    if source.string("wiki_status") == Some("withdrawn") {
        CitationState::Withdrawn
    } else if source.string("wiki_current_revision") == Some(reference.source_revision.as_str()) {
        CitationState::Current
    } else {
        CitationState::Historical
    }
}

/// A display destination constructed solely from two validated canonical paths.
/// Parent traversal is deliberately never passed through VaultRelativePath.
struct SourceLink(String);
impl SourceLink {
    fn relative(page: &VaultRelativePath, target: &VaultRelativePath) -> Result<Self> {
        let mut parent: Vec<_> = page.as_str().split('/').collect();
        parent.pop();
        let target_parts: Vec<_> = target.as_str().split('/').collect();
        let common = parent
            .iter()
            .zip(&target_parts)
            .take_while(|(a, b)| a == b)
            .count();
        let mut parts = vec![".."; parent.len() - common];
        parts.extend_from_slice(&target_parts[common..]);
        // Check the unencoded display path resolves back to its authenticated target.
        let mut resolved = parent;
        for part in &parts {
            if *part == ".." {
                resolved
                    .pop()
                    .ok_or_else(|| integrity("derived citation link escapes the vault"))?;
            } else {
                resolved.push(part);
            }
        }
        if resolved.join("/") != target.as_str() {
            return Err(integrity(
                "derived citation link differs from authenticated target",
            ));
        }
        let raw = parts.join("/");
        let mut encoded = String::new();
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for byte in raw.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
                encoded.push(char::from(byte));
            } else {
                encoded.push('%');
                encoded.push(char::from(HEX[usize::from(byte >> 4)]));
                encoded.push(char::from(HEX[usize::from(byte & 15)]));
            }
        }
        // Prevent a first component that looks like an external URI scheme.
        Ok(Self(if encoded.starts_with("../") {
            encoded
        } else {
            format!("./{encoded}")
        }))
    }
}
fn render_block(
    page: &VaultRelativePath,
    refs: &PageSourceRefs,
    presentations: &[CitationPresentation],
) -> Result<String> {
    if refs.is_empty() {
        return Ok(String::new());
    }
    let mut block = format!("{BEGIN}\n\n### Sources\n\n");
    for (index, presentation) in presentations.iter().enumerate() {
        let mut content = SourceLink::relative(page, &presentation.content_path)?;
        if let Some(anchor) = &presentation.capture_anchor {
            content.0.push_str(&format!("#^{anchor}"));
        }
        let title = markdown_link_title(&if presentation.capture_anchor.is_some() {
            format!(
                "{}: Saved copy from {}",
                presentation.source_title, presentation.captured_at
            )
        } else {
            presentation.source_title.clone()
        });
        let status = match presentation.state {
            CitationState::Current => "",
            CitationState::Historical => " (older saved copy)",
            CitationState::Withdrawn => " (withdrawn)",
        };
        block.push_str(&format!(
            "{}. [{title}]({}){status}\n",
            index + 1,
            content.0
        ));
    }
    block.push_str("\nSource status checked when this page was saved.\n\n<!--\n");
    // Keep exact references in the Markdown without exposing storage details in
    // ordinary reading. JSON Unicode escapes are lossless and prevent comment
    // termination, reserved-marker injection and HTML comment syntax hazards.
    let encoded = serde_json::to_string_pretty(refs)
        .map_err(|e| WikiError::invalid(e.to_string()))?
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('-', "\\u002d");
    block.push_str(&encoded);
    block.push_str(&format!("\n-->\n\n{END}"));
    Ok(block)
}
fn markdown_link_title(title: &str) -> String {
    let mut escaped = String::new();
    for character in title.chars() {
        if character.is_control() || matches!(character, '\u{2028}' | '\u{2029}') {
            escaped.push(' ');
        } else {
            if character.is_ascii_punctuation() {
                escaped.push('\\');
            }
            escaped.push(character);
        }
    }
    escaped
}
fn owned_block(text: &str) -> Result<Option<std::ops::Range<usize>>> {
    let markers: Vec<_> = text.match_indices(RESERVED).collect();
    if markers.is_empty() {
        return Ok(None);
    }
    if markers.len() != 2 {
        return Err(WikiError::invalid(
            "Page has duplicate or malformed source citation markers",
        ));
    }
    // Each ownership marker must be its own HTML comment block. A marker
    // inside a raw HTML container or fenced example grants no ownership.
    let html: Vec<_> = pulldown_cmark::Parser::new(text)
        .into_offset_iter()
        .filter_map(|(event, range)| {
            matches!(
                event,
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::HtmlBlock)
            )
            .then_some(range)
        })
        .collect();
    let mut ranges = Vec::new();
    for ((start, _), expected) in markers.into_iter().zip([BEGIN, END]) {
        let end = start + expected.len();
        if !text[start..].starts_with(expected)
            || !html.iter().any(|range| {
                range.start == start
                    && text[range.clone()].trim_end_matches(['\r', '\n']) == expected
            })
            || (start != 0 && text.as_bytes()[start - 1] != b'\n')
            || !(end == text.len()
                || text[end..].starts_with('\n')
                || text[end..].starts_with("\r\n"))
        {
            return Err(WikiError::invalid(
                "Page has malformed source citation markers",
            ));
        }
        ranges.push(start..end);
    }
    Ok(Some(ranges[0].start..ranges[1].end))
}

/// Preserve the exact targets of the owned block when a Page moves. This is a
/// display-link transformation; it makes no new freshness or evidence claim.
pub(crate) fn rebase_source_citation_links(
    bytes: &[u8],
    from: &VaultRelativePath,
    to: &VaultRelativePath,
) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(bytes).map_err(|_| WikiError::invalid("Page is not UTF-8"))?;
    let range = match owned_block(text) {
        Ok(Some(range)) => range,
        Ok(None) => return Ok(bytes.to_vec()),
        Err(error) => {
            // Ordinary uncited authoring may contain literal examples. Only
            // actual standalone reserved HTML comments opt a move into owned
            // citation semantics; examples inside code/raw HTML remain author text.
            let has_owned_marker =
                pulldown_cmark::Parser::new(text)
                    .into_offset_iter()
                    .any(|(event, range)| {
                        matches!(
                            event,
                            pulldown_cmark::Event::Start(pulldown_cmark::Tag::HtmlBlock)
                        ) && text[range]
                            .trim_end_matches(['\r', '\n'])
                            .starts_with(RESERVED)
                    });
            if has_owned_marker {
                return Err(error);
            }
            return Ok(bytes.to_vec());
        }
    };
    let block = &text[range.clone()];
    let mut edits = Vec::new();
    for link in crate::records::extract_links(block) {
        let raw = &block[link.range.clone()];
        let at = raw
            .find("](")
            .ok_or_else(|| WikiError::invalid("generated source link is not inline Markdown"))?
            + 2;
        if !raw.ends_with(')') || raw[at..raw.len() - 1] != link.destination {
            return Err(WikiError::invalid(
                "generated source link cannot be safely rebased",
            ));
        }
        let mut decoded = Vec::new();
        let href = link.destination.as_bytes();
        let mut cursor = 0;
        while cursor < href.len() {
            if href[cursor] == b'%' {
                let hex = href
                    .get(cursor + 1..cursor + 3)
                    .ok_or_else(|| WikiError::invalid("invalid encoded source link"))?;
                let hex = std::str::from_utf8(hex)
                    .map_err(|_| WikiError::invalid("invalid encoded source link"))?;
                decoded.push(
                    u8::from_str_radix(hex, 16)
                        .map_err(|_| WikiError::invalid("invalid encoded source link"))?,
                );
                cursor += 3;
            } else {
                decoded.push(href[cursor]);
                cursor += 1;
            }
        }
        let decoded = std::str::from_utf8(&decoded)
            .map_err(|_| WikiError::invalid("source link is not UTF-8"))?;
        if decoded.starts_with('/') || decoded.contains(['\\', '\0']) {
            return Err(WikiError::invalid(
                "generated source link is not an in-vault relative path",
            ));
        }
        let (decoded, fragment) = decoded
            .split_once('#')
            .map_or((decoded, String::new()), |(path, fragment)| {
                (path, format!("#{fragment}"))
            });
        let mut target: Vec<_> = from.as_str().split('/').collect();
        target.pop();
        for component in decoded.split('/') {
            match component {
                "." => {}
                ".." => {
                    target
                        .pop()
                        .ok_or_else(|| WikiError::invalid("source link escapes the vault"))?;
                }
                "" => {
                    return Err(WikiError::invalid(
                        "generated source link has an empty component",
                    ));
                }
                value => target.push(value),
            }
        }
        let target = VaultRelativePath::new(target.join("/"))?;
        if format!("{}{}", SourceLink::relative(from, &target)?.0, fragment) != link.destination {
            return Err(WikiError::invalid(
                "source link is not a canonical generated destination",
            ));
        }
        let replacement = format!("{}{}", SourceLink::relative(to, &target)?.0, fragment);
        edits.push((
            range.start + link.range.start + at..range.start + link.range.end - 1,
            replacement,
        ));
    }
    edits.sort_by_key(|(range, _)| range.start);
    let mut result = Vec::new();
    let mut cursor = 0;
    for (range, value) in edits {
        if range.start < cursor || range.end > bytes.len() {
            return Err(WikiError::invalid(
                "generated source link edit ranges overlap",
            ));
        }
        result.extend_from_slice(&bytes[cursor..range.start]);
        result.extend_from_slice(value.as_bytes());
        cursor = range.end;
    }
    result.extend_from_slice(&bytes[cursor..]);
    if result.len() > MAX_INPUT_BYTES {
        return Err(budget("rebased Page exceeds 16 MiB"));
    }
    Ok(result)
}
fn replace_block(
    text: &str,
    owned: Option<std::ops::Range<usize>>,
    block: &str,
) -> Result<Vec<u8>> {
    let (prefix, suffix, separator) = if let Some(range) = owned {
        (&text[..range.start], &text[range.end..], "")
    } else if block.is_empty() {
        (text, "", "")
    } else {
        (text, "", if text.ends_with('\n') { "\n" } else { "\n\n" })
    };
    let length = prefix
        .len()
        .checked_add(suffix.len())
        .and_then(|n| n.checked_add(separator.len()))
        .and_then(|n| n.checked_add(block.len()))
        .ok_or_else(|| budget("rendered Page exceeds 16 MiB"))?;
    if length > MAX_INPUT_BYTES {
        return Err(budget("rendered Page exceeds 16 MiB"));
    }
    let mut bytes = Vec::with_capacity(length);
    bytes.extend_from_slice(prefix.as_bytes());
    bytes.extend_from_slice(separator.as_bytes());
    bytes.extend_from_slice(block.as_bytes());
    bytes.extend_from_slice(suffix.as_bytes());
    Ok(bytes)
}
