use super::*;
use crate::{
    config::{
        provider_setup::{EmbeddingSetup, SetupCommandOutput, build_embedding_document},
        providers::{MAX_PROVIDER_CONFIG_BYTES, ProviderConfig},
    },
    jobs::{Currency, PriceValidity, RateCard, budgets::rate_card_fingerprint},
};
use std::{collections::BTreeMap, path::Path, path::PathBuf};

fn wiki(base: &Path, name: &str) -> DefaultWiki {
    DefaultWiki {
        path: base.join(name),
        wiki_id: RecordId::new(format!("vault-{name}")).unwrap(),
    }
}

fn parse(bytes: &[u8]) -> Table {
    toml::from_str(std::str::from_utf8(bytes).unwrap()).unwrap()
}

fn fixture(wiki: &DefaultWiki, generation: bool, authorized: bool) -> Vec<u8> {
    let mut document = parse_document(None).unwrap();
    table_for_test(&mut document, "services").insert(
        "manual-embed".into(),
        Value::Table(Table::from_iter([
            ("adapter".into(), Value::String("embeddings-v1".into())),
            (
                "url".into(),
                Value::String("https://embed.example/private-path?tenant=private-tenant".into()),
            ),
            ("model".into(), Value::String("old-embedding".into())),
            ("revision".into(), Value::String("revision-one".into())),
            ("dimensions".into(), Value::Integer(1024)),
            ("max_batch_items".into(), Value::Integer(16)),
            ("max_batch_bytes".into(), Value::Integer(65536)),
            ("timeout_seconds".into(), Value::Integer(75)),
            (
                "auth".into(),
                Value::Table(Table::from_iter([
                    ("kind".into(), Value::String("static".into())),
                    ("key".into(), Value::String("PRIVATE-LITERAL-MARKER".into())),
                    ("header".into(), Value::String("X-Provider-Key".into())),
                    ("prefix".into(), Value::String("private-prefix ".into())),
                ])),
            ),
            (
                "headers".into(),
                Value::Table(Table::from_iter([(
                    "X-Deployment".into(),
                    Value::String("private-header-marker".into()),
                )])),
            ),
            (
                "secret_headers".into(),
                Value::Table(Table::from_iter([(
                    "X-Tenant-Key".into(),
                    Value::Table(Table::from_iter([
                        ("kind".into(), Value::String("env".into())),
                        (
                            "name".into(),
                            Value::String("MISSING_PRIVATE_TENANT_KEY".into()),
                        ),
                    ])),
                )])),
            ),
        ])),
    );
    let mut profile =
        Table::from_iter([("embedding".into(), Value::String("manual-embed".into()))]);
    if generation {
        table_for_test(&mut document, "services").insert(
            "manual-generate".into(),
            Value::Table(Table::from_iter([
                ("url".into(), Value::String("https://generate.example/private-generation?tenant=private-generation-tenant".into())),
                ("model".into(), Value::String("old-generation".into())),
                ("response_mode".into(), Value::String("json-schema".into())),
                ("connect_timeout_seconds".into(), Value::Integer(5)),
                ("auth".into(), Value::Table(Table::from_iter([
                    ("kind".into(), Value::String("static".into())),
                    ("key_env".into(), Value::String("MISSING_GENERATION_KEY".into())),
                ]))),
            ])),
        );
        profile.insert("generation".into(), Value::String("manual-generate".into()));
    }
    table_for_test(&mut document, "profiles").insert("manual".into(), Value::Table(profile));
    if authorized {
        add_binding(&mut document, "manual-binding", wiki, "manual");
    }
    let mut bytes =
        b"# Manual formatting and comments are retained exactly.\n# owner-comment-marker\n"
            .to_vec();
    bytes.extend(toml::to_string(&document).unwrap().as_bytes());
    ProviderConfig::validate_setup_bytes(&bytes).unwrap();
    bytes
}

fn table_for_test<'a>(document: &'a mut Table, name: &str) -> &'a mut Table {
    document.get_mut(name).unwrap().as_table_mut().unwrap()
}

fn add_binding(document: &mut Table, name: &str, wiki: &DefaultWiki, profile: &str) {
    table_for_test(document, "vault_bindings").insert(
        name.into(),
        Value::Table(Table::from_iter([
            (
                "root".into(),
                Value::String(wiki.path.to_str().unwrap().into()),
            ),
            ("wiki_id".into(), Value::String(wiki.wiki_id.to_string())),
            (
                "allowed_profiles".into(),
                Value::Array(vec![Value::String(profile.into())]),
            ),
        ])),
    );
}

fn model_patch(role: ProviderRole, model: &str) -> ProviderRolePatch {
    let mut patch = ProviderRolePatch::new(role);
    patch.model = Some(model.into());
    patch
}

