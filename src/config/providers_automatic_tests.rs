use super::*;
use crate::{
    config::{
        provider_edit::{
            ProviderAuthPatch, ProviderGrantDecision, ProviderRole, ProviderRolePatch,
            plan_provider_edit, plan_provider_edit_with_automatic,
        },
        provider_setup::{
            EmbeddingSetup, ProviderSetupDocument, SetupAuth, SetupCommandOutput,
            build_embedding_document, build_embedding_document_with_automatic,
        },
    },
    vault::VaultRoot,
};
use toml::{Table, Value};

fn fixture() -> (tempfile::TempDir, DefaultWiki, VaultFs, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("wiki");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("WIKI.md"),
        b"---\nwiki_schema: \"1\"\nwiki_id: vault_auto\nwiki_kind: vault\ntitle: Automatic tests\n---\n").unwrap();
    let fs = VaultFs::new(VaultRoot::explicit(&path).unwrap());
    let wiki = DefaultWiki {
        path: fs.root().path().to_path_buf(),
        wiki_id: RecordId::new("vault_auto").unwrap(),
    };
    let config = temp.path().join("providers.toml");
    (temp, wiki, fs, config)
}

fn setup(grant: bool) -> EmbeddingSetup {
    EmbeddingSetup {
        model: "embedding-test".into(),
        url: "https://gateway.example/private-path?tenant=private-tenant".into(),
        dimensions: Some("1024".into()),
        auth: SetupAuth::Environment("MISSING_AUTOMATIC_KEY".into()),
        grant,
    }
}

fn reviewed(wiki: &DefaultWiki, grant: bool) -> ProviderSetupDocument {
    build_embedding_document_with_automatic(
        None,
        &setup(grant),
        wiki,
        ProviderAutomaticEmbeddingDecision::Reviewed,
    )
    .unwrap()
}

fn private_write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn load(path: &Path, bytes: &[u8]) -> ProviderConfig {
    private_write(path, bytes);
    ProviderConfig::load(path).unwrap()
}

fn document(bytes: &[u8]) -> Table {
    toml::from_str(std::str::from_utf8(bytes).unwrap()).unwrap()
}

fn encode(document: &Table) -> Vec<u8> {
    toml::to_string(document).unwrap().into_bytes()
}

fn grant_mut(document: &mut Table) -> &mut Table {
    document["vault_bindings"]
        .as_table_mut()
        .unwrap()
        .iter_mut()
        .find_map(|(_, binding)| {
            binding
                .as_table_mut()
                .unwrap()
                .get_mut("automatic_embedding")
                .and_then(Value::as_table_mut)
        })
        .unwrap()
}

#[test]
fn old_explicit_grant_is_automatic_off() {
    let (_temp, wiki, fs, path) = fixture();
    let built = build_embedding_document(None, &setup(true), &wiki).unwrap();
    let config = load(&path, &built.bytes);
    assert_eq!(
        config
            .automatic_embedding_consent(&fs, &wiki.wiki_id, &built.profile)
            .unwrap(),
        AutomaticEmbeddingConsent::Absent
    );
    assert!(
        config
            .authorize(&fs, &wiki.wiki_id, &built.profile, Capability::Embed)
            .is_ok()
    );
    assert!(
        config
            .authorize_automatic_embeddings(&fs, &wiki.wiki_id, &built.profile)
            .is_err()
    );
    let plan = plan_provider_edit(
        Some(&built.bytes),
        &wiki,
        Some(&built.profile),
        &[],
        ProviderGrantDecision::EmbeddingOnly,
    )
    .unwrap();
    assert_eq!(
        plan.automatic_embedding_consent,
        AutomaticEmbeddingConsent::Absent
    );
    assert_eq!(plan.proposed_bytes(), built.bytes);
}

