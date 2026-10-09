//! Published, exact-key graph inputs. A selected capture is not a complete vault
//! and never turns an unobserved path or SQL miss into filesystem absence.
use super::{
    extraction_types::*,
    import,
    normalized_meter::GraphOperationMeter,
    normalized_types::GraphOperationFamily,
    packet,
    policy_inputs::{self, PolicyAttempt, PolicyInputKey},
    resolution_types::*,
    review_types::*,
};
use crate::{
    catalog::{
        IdentityClaimRow, RecordRow,
        eligibility_facts::{EligibilityEdge, EligibilityRole},
        policy_facts::PolicyKind,
        query::QuerySnapshot,
        query_types::QueryCatalog,
    },
    changes::{ReadDependency, ScanDocument, ValidationInput},
    domain::*,
    records::{ParsedNote, parse_note},
    sources::{SourceView, revision::canonical_path},
    vault::{ExpectedState, VaultFs},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
};

pub(crate) enum GraphInputRequest<'a> {
    Packet(&'a ExportRequest),
    Import(&'a RecordId),
    Resolve(&'a ResolutionRequest),
    Review(&'a ReviewRequest),
}

// No Deserialize, public fields, or unchecked constructor. Certificates are
// publication-relative and remain separate from semantic/write authority.
pub(crate) struct SelectedGraphInput {
    root: std::path::PathBuf,
    input: ValidationInput,
    dependencies: Vec<ReadDependency>,
    snapshot: ReadSnapshot,
    family: GraphOperationFamily,
    certificates: BTreeMap<PolicyInputKey, BTreeMap<VaultRelativePath, Blake3Hash>>,
    identities: BTreeMap<RecordId, Vec<IdentityClaimRow>>,
    evidence_members: BTreeMap<RecordId, BTreeSet<RecordId>>,
    source_tuples: BTreeMap<(RecordId, RecordId), crate::catalog::query_types::SourceRevisionTuple>,
}
impl SelectedGraphInput {
    pub(crate) fn validation_input(&self) -> &ValidationInput {
        &self.input
    }
    pub(crate) fn dependencies(&self) -> &[ReadDependency] {
        &self.dependencies
    }
    pub(crate) fn snapshot(&self) -> &ReadSnapshot {
        &self.snapshot
    }
    pub(crate) fn family(&self) -> GraphOperationFamily {
        self.family
    }
    pub(crate) fn certificates(&self) -> BTreeSet<PolicyInputKey> {
        self.certificates.keys().cloned().collect()
    }
    pub(crate) fn policy_membership(
        &self,
        key: &PolicyInputKey,
    ) -> Option<&BTreeMap<VaultRelativePath, Blake3Hash>> {
        self.certificates.get(key)
    }
    /// Callers must charge their shared operation meter for this construction.
    /// Prefer view_metered; this frozen accessor does no filesystem fallback.
    pub(crate) fn view<'a>(&self, fs: &'a VaultFs) -> Result<SourceView<'a>> {
        if fs.root().path() != self.root.as_path() {
            return Err(conflict(
                "selected graph capture belongs to another physical vault root",
            ));
        }
        SourceView::from_closed_input(fs, &self.input)
    }
    pub(crate) fn view_metered<'a>(
        &self,
        fs: &'a VaultFs,
        meter: &mut GraphOperationMeter,
    ) -> Result<SourceView<'a>> {
        // from_input parses/clones canonical bytes and clones noncanonical assets.
        // Charge input copies and parse/hash work before those operations.
        for d in &self.input.documents {
            for _ in 0..3 {
                meter.charge_processing(d.bytes.len())?;
            }
        }
        self.view(fs)
    }
    /// Empty complete published ID claims only. This is never proof that an
    /// arbitrary filesystem destination is absent; the projector probes it.
    pub(crate) fn identity_is_absent(&self, id: &RecordId) -> bool {
        self.identities.get(id).is_some_and(Vec::is_empty)
    }
    pub(crate) fn evidence_members(&self, assertion: &RecordId) -> Option<&BTreeSet<RecordId>> {
        self.evidence_members.get(assertion)
    }
    pub(crate) fn source_revision_tuple(
        &self,
        source: &RecordId,
        revision: &RecordId,
    ) -> Option<&crate::catalog::query_types::SourceRevisionTuple> {
        self.source_tuples.get(&(source.clone(), revision.clone()))
    }
}

