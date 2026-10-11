use super::*;
use crate::retrieval::discovery::admission::{DiscoveryMaintenanceLimits, StorageGeometry};
fn admission() -> DiscoveryAdmission {
    DiscoveryAdmission::derive(
        Blake3Hash::digest("input"),
        1,
        2,
        DiscoveryMaintenanceLimits::default(),
        StorageGeometry {
            allocation_unit_bytes: 4096,
        },
    )
    .unwrap()
}
const TEST_AS_BYTES: u64 = 1024 * 1024 * 1024;
use crate::retrieval::{render::TargetKind, unit_inventory_types::RenderPolicyId};
use std::io::Cursor;

fn node(vector: Vec<f32>) -> GraphNode {
    let policy = RenderPolicyId(Blake3Hash::digest("policy"));
    let owner = VaultRelativePath::new("source.md").unwrap();
    GraphNode {
        id: 0,
        descriptor: UnitDescriptor {
            policy: policy.clone(),
            owner: owner.clone(),
            target: TargetKind::Document,
            target_id: None,
            source_hash: Blake3Hash::digest("source"),
            source_span: None,
            unit_id: Blake3Hash::digest("unit"),
            input_hash: Blake3Hash::digest("input"),
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
        vector,
        neighbors: vec![vec![]],
    }
}
#[test]
fn framing_rejects_oversize_zero_truncated_header_and_body() {
    for bytes in [
        vec![1, 0],
        0u32.to_le_bytes().to_vec(),
        ((FRAME_BYTES + 1) as u32).to_le_bytes().to_vec(),
        vec![3, 0, 0, 0, 1],
    ] {
        assert!(read_frame(&mut Cursor::new(bytes), FRAME_BYTES).is_err());
    }
    assert!(read_frame(&mut Cursor::new(Vec::<u8>::new()), FRAME_BYTES)
        .unwrap()
        .is_none());
}
#[test]
fn serializer_refuses_before_allocating_an_unbounded_frame() {
    assert!(bounded_json(&vec![0u8; 100], 8).is_err());
    let mut wire = Vec::new();
    write_frame(&mut wire, &"ok", 32).unwrap();
    assert_eq!(
        read_frame(&mut Cursor::new(wire), 32).unwrap().unwrap(),
        b"\"ok\""
    );
}
#[test]
fn float_transport_and_canonical_digest_preserve_bit_controls() {
    let controls = vec![
        0.,
        -0.,
        f32::from_bits(1),
        f32::MIN_POSITIVE,
        f32::MAX,
        0.6,
        0.8,
        f32::from_bits(0x3f7fffff),
        f32::from_bits(0x3f800001),
        f32::from_bits(0x00800001),
    ];
    let original = node(controls);
    let mut wire = Vec::new();
    write_frame(&mut wire, &Frame::Node(original.clone()), FRAME_BYTES).unwrap();
    let bytes = read_frame(&mut Cursor::new(wire), FRAME_BYTES)
        .unwrap()
        .unwrap();
    let Frame::Node(decoded) = serde_json::from_slice::<Frame>(&bytes).unwrap() else {
        panic!("node frame expected")
    };
    assert_eq!(
        original
            .vector
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        decoded
            .vector
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>()
    );
    let initial = Blake3Hash::digest("lwiki-semantic-export-v1");
    assert_eq!(
        advance_digest(&initial, &original).unwrap(),
        advance_digest(&initial, &decoded).unwrap()
    );
    let mut changed = decoded;
    changed.binding.modified_seq += 1;
    assert_ne!(
        advance_digest(&initial, &original).unwrap(),
        advance_digest(&initial, &changed).unwrap()
    );
}
#[cfg(unix)]
#[test]
fn silent_mock_child_is_killed_reaped_and_reader_joined_on_deadline() {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg("cat >/dev/null; exec /bin/sleep 60")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let input = GraphRef {
        format: FORMAT,
        kind: GraphKind::StagingInput,
        root: Blake3Hash::digest("root"),
        root_bytes: 1,
        directory_height: 1,
        node_count: 1,
        dimensions: 2,
        entry: Some(0),
        max_level: 0,
    };
    let start = Instant::now();
    assert!(transport(
        &mut child,
        b"{}".to_vec(),
        &input,
        &Blake3Hash::digest("input"),
        &admission(),
        start + Duration::from_millis(100),
        &mut |_| Ok(())
    )
    .is_err());
    assert!(child.try_wait().unwrap().is_some());
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[cfg(unix)]
#[test]
fn open_partial_header_and_body_are_deadline_bounded() {
    use std::os::unix::net::UnixStream;
    for bytes in [vec![1, 0], vec![4, 0, 0, 0, 1]] {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        writer.write_all(&bytes).unwrap();
        nonblocking(&reader).unwrap();
        let start = Instant::now();
        assert!(
            read_deadline_frame(&mut reader, FRAME_BYTES, start + Duration::from_millis(20))
                .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

#[cfg(unix)]
#[test]
fn child_that_never_reads_request_cannot_block_send() {
    use std::os::unix::net::UnixStream;
    let (_unread, mut writer) = UnixStream::pair().unwrap();
    nonblocking(&writer).unwrap();
    // Fill socket to exercise WouldBlock even though actual request is tiny.
    let bytes = vec![0u8; 8 * 1024 * 1024];
    assert!(write_deadline(
        &mut writer,
        &bytes,
        Instant::now() + Duration::from_millis(20)
    )
    .is_err());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn resource_installation_failure_and_wrong_readback_fail_closed() {
    assert!(install_limit_using(
        TEST_AS_BYTES,
        |_| -1,
        |_| panic!("must not read after failed set")
    )
    .is_err());
    assert!(install_limit_using(TEST_AS_BYTES, |_| 0, |_| 0).is_err());
    assert!(install_limit_using(
        TEST_AS_BYTES,
        |_| 0,
        |observed| {
            observed.rlim_cur = TEST_AS_BYTES as libc::rlim_t;
            observed.rlim_max = TEST_AS_BYTES as libc::rlim_t;
            0
        }
    )
    .is_ok());
}

#[cfg(unix)]
fn mock_transport(wire: &[u8], exit: u8, fail_emit: bool) -> Result<BuildResult> {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("frames");
    std::fs::write(&file, wire).unwrap();
    // Positional argument avoids shell interpolation of file paths.
    let script = if exit == 0 {
        "cat >/dev/null; /bin/cat \"$1\"; exit 0"
    } else {
        "cat >/dev/null; /bin/cat \"$1\"; exit 7"
    };
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("mock")
        .arg(file)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let input = GraphRef {
        format: FORMAT,
        kind: GraphKind::StagingInput,
        root: Blake3Hash::digest("root"),
        root_bytes: 1,
        directory_height: 1,
        node_count: 1,
        dimensions: 2,
        entry: Some(0),
        max_level: 0,
    };
    let result = transport(
        &mut child,
        b"{}".to_vec(),
        &input,
        &Blake3Hash::digest("input"),
        &admission(),
        Instant::now() + Duration::from_secs(2),
        &mut |_| {
            if fail_emit {
                Err(exhausted("injected emit failure"))
            } else {
                Ok(())
            }
        },
    );
    assert!(child.try_wait().unwrap().is_some());
    result
}

#[cfg(unix)]
fn complete_wire() -> Vec<u8> {
    let n = node(vec![1., 0.]);
    let digest = advance_digest(&Blake3Hash::digest("lwiki-semantic-export-v1"), &n).unwrap();
    let mut wire = Vec::new();
    write_frame(&mut wire, &Frame::Node(n), FRAME_BYTES).unwrap();
    write_frame(
        &mut wire,
        &Frame::Complete(BuildResult {
            entry: Some(0),
            max_level: 0,
            node_count: 1,
            native_memory_lower_bound: 1024,
            input_fingerprint: Blake3Hash::digest("input"),
            output_digest: digest,
            resource_limit_kind: "rlimit_as".into(),
            as_bytes: admission().limits.max_child_address_space_bytes,
            admission_fingerprint: admission().fingerprint().unwrap(),
        }),
        FRAME_BYTES,
    )
    .unwrap();
    wire
}

#[cfg(unix)]
#[test]
fn extra_after_complete_and_nonzero_exit_cannot_succeed() {
    let valid = complete_wire();
    assert!(mock_transport(&valid, 0, false).is_ok());
    assert!(mock_transport(&valid, 7, false).is_err());
    let mut extra = valid;
    write_frame(&mut extra, &Frame::Node(node(vec![1., 0.])), FRAME_BYTES).unwrap();
    assert!(mock_transport(&extra, 0, false).is_err());
}

#[cfg(unix)]
#[test]
fn flood_followed_by_emit_refusal_terminates_transport() {
    let mut wire = Vec::new();
    for _ in 0..2000 {
        write_frame(&mut wire, &Frame::Node(node(vec![1., 0.])), FRAME_BYTES).unwrap();
    }
    assert!(mock_transport(&wire, 0, true).is_err());
}

#[cfg(unix)]
#[test]
fn completion_digest_or_fingerprint_mismatch_is_rejected() {
    for fingerprint in [true, false] {
        let valid = complete_wire();
        let mut cursor = Cursor::new(valid);
        let first = read_frame(&mut cursor, FRAME_BYTES).unwrap().unwrap();
        let second = read_frame(&mut cursor, FRAME_BYTES).unwrap().unwrap();
        let Frame::Complete(mut result) = serde_json::from_slice::<Frame>(&second).unwrap() else {
            panic!("completion expected")
        };
        if fingerprint {
            result.input_fingerprint = Blake3Hash::digest("wrong");
        } else {
            result.output_digest = Blake3Hash::digest("wrong");
        }
        let mut wire = Vec::new();
        let frame: Frame = serde_json::from_slice(&first).unwrap();
        write_frame(&mut wire, &frame, FRAME_BYTES).unwrap();
        write_frame(&mut wire, &Frame::Complete(result), FRAME_BYTES).unwrap();
        assert!(mock_transport(&wire, 0, false).is_err());
    }
}

#[cfg(unix)]
#[test]
fn resource_refusal_frame_preserves_failure_and_reaps_child() {
    let mut wire = Vec::new();
    write_frame(
        &mut wire,
        &Frame::Failed {
            code: ErrorCode::CapabilityUnavailable,
            message: "semantic builder address-space limit installation/readback failed".into(),
        },
        4096,
    )
    .unwrap();
    let error = mock_transport(&wire, 7, false).unwrap_err();
    assert_eq!(error.code, ErrorCode::CapabilityUnavailable);
    assert!(error.message.contains("address-space limit"));
}

#[test]
fn request_admission_must_bind_plan_geometry_and_all_finite_limits() {
    let input = GraphRef {
        format: FORMAT,
        kind: GraphKind::StagingInput,
        root: Blake3Hash::digest("root"),
        root_bytes: 1,
        directory_height: 1,
        node_count: 1,
        dimensions: 2,
        entry: Some(0),
        max_level: 0,
    };
    let original = admission();
    validate_admission(&input, &Blake3Hash::digest("input"), &original).unwrap();
    assert!(validate_admission(&input, &Blake3Hash::digest("wrong-plan"), &original).is_err());
    let mut forged = original.clone();
    forged.node_count += 1;
    assert!(validate_admission(&input, &Blake3Hash::digest("input"), &forged).is_err());
    forged = original.clone();
    forged.limits.max_invocation_ms += 1;
    assert!(validate_admission(&input, &Blake3Hash::digest("input"), &forged).is_err());
    forged = original;
    forged.limits.max_child_address_space_bytes += 1;
    assert!(validate_admission(&input, &Blake3Hash::digest("input"), &forged).is_err());
}

#[cfg(unix)]
#[test]
fn completion_cannot_echo_different_admission_or_as_limit() {
    for changed_limit in [false, true] {
        let mut cursor = Cursor::new(complete_wire());
        let first = read_frame(&mut cursor, FRAME_BYTES).unwrap().unwrap();
        let second = read_frame(&mut cursor, FRAME_BYTES).unwrap().unwrap();
        let Frame::Complete(mut result) = serde_json::from_slice::<Frame>(&second).unwrap() else {
            panic!("completion expected")
        };
        if changed_limit {
            result.as_bytes -= 1;
        } else {
            result.admission_fingerprint = Blake3Hash::digest("other-admission");
        }
        let mut wire = Vec::new();
        let frame: Frame = serde_json::from_slice(&first).unwrap();
        write_frame(&mut wire, &frame, FRAME_BYTES).unwrap();
        write_frame(&mut wire, &Frame::Complete(result), FRAME_BYTES).unwrap();
        assert!(mock_transport(&wire, 0, false).is_err());
    }
}