#[test]
fn matching_automatic_permission_is_embedding_only_and_replay_is_exact() {
    let (_temp, wiki, fs, path) = fixture();
    let built = reviewed(&wiki, false);
    let config = load(&path, &built.bytes);
    assert_eq!(
        config
            .automatic_embedding_consent(&fs, &wiki.wiki_id, &built.profile)
            .unwrap(),
        AutomaticEmbeddingConsent::Matching
    );
    assert_eq!(
        config
            .authorize_automatic_embeddings(&fs, &wiki.wiki_id, &built.profile)
            .unwrap()
            .summary()
            .capability,
        Capability::Embed
    );
    assert!(
        config
            .authorize(&fs, &wiki.wiki_id, &built.profile, Capability::Embed)
            .is_err()
    );
    assert!(
        config
            .authorize(&fs, &wiki.wiki_id, &built.profile, Capability::Generate)
            .is_err()
    );
    for decision in [
        ProviderAutomaticEmbeddingDecision::Preserve,
        ProviderAutomaticEmbeddingDecision::Reviewed,
    ] {
        let again = build_embedding_document_with_automatic(
            Some(&built.bytes),
            &setup(false),
            &wiki,
            decision,
        )
        .unwrap();
        assert_eq!(again.bytes, built.bytes);
        let plan = plan_provider_edit_with_automatic(
            Some(&built.bytes),
            &wiki,
            Some(&built.profile),
            &[],
            ProviderGrantDecision::Preserve,
            decision,
        )
        .unwrap();
        assert_eq!(plan.proposed_bytes(), built.bytes);
        assert_eq!(
            plan.automatic_embedding_consent,
            AutomaticEmbeddingConsent::Matching
        );
    }
}

#[test]
fn copied_root_id_and_profile_do_not_match_runtime_permission() {
    let (temp, wiki, fs, path) = fixture();
    let built = reviewed(&wiki, false);
    let config = load(&path, &built.bytes);
    let clone = temp.path().join("clone");
    std::fs::create_dir(&clone).unwrap();
    std::fs::copy(wiki.path.join("WIKI.md"), clone.join("WIKI.md")).unwrap();
    let clone = VaultFs::new(VaultRoot::explicit(&clone).unwrap());
    assert_eq!(
        config
            .automatic_embedding_consent(&clone, &wiki.wiki_id, &built.profile)
            .unwrap(),
        AutomaticEmbeddingConsent::BindingChanged(AutomaticEmbeddingBindingChange::Vault)
    );
    assert!(
        config
            .authorize_automatic_embeddings(&clone, &wiki.wiki_id, &built.profile)
            .is_err()
    );
    let other = RecordId::new("vault_other").unwrap();
    assert!(
        config
            .automatic_embedding_consent(&fs, &other, &built.profile)
            .is_err()
    );
    assert_eq!(
        config
            .automatic_embedding_consent(&fs, &wiki.wiki_id, "other-profile")
            .unwrap(),
        AutomaticEmbeddingConsent::BindingChanged(AutomaticEmbeddingBindingChange::Profile)
    );
}

#[test]
fn auth_rotation_keeps_text_consent_across_copy_on_write() {
    let (_temp, wiki, fs, path) = fixture();
    let built = reviewed(&wiki, true);
    let mut patch = ProviderRolePatch::new(ProviderRole::Embedding);
    patch.auth = ProviderAuthPatch::Replace(SetupAuth::Environment("MISSING_ROTATED_KEY".into()));
    let plan = plan_provider_edit(
        Some(&built.bytes),
        &wiki,
        Some(&built.profile),
        &[patch],
        ProviderGrantDecision::EmbeddingOnly,
    )
    .unwrap();
    assert_ne!(plan.profile(), built.profile);
    assert_eq!(
        plan.previous_automatic_embedding_consent,
        AutomaticEmbeddingConsent::Matching
    );
    assert_eq!(
        plan.automatic_embedding_consent,
        AutomaticEmbeddingConsent::Matching
    );
    assert!(plan.proposed_bytes().starts_with(&built.bytes));
    let config = load(&path, plan.proposed_bytes());
    assert!(
        config
            .authorize_automatic_embeddings(&fs, &wiki.wiki_id, plan.profile())
            .is_ok()
    );
    let replay = plan_provider_edit(
        Some(plan.proposed_bytes()),
        &wiki,
        Some(plan.profile()),
        &[],
        ProviderGrantDecision::Preserve,
    )
    .unwrap();
    assert_eq!(replay.proposed_bytes(), plan.proposed_bytes());
}