fn conflict(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn corrupt(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
fn integrity(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::SourceIntegrity, message)
}
fn exhausted(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn typed(field: &str) -> EligibilityRole {
    EligibilityRole::TypedReference {
        field: field.into(),
    }
}
fn needed<T>(attempt: PolicyAttempt<Option<T>>) -> Result<Option<PolicyInputKey>> {
    match attempt {
        PolicyAttempt::Need(key) => Ok(Some(key)),
        PolicyAttempt::Complete { result, .. } => {
            let _ = result?;
            Ok(None)
        }
    }
}

// A narrow lookup adapter also supports disposable deterministic test catalogs.
// Every production method delegates to the SAME existing QuerySnapshot owner.
trait CaptureCatalog {
    fn snapshot(&self) -> &ReadSnapshot;
    fn vault_id(&self) -> &RecordId;
    fn check(&self) -> Result<()>;
    fn query_bytes(&self) -> usize;
    fn claims(&self, id: &RecordId) -> Result<Vec<IdentityClaimRow>>;
    fn record(&self, id: &RecordId) -> Result<Option<RecordRow>>;
    fn edges(
        &self,
        id: &RecordId,
        roles: &[EligibilityRole],
        reverse: bool,
    ) -> Result<Vec<EligibilityEdge>>;
    fn policy(&self, key: &PolicyInputKey) -> Result<Vec<(VaultRelativePath, Blake3Hash)>>;
    fn affected(
        &self,
        keys: &BTreeSet<PolicyInputKey>,
        paths: &BTreeSet<VaultRelativePath>,
    ) -> Result<BTreeSet<PolicyKind>>;
    fn revision_tuple(
        &self,
        source: &RecordId,
        revision: &RecordId,
    ) -> Result<Option<crate::catalog::query_types::SourceRevisionTuple>>;
    fn direct_states(&self, id: &RecordId) -> Result<BTreeMap<VaultRelativePath, ExpectedState>>;
}
impl CaptureCatalog for QuerySnapshot {
    fn snapshot(&self) -> &ReadSnapshot {
        QueryCatalog::snapshot(self)
    }
    fn vault_id(&self) -> &RecordId {
        QueryCatalog::vault_id(self)
    }
    fn check(&self) -> Result<()> {
        self.check_query_budget()
    }
    fn query_bytes(&self) -> usize {
        self.usage().bytes
    }
    fn claims(&self, id: &RecordId) -> Result<Vec<IdentityClaimRow>> {
        self.identity_claims_for_id(id)
    }
    fn record(&self, id: &RecordId) -> Result<Option<RecordRow>> {
        QueryCatalog::record(self, id)
    }
    fn edges(
        &self,
        id: &RecordId,
        roles: &[EligibilityRole],
        reverse: bool,
    ) -> Result<Vec<EligibilityEdge>> {
        if reverse {
            self.dependent_edges(id, roles)
        } else {
            self.outgoing_edges(id, roles)
        }
    }
    fn policy(&self, key: &PolicyInputKey) -> Result<Vec<(VaultRelativePath, Blake3Hash)>> {
        self.policy_members(key, &BTreeSet::new())
    }
    fn affected(
        &self,
        keys: &BTreeSet<PolicyInputKey>,
        paths: &BTreeSet<VaultRelativePath>,
    ) -> Result<BTreeSet<PolicyKind>> {
        self.policy_affected(keys, paths)
    }
    fn revision_tuple(
        &self,
        source: &RecordId,
        revision: &RecordId,
    ) -> Result<Option<crate::catalog::query_types::SourceRevisionTuple>> {
        self.source_revision_tuple(source, revision)
    }
    fn direct_states(&self, id: &RecordId) -> Result<BTreeMap<VaultRelativePath, ExpectedState>> {
        let fact = self
            .eligibility_fact(id)?
            .ok_or_else(|| corrupt("selected direct-path owner fact absent"))?;
        self.direct_path_states(&fact.direct_paths.into_iter().collect::<Vec<_>>())
            .map(|rows| rows.into_iter().map(|d| (d.path, d.expected)).collect())
    }
}

struct Capture<'a> {
    fs: &'a VaultFs,
    reader: &'a dyn CaptureCatalog,
    meter: &'a mut GraphOperationMeter,
    documents: BTreeMap<VaultRelativePath, ScanDocument>,
    absent: BTreeSet<VaultRelativePath>,
    notes: BTreeMap<VaultRelativePath, ParsedNote>,
    identities: BTreeMap<RecordId, Vec<IdentityClaimRow>>,
    records: BTreeMap<RecordId, Option<RecordRow>>,
    certificates: BTreeMap<PolicyInputKey, BTreeMap<VaultRelativePath, Blake3Hash>>,
    sources: BTreeSet<RecordId>,
    artifacts: BTreeSet<RecordId>,
    evidence_members: BTreeMap<RecordId, BTreeSet<RecordId>>,
    query_bytes_seen: usize,
    policies_verified: BTreeSet<PolicyKind>,
    source_tuples: BTreeMap<(RecordId, RecordId), crate::catalog::query_types::SourceRevisionTuple>,
}
impl<'a> Capture<'a> {
    fn tick(&mut self) -> Result<()> {
        self.meter.check()?;
        self.reader.check()?;
        let bytes = self.reader.query_bytes();
        let additional = bytes
            .checked_sub(self.query_bytes_seen)
            .ok_or_else(|| corrupt("query accounting moved backwards"))?;
        self.meter.charge_processing(additional)?;
        self.query_bytes_seen = bytes;
        Ok(())
    }
    fn read(&mut self, path: &VaultRelativePath, expected: &ExpectedState) -> Result<()> {
        self.tick()?;
        if let Some(old) = self.documents.get(path) {
            if expected != &ExpectedState::Hash(old.hash.clone()) {
                return Err(conflict("selected published path states disagree"));
            }
            return Ok(());
        }
        if self.absent.contains(path) {
            if expected != &ExpectedState::Absent {
                return Err(conflict("selected absence conflicts with required asset"));
            }
            return Ok(());
        }
        if self.documents.len().saturating_add(self.absent.len())
            >= super::normalized_meter::GRAPH_CAPTURE_FILES
        {
            return Err(exhausted(
                "selected present/absent dependency paths exceed capture file ceiling",
            ));
        }
        let physical = self
            .fs
            .root()
            .resolve_budgeted(path, &mut || self.meter.probe())?;
        self.meter.probe()?;
        let file = match File::open(&physical) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if expected != &ExpectedState::Absent {
                    return Err(conflict(format!("selected input missing: {path}")));
                }
                self.absent.insert(path.clone());
                return Ok(());
            }
            Err(e) => {
                return Err(WikiError::new(
                    ErrorCode::Internal,
                    format!("selected read {path}: {e}"),
                ));
            }
        };
        if expected == &ExpectedState::Absent {
            return Err(conflict(format!(
                "expected absent selected path occupied: {path}"
            )));
        }
        self.meter.probe()?;
        let meta = file
            .metadata()
            .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
        if !meta.is_file() {
            return Err(conflict("selected input is not a regular file"));
        }
        let limit = self.meter.remaining_file_bytes()?;
        if meta.len() > limit as u64 {
            return Err(exhausted(
                "selected file exceeds remaining capture allowance before allocation",
            ));
        }
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
        if bytes.len() > limit {
            return Err(exhausted("selected file grew beyond capture allowance"));
        }
        self.meter.capture_read(path, bytes.len())?;
        self.meter.charge_processing(bytes.len())?;
        let hash = Blake3Hash::digest(&bytes);
        if expected != &ExpectedState::Hash(hash.clone()) {
            return Err(conflict(format!("selected input hash changed: {path}")));
        }
        if canonical_path(path) {
            // ParsedNote owns its bytes; account the raw copy and parsing/hash.
            self.meter.charge_processing(bytes.len())?;
            self.meter.charge_processing(bytes.len())?;
            self.notes.insert(path.clone(), parse_note(&bytes));
        }
        self.documents.insert(
            path.clone(),
            ScanDocument {
                path: path.clone(),
                bytes,
                hash,
            },
        );
        Ok(())
    }
    fn record(
        &mut self,
        id: &RecordId,
        kind: Option<RecordKind>,
        guard: Option<&Blake3Hash>,
        optional: bool,
    ) -> Result<Option<CanonicalRecord>> {
        self.tick()?;
        if !self.identities.contains_key(id) {
            let claims = self.reader.claims(id)?;
            for claim in &claims {
                if &claim.id != id {
                    return Err(corrupt("identity certificate differs from selected ID"));
                }
                self.read(&claim.path, &ExpectedState::Hash(claim.hash.clone()))?;
                let note = self
                    .notes
                    .get(&claim.path)
                    .ok_or_else(|| corrupt("identity claim is outside canonical namespace"))?;
                let claimed = note
                    .fields
                    .as_ref()
                    .and_then(|f| f.get("wiki_id"))
                    .and_then(serde_json::Value::as_str);
                if claimed != Some(id.as_str()) {
                    return Err(conflict("selected identity witness changed its raw ID"));
                }
            }
            self.identities.insert(id.clone(), claims);
        }
        if self.identities[id].len() > 1 {
            return Err(WikiError::new(
                ErrorCode::ReferenceAmbiguous,
                "selected ID has duplicate published claims",
            ));
        }
        if !self.records.contains_key(id) {
            let adopted = self.reader.record(id)?;
            self.tick()?;
            self.records.insert(id.clone(), adopted);
        }
        let claims = &self.identities[id];
        let adopted = self.records[id].as_ref();
        let Some(claim) = claims.first() else {
            if adopted.is_some() {
                return Err(corrupt("adopted record has no published identity claim"));
            }
            if optional {
                return Ok(None);
            }
            return Err(WikiError::new(
                ErrorCode::RecordNotFound,
                format!("selected identity absent: {id}"),
            ));
        };
        let adopted =
            adopted.ok_or_else(|| corrupt("selected identity is malformed or unadopted"))?;
        if adopted.path != claim.path
            || adopted.hash != claim.hash
            || claim.kind != Some(adopted.record.kind())
        {
            return Err(corrupt(
                "adopted selected record differs from identity certificate",
            ));
        }
        let actual = self.notes[&claim.path]
            .canonical
            .as_ref()
            .ok_or_else(|| conflict("selected canonical record became malformed"))?;
        if actual != &adopted.record
            || actual.id() != id
            || kind.is_some_and(|k| actual.kind() != k)
            || guard.is_some_and(|h| h != &claim.hash)
        {
            return Err(conflict("selected record kind/guard/envelope differs"));
        }
        if adopted.eligibility == Eligibility::Invalid
            && matches!(actual.kind(), RecordKind::Source | RecordKind::Revision)
        {
            return Err(integrity(
                "selected Source/Revision has invalid published baseline",
            ));
        }
        self.meter
            .charge_processing(self.documents[&claim.path].bytes.len())?;
        Ok(Some(actual.clone()))
    }
    fn require(
        &mut self,
        id: &RecordId,
        kind: RecordKind,
        guard: Option<&Blake3Hash>,
    ) -> Result<CanonicalRecord> {
        self.record(id, Some(kind), guard, false)?
            .ok_or_else(|| corrupt("required record absent"))
    }
    fn field_id(record: &CanonicalRecord, field: &str) -> Result<RecordId> {
        RecordId::new(
            record
                .string(field)
                .ok_or_else(|| integrity(format!("selected record lacks {field}")))?,
        )
    }
    fn source(&mut self, id: &RecordId) -> Result<()> {
        if !self.sources.insert(id.clone()) {
            return Ok(());
        }
        let source = self.require(id, RecordKind::Source, None)?;
        let values = source
            .field("wiki_revisions")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| integrity("selected Source manifest missing"))?;
        let head = Self::field_id(&source, "wiki_current_revision")?;
        let mut retained = BTreeSet::new();
        for (ordinal, value) in values.iter().enumerate() {
            self.tick()?;
            let revision_id = RecordId::new(
                value
                    .as_str()
                    .ok_or_else(|| integrity("invalid Source retained ID"))?,
            )?;
            if !retained.insert(revision_id.clone()) {
                return Err(integrity("duplicate retained revision ID"));
            }
            let revision = self.require(&revision_id, RecordKind::Revision, None)?;
            if revision.string("wiki_source_id") != Some(id.as_str()) {
                return Err(integrity("retained sibling belongs to another Source"));
            }
            let tuple = self
                .reader
                .revision_tuple(id, &revision_id)?
                .ok_or_else(|| corrupt("selected retained ordinal tuple absent"))?;
            if tuple.source_id != *id
                || tuple.revision_id != revision_id
                || tuple.retained_ordinal != ordinal
                || revision.string("wiki_original_hash") != Some(tuple.original_hash.as_str())
                || revision.string("wiki_content_hash")
                    != tuple.content_hash.as_ref().map(Blake3Hash::as_str)
                || revision.string("wiki_extractor_fingerprint")
                    != Some(tuple.extractor_fingerprint.as_str())
                || revision.string("wiki_extraction_status")
                    != Some(tuple.extraction_status.as_str())
            {
                return Err(corrupt("retained revision tuple/ownership/ordinal differs"));
            }
            let rp = self.identities[&revision_id][0].path.clone();
            self.meter.charge_processing(
                tuple.source_id.as_str().len()
                    + tuple.revision_id.as_str().len()
                    + tuple.original_hash.as_str().len()
                    + tuple.content_hash.as_ref().map_or(0, |h| h.as_str().len())
                    + tuple.extractor_fingerprint.as_str().len()
                    + tuple.extraction_status.len()
                    + std::mem::size_of::<usize>(),
            )?;
            self.source_tuples
                .insert((id.clone(), revision_id.clone()), tuple.clone());
            let parent = rp
                .as_str()
                .rsplit_once('/')
                .ok_or_else(|| integrity("revision lacks parent path"))?
                .0;
            let original = VaultRelativePath::new(format!(
                "{parent}/{}",
                revision
                    .string("wiki_original_path")
                    .ok_or_else(|| integrity("retained original path missing"))?
            ))?;
            self.read(&original, &ExpectedState::Hash(tuple.original_hash))?;
            if let Some(hash) = tuple.content_hash {
                let content = VaultRelativePath::new(format!(
                    "{parent}/{}",
                    revision
                        .string("wiki_content_path")
                        .ok_or_else(|| integrity("retained content path missing"))?
                ))?;
                self.read(&content, &ExpectedState::Hash(hash))?;
                std::str::from_utf8(&self.documents[&content].bytes)
                    .map_err(|_| integrity("retained content is not UTF-8"))?;
            }
            // Published direct absences are actual named probes, never SQL misses.
            for (path, state) in self.reader.direct_states(&revision_id)? {
                self.read(&path, &state)?;
            }
        }
        if !retained.contains(&head) {
            return Err(integrity("Source head is not retained"));
        }
        // Exact companion resolution uses the captured registry/identity notes;
        // full payload validation for supported siblings has already occurred.
        let view = self.closed_view()?;
        let (_, head_note) =
            view.resolve(&head, RecordKind::Revision, source.string("wiki_revision"))?;
        let hr = head_note
            .canonical
            .as_ref()
            .ok_or_else(|| integrity("invalid retained head"))?;
        view.resolve(id, RecordKind::Source, hr.string("wiki_source"))?;
        for revision in &retained {
            let (_, note) = view.resolve(revision, RecordKind::Revision, None)?;
            let record = note
                .canonical
                .as_ref()
                .ok_or_else(|| integrity("retained sibling malformed"))?;
            view.resolve(id, RecordKind::Source, record.string("wiki_source"))?;
        }
        Ok(())
    }
    fn closed_view(&mut self) -> Result<SourceView<'a>> {
        let input = self.input()?;
        for doc in &input.documents {
            for _ in 0..3 {
                self.meter.charge_processing(doc.bytes.len())?;
            }
        }
        SourceView::from_closed_input(self.fs, &input)
    }
    fn input(&mut self) -> Result<ValidationInput> {
        let mut documents = Vec::with_capacity(self.documents.len());
        for d in self.documents.values() {
            self.meter.charge_processing(d.bytes.len())?;
            documents.push(d.clone());
        }
        Ok(ValidationInput {
            vault_id: self.reader.vault_id().clone(),
            documents,
            overlay: self
                .absent
                .iter()
                .map(|path| crate::changes::ProposedTarget {
                    path: path.clone(),
                    bytes: None,
                })
                .collect(),
        })
    }
    fn packet(&mut self, id: &RecordId) -> Result<ExtractionPacket> {
        self.require(id, RecordKind::ExtractionPacket, None)?;
        let p = self.identities[id][0].path.clone();
        let n = &self.notes[&p];
        self.meter.charge_processing(n.raw.len())?;
        let packet: ExtractionPacket = packet::decode(
            packet::fenced_json(n, packet::PACKET_FENCE, MAX_PACKET_BYTES)?,
            MAX_PACKET_BYTES,
        )?;
        if packet.packet_id != *id {
            return Err(conflict("packet ID differs from canonical allocation"));
        }
        self.source(&packet.source_id)?;
        self.selected_revision(&packet.source_id, &packet.source_revision)?;
        if let Some(candidates) = &packet.candidate_context {
            self.candidates(candidates)?;
        }
        Ok(packet)
    }
    fn candidates(&mut self, candidates: &[CandidateIdentity]) -> Result<()> {
        for candidate in candidates {
            let r = self.require(&candidate.reference.record_id, RecordKind::Entity, None)?;
            if candidate.reference.vault_id != *self.reader.vault_id()
                || candidate.reference.expected_kind != RecordKind::Entity
                || candidate.title != r.title()
                || r.string("wiki_entity_type") != Some(candidate.entity_type.as_str())
            {
                return Err(conflict("declared candidate differs from canonical entity"));
            }
        }
        Ok(())
    }
    fn selected_revision(&mut self, source: &RecordId, revision: &RecordId) -> Result<()> {
        let s = self.require(source, RecordKind::Source, None)?;
        let r = self.require(revision, RecordKind::Revision, None)?;
        if r.string("wiki_source_id") != Some(source.as_str())
            || !s
                .field("wiki_revisions")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(revision.as_str())))
        {
            return Err(integrity(
                "selected revision is not owned/retained by selected Source",
            ));
        }
        Ok(())
    }
    fn artifact(&mut self, id: &RecordId) -> Result<()> {
        if !self.artifacts.insert(id.clone()) {
            return Ok(());
        }
        self.require(id, RecordKind::Extraction, None)?;
        let path = self.identities[id][0].path.clone();
        let n = &self.notes[&path];
        self.meter.charge_processing(n.raw.len())?;
        let a: ExtractionArtifactV1 = packet::decode(
            packet::fenced_json(n, import::ARTIFACT_FENCE, MAX_ARTIFACT_BYTES)?,
            MAX_ARTIFACT_BYTES,
        )?;
        if a.extraction_id != *id {
            return Err(conflict("extraction artifact ID mismatch"));
        }
        self.packet(&a.packet_id)?;
        self.source(&a.source_id)?;
        self.selected_revision(&a.source_id, &a.source_revision)?;
        for binding in a.bindings.values() {
            match binding {
                MentionBinding::Pending => {}
                MentionBinding::Resolved {
                    entity_id,
                    decision_id,
                } => {
                    self.require(entity_id, RecordKind::Entity, None)?;
                    self.require(decision_id, RecordKind::Decision, None)?;
                }
                MentionBinding::Rejected { decision_id } => {
                    self.require(decision_id, RecordKind::Decision, None)?;
                }
            }
        }
        let materialized: BTreeSet<_> = a.materialized_assertions.iter().collect();
        for (local, assertion) in &a.allocations.assertions {
            let used = materialized.contains(local);
            if used {
                self.assertion(assertion)?;
            } else if self.record(assertion, None, None, true)?.is_some() {
                return Err(conflict("unmaterialized reserved Assertion ID occupied"));
            }
            let evidence = a
                .allocations
                .evidence
                .get(local)
                .ok_or_else(|| conflict("artifact reserved Evidence set absent"))?;
            for eid in evidence {
                if used {
                    self.evidence(eid)?;
                } else if self.record(eid, None, None, true)?.is_some() {
                    return Err(conflict("unmaterialized reserved Evidence ID occupied"));
                }
            }
        }
        // Capture is not a semantic draft seal. The selected constructor runs
        // immutable artifact/binding verification against this closed input.
        Ok(())
    }
    fn evidence(&mut self, id: &RecordId) -> Result<()> {
        let r = self.require(id, RecordKind::Evidence, None)?;
        let sid = Self::field_id(&r, "wiki_source_id")?;
        self.source(&sid)?;
        let rid = Self::field_id(&r, "wiki_source_revision")?;
        self.selected_revision(&sid, &rid)?;
        if let Some(extraction) = r.string("wiki_extraction_id") {
            self.artifact(&RecordId::new(extraction)?)?;
        }
        Ok(())
    }
    fn assertion(&mut self, id: &RecordId) -> Result<()> {
        if self.evidence_members.contains_key(id) {
            return Ok(());
        }
        let assertion = self.require(id, RecordKind::Assertion, None)?;
        for field in ["wiki_subject_id", "wiki_object_id"] {
            if let Some(value) = assertion.string(field) {
                self.require(&RecordId::new(value)?, RecordKind::Entity, None)?;
            }
        }
        let mut members = BTreeSet::new();
        for e in self.reader.edges(id, &[typed("wiki_assertion_id")], true)? {
            if e.target_id != *id || e.role != typed("wiki_assertion_id") {
                return Err(corrupt("Evidence reverse membership differs from key"));
            }
            members.insert(e.owner_id);
        }
        let outgoing = self
            .reader
            .edges(id, &[EligibilityRole::AssertionEvidence], false)?;
        let mut forward_members = BTreeSet::new();
        for e in outgoing {
            if e.owner_id != *id || e.role != EligibilityRole::AssertionEvidence {
                return Err(corrupt("AssertionEvidence membership differs from key"));
            }
            forward_members.insert(e.target_id);
        }
        if members != forward_members {
            return Err(corrupt(
                "Assertion Evidence forward/reverse memberships disagree",
            ));
        }
        // Install before recursion through an extraction's materialized records.
        self.evidence_members.insert(id.clone(), members.clone());
        for eid in members {
            let evidence = self.require(&eid, RecordKind::Evidence, None)?;
            if evidence.string("wiki_assertion_id") != Some(id.as_str()) {
                return Err(corrupt("selected Evidence belongs to another Assertion"));
            }
            self.evidence(&eid)?;
        }
        Ok(())
    }
    fn certificate(&mut self, key: &PolicyInputKey) -> Result<()> {
        if self.certificates.contains_key(key) {
            return Err(corrupt("policy evaluator repeated certified key"));
        }
        let mut members = BTreeMap::new();
        for (path, hash) in self.reader.policy(key)? {
            if members.insert(path.clone(), hash.clone()).is_some() {
                return Err(corrupt("duplicate policy certificate path"));
            }
            self.read(&path, &ExpectedState::Hash(hash))?;
            let note = self
                .notes
                .get(&path)
                .ok_or_else(|| corrupt("policy witness outside canonical namespace"))?;
            self.meter.charge_processing(note.raw.len())?;
            if !policy_inputs::policy_membership_keys(&path, note)?.contains(key) {
                return Err(corrupt("policy witness does not match requested raw key"));
            }
        }
        if let PolicyInputKey::CanonicalIdentity(id) = key {
            self.record(id, None, None, true)?;
            let claims = self.identities[id]
                .iter()
                .map(|c| (c.path.clone(), c.hash.clone()))
                .collect::<BTreeMap<_, _>>();
            if claims != members {
                return Err(corrupt(
                    "policy identity membership differs from complete identity claims",
                ));
            }
        }
        for (path, note) in &self.notes {
            self.meter.check()?;
            if policy_inputs::policy_membership_keys(path, note)?.contains(key)
                && members.get(path) != Some(&note.source_hash)
            {
                return Err(corrupt(
                    "complete policy certificate omitted observed witness",
                ));
            }
        }
        self.certificates.insert(key.clone(), members);
        Ok(())
    }
    fn policy(&mut self, kind: PolicyKind) -> Result<()> {
        if self.policies_verified.contains(&kind) {
            return Ok(());
        }
        loop {
            self.tick()?;
            if kind == PolicyKind::Review {
                // Each fresh evaluator attempt actually decodes/serializes the
                // currently captured receipt family. Charge that work on this
                // same owner, including incomplete certificate attempts.
                for note in self.notes.values().filter(|n| super::review::has_fence(n)) {
                    for _ in 0..4 {
                        self.meter.charge_processing(note.raw.len())?;
                    }
                }
            }
            let keys = self.certificates.keys().cloned().collect();
            let need = {
                let mut work = |work: policy_inputs::PolicyWork| {
                    self.meter.check()?;
                    self.meter.charge_processing(work.bytes)
                };
                match kind {
                    PolicyKind::Review => {
                        needed(policy_inputs::attempt_review_policy_metered_for_vault(
                            &self.notes,
                            &keys,
                            self.reader.vault_id(),
                            &mut work,
                        )?)?
                    }
                    PolicyKind::Remap => needed(policy_inputs::attempt_decision_policy_metered(
                        &self.notes,
                        &keys,
                        &mut work,
                    )?)?,
                }
            };
            let Some(need) = need else {
                break;
            };
            self.certificate(&need)?;
        }
        // The evaluator already bound every v2 family to reader.vault_id().
        // Re-running the full collector solely for binding would duplicate work.
        self.policies_verified.insert(kind);
        Ok(())
    }
}

