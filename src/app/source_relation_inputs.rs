//! Closed captured-input authority for source-relation publication. No reads,
//! discovery, global SourceView, catalog or proposed-artifact authority here.
use crate::{
    changes::ReadDependency,
    domain::{Blake3Hash, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    jobs::{Capability, TaskSpec, TaskStage},
    records::{ParsedNote, parse_note},
    retrieval::source_relations::SourceBinding,
    sources::{
        revision::canonical_path,
        selected::{SelectedSourceBinding, verify_captured_source},
    },
    vault::{ExpectedState, VaultFs},
};
use std::{collections::BTreeMap, time::Instant};

pub(crate) const MAX_CAPTURED_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_CONTROL_BYTES: usize = 64 * 1024;
pub(crate) const MAX_CONTENT_BYTES: usize = 256 * 1024;
const MAX_PATH_BYTES: usize = 4096;
/// Own digest pass ≤T. selected::bound_note hashes WIKI once and Source/Revision
/// twice (≤2T); revision_content_inner hashes both original and content twice
/// (≤4T even when payloads alias each other or control records). Thus ≤7T total.
/// Reserve the whole bound before hashing; retain it on any subsequent failure.
const HASH_RESERVATION_FACTOR: u64 = 7;

#[derive(Clone, Copy, Debug)]
pub(crate) struct InputLimits {
    pub max_captured_bytes: u64,
    pub max_hash_bytes: u64,
    pub deadline: Instant,
}
#[derive(Debug)]
pub(crate) struct AuthenticatedRelationSource {
    pub binding: SourceBinding,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained proof and accounting metadata is inspected by focused contract tests."
        )
    )]
    pub selected_binding: SelectedSourceBinding,
    pub text: String,
    pub dependencies: Vec<ReadDependency>,
    pub captured_bytes: u64,
    /// Conservative charged reservation, NOT observed actual hash work.
    pub reserved_hash_bytes: u64,
    /// Only the known digest pass performed by this module; excludes the opaque
    /// selected validator. Do not report this as the operation's total usage.
    pub known_hash_bytes: u64,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retained proof and accounting metadata is inspected by focused contract tests."
        )
    )]
    pub known_hashes: usize,
}
fn integrity(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::SourceIntegrity, message)
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(budget(
            "captured source-relation validation deadline exceeded",
        ));
    }
    Ok(())
}

