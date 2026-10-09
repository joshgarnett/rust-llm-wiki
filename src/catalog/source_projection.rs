//! Semantic admission for bounded scalar and joint source refreshes. This module never invokes
//! a full graph validator over a partial graph and never scans canonical history.
use super::{
    eligibility::{self, eligible_opposition, opposition_key},
    eligibility_facts::{EligibilityBaseline, EligibilityEdge, EligibilityFact, EligibilityRole},
    eligibility_rules::{self as rules, GenerationContext},
    link_facts::{self, MatchKey, OwnedLinkFact},
    navigation_resolution::{self, NavigationResolution, RegistryCandidate, RegistryProbe},
    normalized_delta::{
        CatalogDelta, DocumentMutation, OwnedClaims, OwnedDiagnostics, OwnedLinks,
        RevisionIdentityRow,
    },
    normalized_fact_delta::{OwnedLinkFacts, OwnedRegistryKeys, RecordFactMutation},
    query::QuerySnapshot,
    query_types::QueryCatalog,
    row_projection, scan,
    types::{CatalogDiagnostic, IdentityClaimRow, LinkRow, RecordRow},
};
use crate::{
    changes::{
        ChangeDraft, ProposedTarget, ReadDependency, ScanDocument, ValidationInput,
        prepare::read_bounded,
    },
    domain::{
        Blake3Hash, CanonicalRecord, Eligibility, ErrorCode, ReadSnapshot, RecordId, RecordKind,
        Result, VaultRelativePath, WikiError,
    },
    records::{LinkResolution, ParsedNote, RegistryEntry, extract_links, parse_note},
    sources::{
        IndexedSourceRefreshPlan, SourceRefreshLookup, SourceView, revision::canonical_path,
    },
    vault::{ExpectedState, VaultFs},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::{Duration, Instant},
};

pub(crate) struct RefreshProjectionLimits {
    pub max_rows: usize,
    pub max_file_bytes: usize,
    pub max_canonical_bytes: usize,
    pub max_elapsed: Duration,
}
impl Default for RefreshProjectionLimits {
    fn default() -> Self {
        Self {
            max_rows: 4096,
            max_file_bytes: 64 * 1024 * 1024,
            max_canonical_bytes: 256 * 1024 * 1024,
            max_elapsed: Duration::from_secs(30),
        }
    }
}
/// Only successful complete projection constructs this capability.
pub(crate) struct ProjectedSourceRefresh {
    parts: ProjectedRefreshParts,
}
pub(super) struct ProjectedRefreshParts {
    pub draft: ChangeDraft,
    pub base: ReadSnapshot,
    pub source_id: RecordId,
    pub before: Vec<ReadDependency>,
    pub after: Vec<ReadDependency>,
    pub delta: CatalogDelta,
}
impl ProjectedSourceRefresh {
    pub(crate) fn draft(&self) -> &ChangeDraft {
        &self.parts.draft
    }

