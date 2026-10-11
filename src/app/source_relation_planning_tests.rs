use super::*;
use crate::{
    app::source_relation_inputs::tests::Fixture,
    config::providers::ProviderConfig,
    domain::{Blake3Hash, CanonicalRecord},
    jobs::Capability,
    providers::types::{RemoteInput, RemoteOperation},
    records::parse_note,
    sources::revision::record_bytes,
};
use std::time::Duration;
fn limits() -> InputLimits {
    InputLimits {
        max_captured_bytes: 1024 * 1024,
        max_hash_bytes: 14 * 1024 * 1024,
        deadline: Instant::now() + Duration::from_secs(10),
    }
}
fn selected(f: &Fixture) -> SelectedSourceBinding {
    f.authenticate().unwrap().selected_binding
}
fn service(f: &Fixture) -> (tempfile::TempDir, TrustedService) {
    // Only fictional private configuration, no broker/credential resolution.
    std::fs::write(
        f.fs.root().path().join("WIKI.md"),
        &f.captured[&VaultRelativePath::new("WIKI.md").unwrap()],
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("providers.toml");
    let root = serde_json::to_string(f.fs.root().path().to_str().unwrap()).unwrap();
    let config = format!(
        "version=1\n[profiles.primary]\ngeneration=\"generate\"\n[services.generate]\nadapter=\"responses-v1\"\nurl=\"https://fictional.example/v1/responses\"\nmodel=\"fictional-model\"\nrevision=\"r1\"\nmax_output_tokens=4096\n[services.generate.auth]\nkind=\"static\"\nkey_env=\"FICTIONAL_UNUSED_KEY\"\n[vault_bindings.main]\nroot={root}\nwiki_id=\"fictional_vault\"\nallowed_profiles=[\"primary\"]\n"
    );
    std::fs::write(&path, config).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let trusted = ProviderConfig::load(&path)
        .unwrap()
        .authorize(&f.fs, &f.vault, "primary", Capability::Generate)
        .unwrap();
    (temp, trusted)
}
#[test]
fn complete_source_packet_and_real_task_identity_are_reproducible_for_four_and_five_dependencies() {
    for shared in [true, false] {
        let f = Fixture::new(shared);
        let binding = selected(&f);
        let (_config, service) = service(&f);
        let a = plan_captured(&f.fs, &binding, &f.captured, &service, 1024, limits()).unwrap();
        let b = plan_captured(&f.fs, &binding, &f.captured, &service, 1024, limits()).unwrap();
        assert_eq!(a.generation.task.key, b.generation.task.key);
        assert_eq!(a.generation.descriptor, b.generation.descriptor);
        assert_eq!(a.generation.task.stage, TaskStage::PrepareSourceRelations);
        assert_eq!(a.generation.task.source_bindings, a.source.dependencies);
        assert_eq!(a.source.dependencies.len(), if shared { 4 } else { 5 });
        assert_eq!(a.reserved_hash_bytes, 14 * a.source.captured_bytes);
        let input: RemoteInput = serde_json::from_slice(&a.generation.descriptor).unwrap();
        let RemoteOperation::Generate {
            data,
            max_output_tokens,
            ..
        } = input.operation
        else {
            panic!("generation required")
        };
        assert_eq!(max_output_tokens, 1024);
        let data: serde_json::Value = serde_json::from_str(&data).unwrap();
        let reconstructed: String = data["fragments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["text"].as_str().unwrap())
            .collect();
        assert_eq!(reconstructed.as_bytes(), f.captured[&f.content]);
        assert_eq!(a.serialized_input_bytes, a.generation.descriptor.len());
        assert!(a.fragment_count > 0);
        let changed =
            plan_captured(&f.fs, &binding, &f.captured, &service, 1025, limits()).unwrap();
        assert_ne!(changed.generation.task.key, a.generation.task.key);
        assert_eq!(
            a.generation.task.input.hash,
            Blake3Hash::digest(&a.generation.descriptor)
        );
    }
}
#[test]
fn stale_and_incomplete_selected_sources_refuse_without_global_reads() {
    for (source, key, value) in [
        (true, "wiki_status", "withdrawn"),
        (false, "wiki_extraction_status", "failed"),
    ] {
        let mut f = Fixture::new(false);
        let mut binding = selected(&f);
        if source {
            f.edit(true, key, serde_json::json!(value));
            binding.source_hash = Blake3Hash::digest(&f.captured[&binding.source_path]);
        } else {
            let mut fields = parse_note(&f.captured[&binding.revision_path])
                .fields
                .unwrap();
            fields.insert(key.into(), serde_json::json!(value));
            fields.remove("wiki_content_path");
            fields.remove("wiki_content_hash");
            let bytes = record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap();
            binding.revision_hash = Blake3Hash::digest(&bytes);
            f.captured.insert(binding.revision_path.clone(), bytes);
            f.rebind();
        }
        // Matching control hashes reach status policy rather than stale-hash refusal.
        let error = selected_proof(&f.fs, &binding, &f.captured, limits())
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::SourceIntegrity);
    }
    let mut f = Fixture::new(false);
    let binding = selected(&f);
    f.captured.remove(&f.content);
    assert!(selected_proof(&f.fs, &binding, &f.captured, limits()).is_err());
}
#[test]
fn input_limits_and_absolute_deadline_precede_unbounded_selected_validation() {
    let mut f = Fixture::new(false);
    let binding = selected(&f);
    let mut expired = limits();
    expired.deadline = Instant::now();
    assert_eq!(
        selected_proof(&f.fs, &binding, &f.captured, expired)
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
    let total: u64 = f.captured.values().map(|v| v.len() as u64).sum();
    let mut short = limits();
    short.max_hash_bytes = 14 * total - 1;
    assert_eq!(
        selected_proof(&f.fs, &binding, &f.captured, short)
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
    f.captured
        .insert(f.content.clone(), vec![b'x'; 256 * 1024 + 1]);
    assert_eq!(
        selected_proof(&f.fs, &binding, &f.captured, limits())
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
    let mut f = Fixture::new(false);
    let binding = selected(&f);
    f.captured
        .insert(binding.source_path.clone(), vec![b'x'; 64 * 1024 + 1]);
    assert_eq!(
        selected_proof(&f.fs, &binding, &f.captured, limits())
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
    let mut f = Fixture::new(false);
    let binding = selected(&f);
    let original = f
        .captured
        .keys()
        .find(|p| p.as_str().ends_with("original.bin"))
        .unwrap()
        .clone();
    f.captured.insert(original, vec![b'x'; 1024 * 1024]);
    assert_eq!(
        selected_proof(&f.fs, &binding, &f.captured, limits())
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
}
#[test]
fn invalid_output_bound_cannot_produce_a_task() {
    let f = Fixture::new(true);
    let binding = selected(&f);
    let (_config, service) = service(&f);
    for bound in [0, 4097, u64::MAX] {
        assert!(plan_captured(&f.fs, &binding, &f.captured, &service, bound, limits()).is_err());
    }
}

fn replace_complete_text(f: &mut Fixture, text: &[u8]) {
    for (path, bytes) in &mut f.captured {
        if path == &f.content || path.as_str().ends_with("original.bin") {
            *bytes = text.to_vec();
        }
    }
    let hash = Blake3Hash::digest(text);
    f.edit(false, "wiki_original_hash", serde_json::json!(hash));
    f.edit(false, "wiki_content_hash", serde_json::json!(hash));
}
#[test]
fn complete_multifragment_unicode_and_escaped_descriptor_overflow_are_never_truncated() {
    let mut f = Fixture::new(false);
    let text = "Fictional condition \"notice\" requires café.\n".repeat(120);
    replace_complete_text(&mut f, text.as_bytes());
    let binding = selected(&f);
    let (_config, service) = service(&f);
    let plan = plan_captured(&f.fs, &binding, &f.captured, &service, 1024, limits()).unwrap();
    assert!(plan.fragment_count > 1);
    assert_eq!(plan.source.text, text);
    let input: RemoteInput = serde_json::from_slice(&plan.generation.descriptor).unwrap();
    let RemoteOperation::Generate { data, .. } = input.operation else {
        panic!("generation required")
    };
    let data: serde_json::Value = serde_json::from_str(&data).unwrap();
    let joined: String = data["fragments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["text"].as_str().unwrap())
        .collect();
    assert_eq!(joined, text);
    replace_complete_text(&mut f, &vec![b'"'; 100_000]);
    let binding = selected(&f);
    let error = plan_captured(&f.fs, &binding, &f.captured, &service, 1024, limits())
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert!(error.message.contains("truncation"));
    assert!(
        error.details["source_relation_planning_initial_reserved_hash_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
}