fn new_role(role: ProviderRole) -> ProviderRolePatch {
    let mut patch = ProviderRolePatch::new(role);
    patch.model = Some("new-model".into());
    patch.url = Some("https://new.example/v1".into());
    patch.auth = ProviderAuthPatch::Replace(SetupAuth::Environment("NEW_KEY".into()));
    patch
}

fn selected_service<'a>(document: &'a Table, profile: &str, role: ProviderRole) -> &'a Table {
    let id = document["profiles"][profile][role.as_str()]
        .as_str()
        .unwrap();
    document["services"][id].as_table().unwrap()
}

fn count(bytes: &[u8], section: &str) -> usize {
    parse(bytes)[section].as_table().unwrap().len()
}

#[test]
fn model_edit_preserves_manual_bytes_auth_headers_and_other_role() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let bytes = fixture(&wiki, true, true);
    let before = parse(&bytes);
    let plan = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[model_patch(ProviderRole::Embedding, "new-embedding")],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    assert!(plan.proposed_bytes().starts_with(&bytes));
    let after = parse(plan.proposed_bytes());
    for section in ["services", "profiles", "vault_bindings"] {
        for (name, original) in before[section].as_table().unwrap() {
            assert_eq!(&after[section][name], original);
        }
    }
    let copied = selected_service(&after, plan.profile(), ProviderRole::Embedding);
    let mut expected = before["services"]["manual-embed"]
        .as_table()
        .unwrap()
        .clone();
    expected.insert("model".into(), Value::String("new-embedding".into()));
    assert_eq!(copied, &expected);
    assert_eq!(
        after["profiles"][plan.profile()]["generation"].as_str(),
        Some("manual-generate")
    );
    assert_eq!(
        plan.changed_fields,
        vec![ProviderChangedField {
            role: ProviderRole::Embedding,
            field: ProviderField::Model
        }]
    );
    assert_eq!(plan.previous_profile(), Some("manual"));
}

#[test]
fn generation_cap_edit_retains_exact_embedding_reference_and_planned_identity() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let bytes = fixture(&wiki, true, true);
    let mut patch = ProviderRolePatch::new(ProviderRole::Generation);
    patch.max_output_tokens = Some(8192);
    let plan = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[patch],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    let after = parse(plan.proposed_bytes());
    assert_eq!(
        after["profiles"][plan.profile()]["embedding"].as_str(),
        Some("manual-embed")
    );
    assert_eq!(
        plan.previous_embedding_identity
            .as_ref()
            .unwrap()
            .fingerprint,
        plan.new_embedding_identity.as_ref().unwrap().fingerprint
    );
    assert_eq!(plan.roles[1].effective_output_token_limit, Some(8192));
    assert!(plan.roles[1].changed);
    assert!(!plan.roles[1].destination_changed);
    assert!(!plan.roles[1].model_identity_changed);
    assert_eq!(
        plan.changed_fields,
        vec![ProviderChangedField {
            role: ProviderRole::Generation,
            field: ProviderField::MaxOutputTokens
        }]
    );
    assert_eq!(count(plan.proposed_bytes(), "services"), 3);
}

#[test]
fn auth_only_edit_keeps_transport_settings_and_manual_auth_header_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let bytes = fixture(&wiki, true, true);
    let before = parse(&bytes);
    let mut patch = ProviderRolePatch::new(ProviderRole::Embedding);
    patch.auth = ProviderAuthPatch::Replace(SetupAuth::Environment("REPLACEMENT_KEY".into()));
    let plan = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[patch],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    let after = parse(plan.proposed_bytes());
    let copied = selected_service(&after, plan.profile(), ProviderRole::Embedding);
    let original = before["services"]["manual-embed"].as_table().unwrap();
    for (key, value) in original {
        if key != "auth" {
            assert_eq!(copied.get(key), Some(value));
        }
    }
    assert_eq!(copied["auth"]["header"], original["auth"]["header"]);
    assert_eq!(copied["auth"]["prefix"], original["auth"]["prefix"]);
    assert_eq!(copied["auth"]["key_env"].as_str(), Some("REPLACEMENT_KEY"));
    assert!(copied["auth"].as_table().unwrap().get("key").is_none());
    assert_eq!(
        plan.changed_fields,
        vec![ProviderChangedField {
            role: ProviderRole::Embedding,
            field: ProviderField::Auth
        }]
    );
    assert!(!plan.roles[0].destination_changed);
    assert!(!plan.roles[0].model_identity_changed);
    assert_eq!(
        plan.previous_embedding_identity
            .as_ref()
            .unwrap()
            .fingerprint,
        plan.new_embedding_identity.as_ref().unwrap().fingerprint
    );
}