    pub(super) fn into_parts(self) -> ProjectedRefreshParts {
        self.parts
    }
}
pub(super) fn corrupt(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
pub(super) fn conflict(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
pub(super) fn budget() -> WikiError {
    WikiError::new(
        ErrorCode::BudgetExceeded,
        "source projection exceeds cumulative allowance",
    )
}
pub(super) fn typed(field: &str) -> EligibilityRole {
    EligibilityRole::TypedReference {
        field: field.into(),
    }
}
pub(super) fn record_id(record: &CanonicalRecord, field: &str) -> Result<RecordId> {
    RecordId::new(
        record
            .string(field)
            .ok_or_else(|| corrupt(format!("record lacks {field}")))?,
    )
}
pub(super) fn baseline() -> EligibilityBaseline {
    EligibilityBaseline {
        eligibility: Eligibility::Current,
        reasons: vec![],
        identity_eligibility: None,
        description_eligibility: None,
        disputed: false,
    }
}
pub(super) fn entry(row: &RecordRow) -> RegistryEntry {
    RegistryEntry {
        id: row.record.id().clone(),
        kind: row.record.kind(),
        path: row.path.clone(),
        aliases: scan::list(&row.record, "aliases"),
    }
}
pub(super) fn state(bytes: Option<&[u8]>) -> ExpectedState {
    bytes.map_or(ExpectedState::Absent, |bytes| {
        ExpectedState::Hash(Blake3Hash::digest(bytes))
    })
}
pub(super) fn deps(map: BTreeMap<VaultRelativePath, ExpectedState>) -> Vec<ReadDependency> {
    map.into_iter()
        .map(|(path, expected)| ReadDependency { path, expected })
        .collect()
}

pub(super) struct Work<'a> {
    pub(super) fs: &'a VaultFs,
    pub(super) reader: &'a QuerySnapshot,
    pub(super) limits: &'a RefreshProjectionLimits,
    pub(super) started: Instant,
    pub(super) bytes: usize,
    pub(super) visits: usize,
    pub(super) before: BTreeMap<VaultRelativePath, ExpectedState>,
    pub(super) captured: BTreeMap<VaultRelativePath, ScanDocument>,
    pub(super) old: BTreeMap<RecordId, RecordRow>,
    pub(super) now: BTreeMap<RecordId, RecordRow>,
    pub(super) facts: BTreeMap<RecordId, EligibilityFact>,
    pub(super) overlay: BTreeMap<VaultRelativePath, Vec<u8>>,
    pub(super) removed_paths: BTreeSet<VaultRelativePath>,
    pub(super) dynamic: BTreeSet<RecordId>,
    pub(super) support: BTreeMap<RecordId, BTreeSet<RecordId>>,
    pub(super) declared: BTreeMap<RecordId, BTreeSet<RecordId>>,
    pub(super) generation: BTreeMap<RecordId, Option<RecordId>>,
    pub(super) lifecycle_diagnostics: BTreeMap<RecordId, Option<CatalogDiagnostic>>,
    pub(super) opposition: BTreeMap<eligibility::OppositionKey, Vec<(RecordId, bool)>>,
    pub(super) new_registry: Vec<RegistryEntry>,
    pub(super) replaced_registry: BTreeSet<VaultRelativePath>,
    /// Owners whose complete structural before/after states passed Page
    /// admission. Their already-invalid references may change diagnostic shape.
    /// Refresh leaves this empty and keeps its stricter navigation-only guard.
    pub(super) admitted_typed_navigation: BTreeSet<VaultRelativePath>,
    pub(super) edge_overrides: BTreeMap<RecordId, BTreeSet<EligibilityEdge>>,
    pub(super) key_cache: BTreeMap<MatchKey, RegistryProbe>,
}
impl<'a> Work<'a> {
    pub(super) fn new(
        fs: &'a VaultFs,
        reader: &'a QuerySnapshot,
        limits: &'a RefreshProjectionLimits,
    ) -> Result<Self> {
        let ceiling = RefreshProjectionLimits::default();
        if limits.max_rows == 0
            || limits.max_rows > ceiling.max_rows
            || limits.max_file_bytes == 0
            || limits.max_file_bytes > ceiling.max_file_bytes
            || limits.max_canonical_bytes == 0
            || limits.max_canonical_bytes > ceiling.max_canonical_bytes
            || limits.max_elapsed.is_zero()
            || limits.max_elapsed > ceiling.max_elapsed
        {
            return Err(budget());
        }
        reader.require_fact_layout()?;
        Ok(Self {
            fs,
            reader,
            limits,
            started: Instant::now(),
            bytes: 0,
            visits: 0,
            before: BTreeMap::new(),
            captured: BTreeMap::new(),
            old: BTreeMap::new(),
            now: BTreeMap::new(),
            facts: BTreeMap::new(),
            overlay: BTreeMap::new(),
            removed_paths: BTreeSet::new(),
            dynamic: BTreeSet::new(),
            support: BTreeMap::new(),
            declared: BTreeMap::new(),
            generation: BTreeMap::new(),
            lifecycle_diagnostics: BTreeMap::new(),
            opposition: BTreeMap::new(),
            new_registry: vec![],
            replaced_registry: BTreeSet::new(),
            admitted_typed_navigation: BTreeSet::new(),
            edge_overrides: BTreeMap::new(),
            key_cache: BTreeMap::new(),
        })
    }
}
impl Work<'_> {
    pub(super) fn tick(&mut self) -> Result<()> {
        self.visits = self
            .visits
            .checked_add(1)
            .filter(|n| *n <= self.limits.max_rows * 32)
            .ok_or_else(budget)?;
        if self.started.elapsed() > self.limits.max_elapsed {
            return Err(budget());
        }
        Ok(())
    }
    pub(super) fn charge(&mut self, bytes: usize) -> Result<()> {
        self.tick()?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= self.limits.max_canonical_bytes)
            .ok_or_else(budget)?;
        Ok(())
    }
    pub(super) fn observe(
        &mut self,
        path: &VaultRelativePath,
        expected: ExpectedState,
    ) -> Result<()> {
        if self.before.get(path).is_some_and(|old| old != &expected) {
            return Err(conflict(format!("selected state changed: {path}")));
        }
        if !self.before.contains_key(path) && self.before.len() >= self.limits.max_rows {
            return Err(budget());
        }
        self.before.insert(path.clone(), expected);
        Ok(())
    }
    pub(super) fn capture(
        &mut self,
        path: &VaultRelativePath,
        expected: &ExpectedState,
    ) -> Result<()> {
        self.tick()?;
        if let Some(doc) = self.captured.get(path) {
            if expected != &ExpectedState::Hash(doc.hash.clone()) {
                return Err(conflict(format!("captured state changed: {path}")));
            }
            return self.observe(path, expected.clone());
        }
        let remaining = self.limits.max_canonical_bytes.saturating_sub(self.bytes);
        let bytes = read_bounded(self.fs, path, self.limits.max_file_bytes.min(remaining))?;
        self.charge(bytes.as_ref().map_or(0, Vec::len))?;
        if &state(bytes.as_deref()) != expected {
            return Err(conflict(format!("selected file changed: {path}")));
        }
        self.observe(path, expected.clone())?;
        if let Some(bytes) = bytes {
            self.captured.insert(
                path.clone(),
                ScanDocument {
                    path: path.clone(),
                    hash: Blake3Hash::digest(&bytes),
                    bytes,
                },
            );
        }
        Ok(())
    }
    pub(super) fn load(&mut self, id: &RecordId) -> Result<bool> {
        self.tick()?;
        // A move retires the old path while preserving the proposed identity.
        // Deletion removes the proposed row, so its old-path tombstone still
        // prevents reloading that identity from the pinned catalog below.
        if let Some(row) = self.now.get(id) {
            if self.removed_paths.contains(&row.path) {
                return Err(corrupt("surviving proposed record has a removed path"));
            }
            return Ok(true);
        }
        if self
            .old
            .get(id)
            .is_some_and(|row| self.removed_paths.contains(&row.path))
        {
            return Ok(false);
        }
        if self.now.len() >= self.limits.max_rows {
            return Err(budget());
        }
        let Some(row) = self.reader.record(id)? else {
            return Ok(false);
        };
        if !row.dependencies.is_empty() {
            return Err(corrupt(
                "normalized source projection encountered legacy proofs",
            ));
        }
        let fact = self
            .reader
            .eligibility_fact(id)?
            .ok_or_else(|| corrupt("selected record lacks static eligibility fact"))?;
        self.capture(&row.path, &ExpectedState::Hash(row.hash.clone()))?;
        let note = parse_note(&self.captured[&row.path].bytes);
        if note.canonical.as_ref() != Some(&row.record) {
            return Err(conflict(
                "selected canonical bytes disagree with pinned record",
            ));
        }
        self.old.insert(id.clone(), row.clone());
        self.now.insert(id.clone(), row);
        self.facts.insert(id.clone(), fact);
        Ok(true)
    }
    pub(super) fn require(&mut self, id: &RecordId) -> Result<()> {
        if self.load(id)? {
            Ok(())
        } else {
            Err(corrupt(format!("selected boundary record missing: {id}")))
        }
    }
    pub(super) fn mark(&mut self, id: &RecordId) -> Result<bool> {
        self.require(id)?;
        Ok(self.dynamic.insert(id.clone()))
    }
    pub(super) fn note(&self, path: &VaultRelativePath) -> Result<ParsedNote> {
        if self.removed_paths.contains(path) {
            return Err(corrupt("selected proposed note is absent"));
        }
        let bytes = self
            .overlay
            .get(path)
            .map(Vec::as_slice)
            .or_else(|| self.captured.get(path).map(|doc| doc.bytes.as_slice()))
            .ok_or_else(|| corrupt("selected note was not captured"))?;
        Ok(parse_note(bytes))
    }
    pub(super) fn edges(
        &mut self,
        id: &RecordId,
        roles: &[EligibilityRole],
        reverse: bool,
    ) -> Result<Vec<EligibilityEdge>> {
        self.tick()?;
        let mut edges = if reverse {
            self.reader.dependent_edges(id, roles)?
        } else {
            self.reader.outgoing_edges(id, roles)?
        };
        edges.retain(|edge| !self.edge_overrides.contains_key(&edge.owner_id));
        for values in self.edge_overrides.values() {
            edges.extend(
                values
                    .iter()
                    .filter(|edge| {
                        roles.contains(&edge.role)
                            && if reverse {
                                &edge.target_id == id
                            } else {
                                &edge.owner_id == id
                            }
                    })
                    .cloned(),
            );
        }
        for _ in &edges {
            self.tick()?;
        }
        Ok(edges)
    }
    pub(super) fn lifecycle_boundary(&mut self, id: &RecordId) -> Result<()> {
        let row = self.now[id].clone();
        if self.facts[id].baseline.eligibility == Eligibility::Invalid {
            if row.record.kind() == RecordKind::RunEvent && self.edge_overrides.contains_key(id) {
                self.edge_overrides
                    .get_mut(id)
                    .unwrap()
                    .retain(|edge| edge.role != EligibilityRole::GenerationPacket);
            }
            return Ok(());
        }
        match row.record.kind() {
            RecordKind::Revision | RecordKind::Evidence | RecordKind::ExtractionPacket => {
                self.require(&record_id(&row.record, "wiki_source_id")?)?;
            }
            RecordKind::Extraction => {
                for revision in scan::list(&row.record, "wiki_source_revision_ids") {
                    let revision = RecordId::new(revision)?;
                    self.require(&revision)?;
                    let source = record_id(&self.now[&revision].record, "wiki_source_id")?;
                    self.require(&source)?;
                }
            }
            RecordKind::RunEvent
                if row.record.string("wiki_event_type") == Some("generation_output") =>
            {
                let edges = self.edges(id, &[EligibilityRole::GenerationPacket], false)?;
                if edges.len() > 1 {
                    return Err(corrupt("generation output has multiple bound packets"));
                }
                let packet = if self.edge_overrides.contains_key(id) {
                    // Full rebuild records this body-derived edge only for
                    // structurally valid outputs. Restoration must reconstruct
                    // the binding rather than treating an absent old edge as
                    // proof that the unchanged body is unbound.
                    let note = self.note(&row.path)?;
                    let output = crate::graph::packet::fenced_json(
                        &note,
                        "lwiki-api-extraction-output-v1",
                        crate::graph::MAX_ARTIFACT_BYTES,
                    )
                    .ok()
                    .and_then(|json| {
                        crate::graph::packet::decode::<
                            crate::graph::generation_cache::GenerationOutput,
                        >(json, crate::graph::MAX_ARTIFACT_BYTES)
                        .ok()
                    });
                    let mut bound = None;
                    if let Some(output) = output {
                        self.load(&output.packet_id)?;
                        if output.version == 1
                            && output.attempt.run_id.as_str()
                                == row.record.string("wiki_run_id").unwrap_or_default()
                            && output.attempt.task_key == output.task_key
                            && Blake3Hash::digest(output.response.as_bytes())
                                == output.response_hash
                            && self.now.get(&output.packet_id).is_some_and(|packet| {
                                packet.record.kind() == RecordKind::ExtractionPacket
                                    && packet.record.string("wiki_packet_fingerprint")
                                        == Some(output.packet_fingerprint.as_str())
                            })
                        {
                            bound = Some(output.packet_id);
                        }
                    }
                    let edges = self.edge_overrides.get_mut(id).unwrap();
                    edges.retain(|edge| edge.role != EligibilityRole::GenerationPacket);
                    if let Some(packet) = &bound {
                        edges.insert(EligibilityEdge {
                            owner_id: id.clone(),
                            target_id: packet.clone(),
                            role: EligibilityRole::GenerationPacket,
                        });
                    }
                    bound
                } else {
                    edges.first().map(|e| e.target_id.clone())
                };
                if let Some(packet) = &packet {
                    self.require(packet)?;
                    let source = record_id(&self.now[packet].record, "wiki_source_id")?;
                    self.load(&source)?;
                }
                self.generation.insert(id.clone(), packet);
            }
            _ => {}
        }
        Ok(())
    }
    pub(super) fn discover(&mut self, seeds: BTreeSet<RecordId>) -> Result<()> {
        let mut queue: VecDeque<_> = seeds.into_iter().collect();
        let mut seen = BTreeSet::new();
        while let Some(id) = queue.pop_front() {
            self.tick()?;
            if !seen.insert(id.clone()) {
                continue;
            }
            self.mark(&id)?;
            self.lifecycle_boundary(&id)?;
            let row = self.now[&id].clone();
            if row.record.kind() == RecordKind::Evidence {
                let assertion = record_id(&row.record, "wiki_assertion_id")?;
                if self.load(&assertion)? {
                    queue.push_back(assertion);
                } else if self.facts[&id].baseline.eligibility != Eligibility::Invalid {
                    return Err(corrupt("valid evidence lacks its assertion boundary"));
                }
            }
            if row.record.kind() == RecordKind::Assertion {
                let edges = self.edges(&id, &[EligibilityRole::AssertionEvidence], false)?;
                let members: BTreeSet<_> = edges.into_iter().map(|e| e.target_id).collect();
                for member in &members {
                    self.require(member)?;
                    if self.now[member].record.kind() != RecordKind::Evidence
                        || self.now[member].record.string("wiki_assertion_id") != Some(id.as_str())
                    {
                        return Err(corrupt("support edge disagrees with canonical membership"));
                    }
                }
                // Full compute aggregates evidence BEFORE its declared-support
                // propagation pass. Reconstruct that stage for every member,
                // including unchanged other-source support; cached final state
                // is not necessarily its pre-dependency lifecycle state.
                queue.extend(members.iter().cloned());
                self.support.insert(id.clone(), members);
                if row.record.string("wiki_status") == Some("accepted") {
                    let (key, _) = opposition_key(&row.record)
                        .ok_or_else(|| corrupt("assertion lacks proposition"))?;
                    if !self.opposition.contains_key(&key) {
                        let members = self.reader.opposition_members(&key)?;
                        if !members.iter().any(|(member, _)| member == &id) {
                            return Err(corrupt(
                                "accepted assertion omitted from proposition group",
                            ));
                        }
                        for (member, polarity) in &members {
                            self.require(member)?;
                            if self.now[member].record.string("wiki_status") != Some("accepted")
                                || opposition_key(&self.now[member].record)
                                    != Some((key.clone(), *polarity))
                            {
                                return Err(corrupt(
                                    "opposition group differs from canonical proposition",
                                ));
                            }
                            queue.push_back(member.clone());
                        }
                        self.opposition.insert(key, members);
                    }
                }
            }
            let targets: BTreeSet<_> = scan::list(&row.record, "wiki_depends_on_ids")
                .into_iter()
                .map(RecordId::new)
                .collect::<Result<_>>()?;
            let edges = self.edges(&id, &[EligibilityRole::DeclaredSupport], false)?;
            if edges.iter().map(|e| &e.target_id).collect::<BTreeSet<_>>()
                != targets.iter().collect()
            {
                return Err(corrupt("declared support edge set is incomplete"));
            }
            for target in &targets {
                self.load(target)?;
            }
            self.declared.insert(id.clone(), targets);
            for edge in self.edges(&id, &[EligibilityRole::DeclaredSupport], true)? {
                queue.push_back(edge.owner_id);
            }
        }
        Ok(())
    }
    pub(super) fn recompute(&mut self) -> Result<()> {
        let ids: Vec<_> = self.dynamic.iter().cloned().collect();
        for id in &ids {
            rules::restore_baseline(self.now.get_mut(id).unwrap(), &self.facts[id].baseline);
        }
        // Dynamic source facts are read from an explicit finite boundary. These
        // snapshots are the admitted workset, never the whole published catalog.
        let boundary = self.now.clone();
        for id in &ids {
            self.tick()?;
            let row = &boundary[id];
            let context = if row.record.kind() == RecordKind::RunEvent
                && row.record.string("wiki_event_type") == Some("generation_output")
                && row.eligibility != Eligibility::Invalid
            {
                self.generation_context(row, &boundary)?
            } else {
                GenerationContext::NotGeneration
            };
            let diagnostic = rules::lifecycle(self.now.get_mut(id).unwrap(), &boundary, context)?;
            self.lifecycle_diagnostics.insert(id.clone(), diagnostic);
        }
        let boundary = self.now.clone();
        for id in &ids {
            if self.now[id].record.kind() == RecordKind::Evidence
                && self.now[id].eligibility != Eligibility::Invalid
            {
                let source = record_id(&self.now[id].record, "wiki_source_id")?;
                rules::evidence_lifecycle(
                    self.now.get_mut(id).unwrap(),
                    &boundary
                        .get(&source)
                        .ok_or_else(|| corrupt("evidence source boundary omitted"))?
                        .record,
                )?;
            }
        }
        let boundary = self.now.clone();
        for (id, members) in &self.support {
            let evidence: Vec<_> = members
                .iter()
                .map(|id| {
                    boundary
                        .get(id)
                        .ok_or_else(|| corrupt("support member omitted"))
                })
                .collect::<Result<_>>()?;
            rules::assertion_support(self.now.get_mut(id).unwrap(), members, &evidence)?;
        }
        for _ in 0..=ids.len() {
            self.tick()?;
            let snapshot = self.now.clone();
            let mut changed = false;
            for id in &ids {
                let targets = self
                    .declared
                    .get(id)
                    .ok_or_else(|| corrupt("declared boundary omitted"))?;
                let states = targets
                    .iter()
                    .map(|id| (id.clone(), snapshot.get(id).map(|row| row.eligibility)))
                    .collect();
                changed |= rules::declared_dependencies(self.now.get_mut(id).unwrap(), &states)?;
            }
            if !changed {
                break;
            }
        }
        for members in self.opposition.values() {
            let mut sides = [false, false];
            for (id, polarity) in members {
                if eligible_opposition(&self.now[id]) {
                    sides[usize::from(*polarity)] = true;
                }
            }
            if sides == [true, true] {
                for (id, _) in members {
                    if eligible_opposition(&self.now[id]) {
                        rules::mark_opposition(self.now.get_mut(id).unwrap());
                    }
                }
            }
        }
        for id in ids {
            rules::finalize(self.now.get_mut(&id).unwrap());
        }
        Ok(())
    }
    pub(super) fn generation_context<'b>(
        &self,
        row: &RecordRow,
        boundary: &'b BTreeMap<RecordId, RecordRow>,
    ) -> Result<GenerationContext<'b>> {
        let Some(packet_id) = self
            .generation
            .get(row.record.id())
            .ok_or_else(|| corrupt("generation context not discovered"))?
        else {
            return Ok(GenerationContext::Unbound);
        };
        let packet = boundary
            .get(packet_id)
            .ok_or_else(|| corrupt("generation packet omitted"))?;
        let note = self.note(&row.path)?;
        let json = crate::graph::packet::fenced_json(
            &note,
            "lwiki-api-extraction-output-v1",
            crate::graph::MAX_ARTIFACT_BYTES,
        )?;
        let out: crate::graph::generation_cache::GenerationOutput =
            crate::graph::packet::decode(json, crate::graph::MAX_ARTIFACT_BYTES)?;
        if out.version != 1
            || &out.packet_id != packet_id
            || out.attempt.run_id.as_str() != row.record.string("wiki_run_id").unwrap_or_default()
            || out.attempt.task_key != out.task_key
            || Blake3Hash::digest(out.response.as_bytes()) != out.response_hash
            || packet.record.kind() != RecordKind::ExtractionPacket
            || packet.record.string("wiki_packet_fingerprint")
                != Some(out.packet_fingerprint.as_str())
        {
            return Err(corrupt("generation role differs from authenticated body"));
        }
        let source = record_id(&packet.record, "wiki_source_id")?;
        Ok(GenerationContext::Bound {
            packet: &packet.record,
            source: boundary
                .get(&source)
                .filter(|r| r.record.kind() == RecordKind::Source)
                .map(|r| &r.record),
        })
    }
}

