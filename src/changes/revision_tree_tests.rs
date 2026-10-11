use super::*;
use crate::{changes::journal, domain::RecordId, vault::VaultRoot};

const SOURCE: &str = "01940000-0000-7000-8000-000000000001";
const REVISION: &str = "01940000-0000-7000-8000-000000000002";
fn path(value: impl Into<String>) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn fixture() -> (ChangeManifest, ChangePlan) {
    let root = format!("sources/{SOURCE}/revisions/{REVISION}");
    let original = b"complete capture\n".to_vec();
    let hash = Blake3Hash::digest(&original);
    let revision = format!("---\nwiki_schema: '1'\nwiki_id: {REVISION}\nwiki_kind: revision\ntitle: Capture\nwiki_source_id: {SOURCE}\nwiki_source: '[[sources/{SOURCE}/source.md]]'\nwiki_captured_at: '2026-10-11T00:00:00Z'\nwiki_original_path: original.bin\nwiki_original_hash: {hash}\nwiki_content_path: content.md\nwiki_content_hash: {hash}\nwiki_extraction_status: complete\nwiki_extractor: utf8-preserve-v1\nwiki_extractor_fingerprint: {}\nwiki_media_type: text/markdown\n---\n", Blake3Hash::digest(b"utf8-preserve-v1")).into_bytes();
    let head = format!("---\nwiki_schema: '1'\nwiki_id: {SOURCE}\nwiki_kind: source\ntitle: Source\nwiki_status: active\nwiki_origin_kind: local-file\nwiki_origin: capture.md\nwiki_current_revision: {REVISION}\nwiki_revision: '[[sources/{SOURCE}/revisions/{REVISION}/revision.md]]'\nwiki_revisions:\n  - {REVISION}\n---\n").into_bytes();
    let payloads = [original.clone(), original, revision, head];
    let targets = [
        path(format!("{root}/content.md")),
        path(format!("{root}/original.bin")),
        path(format!("{root}/revision.md")),
        path(format!("sources/{SOURCE}/source.md")),
    ];
    let operations = targets
        .iter()
        .zip(&payloads)
        .enumerate()
        .map(|(index, (target, bytes))| {
            let hash = Blake3Hash::digest(bytes);
            ChangeOp {
                target: target.clone(),
                before: if index == 3 {
                    ExpectedState::Hash(Blake3Hash::digest(b"old head"))
                } else {
                    ExpectedState::Absent
                },
                after: ExpectedState::Hash(hash.clone()),
                before_payload: None,
                after_payload: Some(PayloadRef {
                    path: crate::storage::layout::object_path(&hash).unwrap(),
                    hash,
                    byte_len: bytes.len() as u64,
                }),
                role: if index == 3 {
                    OperationRole::MutableRecord
                } else {
                    OperationRole::ImmutableAsset
                },
                apply_after: if index == 3 {
                    vec![0, 1, 2]
                } else if index == 2 {
                    vec![0, 1]
                } else {
                    vec![]
                },
            }
        })
        .collect::<Vec<_>>();
    let plan = ChangePlan {
        read_preconditions: vec![],
        operations: operations
            .iter()
            .zip(payloads)
            .map(|(op, data)| ExpectedWrite {
                target: op.target.clone(),
                expected: op.before.clone(),
                proposed: Some(data),
                apply_after: op
                    .apply_after
                    .iter()
                    .map(|&index| targets[index].clone())
                    .collect(),
            })
            .collect(),
        roles: operations.iter().map(|op| op.role).collect(),
        before: vec![None; 4],
    };
    let manifest = ChangeManifest {
        version: 3,
        vault_id: RecordId::new(SOURCE).unwrap(),
        change_id: RecordId::new(REVISION).unwrap(),
        title: "refresh".into(),
        created_at: "2026-10-11T00:00:00Z".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations,
        revision_trees: vec![RevisionTreeGroup {
            root: path(root),
            members: vec![0, 1, 2],
        }],
    };
    (manifest, plan)
}
fn decode(manifest: &ChangeManifest, events: Vec<ChangeEvent>) -> Result<JournalState> {
    let hash = Blake3Hash::digest(b"manifest");
    let bytes = events
        .into_iter()
        .enumerate()
        .flat_map(|(sequence, event)| {
            journal::encode_frame(&JournalFrame {
                version: if manifest.version == 3 { 2 } else { 1 },
                sequence: sequence as u64,
                change_id: manifest.change_id.clone(),
                manifest_hash: hash.clone(),
                event,
            })
            .unwrap()
        })
        .collect::<Vec<_>>();
    journal::decode_journal(&bytes, manifest, &hash)
}
fn start() -> Vec<ChangeEvent> {
    vec![ChangeEvent::Prepared, ChangeEvent::Applying]
}