#[test]
fn unchanged_and_explicit_effective_defaults_preserve_manual_profile_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let bytes = fixture(&wiki, true, true);
    let mut generation = ProviderRolePatch::new(ProviderRole::Generation);
    generation.adapter = Some(ProviderAdapter::ResponsesV1);
    generation.max_output_tokens = Some(4096);
    generation.auth =
        ProviderAuthPatch::Replace(SetupAuth::Environment("MISSING_GENERATION_KEY".into()));
    for patches in [
        vec![],
        vec![generation],
        vec![model_patch(ProviderRole::Embedding, "old-embedding")],
    ] {
        let plan = plan_provider_edit(
            Some(&bytes),
            &wiki,
            Some("manual"),
            &patches,
            ProviderGrantDecision::Preserve,
        )
        .unwrap();
        assert_eq!(plan.proposed_bytes(), bytes);
        assert_eq!(plan.profile(), "manual");
        assert!(plan.changed_fields.is_empty());
        assert!(plan.consent_requirements.is_empty());
        assert!(plan.consent_satisfied);
    }
}

#[test]
fn unchanged_ungranted_profile_returns_consent_without_appending() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let bytes = fixture(&wiki, true, false);
    let plan = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[],
        ProviderGrantDecision::Preserve,
    )
    .unwrap();
    assert_eq!(plan.proposed_bytes(), bytes);
    assert_eq!(plan.profile(), "manual");
    assert!(!plan.consent_satisfied);
    assert_eq!(plan.consent_requirements.len(), 2);
    assert!(
        plan.consent_requirements
            .iter()
            .all(|requirement| requirement.reason == ProviderConsentReason::MissingPermission)
    );
}

#[test]
fn identical_rerun_and_return_to_previously_chosen_settings_do_not_grow() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let bytes = fixture(&wiki, true, true);
    let a = model_patch(ProviderRole::Embedding, "choice-a");
    let b = model_patch(ProviderRole::Embedding, "choice-b");
    let first = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        std::slice::from_ref(&a),
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    let repeated = plan_provider_edit(
        Some(first.proposed_bytes()),
        &wiki,
        Some(first.profile()),
        std::slice::from_ref(&a),
        ProviderGrantDecision::Preserve,
    )
    .unwrap();
    assert_eq!(repeated.proposed_bytes(), first.proposed_bytes());
    let second = plan_provider_edit(
        Some(first.proposed_bytes()),
        &wiki,
        Some(first.profile()),
        &[b],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    let return_to_first = plan_provider_edit(
        Some(second.proposed_bytes()),
        &wiki,
        Some(second.profile()),
        &[a],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    assert_eq!(return_to_first.proposed_bytes(), second.proposed_bytes());
    assert_eq!(return_to_first.profile(), first.profile());
}

#[test]
fn shared_embedding_profile_gets_scoped_generation_without_expanding_other_wiki() {
    let temp = tempfile::tempdir().unwrap();
    let first_wiki = wiki(temp.path(), "one");
    let second_wiki = wiki(temp.path(), "two");
    let mut document = parse(&fixture(&first_wiki, false, true));
    add_binding(&mut document, "second-binding", &second_wiki, "manual");
    let bytes = toml::to_string(&document).unwrap().into_bytes();
    let plan = plan_provider_edit(
        Some(&bytes),
        &first_wiki,
        Some("manual"),
        &[new_role(ProviderRole::Generation)],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    let after = parse(plan.proposed_bytes());
    assert!(plan.proposed_bytes().starts_with(&bytes));
    assert!(
        after["profiles"]["manual"]
            .as_table()
            .unwrap()
            .get("generation")
            .is_none()
    );
    assert_ne!(plan.profile(), "manual");
    assert_eq!(
        after["vault_bindings"]["second-binding"],
        document["vault_bindings"]["second-binding"]
    );
    assert!(bound_to(&after, &first_wiki, plan.profile()).unwrap());
    assert!(!bound_to(&after, &second_wiki, plan.profile()).unwrap());
    assert_eq!(
        plan.consent_requirements,
        vec![ProviderConsentRequirement {
            role: ProviderRole::Generation,
            reason: ProviderConsentReason::AddedRole
        }]
    );
}

#[test]
fn changed_role_does_not_inherit_existing_grant_without_review() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let bytes = fixture(&wiki, true, true);
    let plan = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[model_patch(ProviderRole::Embedding, "unreviewed")],
        ProviderGrantDecision::Preserve,
    )
    .unwrap();
    let after = parse(plan.proposed_bytes());
    assert!(!plan.consent_satisfied);
    assert_eq!(count(plan.proposed_bytes(), "vault_bindings"), 1);
    assert!(!bound_to(&after, &wiki, plan.profile()).unwrap());
    assert!(bound_to(&after, &wiki, "manual").unwrap());
    assert_eq!(
        plan.consent_requirements,
        vec![ProviderConsentRequirement {
            role: ProviderRole::Embedding,
            reason: ProviderConsentReason::ChangedRole
        }]
    );
}