struct AdmittedRefresh {
    source_id: RecordId,
    old_head: RecordId,
    new_head: RecordId,
    old_retained: Vec<String>,
    is_new: bool,
    draft: Option<ChangeDraft>,
    content: Option<Vec<u8>>,
}

/// Validate a scalar generated envelope without discovering or sealing its
/// downstream graph. A batch installs every envelope into the same Work first.
fn admit_refresh(
    work: &mut Work<'_>,
    plan: IndexedSourceRefreshPlan,
    deduplicate_captured: bool,
) -> Result<AdmittedRefresh> {
    let limits = work.limits;
    if QueryCatalog::snapshot(work.reader) != &plan.base_snapshot
        || plan.base_snapshot.publication().is_none()
    {
        return Err(conflict("source plan differs from pinned publication"));
    }
    for document in plan.captured {
        work.charge(document.bytes.len())?;
        if document.bytes.len() > limits.max_file_bytes
            || Blake3Hash::digest(&document.bytes) != document.hash
        {
            return Err(conflict("invalid captured source input"));
        }
        if let Some(prior) = work.captured.get(&document.path) {
            if !deduplicate_captured || prior.hash != document.hash || prior.bytes != document.bytes
            {
                return Err(conflict("invalid overlapping captured source input"));
            }
        }
        work.observe(&document.path, ExpectedState::Hash(document.hash.clone()))?;
        work.captured
            .entry(document.path.clone())
            .or_insert(document);
    }
    for dependency in &plan.plan.dependencies {
        work.capture(&dependency.path, &dependency.expected)?;
    }
    let source_id = plan.plan.source_id.clone();
    let old_head = plan.previous_revision.clone();
    let new_head = plan.plan.revision_id.clone();
    work.require(&source_id)?;
    work.require(&old_head)?;
    let original = work.now[&source_id].clone();
    if original.record.kind() != RecordKind::Source
        || original.eligibility == Eligibility::Invalid
        || original.record.string("wiki_current_revision") != Some(old_head.as_str())
    {
        return Err(conflict(
            "source refresh baseline differs from selected head",
        ));
    }
    if deduplicate_captured {
        let head = &work.now[&old_head];
        if head.record.kind() != RecordKind::Revision
            || head.record.string("wiki_source_id") != Some(source_id.as_str())
            || work.facts[&old_head].baseline.eligibility == Eligibility::Invalid
        {
            return Err(conflict(
                "selected batch baseline head is invalid or foreign",
            ));
        }
    }
    let Some(draft) = plan.plan.draft else {
        if new_head != old_head || !plan.plan.reused {
            return Err(conflict("invalid source no-op identity"));
        }
        return Ok(AdmittedRefresh {
            source_id,
            old_head,
            new_head,
            old_retained: scan::list(&original.record, "wiki_revisions"),
            is_new: false,
            draft: None,
            content: None,
        });
    };
    if draft.origin.is_some() || draft.inverse_of.is_some() {
        return Err(conflict(
            "source refresh cannot confer graph or inverse authority",
        ));
    }
    let mut operations = BTreeMap::new();
    for operation in &draft.operations {
        work.tick()?;
        let bytes = operation
            .proposed
            .as_ref()
            .ok_or_else(|| conflict("source refresh cannot delete canonical files"))?;
        if bytes.len() > limits.max_file_bytes
            || operations
                .insert(operation.target.clone(), operation)
                .is_some()
            || work.overlay.contains_key(&operation.target)
        {
            return Err(conflict("invalid or duplicate refresh operation"));
        }
        work.charge(bytes.len())?;
        work.capture(&operation.target, &operation.expected)?;
        work.overlay.insert(operation.target.clone(), bytes.clone());
    }
    let source_operation = operations
        .get(&original.path)
        .ok_or_else(|| conflict("source refresh lacks exact source mutation"))?;
    if source_operation.expected != ExpectedState::Hash(original.hash.clone()) {
        return Err(conflict(
            "source operation before-state differs from selected source",
        ));
    }
    let source_note = work.note(&original.path)?;
    let source_record = source_note
        .canonical
        .as_ref()
        .ok_or_else(|| conflict("source refresh produced invalid source metadata"))?;
    if source_record.id() != &source_id || source_record.kind() != RecordKind::Source {
        return Err(conflict("source refresh changes source identity"));
    }
    let fixed = |name: &&String| {
        !matches!(
            name.as_str(),
            "title" | "wiki_current_revision" | "wiki_revision" | "wiki_revisions"
        )
    };
    if original
        .record
        .fields()
        .iter()
        .filter(|(key, _)| fixed(key))
        .ne(source_record.fields().iter().filter(|(key, _)| fixed(key)))
        || source_note.body() != parse_note(&work.captured[&original.path].bytes).body()
    {
        return Err(conflict("source refresh changed fixed metadata or body"));
    }
    if source_record.string("wiki_current_revision") != Some(new_head.as_str()) {
        return Err(conflict("proposed head differs from planned revision"));
    }
    let old_retained = scan::list(&original.record, "wiki_revisions");
    let mut expected_retained = old_retained.clone();
    let is_new = !plan.plan.reused;
    if is_new {
        expected_retained.push(new_head.to_string());
    }
    if scan::list(source_record, "wiki_revisions") != expected_retained
        || expected_retained.iter().collect::<BTreeSet<_>>().len() != expected_retained.len()
    {
        return Err(conflict(
            "refresh revision inventory is not exact append or reuse",
        ));
    }
    if is_new {
        let revision_path = VaultRelativePath::new(format!(
            "sources/{source_id}/revisions/{new_head}/revision.md"
        ))?;
        if work
            .reader
            .revision_identity_is_reserved(&new_head, &revision_path)?
            || draft.allocated_ids != BTreeMap::from([("revision".into(), new_head.clone())])
        {
            return Err(conflict(
                "new revision identity or path is already claimed, referenced, or unallocated",
            ));
        }
        let note = work.note(&revision_path)?;
        let record = note
            .canonical
            .clone()
            .ok_or_else(|| conflict("new revision is not valid canonical metadata"))?;
        if record.id() != &new_head
            || record.kind() != RecordKind::Revision
            || record.string("wiki_source_id") != Some(source_id.as_str())
        {
            return Err(conflict("new revision owner or identity differs"));
        }
        // This capability admits the capture generator's envelope, not arbitrary
        // record adoption. In particular, new graph edges or aliases would make
        // cached structural and policy facts insufficient.
        if !note.body().is_empty()
            || record.fields().keys().any(|field| {
                !matches!(
                    field.as_str(),
                    "wiki_schema"
                        | "wiki_id"
                        | "wiki_kind"
                        | "title"
                        | "wiki_source_id"
                        | "wiki_source"
                        | "wiki_captured_at"
                        | "wiki_original_path"
                        | "wiki_original_hash"
                        | "wiki_extractor"
                        | "wiki_extractor_fingerprint"
                        | "wiki_extraction_status"
                        | "wiki_media_type"
                        | "wiki_content_path"
                        | "wiki_content_hash"
                )
            })
        {
            return Err(conflict(
                "new revision differs from generated capture metadata",
            ));
        }
        let row = RecordRow {
            record,
            path: revision_path.clone(),
            hash: note.source_hash.clone(),
            authored_status: None,
            eligibility: Eligibility::Current,
            reasons: vec![],
            identity_eligibility: None,
            description_eligibility: None,
            disputed: false,
            dependencies: vec![],
        };
        let mut paths = BTreeSet::from([revision_path.clone()]);
        for field in ["wiki_original_path", "wiki_content_path"] {
            if let Some(name) = row.record.string(field) {
                paths.insert(asset_path(&row, name)?);
            }
        }
        let expected_paths: BTreeSet<_> = paths
            .iter()
            .cloned()
            .chain(std::iter::once(original.path.clone()))
            .collect();
        if operations.keys().cloned().collect::<BTreeSet<_>>() != expected_paths
            || operations
                .iter()
                .any(|(path, op)| path != &original.path && op.expected != ExpectedState::Absent)
        {
            return Err(conflict(
                "source refresh writes outside exact new immutable tree",
            ));
        }
        work.facts.insert(
            new_head.clone(),
            EligibilityFact {
                structural: Default::default(),
                baseline: baseline(),
                direct_paths: paths,
            },
        );
        work.new_registry.push(entry(&row));
        work.now.insert(new_head.clone(), row);
    } else {
        if !draft.allocated_ids.is_empty()
            || operations.len() != 1
            || !old_retained.iter().any(|id| id == new_head.as_str())
        {
            return Err(conflict(
                "historical reuse changes immutable files or inventory",
            ));
        }
        work.require(&new_head)?;
    }
    let mut source = original.clone();
    source.record = source_record.clone();
    source.hash = source_note.source_hash.clone();
    work.now.insert(source_id.clone(), source);
    let head = work.now[&new_head].clone();
    if head.record.kind() != RecordKind::Revision
        || head.record.string("wiki_source_id") != Some(source_id.as_str())
        || work.facts[&new_head].baseline.eligibility == Eligibility::Invalid
    {
        return Err(conflict("selected head is invalid or foreign"));
    }
    Ok(AdmittedRefresh {
        source_id,
        old_head,
        new_head,
        old_retained,
        is_new,
        draft: Some(draft),
        content: None,
    })
}

