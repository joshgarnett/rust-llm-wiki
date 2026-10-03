//! Pure verification of one explicitly selected captured source.
//!
//! This proves only the supplied bytes and their relationships. The caller owns
//! safe path resolution, metered capture, operational guards and a final reread.
//! In particular this subset is not a complete identity registry, a discovery
//! proof, or a claim that the vault is globally current.
use super::{
    SourceView,
    revision::{canonical_path, dependencies, integrity},
};
use crate::{
    changes::ReadDependency,
    domain::{Blake3Hash, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    records::{LinkResolution, ParsedNote, RegistryEntry, parse_note, resolve_typed},
    vault::{ExpectedState, VaultFs},
};
use std::collections::BTreeMap;

const MAX_CAPTURED_FILES: usize = 5;
const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CAPTURED_BYTES: usize = 128 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct SelectedSourceBinding {
    pub vault_id: RecordId,
    pub source_id: RecordId,
    pub revision_id: RecordId,
    pub source_path: VaultRelativePath,
    pub revision_path: VaultRelativePath,
    pub content_path: VaultRelativePath,
    pub source_hash: Blake3Hash,
    pub revision_hash: Blake3Hash,
    pub content_hash: Blake3Hash,
}

#[derive(Debug)]
pub(crate) struct SelectedSourceProof {
    pub content: Vec<u8>,
    pub dependencies: Vec<ReadDependency>,
}

fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}

fn preflight(captured: &BTreeMap<VaultRelativePath, Vec<u8>>) -> Result<()> {
    if captured.len() > MAX_CAPTURED_FILES {
        return Err(budget("selected source capture exceeds file ceiling"));
    }
    let mut total = 0usize;
    for bytes in captured.values() {
        if bytes.len() > MAX_FILE_BYTES {
            return Err(budget("selected source file exceeds byte ceiling"));
        }
        total = total
            .checked_add(bytes.len())
            .filter(|&sum| sum <= MAX_CAPTURED_BYTES)
            .ok_or_else(|| budget("selected source capture exceeds aggregate byte ceiling"))?;
    }
    Ok(())
}

fn bound_note(
    captured: &BTreeMap<VaultRelativePath, Vec<u8>>,
    path: &VaultRelativePath,
    id: &RecordId,
    kind: RecordKind,
    hash: Option<&Blake3Hash>,
) -> Result<ParsedNote> {
    let bytes = captured
        .get(path)
        .ok_or_else(|| integrity(format!("selected record absent from capture: {path}")))?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(budget("selected source record exceeds byte ceiling"));
    }
    if !canonical_path(path) {
        return Err(integrity(format!(
            "selected record path is not canonical: {path}"
        )));
    }
    if hash.is_some_and(|expected| &Blake3Hash::digest(bytes) != expected) {
        return Err(integrity(format!("selected record hash mismatch: {path}")));
    }
    let note = parse_note(bytes);
    if !note
        .canonical
        .as_ref()
        .is_some_and(|record| record.kind() == kind && record.id() == id)
    {
        return Err(integrity(format!(
            "selected record identity or kind mismatch: {path}"
        )));
    }
    Ok(note)
}