#[test]
fn unchanged_in_place_credentials_preserve_consent_but_invalidate_old_guard() {
    let (_temp, wiki, fs, path) = fixture();
    let built = reviewed(&wiki, false);
    let old = load(&path, &built.bytes);
    let trusted = old
        .authorize_automatic_embeddings(&fs, &wiki.wiki_id, &built.profile)
        .unwrap();
    let mut rotated = document(&built.bytes);
    rotated["services"][&built.service]
        .as_table_mut()
        .unwrap()
        .insert(
            "auth".into(),
            Value::Table(Table::from_iter([
                ("kind".into(), Value::String("static".into())),
                (
                    "key_env".into(),
                    Value::String("MISSING_ROTATED_KEY".into()),
                ),
            ])),
        );
    let fresh = load(&path, &encode(&rotated));
    assert!(trusted.recheck(&fs).is_err());
    assert_eq!(
        fresh
            .automatic_embedding_consent(&fs, &wiki.wiki_id, &built.profile)
            .unwrap(),
        AutomaticEmbeddingConsent::Matching
    );
    assert!(
        fresh
            .authorize_automatic_embeddings(&fs, &wiki.wiki_id, &built.profile)
            .is_ok()
    );
}

#[test]
fn copy_on_write_never_transfers_mismatched_original_or_another_wikis_permission() {
    let (temp, wiki, _fs, _path) = fixture();
    let built = reviewed(&wiki, true);
    let mut stale = document(&built.bytes);
    stale["services"][&built.service]
        .as_table_mut()
        .unwrap()
        .insert(
            "revision".into(),
            Value::String("changed-after-consent".into()),
        );
    let stale = encode(&stale);
    let mut auth = ProviderRolePatch::new(ProviderRole::Embedding);
    auth.auth = ProviderAuthPatch::Replace(SetupAuth::Environment("MISSING_ROTATED_KEY".into()));
    let plan = plan_provider_edit(
        Some(&stale),
        &wiki,
        Some(&built.profile),
        std::slice::from_ref(&auth),
        ProviderGrantDecision::EmbeddingOnly,
    )
    .unwrap();
    assert_eq!(
        plan.previous_automatic_embedding_consent,
        AutomaticEmbeddingConsent::BindingChanged(AutomaticEmbeddingBindingChange::Revision)
    );
    assert_ne!(
        plan.automatic_embedding_consent,
        AutomaticEmbeddingConsent::Matching
    );
    let other_wiki = DefaultWiki {
        path: temp.path().join("other-wiki"),
        wiki_id: RecordId::new("vault_other").unwrap(),
    };
    let plan = plan_provider_edit(
        Some(&built.bytes),
        &other_wiki,
        Some(&built.profile),
        &[auth],
        ProviderGrantDecision::EmbeddingOnly,
    )
    .unwrap();
    assert_ne!(
        plan.previous_automatic_embedding_consent,
        AutomaticEmbeddingConsent::Matching
    );
    assert_ne!(
        plan.automatic_embedding_consent,
        AutomaticEmbeddingConsent::Matching
    );
    let original_grants = document(&built.bytes)["vault_bindings"]
        .as_table()
        .unwrap()
        .len();
    // The independently reviewed explicit embedding binding is appended, but
    // no automatic binding for the other wiki is copied.
    assert_eq!(
        document(plan.proposed_bytes())["vault_bindings"]
            .as_table()
            .unwrap()
            .len(),
        original_grants + 1
    );
}