fn refresh_seeds(work: &mut Work<'_>, refresh: &AdmittedRefresh) -> Result<BTreeSet<RecordId>> {
    let source_id = &refresh.source_id;
    let old_head = &refresh.old_head;
    let new_head = &refresh.new_head;
    let mut seeds = BTreeSet::from([source_id.clone()]);
    if old_head != new_head {
        seeds.extend([old_head.clone(), new_head.clone()]);
        for revision in [old_head, new_head] {
            for edge in work.edges(
                revision,
                &[
                    typed("wiki_source_revision"),
                    EligibilityRole::ExtractionRevision,
                ],
                true,
            )? {
                work.require(&edge.owner_id)?;
                let owner = &work.now[&edge.owner_id];
                let valid = match edge.role {
                    EligibilityRole::ExtractionRevision => {
                        owner.record.kind() == RecordKind::Extraction
                            && scan::list(&owner.record, "wiki_source_revision_ids")
                                .iter()
                                .any(|id| id == revision.as_str())
                    }
                    _ => {
                        matches!(
                            owner.record.kind(),
                            RecordKind::Evidence | RecordKind::ExtractionPacket
                        ) && owner.record.string("wiki_source_revision") == Some(revision.as_str())
                    }
                };
                if !valid {
                    return Err(corrupt(
                        "revision dependent role differs from canonical ownership",
                    ));
                }
                if owner.record.kind() == RecordKind::ExtractionPacket {
                    for output in
                        work.edges(&edge.owner_id, &[EligibilityRole::GenerationPacket], true)?
                    {
                        seeds.insert(output.owner_id);
                    }
                }
                seeds.insert(edge.owner_id);
            }
        }
    }
    Ok(seeds)
}