pub(crate) fn capture_graph_inputs(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    request: GraphInputRequest<'_>,
    meter: &mut GraphOperationMeter,
) -> Result<SelectedGraphInput> {
    if !reader.normalized_layout() {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "selected graph inputs require normalized publication",
        ));
    }
    capture(fs, reader, request, meter)
}
fn capture(
    fs: &VaultFs,
    reader: &dyn CaptureCatalog,
    request: GraphInputRequest<'_>,
    meter: &mut GraphOperationMeter,
) -> Result<SelectedGraphInput> {
    let mut c = Capture {
        fs,
        reader,
        meter,
        documents: BTreeMap::new(),
        absent: BTreeSet::new(),
        notes: BTreeMap::new(),
        identities: BTreeMap::new(),
        records: BTreeMap::new(),
        certificates: BTreeMap::new(),
        sources: BTreeSet::new(),
        artifacts: BTreeSet::new(),
        evidence_members: BTreeMap::new(),
        // One capture per command consumes the reader's already cumulative
        // decoded bytes, including its header/identity probes before capture.
        query_bytes_seen: 0,
        policies_verified: BTreeSet::new(),
        source_tuples: BTreeMap::new(),
    };
    let vault = c.require(reader.vault_id(), RecordKind::Vault, None)?;
    if vault.id() != reader.vault_id() || c.identities[vault.id()][0].path.as_str() != "WIKI.md" {
        return Err(corrupt("published actual Vault identity/header differs"));
    }
    let mut changed_keys = BTreeSet::new();
    let mut changed_paths = BTreeSet::new();
    let family = match request {
        GraphInputRequest::Packet(r) => {
            c.source(&r.source_id)?;
            if let Some(revision) = &r.revision_id {
                c.selected_revision(&r.source_id, revision)?;
            }
            c.candidates(&r.candidate_context)?;
            GraphOperationFamily::PacketPersist
        }
        GraphInputRequest::Import(packet) => {
            c.packet(packet)?;
            changed_keys.insert(PolicyInputKey::Extractions);
            GraphOperationFamily::GraphImport
        }
        GraphInputRequest::Resolve(r) => {
            c.require(
                &r.extraction_id,
                RecordKind::Extraction,
                Some(&r.expected_hash),
            )?;
            c.artifact(&r.extraction_id)?;
            changed_keys.insert(PolicyInputKey::Extractions);
            changed_keys.insert(PolicyInputKey::CanonicalIdentity(r.extraction_id.clone()));
            changed_paths.insert(c.identities[&r.extraction_id][0].path.clone());
            for mapping in &r.mappings {
                if let ResolutionMapping::BindMention {
                    entity_id,
                    expected_entity_hash,
                    ..
                } = mapping
                {
                    c.require(entity_id, RecordKind::Entity, Some(expected_entity_hash))?;
                }
            }
            GraphOperationFamily::MentionResolve
        }
        GraphInputRequest::Review(r) => {
            let targets = r
                .decisions
                .iter()
                .map(|d| d.assertion_id.clone())
                .collect::<BTreeSet<_>>();
            let declared = r
                .supersedes
                .iter()
                .map(|p| (p.record_id.clone(), p.hash.clone()))
                .collect::<BTreeMap<_, _>>();
            let mut predecessors = BTreeSet::new();
            for d in &r.decisions {
                c.require(
                    &d.assertion_id,
                    RecordKind::Assertion,
                    Some(&d.expected_hash),
                )?;
                c.assertion(&d.assertion_id)?;
                let mut actual = BTreeMap::new();
                for eid in &c.evidence_members[&d.assertion_id] {
                    let claim = &c.identities[eid][0];
                    let evidence = c.notes[&claim.path]
                        .canonical
                        .as_ref()
                        .ok_or_else(|| corrupt("captured Evidence malformed"))?;
                    if evidence.string("wiki_status") == Some("active") {
                        actual.insert(eid.clone(), claim.hash.clone());
                    }
                }
                let expected = d
                    .evidence_checks
                    .iter()
                    .map(|e| (e.evidence_id.clone(), e.expected_hash.clone()))
                    .collect::<BTreeMap<_, _>>();
                if actual != expected || expected.len() != d.evidence_checks.len() {
                    return Err(conflict(
                        "review checks differ from complete active Evidence membership",
                    ));
                }
                c.certificate(&PolicyInputKey::ActiveDecisionOutput(
                    d.assertion_id.clone(),
                ))?;
                for edge in
                    reader.edges(&d.assertion_id, &[EligibilityRole::DecisionInput], true)?
                {
                    c.require(&edge.owner_id, RecordKind::Decision, None)?;
                }
            }
            for note in c.notes.values() {
                if let Some(record) = &note.canonical {
                    if record.kind() == RecordKind::Decision
                        && record.string("wiki_status") == Some("active")
                        && matches!(record.string("wiki_action"), Some("accept" | "reject"))
                    {
                        let ids = ["wiki_input_ids", "wiki_output_ids"]
                            .into_iter()
                            .flat_map(|field| {
                                record
                                    .field(field)
                                    .and_then(serde_json::Value::as_array)
                                    .into_iter()
                                    .flatten()
                            })
                            .map(|v| {
                                RecordId::new(
                                    v.as_str()
                                        .ok_or_else(|| corrupt("invalid predecessor ID"))?,
                                )
                            })
                            .collect::<Result<BTreeSet<_>>>()?;
                        if !ids.is_disjoint(&targets) {
                            if !ids.is_subset(&targets)
                                || declared.get(record.id()) != Some(&note.source_hash)
                            {
                                return Err(conflict("review predecessor full scope/hash differs"));
                            }
                            predecessors.insert(record.id().clone());
                        }
                    }
                }
            }
            if predecessors != declared.keys().cloned().collect() {
                return Err(conflict("review predecessor list missing/spurious"));
            }
            c.policy(PolicyKind::Review)?;
            GraphOperationFamily::AssertionReview
        }
    };
    // Captured WIKI/supplier/candidate notes are unchanged inputs, not changed
    // policy keys. Future allocated writes are expanded by the projector.
    for kind in reader.affected(&changed_keys, &changed_paths)? {
        c.policy(kind)?;
    }
    c.tick()?;
    let dependencies = c
        .documents
        .iter()
        .map(|(path, d)| ReadDependency {
            path: path.clone(),
            expected: ExpectedState::Hash(d.hash.clone()),
        })
        .chain(c.absent.iter().map(|path| ReadDependency {
            path: path.clone(),
            expected: ExpectedState::Absent,
        }))
        .collect();
    let input = ValidationInput {
        vault_id: reader.vault_id().clone(),
        documents: c.documents.into_values().collect(),
        overlay: c
            .absent
            .into_iter()
            .map(|path| crate::changes::ProposedTarget { path, bytes: None })
            .collect(),
    };
    Ok(SelectedGraphInput {
        root: fs.root().path().to_path_buf(),
        input,
        dependencies,
        snapshot: reader.snapshot().clone(),
        family,
        certificates: c.certificates,
        identities: c.identities,
        evidence_members: c.evidence_members,
        source_tuples: c.source_tuples,
    })
}

#[cfg(test)]
#[path = "normalized_input_tests.rs"]
mod tests;
