//! Rebuildable projection contracts, owned by the orchestrator.
use crate::vault::VaultFs;
use crate::{
    changes::ReadDependency,
    domain::{
        Blake3Hash, CanonicalRecord, Eligibility, ErrorCode, ReadSnapshot, RecordId, RecordKind,
        Result, VaultRelativePath,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Coordinator without an open connection; readers own their pinned transaction.
pub struct Catalog {
    pub(crate) fs: VaultFs,
    pub(crate) vault_id: RecordId,
    pub(crate) options: CatalogOptions,
}

#[derive(Clone)]
pub struct CatalogOptions {
    pub busy_timeout_ms: u64,
    pub fault: Option<Arc<dyn PublicationFault>>,
}
impl Default for CatalogOptions {
    fn default() -> Self {
        Self {
            busy_timeout_ms: 1_000,
            fault: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationCheckpoint {
    AfterOrdinaryRows,
    AfterDocumentsFts,
    AfterGraphFts,
    AfterPointer,
    AfterCommit,
    MigrationBeforeCommit,
}
pub trait PublicationFault: Send + Sync {
    fn check(&self, checkpoint: PublicationCheckpoint) -> Result<()>;
}

pub struct ReaderSnapshot {
    pub(crate) connection: rusqlite::Connection,
    pub(crate) snapshot: ReadSnapshot,
    pub(crate) projection: CatalogProjection,
    pub(crate) verification: SnapshotVerification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SnapshotVerification {
    IndexSnapshot,
    VerifiedSnapshot {
        verified_at: String,
    },
    /// Selected canonical source bytes checked against a pinned discovery index.
    /// This does not establish global membership, unique IDs or completeness.
    IndexedEvidence {
        verified_at: String,
        discovery_generation: u64,
        evidence_domain: String,
        global_membership_verified: bool,
        catalog_rows_decoded: usize,
        catalog_bytes_decoded: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pending_operation_at_start: Option<RecordId>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncReport {
    pub snapshot: ReadSnapshot,
    pub reused: bool,
    /// Retained notice of known vector loss during this cache's migration history.
    pub vector_cache_lost: bool,
    /// Retained notice that an absent/unknown old cache's vector contents are unknown.
    pub vector_loss_unknown: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogProjection {
    pub vault_id: RecordId,
    pub parser_fingerprint: Blake3Hash,
    pub control_manifest: Blake3Hash,
    pub documents: Vec<DocumentRow>,
    pub records: BTreeMap<RecordId, RecordRow>,
    pub graph: Vec<GraphRow>,
    pub links: Vec<LinkRow>,
    pub diagnostics: Vec<CatalogDiagnostic>,
    pub dependencies: Vec<ReadDependency>,
}

/// Complete validation authority without retrieval text or display rows.
/// Kept distinct from `CatalogProjection` so an omitted retrieval projection
/// cannot accidentally be published or treated as a complete catalog.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ValidationProjection {
    pub vault_id: RecordId,
    pub parser_fingerprint: Blake3Hash,
    pub control_manifest: Blake3Hash,
    pub records: BTreeMap<RecordId, RecordRow>,
    pub diagnostics: Vec<CatalogDiagnostic>,
    pub dependencies: Vec<ReadDependency>,
}

/// Full-build semantic results with factored proof inputs. The record rows do
/// not carry legacy transitive proofs and must never be passed to their readers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NormalizedValidationProjection {
    pub validation: ValidationProjection,
    pub facts: super::eligibility_facts::NormalizedEligibilityFacts,
}

/// Receives provisional retrieval rows without retaining corpus text in the projector.
/// A failed callback aborts projection; callers must discard provisional publication
/// state unless projection and subsequent freshness checks both succeed.
pub(crate) trait RetrievalSink {
    fn identity_claim(&mut self, row: IdentityClaimRow) -> Result<()>;
    fn document(&mut self, row: DocumentRow) -> Result<()>;
    fn graph(&mut self, row: GraphRow) -> Result<()>;
    fn link(&mut self, row: LinkRow) -> Result<()>;
    fn link_fact(&mut self, _row: super::link_facts::OwnedLinkFact) -> Result<()> {
        Ok(())
    }
    fn registry_keys(
        &mut self,
        _entry: &crate::records::RegistryEntry,
        _keys: &[super::link_facts::MatchKey],
    ) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdentityClaimRow {
    pub id: RecordId,
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
    pub kind: Option<RecordKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordRow {
    pub record: CanonicalRecord,
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
    pub authored_status: Option<String>,
    pub eligibility: Eligibility,
    pub reasons: Vec<String>,
    pub identity_eligibility: Option<Eligibility>,
    pub description_eligibility: Option<Eligibility>,
    pub disputed: bool,
    pub dependencies: Vec<ReadDependency>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentRow {
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
    pub record_id: Option<RecordId>,
    pub kind: Option<RecordKind>,
    pub title: String,
    pub aliases: Vec<String>,
    pub headings: String,
    pub tags: Vec<String>,
    pub body: String,
    pub raw_text: String,
    pub source_id: Option<RecordId>,
    pub owner_revision: Option<RecordId>,
    pub eligibility: Eligibility,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphRow {
    pub target_id: RecordId,
    pub target_kind: RecordKind,
    pub name: String,
    pub aliases: Vec<String>,
    pub endpoints: String,
    pub predicate: String,
    pub qualifiers: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkRow {
    pub from_path: VaultRelativePath,
    pub byte_start: u64,
    pub target_id: Option<RecordId>,
    pub target_path: Option<VaultRelativePath>,
    pub resolution: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogDiagnostic {
    pub path: VaultRelativePath,
    pub record_id: Option<RecordId>,
    pub code: ErrorCode,
    pub details: Value,
}

/// Pure full-graph validation. Publishing requires a separate held writer permit.
pub struct CatalogGraphValidator;