#[test]
fn binding_requires_both_exact_root_and_wiki_id() {
    let temp = tempfile::tempdir().unwrap();
    let original = wiki(temp.path(), "one");
    let bytes = fixture(&original, true, true);
    let root_mismatch = DefaultWiki {
        path: temp.path().join("other-root"),
        wiki_id: original.wiki_id.clone(),
    };
    let id_mismatch = DefaultWiki {
        path: original.path.clone(),
        wiki_id: RecordId::new("vault-other").unwrap(),
    };
    for selected in [root_mismatch, id_mismatch] {
        let plan = plan_provider_edit(
            Some(&bytes),
            &selected,
            Some("manual"),
            &[],
            ProviderGrantDecision::Preserve,
        )
        .unwrap();
        assert_eq!(plan.proposed_bytes(), bytes);
        assert!(!plan.consent_satisfied);
        assert_eq!(plan.consent_requirements.len(), 2);
        assert!(plan.roles.iter().all(|role| !role.previous_permission));
        let granted = plan_provider_edit(
            Some(&bytes),
            &selected,
            Some("manual"),
            &[],
            ProviderGrantDecision::ReviewedAllRoles,
        )
        .unwrap();
        assert!(granted.proposed_bytes().starts_with(&bytes));
        assert_eq!(count(granted.proposed_bytes(), "vault_bindings"), 2);
        assert!(bound_to(&parse(granted.proposed_bytes()), &selected, "manual").unwrap());
    }
}

#[test]
fn embedding_only_grant_never_authorizes_added_changed_or_ungranted_generation() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    for (generation, authorized, patch) in [
        (false, true, new_role(ProviderRole::Generation)),
        (
            true,
            true,
            model_patch(ProviderRole::Generation, "changed-generation"),
        ),
        (
            true,
            false,
            model_patch(ProviderRole::Embedding, "changed-embedding"),
        ),
    ] {
        let bytes = fixture(&wiki, generation, authorized);
        let error = plan_provider_edit(
            Some(&bytes),
            &wiki,
            Some("manual"),
            &[patch],
            ProviderGrantDecision::EmbeddingOnly,
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert!(error.message.contains("--grant-provider"));
    }
    let bytes = fixture(&wiki, true, true);
    let plan = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[model_patch(ProviderRole::Embedding, "reviewed-embedding")],
        ProviderGrantDecision::EmbeddingOnly,
    )
    .unwrap();
    assert!(plan.consent_satisfied);
    let document = parse(plan.proposed_bytes());
    assert!(bound_to(&document, &wiki, plan.profile()).unwrap());
    assert_eq!(
        document["profiles"][plan.profile()]["generation"].as_str(),
        Some("manual-generate")
    );
}

#[test]
fn fresh_combined_roles_can_reuse_the_proposed_command_auth_without_running_it() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let missing_program = temp
        .path()
        .join("nonexistent-helper")
        .to_str()
        .unwrap()
        .to_owned();
    let command = SetupAuth::command(
        vec![missing_program, "--private-argument-marker".into()],
        SetupCommandOutput::Text,
        300,
        60,
        10,
    )
    .unwrap();
    let mut embedding = new_role(ProviderRole::Embedding);
    embedding.auth = ProviderAuthPatch::Replace(command);
    let mut generation = new_role(ProviderRole::Generation);
    generation.auth = ProviderAuthPatch::CopyFrom(ProviderRole::Embedding);
    let plan = plan_provider_edit(
        None,
        &wiki,
        None,
        &[embedding, generation],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    let document = parse(plan.proposed_bytes());
    assert_eq!(
        selected_service(&document, plan.profile(), ProviderRole::Embedding)["auth"],
        selected_service(&document, plan.profile(), ProviderRole::Generation)["auth"]
    );
    assert_eq!(
        plan.roles[0].command_auth.as_ref().unwrap().ttl_seconds,
        300
    );
    assert_eq!(
        plan.roles[1]
            .command_auth
            .as_ref()
            .unwrap()
            .refresh_skew_seconds,
        60
    );
    assert_eq!(
        plan.roles[1].command_auth.as_ref().unwrap().timeout_seconds,
        10
    );
    assert_eq!(plan.consent_requirements.len(), 2);
}