/// `fs` is passed only to verify_captured_source's closed API. The caller owns
/// bounded capture and read accounting, global identity guards and final reread.
pub(crate) fn authenticate_captured(
    fs: &VaultFs,
    vault: &RecordId,
    task: &TaskSpec,
    captured: &BTreeMap<VaultRelativePath, Vec<u8>>,
    limits: InputLimits,
) -> Result<AuthenticatedRelationSource> {
    let mut reserved = 0u64;
    let mut known = 0u64;
    let mut hashes = 0usize;
    let result = (|| {
        check(limits.deadline)?;
        if task.stage != TaskStage::PrepareSourceRelations
            || task.capability != Some(Capability::Generate)
            || !(4..=5).contains(&captured.len())
            || task.source_bindings.len() != captured.len()
        {
            return Err(integrity(
                "relation task requires exactly four or five captured source dependencies",
            ));
        }
        let mut guards = BTreeMap::new();
        for dependency in &task.source_bindings {
            if !matches!(dependency.expected, ExpectedState::Hash(_))
                || dependency.path.as_str().len() > MAX_PATH_BYTES
                || guards
                    .insert(&dependency.path, &dependency.expected)
                    .is_some()
            {
                return Err(integrity(
                    "captured relation guards must be unique bounded paths with Hash states",
                ));
            }
        }
        if !guards.keys().copied().eq(captured.keys()) {
            return Err(integrity("task guards differ from captured input paths"));
        }
        let mut total = 0u64;
        for (path, bytes) in captured {
            check(limits.deadline)?;
            if path.as_str().len() > MAX_PATH_BYTES
                || (canonical_path(path) && bytes.len() > MAX_CONTROL_BYTES)
            {
                return Err(budget("captured relation control or path exceeds ceiling"));
            }
            total = total
                .checked_add(bytes.len() as u64)
                .filter(|&n| n <= MAX_CAPTURED_BYTES as u64 && n <= limits.max_captured_bytes)
                .ok_or_else(|| budget("captured relation input exceeds aggregate allowance"))?;
        }
        let bound = total
            .checked_mul(HASH_RESERVATION_FACTOR)
            .ok_or_else(|| budget("captured relation hash reservation overflow"))?;
        if bound > limits.max_hash_bytes {
            return Err(budget(
                "captured relation hash reservation exceeds remaining allowance",
            ));
        }
        check(limits.deadline)?;
        // Once work starts failures retain this entire conservative charge.
        reserved = bound;
        let mut notes: BTreeMap<VaultRelativePath, ParsedNote> = BTreeMap::new();
        for (path, bytes) in captured {
            check(limits.deadline)?;
            known += bytes.len() as u64;
            hashes += 1;
            let hash = if canonical_path(path) {
                let note = parse_note(bytes); // Includes one exact digest.
                let hash = note.source_hash.clone();
                if note.canonical.is_none() {
                    return Err(integrity("captured relation control is not canonical"));
                }
                notes.insert(path.clone(), note);
                hash
            } else {
                Blake3Hash::digest(bytes)
            };
            if guards[path] != &ExpectedState::Hash(hash) {
                return Err(integrity(
                    "captured relation bytes differ from authenticated task guard",
                ));
            }
            check(limits.deadline)?;
        }
        let wiki_path = VaultRelativePath::new("WIKI.md")?;
        let wiki = notes
            .get(&wiki_path)
            .and_then(|n| n.canonical.as_ref())
            .ok_or_else(|| integrity("captured relation vault marker missing"))?;
        if wiki.kind() != RecordKind::Vault || wiki.id() != vault {
            return Err(integrity("captured relation vault identity differs"));
        }
        let mut sources = notes.iter().filter(|(_, n)| {
            n.canonical
                .as_ref()
                .is_some_and(|r| r.kind() == RecordKind::Source)
        });
        let (source_path, source_note) = sources
            .next()
            .ok_or_else(|| integrity("captured relation Source missing"))?;
        if sources.next().is_some() {
            return Err(integrity("captured relation contains multiple Sources"));
        }
        let mut revisions = notes.iter().filter(|(_, n)| {
            n.canonical
                .as_ref()
                .is_some_and(|r| r.kind() == RecordKind::Revision)
        });
        let (revision_path, revision_note) = revisions
            .next()
            .ok_or_else(|| integrity("captured relation Revision missing"))?;
        if revisions.next().is_some() || notes.len() != 3 {
            return Err(integrity(
                "captured relation requires only WIKI, one Source and one Revision controls",
            ));
        }
        let source = source_note
            .canonical
            .as_ref()
            .expect("canonical selected above");
        let revision = revision_note
            .canonical
            .as_ref()
            .expect("canonical selected above");
        if source.string("wiki_status") != Some("active")
            || source.string("wiki_current_revision") != Some(revision.id().as_str())
            || revision.string("wiki_source_id") != Some(source.id().as_str())
            || revision.string("wiki_extraction_status") != Some("complete")
        {
            return Err(integrity(
                "captured relation Source is withdrawn, stale or has a foreign/incomplete revision",
            ));
        }
        let parent = revision_path
            .as_str()
            .rsplit_once('/')
            .map(|(p, _)| p)
            .ok_or_else(|| integrity("captured relation Revision directory missing"))?;
        let relative = revision
            .string("wiki_content_path")
            .ok_or_else(|| integrity("captured relation content path missing"))?;
        let content_path = VaultRelativePath::new(format!("{parent}/{relative}"))?;
        let content = captured
            .get(&content_path)
            .ok_or_else(|| integrity("captured relation content missing"))?;
        if content.len() > MAX_CONTENT_BYTES {
            return Err(budget(
                "captured relation content exceeds 256 KiB; no truncation performed",
            ));
        }
        let content_hash = Blake3Hash::new(
            revision
                .string("wiki_content_hash")
                .ok_or_else(|| integrity("captured relation content hash missing"))?,
        )?;
        let selected_binding = SelectedSourceBinding {
            vault_id: vault.clone(),
            source_id: source.id().clone(),
            revision_id: revision.id().clone(),
            source_path: source_path.clone(),
            revision_path: revision_path.clone(),
            content_path,
            source_hash: source_note.source_hash.clone(),
            revision_hash: revision_note.source_hash.clone(),
            content_hash,
        };
        check(limits.deadline)?;
        let proof = verify_captured_source(fs, &selected_binding, captured)?;
        check(limits.deadline)?;
        let expected: Vec<_> = guards
            .into_iter()
            .map(|(path, expected)| ReadDependency {
                path: path.clone(),
                expected: expected.clone(),
            })
            .collect();
        if proof.dependencies != expected {
            return Err(integrity(
                "selected relation proof dependencies differ from authenticated task",
            ));
        }
        let text = String::from_utf8(proof.content)
            .map_err(|_| integrity("captured relation content is not UTF-8"))?;
        let binding = SourceBinding {
            source_id: selected_binding.source_id.clone(),
            // RevisionId is the domain alias for RecordId; no lossy conversion.
            source_revision: selected_binding.revision_id.clone(),
            raw_hash: selected_binding.content_hash.clone(),
        };
        Ok(AuthenticatedRelationSource {
            binding,
            selected_binding,
            text,
            dependencies: proof.dependencies,
            captured_bytes: total,
            reserved_hash_bytes: reserved,
            known_hash_bytes: known,
            known_hashes: hashes,
        })
    })();
    result.map_err(|mut error| {
        error.details = serde_json::json!({
            "source_relation_inputs_reserved_hash_bytes": reserved,
            "source_relation_inputs_known_hash_bytes": known,
            "source_relation_inputs_known_hashes": hashes,
            "source_relation_inputs_actual_total_hash_bytes": "unavailable",
        });
        error
    })
}

#[cfg(test)]
#[path = "source_relation_inputs_tests.rs"]
pub(crate) mod tests;