#[test]
fn changed_endpoint_or_model_requires_renewed_automatic_review() {
    let (_temp, wiki, fs, path) = fixture();
    let built = reviewed(&wiki, true);
    for (model, url, dimensions) in [
        (Some("new-model"), None, None),
        (
            None,
            Some("https://gateway.example/different-private-path?tenant=private-tenant"),
            None,
        ),
        (None, None, Some("2048")),
    ] {
        let mut patch = ProviderRolePatch::new(ProviderRole::Embedding);
        patch.model = model.map(Into::into);
        patch.url = url.map(Into::into);
        patch.dimensions = dimensions.map(Into::into);
        let unreviewed = plan_provider_edit(
            Some(&built.bytes),
            &wiki,
            Some(&built.profile),
            std::slice::from_ref(&patch),
            ProviderGrantDecision::EmbeddingOnly,
        )
        .unwrap();
        assert_ne!(
            unreviewed.automatic_embedding_consent,
            AutomaticEmbeddingConsent::Matching
        );
        let config = load(&path, unreviewed.proposed_bytes());
        assert!(
            config
                .authorize_automatic_embeddings(&fs, &wiki.wiki_id, unreviewed.profile())
                .is_err()
        );
        let renewed = plan_provider_edit_with_automatic(
            Some(&built.bytes),
            &wiki,
            Some(&built.profile),
            &[patch],
            ProviderGrantDecision::EmbeddingOnly,
            ProviderAutomaticEmbeddingDecision::Reviewed,
        )
        .unwrap();
        assert_eq!(
            renewed.automatic_embedding_consent,
            AutomaticEmbeddingConsent::Matching
        );
        let config = load(&path, renewed.proposed_bytes());
        assert!(
            config
                .authorize_automatic_embeddings(&fs, &wiki.wiki_id, renewed.profile())
                .is_ok()
        );
    }
}

#[test]
fn stale_in_place_identity_reports_each_changed_binding() {
    let (_temp, wiki, fs, path) = fixture();
    let built = reviewed(&wiki, false);
    for (field, value, reason) in [
        (
            "url",
            Value::String("https://gateway.example/another-path".into()),
            AutomaticEmbeddingBindingChange::Endpoint,
        ),
        (
            "url",
            Value::String("https://gateway.example/private-path?tenant=another-tenant".into()),
            AutomaticEmbeddingBindingChange::Endpoint,
        ),
        (
            "allow_loopback_http",
            Value::Boolean(true),
            AutomaticEmbeddingBindingChange::Endpoint,
        ),
        (
            "model",
            Value::String("new-model".into()),
            AutomaticEmbeddingBindingChange::Model,
        ),
        (
            "revision",
            Value::String("revision-two".into()),
            AutomaticEmbeddingBindingChange::Revision,
        ),
        (
            "dimensions",
            Value::String("auto".into()),
            AutomaticEmbeddingBindingChange::Dimensions,
        ),
    ] {
        let mut changed = document(&built.bytes);
        changed["services"][&built.service]
            .as_table_mut()
            .unwrap()
            .insert(field.into(), value);
        let config = load(&path, &encode(&changed));
        assert_eq!(
            config
                .automatic_embedding_consent(&fs, &wiki.wiki_id, &built.profile)
                .unwrap(),
            AutomaticEmbeddingConsent::BindingChanged(reason)
        );
        assert!(
            config
                .authorize_automatic_embeddings(&fs, &wiki.wiki_id, &built.profile)
                .is_err()
        );
    }
}

#[test]
fn ca_content_change_disables_automatic_permission_and_stale_descriptor() {
    let (temp, wiki, fs, path) = fixture();
    let built = build_embedding_document(None, &setup(false), &wiki).unwrap();
    let ca = temp.path().join("provider-ca.pem");
    private_write(&ca, b"first certificate authority");
    let mut original = document(&built.bytes);
    original["services"][&built.service]
        .as_table_mut()
        .unwrap()
        .insert("ca_file".into(), Value::String(ca.to_str().unwrap().into()));
    let original = encode(&original);
    let plan = plan_provider_edit_with_automatic(
        Some(&original),
        &wiki,
        Some(&built.profile),
        &[],
        ProviderGrantDecision::Preserve,
        ProviderAutomaticEmbeddingDecision::Reviewed,
    )
    .unwrap();
    let old = load(&path, plan.proposed_bytes());
    assert!(
        old.authorize_automatic_embeddings(&fs, &wiki.wiki_id, plan.profile())
            .is_ok()
    );
    private_write(&ca, b"second certificate authority");
    assert!(
        old.automatic_embedding_consent(&fs, &wiki.wiki_id, plan.profile())
            .is_err()
    );
    let fresh = ProviderConfig::load(&path).unwrap();
    assert_eq!(
        fresh
            .automatic_embedding_consent(&fs, &wiki.wiki_id, plan.profile())
            .unwrap(),
        AutomaticEmbeddingConsent::BindingChanged(AutomaticEmbeddingBindingChange::Endpoint)
    );
}