#[test]
fn auth_reuse_retains_manual_literal_and_refuses_absent_source_or_cycles() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let bytes = fixture(&wiki, false, true);
    let mut generation = new_role(ProviderRole::Generation);
    generation.auth = ProviderAuthPatch::CopyFrom(ProviderRole::Embedding);
    let plan = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[generation],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    let document = parse(plan.proposed_bytes());
    assert_eq!(
        selected_service(&document, plan.profile(), ProviderRole::Generation)["auth"],
        document["services"]["manual-embed"]["auth"]
    );
    let mut embedding = new_role(ProviderRole::Embedding);
    embedding.auth = ProviderAuthPatch::CopyFrom(ProviderRole::Generation);
    assert!(
        plan_provider_edit(
            None,
            &wiki,
            None,
            &[embedding.clone()],
            ProviderGrantDecision::Preserve
        )
        .unwrap_err()
        .message
        .contains("source role")
    );
    let mut generation = new_role(ProviderRole::Generation);
    generation.auth = ProviderAuthPatch::CopyFrom(ProviderRole::Embedding);
    assert!(
        plan_provider_edit(
            None,
            &wiki,
            None,
            &[embedding, generation],
            ProviderGrantDecision::Preserve
        )
        .unwrap_err()
        .message
        .contains("cycle")
    );
}

#[test]
fn missing_required_settings_and_wrong_role_fields_refuse_before_publication() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    for field in ["model", "url", "auth"] {
        let mut patch = new_role(ProviderRole::Generation);
        match field {
            "model" => patch.model = None,
            "url" => patch.url = None,
            _ => patch.auth = ProviderAuthPatch::Preserve,
        }
        assert_eq!(
            plan_provider_edit(
                None,
                &wiki,
                None,
                &[patch],
                ProviderGrantDecision::ReviewedAllRoles
            )
            .unwrap_err()
            .code,
            ErrorCode::ConfigInvalid
        );
    }
    let mut patch = new_role(ProviderRole::Embedding);
    patch.max_output_tokens = Some(8192);
    assert!(
        plan_provider_edit(None, &wiki, None, &[patch], ProviderGrantDecision::Preserve).is_err()
    );
    let mut patch = new_role(ProviderRole::Generation);
    patch.dimensions = Some("auto".into());
    assert!(
        plan_provider_edit(None, &wiki, None, &[patch], ProviderGrantDecision::Preserve).is_err()
    );
    let mut patch = new_role(ProviderRole::Embedding);
    patch.adapter = Some(ProviderAdapter::ResponsesV1);
    assert!(
        plan_provider_edit(None, &wiki, None, &[patch], ProviderGrantDecision::Preserve).is_err()
    );
}

#[test]
fn invalid_adapter_conversion_keeps_existing_manual_fields_and_refuses() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let mut document = parse(&fixture(&wiki, true, true));
    let generation = document["services"]["manual-generate"]
        .as_table_mut()
        .unwrap();
    generation.insert(
        "adapter".into(),
        Value::String("chat-completions-v1".into()),
    );
    generation.insert("instruction_role".into(), Value::String("developer".into()));
    generation.insert(
        "output_limit_field".into(),
        Value::String("max_tokens".into()),
    );
    let bytes = toml::to_string(&document).unwrap().into_bytes();
    ProviderConfig::validate_setup_bytes(&bytes).unwrap();
    let mut patch = ProviderRolePatch::new(ProviderRole::Generation);
    patch.adapter = Some(ProviderAdapter::ResponsesV1);
    let error = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[patch],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigInvalid);
    assert!(error.message.contains("adapter settings"));
}

#[test]
fn changed_url_keeps_non_url_fields_but_changes_planned_embedding_identity() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let bytes = fixture(&wiki, true, true);
    let mut patch = ProviderRolePatch::new(ProviderRole::Embedding);
    patch.url = Some("https://embed.example/new-path?tenant=another-private-tenant".into());
    let plan = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[patch],
        ProviderGrantDecision::Preserve,
    )
    .unwrap();
    let document = parse(plan.proposed_bytes());
    let mut expected = document["services"]["manual-embed"]
        .as_table()
        .unwrap()
        .clone();
    expected.insert(
        "url".into(),
        Value::String("https://embed.example/new-path?tenant=another-private-tenant".into()),
    );
    assert_eq!(
        selected_service(&document, plan.profile(), ProviderRole::Embedding),
        &expected
    );
    assert_ne!(
        plan.previous_embedding_identity
            .as_ref()
            .unwrap()
            .fingerprint,
        plan.new_embedding_identity.as_ref().unwrap().fingerprint
    );
    assert!(!plan.consent_satisfied);
}