#[test]
fn contracted_dag_orders_tree_before_head_and_rejects_cycle() {
    let (mut manifest, _) = fixture();
    assert_eq!(execution_order(&manifest).unwrap(), vec![0, 3]);
    manifest.operations[0].apply_after = vec![3];
    assert!(validate_structure(&manifest).is_err());
}
#[test]
fn groups_are_exact_disjoint_sorted_and_complete() {
    let (manifest, _) = fixture();
    validate_structure(&manifest).unwrap();
    for members in [
        vec![],
        vec![0, 1],
        vec![0, 0, 2],
        vec![2, 1, 0],
        vec![0, 1, 4],
        vec![0, 1, 3],
    ] {
        let mut changed = manifest.clone();
        changed.revision_trees[0].members = members;
        assert!(validate_structure(&changed).is_err());
    }
    let mut changed = manifest.clone();
    changed
        .revision_trees
        .push(changed.revision_trees[0].clone());
    assert!(validate_structure(&changed).is_err());
}
#[test]
fn legacy_wire_omits_groups_and_rejects_nonempty_groups() {
    let (mut manifest, _) = fixture();
    for version in [1, 2] {
        manifest.version = version;
        assert!(validate_structure(&manifest).is_err());
        let mut old = manifest.clone();
        old.revision_trees.clear();
        assert!(
            !serde_json::to_string(&old)
                .unwrap()
                .contains("revision_trees")
        );
        validate_structure(&old).unwrap();
    }
}
#[test]
fn writer_groups_only_exact_authenticated_text_refresh_and_is_transactional() {
    let (mut manifest, mut plan) = fixture();
    manifest.version = 2;
    manifest.revision_trees.clear();
    let original = manifest.clone();
    plan.operations[0].proposed.as_mut().unwrap().push(b'x');
    assert!(!try_group_refresh(&mut manifest, &plan).unwrap());
    assert_eq!(manifest, original);
    let (_, plan) = fixture();
    assert!(try_group_refresh(&mut manifest, &plan).unwrap());
    assert_eq!(manifest.version, 3);
}
#[test]
fn payload_validation_binds_record_and_member_commitments() {
    let (manifest, plan) = fixture();
    for (index, op) in plan.operations.iter().enumerate() {
        validate_operation_payload(&manifest, index, op.proposed.as_deref()).unwrap();
    }
    let mut changed = manifest.clone();
    changed.operations[0].after = ExpectedState::Hash(Blake3Hash::digest(b"other"));
    assert!(
        validate_operation_payload(&changed, 2, plan.operations[2].proposed.as_deref()).is_err()
    );
}
#[test]
fn typed_journal_completes_all_tree_members_before_heads() {
    let (manifest, _) = fixture();
    let mut events = start();
    events.extend([
        ChangeEvent::TreeIntent { group: 0 },
        ChangeEvent::TreeDone { group: 0 },
        ChangeEvent::Intent { op: 3 },
        ChangeEvent::Done { op: 3 },
        ChangeEvent::FilesApplied,
    ]);
    assert_eq!(
        decode(&manifest, events).unwrap().status,
        Some(ChangeStatus::FilesApplied)
    );
}
#[test]
fn journal_rejects_file_bypass_done_alone_duplicates_and_wrong_group() {
    let (manifest, _) = fixture();
    for suffix in [
        vec![ChangeEvent::Intent { op: 0 }],
        vec![ChangeEvent::Done { op: 0 }],
        vec![ChangeEvent::TreeDone { group: 0 }],
        vec![ChangeEvent::TreeIntent { group: 1 }],
        vec![ChangeEvent::Intent { op: 3 }],
        vec![
            ChangeEvent::TreeIntent { group: 0 },
            ChangeEvent::TreeIntent { group: 0 },
        ],
        vec![
            ChangeEvent::TreeIntent { group: 0 },
            ChangeEvent::TreeDone { group: 0 },
            ChangeEvent::TreeIntent { group: 0 },
        ],
        vec![
            ChangeEvent::TreeIntent { group: 0 },
            ChangeEvent::FilesApplied,
        ],
    ] {
        let mut events = start();
        events.extend(suffix);
        assert!(decode(&manifest, events).is_err());
    }
}
#[test]
fn journal_rejects_mixed_frame_versions_but_inspects_v3_when_feature_off() {
    let (manifest, _) = fixture();
    let hash = Blake3Hash::digest(b"manifest");
    let frame = JournalFrame {
        version: 1,
        sequence: 0,
        change_id: manifest.change_id.clone(),
        manifest_hash: hash.clone(),
        event: ChangeEvent::Prepared,
    };
    assert!(
        journal::decode_journal(&journal::encode_frame(&frame).unwrap(), &manifest, &hash).is_err()
    );
    decode(&manifest, vec![ChangeEvent::Prepared]).unwrap();
}
#[test]
fn historical_tree_intent_survives_application_epochs() {
    let (manifest, _) = fixture();
    let mut events = start();
    events.extend([
        ChangeEvent::TreeIntent { group: 0 },
        ChangeEvent::TreeDone { group: 0 },
        ChangeEvent::Applying,
    ]);
    let state = decode(&manifest, events).unwrap();
    assert!(historical_intent(&state, 0));
    assert!(!historical_intent(&state, 1));
}
#[test]
fn complete_root_requires_intent_and_partial_extra_and_empty_roots_conflict() {
    let (manifest, plan) = fixture();
    let temp = tempfile::tempdir().unwrap();
    let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
    let state = decode(&manifest, start()).unwrap();
    require_observations(&fs, &manifest, &state, false).unwrap();
    let root = fs.root().resolve(&manifest.revision_trees[0].root).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    assert!(observe_group(&fs, &manifest, 0).is_err());
    assert!(require_abandon_trees_absent(&fs, &manifest).is_err());
    for op in plan.operations.iter().take(3) {
        std::fs::write(
            fs.root().resolve(&op.target).unwrap(),
            op.proposed.as_ref().unwrap(),
        )
        .unwrap();
    }
    assert!(observe_group(&fs, &manifest, 0).unwrap());
    assert!(require_observations(&fs, &manifest, &state, false).is_err());
    let mut events = start();
    events.extend([ChangeEvent::TreeIntent { group: 0 }, ChangeEvent::Applying]);
    require_observations(&fs, &manifest, &decode(&manifest, events).unwrap(), false).unwrap();
    std::fs::write(root.join("external.md"), b"preserve").unwrap();
    assert!(observe_group(&fs, &manifest, 0).is_err());
    assert_eq!(
        std::fs::read(root.join("external.md")).unwrap(),
        b"preserve"
    );
}
#[cfg(unix)]
#[test]
fn tree_observation_rejects_member_symlinks_and_hardlinks() {
    let (manifest, plan) = fixture();
    let temp = tempfile::tempdir().unwrap();
    let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
    let root = fs.root().resolve(&manifest.revision_trees[0].root).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for op in plan.operations.iter().take(3) {
        std::fs::write(
            fs.root().resolve(&op.target).unwrap(),
            op.proposed.as_ref().unwrap(),
        )
        .unwrap();
    }
    std::fs::hard_link(root.join("content.md"), temp.path().join("external")).unwrap();
    assert!(observe_group(&fs, &manifest, 0).is_err());
    std::fs::remove_file(root.join("content.md")).unwrap();
    std::os::unix::fs::symlink(temp.path().join("external"), root.join("content.md")).unwrap();
    assert!(observe_group(&fs, &manifest, 0).is_err());
}
#[cfg(not(feature = "experimental-revision-tree-install"))]
#[test]
fn feature_off_execution_refuses_without_creating_any_paths() {
    let (manifest, _) = fixture();
    let temp = tempfile::tempdir().unwrap();
    let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
    assert_eq!(
        require_execution(&fs, &manifest).unwrap_err().code,
        ErrorCode::CapabilityUnavailable
    );
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[cfg(all(
    feature = "experimental-revision-tree-install",
    any(target_os = "macos", target_os = "linux")
))]
#[test]
fn retained_group_workspace_admission_precedes_any_payload_read_or_write() {
    let (mut manifest, _) = fixture();
    let temp = tempfile::tempdir().unwrap();
    let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
    require_execution(&fs, &manifest).unwrap();
    // Declared payloads fit the wire aggregate, but not the complete stage's
    // workspace. There are deliberately no payload files to read or retain.
    manifest.operations[0]
        .after_payload
        .as_mut()
        .unwrap()
        .byte_len = (super::super::prepare::MAX_GRAPH_INPUT_BYTES - 50_000) as u64;
    validate_structure(&manifest).unwrap();
    assert_eq!(
        require_execution(&fs, &manifest).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn resolution_cannot_authorize_partial_group_or_complete_group_without_prior_intent() {
    let (manifest, _) = fixture();
    for partial in [false, true] {
        let observations = manifest
            .operations
            .iter()
            .enumerate()
            .map(|(index, op)| TargetObservation {
                target: op.target.clone(),
                before: op.before.clone(),
                after: op.after.clone(),
                observed: if index < 3 && (!partial || index == 0) {
                    op.after.clone()
                } else {
                    op.before.clone()
                },
            })
            .collect::<Vec<_>>();
        let mut events = start();
        events.push(ChangeEvent::Conflict {
            phase: "external tree".into(),
            observations: observations.clone(),
        });
        let state = decode(&manifest, events.clone()).unwrap();
        let conflict = state.frames.last().unwrap();
        events.push(ChangeEvent::ResolutionAccepted {
            mode: ConflictResolutionMode::Resume,
            conflict_sequence: conflict.sequence,
            conflict_hash: journal::frame_hash(conflict).unwrap(),
            observations,
        });
        assert!(decode(&manifest, events).is_err());
    }
}

#[test]
fn retained_v3_aggregate_admission_rejects_oversize_and_overflow_before_execution() {
    let (manifest, _) = fixture();
    for size in [MAX_GRAPH_INPUT_BYTES as u64 + 1, u64::MAX] {
        let mut changed = manifest.clone();
        changed.operations[0]
            .after_payload
            .as_mut()
            .unwrap()
            .byte_len = size;
        assert_eq!(
            validate_structure(&changed).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        for version in [1, 2] {
            changed.version = version;
            changed.revision_trees.clear();
            validate_structure(&changed).unwrap();
        }
    }
    let mut changed = manifest.clone();
    changed.operations[3].before_payload = Some(PayloadRef {
        path: path("changes/before"),
        hash: Blake3Hash::digest(b"before"),
        byte_len: MAX_GRAPH_INPUT_BYTES as u64,
    });
    assert_eq!(
        validate_structure(&changed).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
}