fn emit_refresh_identity(
    work: &mut Work<'_>,
    refresh: &AdmittedRefresh,
    delta: &mut CatalogDelta,
) -> Result<()> {
    let source_id = &refresh.source_id;
    let old_head = &refresh.old_head;
    let new_head = &refresh.new_head;
    if refresh.is_new {
        let head = &work.now[new_head];
        let required_hash = |field| {
            Blake3Hash::new(
                head.record
                    .string(field)
                    .ok_or_else(|| corrupt(format!("revision missing {field}")))?,
            )
        };
        delta.revisions.push(RevisionIdentityRow {
            source_id: source_id.clone(),
            revision_id: new_head.clone(),
            retained_ordinal: refresh.old_retained.len(),
            original_hash: required_hash("wiki_original_hash")?,
            content_hash: head
                .record
                .string("wiki_content_hash")
                .map(Blake3Hash::new)
                .transpose()?,
            extractor_fingerprint: required_hash("wiki_extractor_fingerprint")?,
            extraction_status: head.record.string("wiki_extraction_status").unwrap().into(),
        });
        let facts = delta.facts.as_mut().unwrap();
        facts.records.push(RecordFactMutation {
            record_id: new_head.clone(),
            fact: work.facts[new_head].clone(),
        });
        facts.edge_inserts.extend([
            EligibilityEdge {
                owner_id: new_head.clone(),
                target_id: source_id.clone(),
                role: typed("wiki_source_id"),
            },
            EligibilityEdge {
                owner_id: source_id.clone(),
                target_id: new_head.clone(),
                role: EligibilityRole::SourceInventory,
            },
        ]);
        for registry in work
            .new_registry
            .iter()
            .filter(|entry| entry.id == *new_head)
        {
            facts.registry.push(OwnedRegistryKeys {
                record_id: registry.id.clone(),
                path: registry.path.clone(),
                keys: link_facts::registry_keys(registry)?,
            });
        }
    }
    if old_head != new_head {
        let old_edge = EligibilityEdge {
            owner_id: source_id.clone(),
            target_id: old_head.clone(),
            role: typed("wiki_current_revision"),
        };
        if work.edges(source_id, &[typed("wiki_current_revision")], false)?
            != vec![old_edge.clone()]
        {
            return Err(corrupt("source head relation differs from baseline"));
        }
        let facts = delta.facts.as_mut().unwrap();
        facts.edge_deletes.push(old_edge);
        facts.edge_inserts.push(EligibilityEdge {
            owner_id: source_id.clone(),
            target_id: new_head.clone(),
            role: typed("wiki_current_revision"),
        });
    }
    Ok(())
}