#[test]
fn historical_grant_does_not_satisfy_review_for_a_new_selection() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let bytes = fixture(&wiki, true, true);
    let changed = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[model_patch(ProviderRole::Generation, "different-model")],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    let back = plan_provider_edit(
        Some(changed.proposed_bytes()),
        &wiki,
        Some(changed.profile()),
        &[model_patch(ProviderRole::Generation, "old-generation")],
        ProviderGrantDecision::Preserve,
    )
    .unwrap();
    assert_eq!(back.profile(), "manual");
    assert_eq!(back.proposed_bytes(), changed.proposed_bytes());
    assert!(bound_to(&parse(back.proposed_bytes()), &wiki, "manual").unwrap());
    assert!(!back.consent_satisfied);
    assert_eq!(
        back.consent_requirements,
        vec![ProviderConsentRequirement {
            role: ProviderRole::Generation,
            reason: ProviderConsentReason::ChangedRole,
        }]
    );
}

#[test]
fn service_name_collision_is_an_actionable_conflict_and_keeps_original_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let mut document = parse(&fixture(&wiki, true, true));
    let original = document["services"]["manual-embed"]
        .as_table()
        .unwrap()
        .clone();
    let mut proposed = original.clone();
    proposed.insert("model".into(), Value::String("collision-candidate".into()));
    let hash = digest("lwiki.provider.edit-service.v1", &normalized(&proposed)).unwrap();
    let name = format!("lwiki-embedding-{}", &hash[..24]);
    table_for_test(&mut document, "services").insert(name, Value::Table(original));
    let bytes = toml::to_string(&document).unwrap().into_bytes();
    ProviderConfig::validate_setup_bytes(&bytes).unwrap();
    let error = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[model_patch(ProviderRole::Embedding, "collision-candidate")],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert!(error.message.contains("preserved"));
    assert_eq!(parse(&bytes), document);
}

#[test]
fn safe_preview_and_debug_omit_private_urls_headers_argv_and_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let bytes = fixture(&wiki, true, true);
    let command = SetupAuth::command(
        vec![
            temp.path()
                .join("private-program-marker")
                .to_str()
                .unwrap()
                .into(),
            "private-argv-marker".into(),
        ],
        SetupCommandOutput::Json,
        300,
        60,
        10,
    )
    .unwrap();
    let mut patch = model_patch(ProviderRole::Embedding, "updated-model");
    patch.auth = ProviderAuthPatch::Replace(command.clone());
    let patch_debug = format!("{patch:?}");
    let plan = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[patch],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    let public = format!(
        "{} {plan:?} {command:?} {patch_debug}",
        serde_json::to_string(&plan.roles).unwrap()
    );
    for private in [
        "PRIVATE-LITERAL-MARKER",
        "private-header-marker",
        "private-prefix",
        "private-tenant",
        "private-path",
        "private-generation-tenant",
        "private-program-marker",
        "private-argv-marker",
        "MISSING_PRIVATE_TENANT_KEY",
    ] {
        assert!(!public.contains(private), "private marker was exposed");
    }
    assert!(public.contains("embed.example"));
    assert!(public.contains("generate.example"));
    assert!(public.contains("command"));
    assert_eq!(plan.projected_bytes, plan.proposed_bytes().len());
}

#[test]
fn command_constructor_enforces_broker_argv_timing_and_absolute_path_bounds() {
    let temp = tempfile::tempdir().unwrap();
    let executable = temp
        .path()
        .join("nonexistent-program")
        .to_str()
        .unwrap()
        .to_owned();
    for (argv, ttl, skew, timeout) in [
        (vec![], 300, 60, 10),
        (vec!["relative-program".into()], 300, 60, 10),
        (vec![executable.clone(); 65], 300, 60, 10),
        (vec![executable.clone(), "x".repeat(4097)], 300, 60, 10),
        (
            vec![
                executable.clone(),
                "x".repeat(4096),
                "x".repeat(4096),
                "x".repeat(4096),
                "x".repeat(4096),
            ],
            300,
            60,
            10,
        ),
        (
            vec![executable.clone(), "control\nPRIVATE-MARKER".into()],
            300,
            60,
            10,
        ),
        (vec![executable.clone()], 0, 0, 10),
        (vec![executable.clone()], 3601, 60, 10),
        (vec![executable.clone()], 300, 300, 10),
        (vec![executable.clone()], 300, 60, 0),
        (vec![executable], 300, 60, 11),
    ] {
        let error =
            SetupAuth::command(argv, SetupCommandOutput::Text, ttl, skew, timeout).unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert!(!format!("{error:?}").contains("PRIVATE-MARKER"));
    }
}