/// Verify only captured bytes, without accessing `fs` (even for missing assets).
///
/// `fs` supplies the existing SourceView lifetime/API; its closed mode ensures it
/// cannot be consulted. Internal limits are allocation safety ceilings, separate
/// from the caller's shared proof meter. All limits precede payload cloning.
pub(crate) fn verify_captured_source(
    fs: &VaultFs,
    binding: &SelectedSourceBinding,
    captured: &BTreeMap<VaultRelativePath, Vec<u8>>,
) -> Result<SelectedSourceProof> {
    preflight(captured)?;
    let wiki_path = VaultRelativePath::new("WIKI.md")?;
    // Check all record lengths before parsing/cloning even the first envelope.
    for path in [&wiki_path, &binding.source_path, &binding.revision_path] {
        if captured
            .get(path)
            .is_some_and(|bytes| bytes.len() > MAX_RECORD_BYTES)
        {
            return Err(budget("selected source record exceeds byte ceiling"));
        }
    }
    let wiki = bound_note(
        captured,
        &wiki_path,
        &binding.vault_id,
        RecordKind::Vault,
        None,
    )?;
    let source = bound_note(
        captured,
        &binding.source_path,
        &binding.source_id,
        RecordKind::Source,
        Some(&binding.source_hash),
    )?;
    let revision = bound_note(
        captured,
        &binding.revision_path,
        &binding.revision_id,
        RecordKind::Revision,
        Some(&binding.revision_hash),
    )?;
    let source_record = source.canonical.as_ref().expect("bound canonical record");
    if source_record.string("wiki_status") != Some("active")
        || source_record.string("wiki_current_revision") != Some(binding.revision_id.as_str())
    {
        return Err(integrity(
            "selected source is not active at the bound revision head",
        ));
    }
    let revision_record = revision.canonical.as_ref().expect("bound canonical record");
    let parent = binding
        .revision_path
        .as_str()
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .ok_or_else(|| integrity("selected revision has no directory"))?;
    let content_relative = revision_record
        .string("wiki_content_path")
        .ok_or_else(|| integrity("selected revision has no extracted content path"))?;
    let content_path = VaultRelativePath::new(format!("{parent}/{content_relative}"))?;
    if content_path != binding.content_path
        || revision_record.string("wiki_content_hash") != Some(binding.content_hash.as_str())
    {
        return Err(integrity(
            "selected content path or hash disagrees with revision",
        ));
    }

    // Full-vault resolution permits stale navigation after checking the complete
    // registry. This subset cannot distinguish a stale path from a conflicting
    // record omitted from the capture. A present companion must therefore match
    // its bound record here. Omitted optional navigation remains permitted.
    let registry = [
        RegistryEntry {
            id: binding.source_id.clone(),
            kind: RecordKind::Source,
            path: binding.source_path.clone(),
            aliases: vec![],
        },
        RegistryEntry {
            id: binding.revision_id.clone(),
            kind: RecordKind::Revision,
            path: binding.revision_path.clone(),
            aliases: vec![],
        },
    ];
    for (companion, id, kind, path) in [
        (
            source_record.string("wiki_revision"),
            &binding.revision_id,
            RecordKind::Revision,
            &binding.revision_path,
        ),
        (
            revision_record.string("wiki_source"),
            &binding.source_id,
            RecordKind::Source,
            &binding.source_path,
        ),
    ] {
        if let Some(companion) = companion
            && !matches!(
                resolve_typed(&registry, id, kind, Some(companion)),
                LinkResolution::Resolved { path: actual, companion_stale: false, .. }
                    if &actual == path
            )
        {
            return Err(integrity(
                "selected companion is not bound to its captured record path",
            ));
        }
    }

    // Build a deliberately closed, selected-only view. Reuse the canonical
    // revision validator without representing this subset as ValidationInput's
    // complete scan. Local ID resolution here says nothing about other files.
    let mut deps = BTreeMap::from([(
        wiki_path.clone(),
        ExpectedState::Hash(wiki.source_hash.clone()),
    )]);
    let notes = BTreeMap::from([
        (wiki_path, wiki),
        (binding.source_path.clone(), source),
        (binding.revision_path.clone(), revision),
    ]);
    let overlay = captured
        .iter()
        .filter(|(path, _)| !notes.contains_key(*path))
        .map(|(path, bytes)| (path.clone(), Some(bytes.clone())))
        .collect();
    let view = SourceView {
        fs,
        notes: notes.into(),
        overlay,
        closed: true,
    };
    let content = view.revision_content(&binding.source_id, &binding.revision_id, &mut deps)?;
    if deps.len() != captured.len() || !deps.keys().eq(captured.keys()) {
        return Err(integrity(
            "capture includes files outside the selected source proof",
        ));
    }
    Ok(SelectedSourceProof {
        content,
        dependencies: dependencies(deps),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::CanonicalRecord,
        sources::revision::{common, record_bytes},
        vault::VaultRoot,
    };
    use serde_json::{Value, json};

    struct Fixture {
        temp: tempfile::TempDir,
        fs: VaultFs,
        binding: SelectedSourceBinding,
        captured: BTreeMap<VaultRelativePath, Vec<u8>>,
        original_path: VaultRelativePath,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
            let vault_id = RecordId::generate(RecordKind::Vault).unwrap();
            let source_id = RecordId::generate(RecordKind::Source).unwrap();
            let revision_id = RecordId::generate(RecordKind::Revision).unwrap();
            let source_path =
                VaultRelativePath::new(format!("sources/{source_id}/source.md")).unwrap();
            let parent = format!("sources/{source_id}/revisions/{revision_id}");
            let revision_path = VaultRelativePath::new(format!("{parent}/revision.md")).unwrap();
            let original_path = VaultRelativePath::new(format!("{parent}/original.bin")).unwrap();
            let content_path = VaultRelativePath::new(format!("{parent}/content.md")).unwrap();
            let content = "Selected café evidence.\n".as_bytes().to_vec();
            let content_hash = Blake3Hash::digest(&content);
            let wiki = record_bytes(
                CanonicalRecord::new(common(&vault_id, RecordKind::Vault, "Fixture")).unwrap(),
                b"",
            )
            .unwrap();
            let mut fields = common(&source_id, RecordKind::Source, "Selected source");
            fields.extend(BTreeMap::from([
                ("wiki_status".into(), json!("active")),
                ("wiki_origin_kind".into(), json!("local-file")),
                ("wiki_origin".into(), json!("fixture.txt")),
                ("wiki_current_revision".into(), json!(revision_id)),
                ("wiki_revisions".into(), json!([revision_id])),
                (
                    "wiki_revision".into(),
                    json!(format!("[[{revision_path}]]")),
                ),
            ]));
            let source = record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap();
            let mut fields = common(&revision_id, RecordKind::Revision, "Selected revision");
            fields.extend(BTreeMap::from([
                ("wiki_source_id".into(), json!(source_id)),
                ("wiki_source".into(), json!(format!("[[{source_path}]]"))),
                ("wiki_captured_at".into(), json!("2026-10-03T00:00:00Z")),
                ("wiki_original_path".into(), json!("original.bin")),
                ("wiki_original_hash".into(), json!(content_hash)),
                ("wiki_extractor".into(), json!("utf8-preserve")),
                (
                    "wiki_extractor_fingerprint".into(),
                    json!(Blake3Hash::digest(b"fixture extractor")),
                ),
                ("wiki_extraction_status".into(), json!("complete")),
                ("wiki_content_path".into(), json!("content.md")),
                ("wiki_content_hash".into(), json!(content_hash)),
            ]));
            let revision = record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap();
            let binding = SelectedSourceBinding {
                vault_id,
                source_id,
                revision_id,
                source_path: source_path.clone(),
                revision_path: revision_path.clone(),
                content_path: content_path.clone(),
                source_hash: Blake3Hash::digest(&source),
                revision_hash: Blake3Hash::digest(&revision),
                content_hash,
            };
            let captured = BTreeMap::from([
                (VaultRelativePath::new("WIKI.md").unwrap(), wiki),
                (source_path, source),
                (revision_path, revision),
                (original_path.clone(), content.clone()),
                (content_path, content),
            ]);
            Self {
                temp,
                fs,
                binding,
                captured,
                original_path,
            }
        }

        fn verify(&self) -> Result<SelectedSourceProof> {
            verify_captured_source(&self.fs, &self.binding, &self.captured)
        }

        fn edit(&mut self, source: bool, key: &str, value: Value) {
            let path = if source {
                &self.binding.source_path
            } else {
                &self.binding.revision_path
            };
            let mut fields = parse_note(&self.captured[path]).fields.unwrap();
            fields.insert(key.into(), value);
            let bytes = record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap();
            let hash = Blake3Hash::digest(&bytes);
            if source {
                self.binding.source_hash = hash;
            } else {
                self.binding.revision_hash = hash;
            }
            self.captured.insert(path.clone(), bytes);
        }

        fn materialize(&self) {
            for (path, bytes) in &self.captured {
                let target = self.temp.path().join(path.as_str());
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(target, bytes).unwrap();
            }
        }
    }

    #[test]
    fn valid_capture_provenance_and_no_filesystem_dependency() {
        let fixture = Fixture::new();
        // The handle was made while this root existed. Remove it entirely:
        // successful validation cannot require even WIKI.md to exist on disk.
        std::fs::remove_dir_all(fixture.temp.path()).unwrap();
        let proof = fixture.verify().unwrap();
        assert_eq!(
            proof.content,
            fixture.captured[&fixture.binding.content_path]
        );
        let expected: Vec<_> = fixture
            .captured
            .iter()
            .map(|(path, bytes)| ReadDependency {
                path: path.clone(),
                expected: ExpectedState::Hash(Blake3Hash::digest(bytes)),
            })
            .collect();
        assert_eq!(proof.dependencies, expected);
    }

    #[test]
    fn missing_captured_records_or_assets_never_fall_back_to_matching_disk_bytes() {
        let fixture = Fixture::new();
        fixture.materialize();
        for missing in fixture.captured.keys() {
            let mut captured = fixture.captured.clone();
            captured.remove(missing);
            let error =
                verify_captured_source(&fixture.fs, &fixture.binding, &captured).unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::SourceIntegrity,
                "{missing}: {error:?}"
            );
        }
    }

    #[test]
    fn unrelated_disk_identity_claims_are_outside_this_selected_proof() {
        let fixture = Fixture::new();
        fixture.materialize();
        std::fs::write(
            fixture.temp.path().join("duplicate-source.md"),
            &fixture.captured[&fixture.binding.source_path],
        )
        .unwrap();
        // This intentionally succeeds: callers must not turn selected-only
        // verification into an assertion that identities are globally unique.
        assert_eq!(fixture.verify().unwrap().dependencies.len(), 5);
    }

    #[test]
    fn wrong_bound_vault_ids_hashes_and_paths_fail() {
        for case in 0..9 {
            let mut fixture = Fixture::new();
            match case {
                0 => fixture.binding.vault_id = RecordId::generate(RecordKind::Vault).unwrap(),
                1 => fixture.binding.source_id = RecordId::generate(RecordKind::Source).unwrap(),
                2 => {
                    fixture.binding.revision_id = RecordId::generate(RecordKind::Revision).unwrap()
                }
                3 => fixture.binding.source_hash = Blake3Hash::digest(b"different"),
                4 => fixture.binding.revision_hash = Blake3Hash::digest(b"different"),
                5 => fixture.binding.content_hash = Blake3Hash::digest(b"different"),
                6 => {
                    fixture.binding.source_path =
                        VaultRelativePath::new("elsewhere/source.md").unwrap()
                }
                7 => {
                    fixture.binding.revision_path =
                        VaultRelativePath::new("elsewhere/revision.md").unwrap()
                }
                8 => fixture.binding.content_path = fixture.original_path.clone(),
                _ => unreachable!(),
            }
            assert!(fixture.verify().is_err(), "case {case}");
        }
    }

    #[test]
    fn wrong_owner_head_status_companions_and_manifest_fail_after_rebinding_hashes() {
        for case in 0..8 {
            let mut fixture = Fixture::new();
            let other_source = RecordId::generate(RecordKind::Source).unwrap();
            let other_revision = RecordId::generate(RecordKind::Revision).unwrap();
            match case {
                0 => fixture.edit(false, "wiki_source_id", json!(other_source)),
                1 => fixture.edit(true, "wiki_current_revision", json!(other_revision)),
                2 => fixture.edit(true, "wiki_status", json!("withdrawn")),
                3 => fixture.edit(true, "wiki_revision", json!("[[elsewhere/revision.md]]")),
                4 => fixture.edit(false, "wiki_source", json!("[[elsewhere/source.md]]")),
                5 => fixture.edit(true, "wiki_revisions", json!([other_revision])),
                6 => fixture.edit(
                    true,
                    "wiki_revisions",
                    json!([fixture.binding.revision_id, fixture.binding.revision_id]),
                ),
                7 => fixture.edit(
                    false,
                    "wiki_original_hash",
                    json!(Blake3Hash::digest(b"different")),
                ),
                _ => unreachable!(),
            }
            assert!(fixture.verify().is_err(), "case {case}");
        }
    }

    #[test]
    fn companion_fragments_labels_extensionless_paths_and_omission_remain_valid() {
        let mut fixture = Fixture::new();
        fixture.edit(
            true,
            "wiki_revision",
            json!(format!(
                "[[{}#heading|Revision]]",
                fixture.binding.revision_path
            )),
        );
        fixture.edit(
            false,
            "wiki_source",
            json!(format!(
                "[[{}]]",
                fixture
                    .binding
                    .source_path
                    .as_str()
                    .strip_suffix(".md")
                    .unwrap()
            )),
        );
        fixture.verify().unwrap();
        for (source, key) in [(true, "wiki_revision"), (false, "wiki_source")] {
            let path = if source {
                &fixture.binding.source_path
            } else {
                &fixture.binding.revision_path
            };
            let mut fields = parse_note(&fixture.captured[path]).fields.unwrap();
            fields.remove(key);
            let bytes = record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap();
            let hash = Blake3Hash::digest(&bytes);
            fixture.captured.insert(path.clone(), bytes);
            if source {
                fixture.binding.source_hash = hash;
            } else {
                fixture.binding.revision_hash = hash;
            }
        }
        fixture.verify().unwrap();
    }

    #[test]
    fn unsupported_and_failed_extraction_are_rejected() {
        for status in ["unsupported", "failed"] {
            let mut fixture = Fixture::new();
            let path = fixture.binding.revision_path.clone();
            let mut fields = parse_note(&fixture.captured[&path]).fields.unwrap();
            fields.insert("wiki_extraction_status".into(), json!(status));
            fields.remove("wiki_content_path");
            fields.remove("wiki_content_hash");
            let bytes = record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap();
            fixture.binding.revision_hash = Blake3Hash::digest(&bytes);
            fixture.captured.insert(path, bytes);
            assert!(fixture.verify().is_err());
        }
    }

    #[test]
    fn entire_payload_hashes_and_utf8_are_checked() {
        let mut fixture = Fixture::new();
        fixture
            .captured
            .get_mut(&fixture.original_path)
            .unwrap()
            .push(b'!');
        assert!(fixture.verify().is_err());
        let mut fixture = Fixture::new();
        fixture
            .captured
            .get_mut(&fixture.binding.content_path)
            .unwrap()
            .push(b'!');
        assert!(fixture.verify().is_err());
        let invalid = vec![b'a', 0xff, b'z'];
        let mut fixture = Fixture::new();
        fixture.edit(
            false,
            "wiki_content_hash",
            json!(Blake3Hash::digest(&invalid)),
        );
        fixture.binding.content_hash = Blake3Hash::digest(&invalid);
        fixture
            .captured
            .insert(fixture.binding.content_path.clone(), invalid);
        let error = fixture.verify().unwrap_err();
        assert!(error.message.contains("UTF-8"), "{error:?}");
    }

    #[test]
    fn extra_files_and_oversized_records_are_refused() {
        let mut fixture = Fixture::new();
        fixture
            .captured
            .insert(VaultRelativePath::new("extra.md").unwrap(), vec![]);
        assert_eq!(
            fixture.verify().unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        let mut fixture = Fixture::new();
        fixture.captured.insert(
            fixture.binding.source_path.clone(),
            vec![b'x'; MAX_RECORD_BYTES + 1],
        );
        // This must be the ceiling failure, before hash or record parsing.
        assert_eq!(
            fixture.verify().unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
    }

    #[test]
    fn payload_and_aggregate_byte_ceilings_precede_record_parsing() {
        // Deliberately omit WIKI.md: ceiling errors must precede even the first
        // canonical-record lookup or allocation. These inputs never clone.
        let fixture = Fixture::new();
        let mut captured =
            BTreeMap::from([(fixture.original_path.clone(), vec![0; MAX_FILE_BYTES + 1])]);
        let error = verify_captured_source(&fixture.fs, &fixture.binding, &captured).unwrap_err();
        assert_eq!(error.code, ErrorCode::BudgetExceeded);
        assert!(error.message.contains("file exceeds byte ceiling"));
        captured
            .get_mut(&fixture.original_path)
            .unwrap()
            .truncate(MAX_FILE_BYTES);
        captured.insert(
            fixture.binding.content_path.clone(),
            vec![0; MAX_FILE_BYTES],
        );
        captured.insert(fixture.binding.source_path.clone(), vec![0]);
        let error = verify_captured_source(&fixture.fs, &fixture.binding, &captured).unwrap_err();
        assert_eq!(error.code, ErrorCode::BudgetExceeded);
        assert!(error.message.contains("aggregate byte ceiling"));
    }
}