fn emit_refresh_content(
    work: &mut Work<'_>,
    refresh: &AdmittedRefresh,
    delta: &mut CatalogDelta,
) -> Result<()> {
    let source_id = &refresh.source_id;
    let old_head = &refresh.old_head;
    let new_head = &refresh.new_head;
    if old_head != new_head {
        for revision in [old_head, new_head] {
            let row = work.now[revision].clone();
            if row.record.string("wiki_extraction_status") == Some("complete") {
                let content_path =
                    asset_path(&row, row.record.string("wiki_content_path").unwrap())?;
                if revision == new_head {
                    let text = String::from_utf8(
                        refresh
                            .content
                            .clone()
                            .ok_or_else(|| corrupt("complete head lacks verified content"))?,
                    )
                    .map_err(|_| conflict("complete source content is not UTF8"))?;
                    delta.documents.push(DocumentMutation::Put {
                        row: row_projection::captured_content_document(
                            content_path,
                            source_id.clone(),
                            &row,
                            text,
                        ),
                    });
                } else {
                    work.metadata_document(
                        &content_path,
                        &row,
                        None,
                        Some((source_id.clone(), revision.clone())),
                        delta,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn emit_refresh_navigation(work: &mut Work<'_>, delta: &mut CatalogDelta) -> Result<()> {
    let changed_keys: Vec<_> = work
        .new_registry
        .iter()
        .map(link_facts::registry_keys)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut link_owners: BTreeSet<_> = work
        .overlay
        .keys()
        .filter(|path| canonical_path(path))
        .cloned()
        .collect();
    if !changed_keys.is_empty() {
        for (path, _) in work.reader.affected_links(&changed_keys)? {
            link_owners.insert(path);
        }
    }
    for owner in link_owners {
        work.emit_links(&owner, delta)?;
    }
    if !changed_keys.is_empty() {
        for assertion in work.reader.affected_assertion_navigation(&changed_keys)? {
            work.emit_assertion_navigation(&assertion, delta)?;
        }
    }
    Ok(())
}

/// None is an authenticated planner no-op; it creates no proposal or SQL epoch.
pub(crate) fn project_refresh(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    plan: IndexedSourceRefreshPlan,
    limits: &RefreshProjectionLimits,
) -> Result<Option<ProjectedSourceRefresh>> {
    let base = plan.base_snapshot.clone();
    let mut work = Work::new(fs, reader, limits)?;
    let mut refresh = admit_refresh(&mut work, plan, false)?;
    let Some(mut draft) = refresh.draft.take() else {
        work.recheck()?;
        return Ok(None);
    };
    refresh.content = work.verify_head(&refresh.source_id, &refresh.new_head)?;
    let seeds = refresh_seeds(&mut work, &refresh)?;
    work.discover(seeds)?;
    work.recompute()?;
    let policy = super::write_projection::project_policy(&mut work)?;
    let mut delta = super::write_projection::empty_delta(policy);
    emit_refresh_identity(&mut work, &refresh, &mut delta)?;
    let changed: Vec<_> = work
        .now
        .iter()
        .filter(|(id, row)| work.old.get(*id) != Some(*row))
        .map(|(id, _)| id.clone())
        .collect();
    for id in changed {
        work.emit_record(&id, &mut delta)?;
    }
    emit_refresh_content(&mut work, &refresh, &mut delta)?;
    emit_refresh_navigation(&mut work, &mut delta)?;
    let mut after = work.before.clone();
    for (path, bytes) in &work.overlay {
        after.insert(path.clone(), ExpectedState::Hash(Blake3Hash::digest(bytes)));
    }
    draft.read_preconditions = deps(work.before.clone());
    delta.dependencies = deps(after.clone());
    delta.validate()?;
    work.recheck()?;
    Ok(Some(ProjectedSourceRefresh {
        parts: ProjectedRefreshParts {
            draft,
            base,
            source_id: refresh.source_id,
            before: deps(work.before),
            after: deps(after),
            delta,
        },
    }))
}

/// Admit one bounded final overlay, with all selected heads installed before
/// discovering shared support, opposition, generation and policy dependents.
pub(crate) fn project_refresh_batch(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    mut plans: Vec<IndexedSourceRefreshPlan>,
    limits: &RefreshProjectionLimits,
) -> Result<Option<super::write_projection::ProjectedWrite>> {
    use super::write_projection::{ProjectedWrite, ProjectedWriteParts};
    use crate::changes::indexed_refresh::{
        IndexedRefreshSourceDependency, IndexedRefreshTarget, IndexedWriteOperation,
    };
    if plans.is_empty() || plans.len() > 16 {
        return Err(WikiError::invalid("refresh batch requires 1–16 plans"));
    }
    plans.sort_by(|left, right| left.plan.source_id.cmp(&right.plan.source_id));
    let source_ids: BTreeSet<_> = plans
        .iter()
        .map(|plan| plan.plan.source_id.clone())
        .collect();
    let mut revision_owners = BTreeMap::new();
    if source_ids.len() != plans.len() {
        return Err(conflict("refresh batch repeats a Source identity"));
    }
    for plan in &plans {
        for revision in [&plan.previous_revision, &plan.plan.revision_id] {
            if source_ids.contains(revision)
                || revision_owners
                    .insert(revision.clone(), plan.plan.source_id.clone())
                    .is_some_and(|owner| owner != plan.plan.source_id)
            {
                return Err(conflict(
                    "refresh batch Source and Revision ownership overlaps",
                ));
            }
        }
    }
    let base = QueryCatalog::snapshot(reader).clone();
    let mut work = Work::new(fs, reader, limits)?;
    let mut refreshes = Vec::with_capacity(plans.len());
    for plan in plans {
        refreshes.push(admit_refresh(&mut work, plan, true)?);
    }
    if refreshes.iter().all(|refresh| refresh.draft.is_none()) {
        work.recheck()?;
        return Ok(None);
    }
    let mut targets = Vec::with_capacity(refreshes.len());
    for refresh in &refreshes {
        let unchanged_source = if refresh.draft.is_none() {
            let path = work.now[&refresh.source_id].path.clone();
            let length = work.captured[&path].bytes.len();
            // Historical replay authenticates unchanged canonical membership
            // from this retained exact before-image, not today's Source bytes.
            // The retained delta and proof each encode this exact string. JSON
            // escaping requires at most six bytes per original UTF-8 byte.
            let encoded = length
                .checked_mul(6)
                .and_then(|n| n.checked_add(2))
                .ok_or_else(budget)?;
            work.charge(
                length
                    .checked_add(encoded.checked_mul(2).ok_or_else(budget)?)
                    .ok_or_else(budget)?,
            )?;
            Some(
                String::from_utf8(work.captured[&path].bytes.clone())
                    .map_err(|_| conflict("unchanged Source is not UTF8"))?,
            )
        } else {
            None
        };
        targets.push(IndexedRefreshTarget {
            source_id: refresh.source_id.clone(),
            previous_revision_id: refresh.old_head.clone(),
            revision_id: refresh.new_head.clone(),
            reused: !refresh.is_new,
            no_op: refresh.draft.is_none(),
            unchanged_source,
        });
    }
    let mut operation = IndexedWriteOperation::SourceRefreshBatch {
        refreshes: targets,
        dependent_sources: Vec::new(),
    };
    operation.validate()?;
    let mut seeds = BTreeSet::new();
    for refresh in &mut refreshes {
        refresh.content = work.verify_head_metered(&refresh.source_id, &refresh.new_head)?;
        seeds.extend(refresh_seeds(&mut work, refresh)?);
    }
    work.discover(seeds)?;
    work.recompute()?;
    let policy = super::write_projection::project_source_refresh_batch_policy(&mut work)?;
    let mut delta = super::write_projection::empty_delta(policy);
    for refresh in &refreshes {
        emit_refresh_identity(&mut work, refresh, &mut delta)?;
    }
    let mut changed: BTreeSet<_> = work
        .now
        .iter()
        .filter(|(id, row)| work.old.get(*id) != Some(*row))
        .map(|(id, _)| id.clone())
        .collect();
    // Unchanged Sources retain explicit canonical rows for exact historical
    // replay validation, while their immutable dependencies remain guarded.
    changed.extend(refreshes.iter().map(|refresh| refresh.source_id.clone()));
    for id in changed {
        work.emit_record(&id, &mut delta)?;
    }
    if let IndexedWriteOperation::SourceRefreshBatch {
        dependent_sources, ..
    } = &mut operation
    {
        for row in delta.records.iter().filter(|row| {
            row.record.kind() == RecordKind::Source && !source_ids.contains(row.record.id())
        }) {
            let bytes = &work.captured[&row.path].bytes;
            let encoded = bytes
                .len()
                .checked_mul(6)
                .and_then(|n| n.checked_add(2))
                .ok_or_else(budget)?;
            let charge = bytes
                .len()
                .checked_add(encoded.checked_mul(2).ok_or_else(budget)?)
                .ok_or_else(budget)?;
            work.charge(charge)?;
            let bytes = String::from_utf8(work.captured[&row.path].bytes.clone())
                .map_err(|_| conflict("dependent Source is not UTF8"))?;
            dependent_sources.push(IndexedRefreshSourceDependency {
                source_id: row.record.id().clone(),
                source_bytes: bytes,
            });
        }
        dependent_sources.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    }
    operation.validate()?;
    for refresh in &refreshes {
        // Document construction retains its own verified content copy.
        if refresh.old_head != refresh.new_head {
            work.charge(refresh.content.as_ref().map_or(0, Vec::len))?;
        }
        emit_refresh_content(&mut work, refresh, &mut delta)?;
    }
    emit_refresh_navigation(&mut work, &mut delta)?;
    let allocated_ids = refreshes
        .iter()
        .enumerate()
        .filter(|(_, refresh)| refresh.is_new)
        .map(|(index, refresh)| (format!("revision_{index}"), refresh.new_head.clone()))
        .collect();
    let mut drafts = refreshes.into_iter().filter_map(|refresh| refresh.draft);
    let mut draft = drafts
        .next()
        .ok_or_else(|| corrupt("changed batch lacks draft"))?;
    draft.title = format!("Refresh {} sources", source_ids.len());
    draft.allocated_ids = allocated_ids;
    for remaining in drafts {
        draft.operations.extend(remaining.operations);
    }
    let mut after = work.before.clone();
    for (path, bytes) in &work.overlay {
        after.insert(path.clone(), ExpectedState::Hash(Blake3Hash::digest(bytes)));
    }
    draft.read_preconditions = deps(work.before.clone());
    delta.dependencies = deps(after.clone());
    delta.validate()?;
    work.recheck()?;
    Ok(Some(ProjectedWrite::from_parts(ProjectedWriteParts {
        operation,
        draft,
        base,
        before: deps(work.before),
        after: deps(after),
        delta,
    })))
}

pub(super) fn asset_path(row: &RecordRow, name: &str) -> Result<VaultRelativePath> {
    let parent = row
        .path
        .as_str()
        .rsplit_once('/')
        .ok_or_else(|| corrupt("revision directory missing"))?
        .0;
    VaultRelativePath::new(format!("{parent}/{name}"))
}

impl Work<'_> {
    pub(super) fn recheck(&mut self) -> Result<()> {
        for (path, expected) in self.before.clone() {
            self.tick()?;
            let remaining = self.limits.max_canonical_bytes.saturating_sub(self.bytes);
            let bytes = read_bounded(self.fs, &path, self.limits.max_file_bytes.min(remaining))?;
            self.charge(bytes.as_ref().map_or(0, Vec::len))?;
            if state(bytes.as_deref()) != expected {
                return Err(conflict(format!(
                    "selected boundary changed during projection: {path}"
                )));
            }
        }
        Ok(())
    }
    pub(super) fn verify_head(
        &mut self,
        source: &RecordId,
        revision: &RecordId,
    ) -> Result<Option<Vec<u8>>> {
        self.verify_head_inner(source, revision, false)
    }
    fn verify_head_metered(
        &mut self,
        source: &RecordId,
        revision: &RecordId,
    ) -> Result<Option<Vec<u8>>> {
        self.verify_head_inner(source, revision, true)
    }
    fn verify_head_inner(
        &mut self,
        source: &RecordId,
        revision: &RecordId,
        metered_copies: bool,
    ) -> Result<Option<Vec<u8>>> {
        let row = self.now[revision].clone();
        for (path_field, hash_field) in [
            ("wiki_original_path", "wiki_original_hash"),
            ("wiki_content_path", "wiki_content_hash"),
        ] {
            if let Some(name) = row.record.string(path_field) {
                let path = asset_path(&row, name)?;
                let hash = Blake3Hash::new(
                    row.record
                        .string(hash_field)
                        .ok_or_else(|| corrupt("revision payload hash missing"))?,
                )?;
                if let Some(bytes) = self.overlay.get(&path) {
                    if Blake3Hash::digest(bytes) != hash {
                        return Err(conflict("new revision payload differs from declared hash"));
                    }
                } else {
                    self.capture(&path, &ExpectedState::Hash(hash))?;
                }
            }
        }
        if metered_copies {
            // ValidationInput copies the finite selected bytes, and SourceView
            // retains another copy. Its bounded payload reads return copies too.
            let selected = self
                .captured
                .values()
                .map(|doc| doc.bytes.len())
                .chain(self.overlay.values().map(Vec::len))
                .try_fold(0usize, |total, bytes| {
                    total.checked_add(bytes).ok_or_else(budget)
                })?;
            let payload = ["wiki_original_path", "wiki_content_path"]
                .into_iter()
                .filter_map(|field| row.record.string(field))
                .try_fold(0usize, |total, name| {
                    let path = asset_path(&row, name)?;
                    let bytes = self
                        .overlay
                        .get(&path)
                        .map(Vec::len)
                        .or_else(|| self.captured.get(&path).map(|doc| doc.bytes.len()))
                        .ok_or_else(|| corrupt("verified payload was not captured"))?;
                    total.checked_add(bytes).ok_or_else(budget)
                })?;
            let canonical_overlay = self
                .overlay
                .iter()
                .filter(|(path, _)| canonical_path(path))
                .try_fold(0usize, |total, (_, bytes)| {
                    total.checked_add(bytes.len()).ok_or_else(budget)
                })?;
            self.charge(
                selected
                    .checked_mul(2)
                    .and_then(|n| n.checked_add(payload))
                    .and_then(|n| n.checked_add(canonical_overlay))
                    .ok_or_else(budget)?,
            )?;
        }
        let input = ValidationInput {
            vault_id: QueryCatalog::vault_id(self.reader).clone(),
            documents: self.captured.values().cloned().collect(),
            overlay: self
                .overlay
                .iter()
                .map(|(path, bytes)| ProposedTarget {
                    path: path.clone(),
                    bytes: Some(bytes.clone()),
                })
                .chain(self.removed_paths.iter().map(|path| ProposedTarget {
                    path: path.clone(),
                    bytes: None,
                }))
                .collect(),
        };
        let view = SourceView::from_closed_input(self.fs, &input)?;
        let mut observed = BTreeMap::new();
        let content = if row.record.string("wiki_extraction_status") == Some("complete") {
            Some(view.revision_content_bounded(
                source,
                revision,
                &mut observed,
                self.limits.max_file_bytes,
                self.limits.max_file_bytes,
            )?)
        } else {
            let path = asset_path(
                &row,
                row.record
                    .string("wiki_original_path")
                    .ok_or_else(|| corrupt("original path missing"))?,
            )?;
            let bytes = view.read_bounded(&path, &mut observed, self.limits.max_file_bytes)?;
            if Some(Blake3Hash::digest(&bytes).as_str()) != row.record.string("wiki_original_hash")
            {
                return Err(conflict("unsupported capture original integrity failed"));
            }
            None
        };
        for (path, expected) in observed {
            let actual = self
                .overlay
                .get(&path)
                .map(|bytes| state(Some(bytes)))
                .or_else(|| self.before.get(&path).cloned())
                .ok_or_else(|| corrupt("closed verification acquired unadmitted path"))?;
            if actual != expected {
                return Err(conflict(
                    "closed verification disagrees with selected state",
                ));
            }
        }
        self.tick()?;
        Ok(content)
    }
    pub(super) fn capture_path(&mut self, path: &VaultRelativePath) -> Result<()> {
        if self.overlay.contains_key(path) || self.removed_paths.contains(path) {
            return Ok(());
        }
        let states = self.reader.direct_path_states(std::slice::from_ref(path))?;
        let dependency = states
            .first()
            .filter(|dependency| &dependency.path == path)
            .ok_or_else(|| corrupt("selected path has no central expected state"))?;
        self.capture(path, &dependency.expected)
    }
    pub(super) fn metadata_document(
        &mut self,
        path: &VaultRelativePath,
        row: &RecordRow,
        canonical: Option<&RecordId>,
        captured: Option<(RecordId, RecordId)>,
        delta: &mut CatalogDelta,
    ) -> Result<()> {
        self.tick()?;
        let metadata = self
            .reader
            .document_metadata(path)?
            .ok_or_else(|| corrupt("selected lifecycle document missing"))?;
        if metadata.record_id.as_ref() != canonical
            || metadata.kind != canonical.map(|_| row.record.kind())
            || metadata.source_id.as_ref() != captured.as_ref().map(|pair| &pair.0)
            || metadata.owner_revision.as_ref() != captured.as_ref().map(|pair| &pair.1)
        {
            return Err(corrupt("lifecycle document ownership differs"));
        }
        if metadata.eligibility != row.eligibility || metadata.reasons != row.reasons {
            delta.documents.push(DocumentMutation::Metadata {
                path: path.clone(),
                eligibility: row.eligibility,
                reasons: row.reasons.clone(),
            });
        }
        Ok(())
    }
    pub(super) fn emit_record(&mut self, id: &RecordId, delta: &mut CatalogDelta) -> Result<()> {
        self.tick()?;
        let row = self.now[id].clone();
        let old = self.old.get(id).cloned();
        let canonical_changed = old
            .as_ref()
            .is_none_or(|old| old.hash != row.hash || old.path != row.path);
        let note = self.note(&row.path)?;
        if canonical_changed {
            delta.documents.push(DocumentMutation::Put {
                row: row_projection::canonical_document(&row.path, &note, Some(&row)),
            });
            delta.claims.push(OwnedClaims {
                path: row.path.clone(),
                rows: vec![IdentityClaimRow {
                    id: id.clone(),
                    path: row.path.clone(),
                    hash: row.hash.clone(),
                    kind: Some(row.record.kind()),
                }],
            });
            // Source and new revision have authenticated valid envelopes and
            // fixed structural baseline. They acquire no dynamic diagnostics.
            let diagnostics = if old.is_some() {
                self.reader
                    .diagnostics(&BTreeSet::from([row.path.clone()]))?
            } else {
                vec![]
            };
            delta.diagnostics.push(OwnedDiagnostics {
                path: row.path.clone(),
                rows: diagnostics,
            });
        } else {
            self.metadata_document(&row.path, &row, Some(id), None, delta)?;
        }
        if matches!(
            row.record.kind(),
            RecordKind::Entity | RecordKind::Assertion
        ) {
            let endpoint_ids = [
                row.record.string("wiki_subject_id"),
                row.record.string("wiki_object_id"),
            ]
            .map(|id| id.map(RecordId::new).transpose())
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
            for endpoint in endpoint_ids.iter().flatten() {
                self.load(endpoint)?;
            }
            let endpoints = [
                endpoint_ids[0]
                    .as_ref()
                    .and_then(|id| self.now.get(id))
                    .map(|row| &row.record),
                endpoint_ids[1]
                    .as_ref()
                    .and_then(|id| self.now.get(id))
                    .map(|row| &row.record),
            ];
            let graph = row_projection::graph_row(&row, Some(&note), endpoints)?;
            let before = old
                .as_ref()
                .map(|old| {
                    let captured = self
                        .captured
                        .get(&old.path)
                        .ok_or_else(|| corrupt("old graph owner was not authenticated"))?;
                    let before_note = parse_note(&captured.bytes);
                    row_projection::graph_row(old, Some(&before_note), endpoints)
                })
                .transpose()?;
            if before.as_ref() != Some(&graph) {
                delta.graph.push(graph);
            }
        }
        delta.records.push(row);
        Ok(())
    }
    pub(super) fn registry_probe(
        &mut self,
        key: &MatchKey,
        include_new: bool,
    ) -> Result<RegistryProbe> {
        self.tick()?;
        if !include_new && !self.replaced_registry.is_empty() {
            return self.reader.registry_probe_for_key(key);
        }
        if !self.key_cache.contains_key(key) {
            let probe = self
                .reader
                .registry_probe_for_key_excluding(key, &self.replaced_registry)?;
            self.key_cache.insert(key.clone(), probe);
        }
        let mut probe = self.key_cache[key].clone();
        for candidate in self.new_registry.iter().filter(|_| include_new) {
            if link_facts::registry_keys(candidate)?.contains(key) {
                probe.insert(RegistryCandidate {
                    id: candidate.id.clone(),
                    kind: candidate.kind,
                    path: candidate.path.clone(),
                })?;
            }
        }
        Ok(probe)
    }
    pub(super) fn navigation(
        &mut self,
        fact: &OwnedLinkFact,
        include_new: bool,
    ) -> Result<NavigationResolution> {
        let mut probe = |key: &MatchKey| self.registry_probe(key, include_new);
        if let Some(target) = &fact.typed {
            navigation_resolution::resolve_typed(
                &target.id,
                target.expected_kind,
                Some(&fact.raw_destination),
                &mut probe,
            )
        } else {
            navigation_resolution::resolve_untyped(&fact.raw_destination, &mut probe)
        }
    }
    pub(super) fn resolve_fact(
        &mut self,
        mut fact: OwnedLinkFact,
    ) -> Result<(LinkRow, OwnedLinkFact)> {
        let resolution = self.navigation(&fact, true)?;
        if let Some(target) = &fact.typed
            && !self.admitted_typed_navigation.contains(&fact.from_path)
            && !matches!(resolution, NavigationResolution::Resolved { .. })
        {
            let before = self.navigation(&fact, false)?;
            if self.overlay.contains_key(&fact.from_path) || before != resolution {
                let mut error = conflict(format!(
                    "source refresh would invalidate typed companion in {} at byte {}: {:?} for {}; reconcile that reference before refreshing",
                    fact.from_path, fact.byte_start, fact.raw_destination, target.id
                ));
                error.details = serde_json::json!({"owner_path":fact.from_path,"byte_start":fact.byte_start,"destination":fact.raw_destination,"target_id":target.id,"expected_kind":target.expected_kind});
                return Err(error);
            }
        }
        let (target_id, target_path) = match &resolution {
            NavigationResolution::Resolved { id, path, .. } => {
                (Some(id.clone()), Some(path.clone()))
            }
            _ => (None, None),
        };
        if fact.typed.is_none() {
            fact = link_facts::untyped_navigation_fact(
                &fact.from_path,
                fact.byte_start,
                &fact.raw_destination,
                &resolution,
            )?;
        }
        Ok((
            LinkRow {
                from_path: fact.from_path.clone(),
                byte_start: fact.byte_start,
                target_id,
                target_path,
                resolution: format!("{resolution:?}"),
            },
            fact,
        ))
    }
    pub(super) fn emit_links(
        &mut self,
        path: &VaultRelativePath,
        delta: &mut CatalogDelta,
    ) -> Result<()> {
        self.capture_path(path)?;
        let note = self.note(path)?;
        let canonical = note.canonical.as_ref();
        let mut facts = Vec::new();
        let body = std::str::from_utf8(note.body()).unwrap_or_default();
        let body_offset = note.raw.len() - note.body().len();
        for link in extract_links(body) {
            self.tick()?;
            let external = matches!(
                crate::records::links::untyped_lookup(&link.destination),
                crate::records::links::UntypedLookup::External
            );
            facts.push(link_facts::untyped_fact(
                path,
                (body_offset + link.range.start) as u64,
                &link.destination,
                if external {
                    &LinkResolution::External
                } else {
                    &LinkResolution::Missing
                },
            )?);
        }
        // Only adopted canonical records own typed frontmatter links, exactly as
        // full scan. A plain/duplicate/invalid note retains body navigation only.
        let adopted = if let Some(record) = canonical {
            if self.now.contains_key(record.id()) {
                self.now
                    .get(record.id())
                    .filter(|row| &row.path == path)
                    .map(|row| row.record.clone())
            } else {
                self.reader
                    .record(record.id())?
                    .filter(|row| &row.path == path)
                    .map(|row| row.record)
            }
        } else {
            None
        };
        if let Some(record) = &adopted {
            for (field, kind, companion) in eligibility::references(record) {
                let Some(companion) = companion else {
                    continue;
                };
                let Some(destination) = record.string(companion) else {
                    continue;
                };
                let Some(value) = record.string(field) else {
                    continue;
                };
                let target = RecordId::new(value)?;
                let line = note.field_starts.get(companion).copied().unwrap_or(0);
                let tail = std::str::from_utf8(&note.raw[line..]).unwrap_or_default();
                let start = line
                    + tail
                        .lines()
                        .next()
                        .and_then(|line| line.find("[["))
                        .unwrap_or(0);
                facts.push(link_facts::typed_fact(
                    path,
                    start as u64,
                    destination,
                    &target,
                    kind,
                )?);
            }
        }
        let mut rows = Vec::new();
        let mut resolved = Vec::new();
        for fact in facts {
            let (row, fact) = self.resolve_fact(fact)?;
            rows.push(row);
            resolved.push(fact);
        }
        rows.sort_by_key(|row| row.byte_start);
        resolved.sort_by_key(|row| row.byte_start);
        if rows
            .windows(2)
            .any(|rows| rows[0].byte_start == rows[1].byte_start)
        {
            return Err(corrupt("duplicate source navigation offsets"));
        }
        delta.links.push(OwnedLinks {
            path: path.clone(),
            rows,
        });
        delta.facts.as_mut().unwrap().links.push(OwnedLinkFacts {
            path: path.clone(),
            rows: resolved,
        });
        Ok(())
    }
    pub(super) fn emit_assertion_navigation(
        &mut self,
        id: &RecordId,
        delta: &mut CatalogDelta,
    ) -> Result<()> {
        self.require(id)?;
        let row = self.now[id].clone();
        if row.record.kind() != RecordKind::Assertion {
            return Err(corrupt("assertion navigation owner is not an assertion"));
        }
        let mut diagnostics = if let Some(owned) = delta
            .diagnostics
            .iter()
            .find(|owned| owned.path == row.path)
        {
            owned.rows.clone()
        } else {
            self.reader
                .diagnostics(&BTreeSet::from([row.path.clone()]))?
        };
        diagnostics.retain(|diagnostic| {
            !diagnostic
                .details
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|reason| reason.starts_with("evidence_link_"))
        });
        for destination in scan::list(&row.record, "wiki_evidence") {
            self.tick()?;
            let external = matches!(
                crate::records::links::untyped_lookup(&destination),
                crate::records::links::UntypedLookup::External
            );
            let fact = link_facts::untyped_fact(
                &row.path,
                0,
                &destination,
                if external {
                    &LinkResolution::External
                } else {
                    &LinkResolution::Missing
                },
            )?;
            let resolution = self.navigation(&fact, true)?;
            let target = if let NavigationResolution::Resolved { id, .. } = &resolution {
                self.load(id)?;
                self.now.get(id).map(|row| &row.record)
            } else {
                None
            };
            if let Some(reason) = rules::compact_evidence_navigation_reason(id, &resolution, target)
            {
                diagnostics.push(scan::diagnostic(
                    &row.path,
                    Some(id),
                    ErrorCode::RecordInvalid,
                    serde_json::json!({"reason":reason,"link":destination}),
                ));
            }
        }
        diagnostics.sort_by(|a, b| {
            (&a.path, format!("{:?}", a.code), a.details.to_string()).cmp(&(
                &b.path,
                format!("{:?}", b.code),
                b.details.to_string(),
            ))
        });
        if let Some(owned) = delta
            .diagnostics
            .iter_mut()
            .find(|owned| owned.path == row.path)
        {
            owned.rows = diagnostics;
        } else {
            delta.diagnostics.push(OwnedDiagnostics {
                path: row.path,
                rows: diagnostics,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!("source_projection_tests.rs");
    include!("source_refresh_batch_projection_tests.rs");
}