#[test]
fn one_ten_fifty_distinct_edits_have_bounded_linear_retention_and_noop_growth() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let mut bytes = fixture(&wiki, true, true);
    let initial_size = bytes.len();
    let mut profile = "manual".to_owned();
    for iteration in 1..=50 {
        let patch = model_patch(ProviderRole::Generation, &format!("choice-{iteration:02}"));
        let plan = plan_provider_edit(
            Some(&bytes),
            &wiki,
            Some(&profile),
            std::slice::from_ref(&patch),
            ProviderGrantDecision::ReviewedAllRoles,
        )
        .unwrap();
        assert!(plan.proposed_bytes().starts_with(&bytes));
        profile = plan.profile().into();
        bytes = plan.proposed_bytes().to_vec();
        if [1, 10, 50].contains(&iteration) {
            assert_eq!(count(&bytes, "services"), 2 + iteration);
            assert_eq!(count(&bytes, "profiles"), 1 + iteration);
            assert_eq!(count(&bytes, "vault_bindings"), 1 + iteration);
            assert!(bytes.len() > initial_size);
            assert!(bytes.len() < MAX_PROVIDER_CONFIG_BYTES);
            let replay = plan_provider_edit(
                Some(&bytes),
                &wiki,
                Some(&profile),
                &[patch],
                ProviderGrantDecision::Preserve,
            )
            .unwrap();
            assert_eq!(replay.proposed_bytes(), bytes);
        }
    }
    // Each small generation edit appends one service/profile/binding. Large
    // copied settings can exhaust the byte ceiling much sooner than this case.
    assert!(bytes.len() - initial_size < 50 * 1200);
}

#[test]
fn byte_ceiling_refuses_before_returning_a_publishable_proposal() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let mut bytes = fixture(&wiki, true, true);
    let remaining = MAX_PROVIDER_CONFIG_BYTES - bytes.len();
    bytes.extend_from_slice(b"#");
    bytes.extend(std::iter::repeat_n(b'x', remaining - 2));
    bytes.push(b'\n');
    assert_eq!(bytes.len(), MAX_PROVIDER_CONFIG_BYTES);
    ProviderConfig::validate_setup_bytes(&bytes).unwrap();
    let error = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[model_patch(ProviderRole::Generation, "needs-space")],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigInvalid);
    assert!(error.message.contains("64 KiB"));
    let noop = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[],
        ProviderGrantDecision::Preserve,
    )
    .unwrap();
    assert_eq!(noop.proposed_bytes(), bytes);
}

#[test]
fn service_and_profile_ceilings_refuse_growth_at_128_but_allow_noop() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    for fill_services in [false, true] {
        let mut document = parse(&fixture(&wiki, false, true));
        if fill_services {
            let base = document["services"]["manual-embed"].clone();
            for index in 1..128 {
                table_for_test(&mut document, "services")
                    .insert(format!("spare-{index}"), base.clone());
            }
        } else {
            let base = document["profiles"]["manual"].clone();
            for index in 1..128 {
                table_for_test(&mut document, "profiles")
                    .insert(format!("spare-{index}"), base.clone());
            }
        }
        // Keep the service ceiling fixture below the independent byte ceiling.
        if fill_services {
            for (_, value) in table_for_test(&mut document, "services").iter_mut() {
                let service = value.as_table_mut().unwrap();
                service.retain(|key, _| ["adapter", "model", "url", "auth"].contains(&key));
                service.insert("url".into(), Value::String("https://e.example".into()));
                service.insert(
                    "auth".into(),
                    Value::Table(Table::from_iter([
                        ("kind".into(), Value::String("static".into())),
                        ("key_env".into(), Value::String("KEY".into())),
                    ])),
                );
            }
        }
        let bytes = toml::to_string(&document).unwrap().into_bytes();
        assert!(bytes.len() < MAX_PROVIDER_CONFIG_BYTES);
        ProviderConfig::validate_setup_bytes(&bytes).unwrap();
        let error = plan_provider_edit(
            Some(&bytes),
            &wiki,
            Some("manual"),
            &[model_patch(ProviderRole::Embedding, "new-setting")],
            ProviderGrantDecision::ReviewedAllRoles,
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert!(error.message.contains("128"));
        let noop = plan_provider_edit(
            Some(&bytes),
            &wiki,
            Some("manual"),
            &[],
            ProviderGrantDecision::Preserve,
        )
        .unwrap();
        assert_eq!(noop.proposed_bytes(), bytes);
    }
}

#[test]
fn copied_rate_card_is_preserved_with_valid_fingerprint() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let mut document = parse(&fixture(&wiki, true, true));
    let mut card = RateCard {
        id: "fixture-card".into(),
        version: 1,
        fingerprint: Blake3Hash::digest("placeholder"),
        currency: Currency::new("USD").unwrap(),
        validity: PriceValidity::DispatchLocked {
            valid_from_utc_ms: 0,
            valid_until_utc_ms: i64::MAX,
        },
        request_fee_nanounits: 0,
        rates: BTreeMap::new(),
    };
    card.fingerprint = rate_card_fingerprint(&card).unwrap();
    let value = Value::try_from(card).unwrap();
    document["services"]["manual-embed"]
        .as_table_mut()
        .unwrap()
        .insert("rate_card".into(), value.clone());
    let bytes = toml::to_string(&document).unwrap().into_bytes();
    ProviderConfig::validate_setup_bytes(&bytes).unwrap();
    let plan = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[model_patch(ProviderRole::Embedding, "new-model")],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    let after = parse(plan.proposed_bytes());
    assert_eq!(
        selected_service(&after, plan.profile(), ProviderRole::Embedding)["rate_card"],
        value
    );
}

