//! Fictional captured-source fixtures, independent of provider/artifact codecs.
use super::*;
use crate::{
    domain::CanonicalRecord,
    jobs::BoundedPayloadRef,
    sources::revision::{common, record_bytes},
    vault::VaultRoot,
};
use serde_json::{Value, json};
use std::time::Duration;
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn hash(value: &[u8]) -> Blake3Hash {
    Blake3Hash::digest(value)
}
fn limits() -> InputLimits {
    InputLimits {
        max_captured_bytes: MAX_CAPTURED_BYTES as u64,
        max_hash_bytes: 7 * MAX_CAPTURED_BYTES as u64,
        deadline: Instant::now() + Duration::from_secs(10),
    }
}
pub(crate) struct Fixture {
    temp: tempfile::TempDir,
    pub(crate) fs: VaultFs,
    pub(crate) vault: RecordId,
    pub(crate) task: TaskSpec,
    pub(crate) captured: BTreeMap<VaultRelativePath, Vec<u8>>,
    source: VaultRelativePath,
    revision: VaultRelativePath,
    original: VaultRelativePath,
    pub(crate) content: VaultRelativePath,
}
impl Fixture {
    pub(crate) fn new(shared_asset: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
        let vault = id("fictional_vault");
        let source_id = id("fictional_source");
        let revision_id = id("fictional_revision");
        let source = VaultRelativePath::new("sources/fictional_source/source.md").unwrap();
        let revision = VaultRelativePath::new(
            "sources/fictional_source/revisions/fictional_revision/revision.md",
        )
        .unwrap();
        let content = VaultRelativePath::new(
            "sources/fictional_source/revisions/fictional_revision/content.md",
        )
        .unwrap();
        let original = if shared_asset {
            content.clone()
        } else {
            VaultRelativePath::new(
                "sources/fictional_source/revisions/fictional_revision/original.bin",
            )
            .unwrap()
        };
        let text = b"Fictional procedure requires notice.\n".to_vec();
        let mut fields = common(&source_id, RecordKind::Source, "Fictional source");
        fields.extend(BTreeMap::from([
            ("wiki_status".into(), json!("active")),
            ("wiki_origin_kind".into(), json!("local-file")),
            ("wiki_origin".into(), json!("fiction.txt")),
            ("wiki_current_revision".into(), json!(revision_id)),
            ("wiki_revisions".into(), json!([revision_id])),
        ]));
        let source_bytes = record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap();
        let mut fields = common(&revision_id, RecordKind::Revision, "Fictional revision");
        fields.extend(BTreeMap::from([
            ("wiki_source_id".into(), json!(source_id)),
            ("wiki_captured_at".into(), json!("2026-10-10T00:00:00Z")),
            (
                "wiki_original_path".into(),
                json!(if shared_asset {
                    "content.md"
                } else {
                    "original.bin"
                }),
            ),
            ("wiki_original_hash".into(), json!(hash(&text))),
            ("wiki_extractor".into(), json!("utf8-preserve")),
            (
                "wiki_extractor_fingerprint".into(),
                json!(hash(b"fictional extractor")),
            ),
            ("wiki_extraction_status".into(), json!("complete")),
            ("wiki_content_path".into(), json!("content.md")),
            ("wiki_content_hash".into(), json!(hash(&text))),
        ]));
        let revision_bytes = record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap();
        let wiki = record_bytes(
            CanonicalRecord::new(common(&vault, RecordKind::Vault, "Fictional vault")).unwrap(),
            b"",
        )
        .unwrap();
        let captured = BTreeMap::from([
            (VaultRelativePath::new("WIKI.md").unwrap(), wiki),
            (source.clone(), source_bytes),
            (revision.clone(), revision_bytes),
            (original.clone(), text.clone()),
            (content.clone(), text),
        ]);
        let task = TaskSpec {
            key: hash(b"authenticated-task"),
            stage: TaskStage::PrepareSourceRelations,
            capability: Some(Capability::Generate),
            priority: 0,
            dependencies: vec![],
            input_hash: hash(b"input"),
            prompt_hash: Some(hash(b"prompt")),
            schema_hash: Some(hash(b"schema")),
            model_hash: Some(hash(b"model")),
            settings_hash: hash(b"settings"),
            source_bindings: vec![],
            input: BoundedPayloadRef {
                path: VaultRelativePath::new(".wiki/cache/generation/inputs/fixture.json").unwrap(),
                hash: hash(b"descriptor"),
                byte_len: 10,
            },
        };
        let mut fixture = Self {
            temp,
            fs,
            vault,
            task,
            captured,
            source,
            revision,
            original,
            content,
        };
        fixture.rebind();
        fixture
    }
    pub(crate) fn rebind(&mut self) {
        self.task.source_bindings = self
            .captured
            .iter()
            .map(|(path, bytes)| ReadDependency {
                path: path.clone(),
                expected: ExpectedState::Hash(hash(bytes)),
            })
            .collect();
    }
    pub(crate) fn authenticate(&self) -> Result<AuthenticatedRelationSource> {
        authenticate_captured(&self.fs, &self.vault, &self.task, &self.captured, limits())
    }
    pub(crate) fn edit(&mut self, source: bool, key: &str, value: Value) {
        let path = if source {
            self.source.clone()
        } else {
            self.revision.clone()
        };
        let mut fields = parse_note(&self.captured[&path]).fields.unwrap();
        fields.insert(key.into(), value);
        self.captured.insert(
            path,
            record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap(),
        );
        self.rebind();
    }
}