#[test]
fn grant_revocation_and_config_race_invalidate_retained_authority() {
    let (_temp, wiki, fs, path) = fixture();
    let built = reviewed(&wiki, false);
    let old = load(&path, &built.bytes);
    let trusted = old
        .authorize_automatic_embeddings(&fs, &wiki.wiki_id, &built.profile)
        .unwrap();
    let mut revoked = document(&built.bytes);
    for (_, binding) in revoked["vault_bindings"].as_table_mut().unwrap().iter_mut() {
        binding
            .as_table_mut()
            .unwrap()
            .remove("automatic_embedding");
    }
    private_write(&path, &encode(&revoked));
    assert!(
        old.automatic_embedding_consent(&fs, &wiki.wiki_id, &built.profile)
            .is_err()
    );
    assert!(trusted.recheck(&fs).is_err());
    assert_eq!(
        ProviderConfig::load(&path)
            .unwrap()
            .automatic_embedding_consent(&fs, &wiki.wiki_id, &built.profile)
            .unwrap(),
        AutomaticEmbeddingConsent::Absent
    );
}

#[test]
fn automatic_only_combined_profile_does_not_grant_generation() {
    let (_temp, wiki, fs, path) = fixture();
    let built = reviewed(&wiki, false);
    let mut generation = ProviderRolePatch::new(ProviderRole::Generation);
    generation.model = Some("generation-test".into());
    generation.url = Some("https://gateway.example/generation".into());
    generation.auth =
        ProviderAuthPatch::Replace(SetupAuth::Environment("MISSING_GENERATION_KEY".into()));
    let plan = plan_provider_edit(
        Some(&built.bytes),
        &wiki,
        Some(&built.profile),
        &[generation],
        ProviderGrantDecision::Preserve,
    )
    .unwrap();
    assert!(!plan.consent_satisfied);
    assert!(!plan.configuration_consent_satisfied);
    assert_eq!(
        plan.automatic_embedding_consent,
        AutomaticEmbeddingConsent::Matching
    );
    let config = load(&path, plan.proposed_bytes());
    assert!(
        config
            .authorize_automatic_embeddings(&fs, &wiki.wiki_id, plan.profile())
            .is_ok()
    );
    assert!(
        config
            .authorize(&fs, &wiki.wiki_id, plan.profile(), Capability::Generate)
            .is_err()
    );
    assert!(
        config
            .authorize(&fs, &wiki.wiki_id, plan.profile(), Capability::Embed)
            .is_err()
    );
}

#[test]
fn automatic_consent_can_configure_embedding_with_untouched_unbound_generation() {
    let (_temp, wiki, fs, path) = fixture();
    let built = reviewed(&wiki, false);
    let mut generation = ProviderRolePatch::new(ProviderRole::Generation);
    generation.model = Some("generation-test".into());
    generation.url = Some("https://gateway.example/generation".into());
    generation.auth =
        ProviderAuthPatch::Replace(SetupAuth::Environment("MISSING_GENERATION_KEY".into()));
    let combined = plan_provider_edit(
        Some(&built.bytes),
        &wiki,
        Some(&built.profile),
        &[generation],
        ProviderGrantDecision::Preserve,
    )
    .unwrap();
    for changed in [false, true] {
        let mut embedding = ProviderRolePatch::new(ProviderRole::Embedding);
        if changed {
            embedding.model = Some("replacement-embedding".into());
        }
        let plan = plan_provider_edit_with_automatic(
            Some(combined.proposed_bytes()),
            &wiki,
            Some(combined.profile()),
            &[embedding],
            ProviderGrantDecision::Preserve,
            ProviderAutomaticEmbeddingDecision::Reviewed,
        )
        .unwrap();
        assert!(!plan.consent_satisfied);
        assert!(plan.configuration_consent_satisfied);
        let config = load(&path, plan.proposed_bytes());
        assert!(
            config
                .authorize_automatic_embeddings(&fs, &wiki.wiki_id, plan.profile())
                .is_ok()
        );
        for role in [Capability::Embed, Capability::Generate] {
            assert!(
                config
                    .authorize(&fs, &wiki.wiki_id, plan.profile(), role)
                    .is_err()
            );
        }
        let mut generation_change = ProviderRolePatch::new(ProviderRole::Generation);
        generation_change.model = Some("changed-generation".into());
        let refused = plan_provider_edit_with_automatic(
            Some(plan.proposed_bytes()),
            &wiki,
            Some(plan.profile()),
            &[generation_change],
            ProviderGrantDecision::Preserve,
            ProviderAutomaticEmbeddingDecision::Reviewed,
        )
        .unwrap();
        assert!(!refused.configuration_consent_satisfied);
    }
}