#[cfg(unix)]
#[test]
fn copied_private_ca_path_is_retained_without_reading_credential_files() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let ca = temp.path().join("ca.pem");
    std::fs::write(&ca, b"fixture CA bytes").unwrap();
    std::fs::set_permissions(&ca, std::fs::Permissions::from_mode(0o600)).unwrap();
    let nonexistent_key = temp.path().join("must-not-read-key");
    let mut document = parse(&fixture(&wiki, true, true));
    let embedding = document["services"]["manual-embed"].as_table_mut().unwrap();
    embedding.insert("ca_file".into(), Value::String(ca.to_str().unwrap().into()));
    embedding.insert(
        "auth".into(),
        Value::Table(Table::from_iter([
            ("kind".into(), Value::String("static".into())),
            (
                "key_file".into(),
                Value::String(nonexistent_key.to_str().unwrap().into()),
            ),
        ])),
    );
    let bytes = toml::to_string(&document).unwrap().into_bytes();
    let plan = plan_provider_edit(
        Some(&bytes),
        &wiki,
        Some("manual"),
        &[model_patch(ProviderRole::Embedding, "ca-model")],
        ProviderGrantDecision::ReviewedAllRoles,
    )
    .unwrap();
    let after = parse(plan.proposed_bytes());
    assert_eq!(
        selected_service(&after, plan.profile(), ProviderRole::Embedding)["ca_file"].as_str(),
        ca.to_str()
    );
    assert!(!nonexistent_key.exists());
}

#[test]
fn legacy_embedding_wrapper_keeps_static_names_and_replay_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let setup = EmbeddingSetup {
        model: "legacy-model".into(),
        url: "https://legacy.example/embeddings".into(),
        dimensions: Some("auto".into()),
        auth: SetupAuth::Environment("LEGACY_KEY".into()),
        grant: true,
    };
    let first = build_embedding_document(None, &setup, &wiki).unwrap();
    let expected = Value::Table(Table::from_iter([
        ("adapter".into(), Value::String("embeddings-v1".into())),
        ("url".into(), Value::String(setup.url.clone())),
        ("model".into(), Value::String(setup.model.clone())),
        ("dimensions".into(), Value::String("auto".into())),
        (
            "auth".into(),
            Value::Table(auth_table(&setup.auth).unwrap()),
        ),
    ]));
    let hash = blake3::hash(toml::to_string(&expected).unwrap().as_bytes())
        .to_hex()
        .to_string();
    assert_eq!(first.profile, format!("lwiki-{}", &hash[..24]));
    assert_eq!(first.service, format!("lwiki-embedding-{}", &hash[..24]));
    let second = build_embedding_document(Some(&first.bytes), &setup, &wiki).unwrap();
    assert_eq!(first.bytes, second.bytes);
}

#[test]
fn duplicate_patches_missing_profile_and_invalid_existing_bytes_refuse_safely() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = wiki(temp.path(), "one");
    let bytes = fixture(&wiki, true, true);
    let patch = model_patch(ProviderRole::Embedding, "changed");
    assert!(
        plan_provider_edit(
            Some(&bytes),
            &wiki,
            Some("manual"),
            &[patch.clone(), patch],
            ProviderGrantDecision::Preserve
        )
        .unwrap_err()
        .message
        .contains("at most one")
    );
    assert!(
        plan_provider_edit(
            Some(&bytes),
            &wiki,
            Some("missing"),
            &[],
            ProviderGrantDecision::Preserve
        )
        .unwrap_err()
        .message
        .contains("profile is missing")
    );
    let error = plan_provider_edit(
        Some(b"PRIVATE-MARKER invalid TOML"),
        &wiki,
        Some("manual"),
        &[],
        ProviderGrantDecision::Preserve,
    )
    .unwrap_err();
    assert!(!error.message.contains("PRIVATE-MARKER"));
    let relative = DefaultWiki {
        path: PathBuf::from("relative"),
        wiki_id: wiki.wiki_id,
    };
    assert!(
        plan_provider_edit(
            None,
            &relative,
            None,
            &[new_role(ProviderRole::Embedding)],
            ProviderGrantDecision::Preserve
        )
        .is_err()
    );
}