#[test]
fn current_captured_source_derives_authority_without_filesystem_and_reports_reservation() {
    for shared in [false, true] {
        let f = Fixture::new(shared);
        std::fs::remove_dir_all(f.temp.path()).unwrap();
        let authenticated = f.authenticate().unwrap();
        assert_eq!(authenticated.binding.source_id, id("fictional_source"));
        assert_eq!(
            authenticated.binding.source_revision,
            id("fictional_revision")
        );
        assert_eq!(
            authenticated.binding.raw_hash,
            hash(&f.captured[&f.content])
        );
        assert_eq!(authenticated.text.as_bytes(), f.captured[&f.content]);
        assert_eq!(authenticated.dependencies, f.task.source_bindings);
        assert_eq!(authenticated.selected_binding.content_path, f.content);
        let total = f.captured.values().map(|b| b.len() as u64).sum::<u64>();
        assert_eq!(authenticated.captured_bytes, total);
        assert_eq!(authenticated.reserved_hash_bytes, 7 * total);
        assert_eq!(authenticated.known_hash_bytes, total);
        assert_eq!(authenticated.known_hashes, if shared { 4 } else { 5 });
    }
}

#[test]
fn withdrawal_wrong_current_head_foreign_revision_vault_and_manifest_fail() {
    for case in 0..5 {
        let mut f = Fixture::new(false);
        match case {
            0 => f.edit(true, "wiki_status", json!("withdrawn")),
            1 => f.edit(true, "wiki_current_revision", json!("another_revision")),
            2 => f.edit(false, "wiki_source_id", json!("another_source")),
            3 => f.vault = id("another_vault"),
            4 => f.edit(true, "wiki_revisions", json!(["another_revision"])),
            _ => unreachable!(),
        }
        let error = f.authenticate().err().unwrap();
        assert_eq!(error.code, ErrorCode::SourceIntegrity, "case {case}");
        assert!(
            error.details["source_relation_inputs_reserved_hash_bytes"]
                .as_u64()
                .unwrap()
                > 0
        );
    }
}

#[test]
fn missing_capture_extra_guard_and_guard_state_hash_or_purpose_fail() {
    for case in 0..7 {
        let mut f = Fixture::new(false);
        match case {
            0 => {
                f.captured.remove(&f.original);
            }
            1 => {
                f.captured.remove(&f.original);
                f.rebind();
            }
            2 => {
                f.captured.remove(&f.original);
                f.captured.insert(
                    VaultRelativePath::new("guard.bin").unwrap(),
                    b"extra guard".to_vec(),
                );
                f.rebind();
            }
            3 => {
                f.task.source_bindings[0].expected = ExpectedState::Absent;
            }
            4 => {
                f.task.source_bindings[0].expected = ExpectedState::Hash(hash(b"changed"));
            }
            5 => {
                f.task.stage = TaskStage::Extract;
            }
            6 => {
                f.task.capability = Some(Capability::Embed);
            }
            _ => unreachable!(),
        }
        assert!(f.authenticate().is_err(), "case {case}");
    }
    let mut f = Fixture::new(false);
    f.task.source_bindings[1] = f.task.source_bindings[0].clone();
    assert!(f.authenticate().is_err());
}

#[test]
fn changed_content_original_content_path_or_content_hash_fail_with_rebound_guards() {
    for case in 0..4 {
        let mut f = Fixture::new(false);
        match case {
            0 => {
                f.captured.get_mut(&f.content).unwrap().push(b'!');
                f.rebind();
            }
            1 => {
                f.captured.get_mut(&f.original).unwrap().push(b'!');
                f.rebind();
            }
            2 => f.edit(
                false,
                "wiki_content_hash",
                json!(hash(b"different content")),
            ),
            3 => f.edit(false, "wiki_content_path", json!("missing.md")),
            _ => unreachable!(),
        }
        assert!(f.authenticate().is_err(), "case {case}");
    }
}

#[test]
fn finite_reservation_deadline_and_byte_bounds_precede_or_bound_work() {
    let f = Fixture::new(false);
    let total = f.captured.values().map(|b| b.len() as u64).sum::<u64>();
    let insufficient = InputLimits {
        max_hash_bytes: total * 7 - 1,
        ..limits()
    };
    let error = authenticate_captured(&f.fs, &f.vault, &f.task, &f.captured, insufficient)
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert_eq!(
        error.details["source_relation_inputs_reserved_hash_bytes"],
        0
    );
    assert_eq!(error.details["source_relation_inputs_known_hashes"], 0);
    let expired = InputLimits {
        deadline: Instant::now(),
        ..limits()
    };
    assert_eq!(
        authenticate_captured(&f.fs, &f.vault, &f.task, &f.captured, expired)
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
    let insufficient = InputLimits {
        max_captured_bytes: total - 1,
        ..limits()
    };
    assert!(authenticate_captured(&f.fs, &f.vault, &f.task, &f.captured, insufficient).is_err());
    let mut f = Fixture::new(false);
    f.captured
        .insert(f.source.clone(), vec![b'x'; MAX_CONTROL_BYTES + 1]);
    f.rebind();
    let error = f.authenticate().err().unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert_eq!(error.details["source_relation_inputs_known_hashes"], 0);
    let mut f = Fixture::new(false);
    f.captured
        .insert(f.content.clone(), vec![b'x'; MAX_CONTENT_BYTES + 1]);
    f.rebind();
    assert_eq!(
        f.authenticate().err().unwrap().code,
        ErrorCode::BudgetExceeded
    );
    let mut f = Fixture::new(false);
    f.captured
        .insert(f.original.clone(), vec![b'x'; MAX_CAPTURED_BYTES]);
    f.rebind();
    let error = f.authenticate().err().unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert_eq!(error.details["source_relation_inputs_known_hashes"], 0);
}
