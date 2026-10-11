use super::*;
use crate::retrieval::{
    render::TargetKind, unit_inventory_types::RenderPolicyId, vectors::VectorReadBudget,
};
use crate::vault::VaultRoot;
use std::time::Duration;

fn fixture() -> (tempfile::TempDir, VaultFs, WriterPermit) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("../../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let permit = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    (temp, fs, permit)
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
fn fingerprint() -> Blake3Hash {
    Blake3Hash::digest("frozen-input")
}
fn node(id: u32, staging: bool) -> GraphNode {
    let policy = RenderPolicyId(Blake3Hash::digest("policy"));
    let owner = VaultRelativePath::new(format!("sources/{id}.md")).unwrap();
    GraphNode {
        id,
        descriptor: UnitDescriptor {
            policy: policy.clone(),
            owner: owner.clone(),
            target: TargetKind::Document,
            target_id: None,
            source_hash: Blake3Hash::digest(format!("source{id}")),
            source_span: None,
            unit_id: Blake3Hash::digest(format!("unit{id}")),
            input_hash: Blake3Hash::digest("shared"),
        },
        binding: UnitOwnerBinding {
            incarnation: Blake3Hash::digest("incarnation"),
            policy,
            owner,
            render_token: Blake3Hash::digest("render"),
            proof_version: 1,
            modified_seq: 1,
            tombstone: false,
            unit_count: 1,
        },
        vector: vec![1., 0.],
        neighbors: if staging { vec![] } else { vec![vec![]] },
    }
}
fn state(kind: GraphKind, count: u32) -> BuildState {
    BuildState::new(fingerprint(), kind, 2, count, Some(0), 0).unwrap()
}
fn meter() -> DiscoveryMeter {
    DiscoveryMeter::new(VectorReadBudget::new(Instant::now() + Duration::from_secs(20)).unwrap())
}
fn snapshot(state: &BuildState) -> Vec<u8> {
    serde_json::to_vec(state).unwrap()
}

#[test]
fn staging_input_and_base_roundtrip_preserve_identity_vector_and_layers() {
    for kind in [GraphKind::StagingInput, GraphKind::Base] {
        let (_temp, fs, permit) = fixture();
        let mut state = state(kind, 2);
        for id in 0..2 {
            state
                .append(
                    &fs,
                    &permit,
                    node(id, kind == GraphKind::StagingInput),
                    &fingerprint(),
                    u64::MAX,
                    deadline(),
                )
                .unwrap();
        }
        let graph = state
            .finish(&fs, &permit, &fingerprint(), u64::MAX, deadline())
            .unwrap();
        let mut reader = GraphReader::new(&fs, graph).unwrap();
        let mut meter = meter();
        for id in 0..2 {
            let decoded = reader.node(id, &mut meter).unwrap();
            let expected = node(id, kind == GraphKind::StagingInput);
            assert_eq!(decoded.descriptor, expected.descriptor);
            assert_eq!(decoded.binding, expected.binding);
            assert_eq!(
                decoded
                    .vector
                    .iter()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>(),
                expected
                    .vector
                    .iter()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>()
            );
            assert_eq!(decoded.neighbors, expected.neighbors);
        }
    }
}

#[test]
fn touched_leaf_corruption_is_rejected_even_after_directory_cached() {
    let (_temp, fs, permit) = fixture();
    let mut state = state(GraphKind::Base, 1);
    state
        .append(
            &fs,
            &permit,
            node(0, false),
            &fingerprint(),
            u64::MAX,
            deadline(),
        )
        .unwrap();
    let leaf = state.frontier[0][0].clone();
    let graph = state
        .finish(&fs, &permit, &fingerprint(), u64::MAX, deadline())
        .unwrap();
    let mut reader = GraphReader::new(&fs, graph).unwrap();
    let mut meter = meter();
    reader.node(0, &mut meter).unwrap();
    let mut bytes = read(&fs, &leaf).unwrap();
    bytes[0] ^= 1;
    std::fs::write(
        fs.root().resolve(&path(&leaf.hash).unwrap()).unwrap(),
        bytes,
    )
    .unwrap();
    assert_eq!(
        reader.node(0, &mut meter).unwrap_err().code,
        ErrorCode::IndexCorrupt
    );
}

#[test]
fn resume_rejects_gaps_overlaps_wrong_input_and_impossible_accounting() {
    let (_temp, fs, permit) = fixture();
    let mut original = state(GraphKind::Base, 3);
    for id in 0..2 {
        original
            .append(
                &fs,
                &permit,
                node(id, false),
                &fingerprint(),
                u64::MAX,
                deadline(),
            )
            .unwrap();
    }
    for first in [0, 2] {
        let mut bad = original.clone();
        bad.frontier[0][1].first = first;
        assert!(bad.validate_resume(&fingerprint(), u64::MAX).is_err());
    }
    assert!(original
        .validate_resume(&Blake3Hash::digest("different-input"), u64::MAX)
        .is_err());
    let mut bad = original.clone();
    bad.written_bytes = 0;
    assert!(bad.validate_resume(&fingerprint(), u64::MAX).is_err());
}

fn synthetic_prefix(count: u32) -> BuildState {
    let mut state = state(GraphKind::Base, count + 1);
    state.next_node = count;
    let mut leaves = Vec::new();
    let mut directories = Vec::new();
    let completed = count / FANOUT as u32;
    for i in 0..completed {
        directories.push(ObjectRef {
            hash: Blake3Hash::digest(format!("directory{i}")),
            first: i * FANOUT as u32,
            count: FANOUT as u32,
            height: 1,
            metadata_bytes: 200,
            vector_bytes: 0,
        });
    }
    for id in completed * FANOUT as u32..count {
        leaves.push(ObjectRef {
            hash: Blake3Hash::digest(format!("leaf{id}")),
            first: id,
            count: 1,
            height: 0,
            metadata_bytes: 200,
            vector_bytes: 8,
        });
    }
    state.frontier = vec![leaves, directories];
    state.written_bytes = 1_000_000;
    state
}

#[test]
fn carry_failure_at_first_and_second_directory_level_preserves_checkpoint() {
    for count in [63, 4095] {
        let (_temp, fs, permit) = fixture();
        let original = synthetic_prefix(count);
        original.validate_resume(&fingerprint(), u64::MAX).unwrap();
        // Successful dry construction determines exact logical bytes for each carry.
        let mut successful = original.clone();
        successful
            .append(
                &fs,
                &permit,
                node(count, false),
                &fingerprint(),
                u64::MAX,
                deadline(),
            )
            .unwrap();
        let mut failing = original.clone();
        let before = snapshot(&failing);
        // One byte below total admits leaf/earlier carry but rejects final directory.
        assert_eq!(
            failing
                .append(
                    &fs,
                    &permit,
                    node(count, false),
                    &fingerprint(),
                    successful.written_bytes - 1,
                    deadline()
                )
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(snapshot(&failing), before);
    }
}

#[test]
fn expired_append_and_finish_preserve_exact_cursor() {
    let (_temp, fs, permit) = fixture();
    let mut state = state(GraphKind::Base, 1);
    let before = snapshot(&state);
    assert!(state
        .append(
            &fs,
            &permit,
            node(0, false),
            &fingerprint(),
            u64::MAX,
            Instant::now()
        )
        .is_err());
    assert_eq!(snapshot(&state), before);
    state
        .append(
            &fs,
            &permit,
            node(0, false),
            &fingerprint(),
            u64::MAX,
            deadline(),
        )
        .unwrap();
    let before = snapshot(&state);
    assert!(state
        .finish(&fs, &permit, &fingerprint(), u64::MAX, Instant::now())
        .is_err());
    assert_eq!(snapshot(&state), before);
}

#[test]
fn cached_directory_cannot_be_reused_with_different_framing() {
    let (_temp, fs, permit) = fixture();
    let mut state = state(GraphKind::Base, 1);
    state
        .append(
            &fs,
            &permit,
            node(0, false),
            &fingerprint(),
            u64::MAX,
            deadline(),
        )
        .unwrap();
    let graph = state
        .finish(&fs, &permit, &fingerprint(), u64::MAX, deadline())
        .unwrap();
    let mut reader = GraphReader::new(&fs, graph).unwrap();
    let mut meter = meter();
    reader.node(0, &mut meter).unwrap();
    reader
        .directories
        .get_mut(&reader.reference.root)
        .unwrap()
        .0
        .metadata_bytes += 1;
    assert_eq!(
        reader.node(0, &mut meter).unwrap_err().code,
        ErrorCode::IndexCorrupt
    );
}

#[test]
fn largest_u32_frontier_fits_prospective_checkpoint_bound() {
    let mut state = state(GraphKind::Base, u32::MAX);
    state.next_node = u32::MAX - 1;
    state.frontier = vec![vec![]; 6];
    let mut first = 0u32;
    let mut remaining = state.next_node;
    for level in (0..6).rev() {
        let count = 64u32.pow(level as u32);
        let entries = remaining / count;
        remaining %= count;
        for i in 0..entries {
            state.frontier[level].push(ObjectRef {
                hash: Blake3Hash::digest(format!("{level}-{i}")),
                first,
                count,
                height: level as u8,
                metadata_bytes: MAX_NODE_METADATA,
                vector_bytes: if level == 0 { 8 } else { 0 },
            });
            first += count;
        }
    }
    state.written_bytes = 100_000_000;
    assert_eq!(first, state.next_node);
    assert!(snapshot(&state).len() <= 60 * 1024);
    state.validate_resume(&fingerprint(), u64::MAX).unwrap();
}