#[test]
fn reviewed_planning_before_wiki_creation_is_not_runtime_authorization() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = DefaultWiki {
        path: temp.path().join("future-wiki"),
        wiki_id: RecordId::new("vault_future").unwrap(),
    };
    let built = reviewed(&wiki, false);
    let planned = ProviderConfig::from_planned_bytes(&built.bytes).unwrap();
    assert_eq!(
        planned
            .planned_automatic_embedding_consent(&wiki, &built.profile)
            .unwrap(),
        AutomaticEmbeddingConsent::Matching
    );
    assert!(!wiki.path.exists());
}

#[test]
fn unsupported_grant_version_scope_and_dimensions_are_rejected() {
    let (_temp, wiki, _fs, _path) = fixture();
    let built = reviewed(&wiki, false);
    for (field, value) in [
        ("version", Value::Integer(2)),
        ("scope", Value::String("all_text_and_generation".into())),
        ("dimensions", Value::Integer(0)),
        ("dimensions", Value::String("arbitrary".into())),
        ("unknown", Value::String("PRIVATE-MARKER".into())),
    ] {
        let mut changed = document(&built.bytes);
        grant_mut(&mut changed).insert(field.into(), value);
        let error = ProviderConfig::validate_setup_bytes(&encode(&changed)).unwrap_err();
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("PRIVATE-MARKER")
        );
    }
}

#[cfg(unix)]
#[test]
fn preview_and_inspection_never_run_helper_or_read_secret_file() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, wiki, fs, path) = fixture();
    let marker = temp.path().join("helper-ran");
    let helper = temp.path().join("hostile-helper");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\ntouch '{}'\nprintf 'PRIVATE-HELPER-OUTPUT'\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut selection = setup(false);
    selection.auth = SetupAuth::command(
        vec![helper.to_str().unwrap().into()],
        SetupCommandOutput::Text,
        300,
        60,
        10,
    )
    .unwrap();
    let built = build_embedding_document_with_automatic(
        None,
        &selection,
        &wiki,
        ProviderAutomaticEmbeddingDecision::Reviewed,
    )
    .unwrap();
    let plan = plan_provider_edit(
        Some(&built.bytes),
        &wiki,
        Some(&built.profile),
        &[],
        ProviderGrantDecision::Preserve,
    )
    .unwrap();
    let config = load(&path, plan.proposed_bytes());
    assert_eq!(
        config
            .automatic_embedding_consent(&fs, &wiki.wiki_id, plan.profile())
            .unwrap(),
        AutomaticEmbeddingConsent::Matching
    );
    assert!(
        config
            .authorize_automatic_embeddings(&fs, &wiki.wiki_id, plan.profile())
            .is_ok()
    );
    let safe = format!(
        "{plan:?} {}",
        config.profile_summary(plan.profile()).unwrap()
    );
    for private in [
        "PRIVATE-HELPER-OUTPUT",
        "private-path",
        "private-tenant",
        helper.to_str().unwrap(),
    ] {
        assert!(!safe.contains(private));
    }
    assert!(!marker.exists());
    selection.auth = SetupAuth::PrivateFile(temp.path().join("NONEXISTENT-PRIVATE-KEY-MARKER"));
    let file_config = build_embedding_document_with_automatic(
        None,
        &selection,
        &wiki,
        ProviderAutomaticEmbeddingDecision::Reviewed,
    )
    .unwrap();
    let config = load(&path, &file_config.bytes);
    assert_eq!(
        config
            .automatic_embedding_consent(&fs, &wiki.wiki_id, &file_config.profile)
            .unwrap(),
        AutomaticEmbeddingConsent::Matching
    );
    assert!(
        config
            .authorize_automatic_embeddings(&fs, &wiki.wiki_id, &file_config.profile)
            .is_ok()
    );
}
