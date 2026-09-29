use lwiki::{
    app::offline::init,
    app::*,
    catalog::scan::scan,
    changes::*,
    config::{local, *},
    domain::*,
    records::parse_note,
    sources::*,
    vault::{VaultFs, VaultRoot, discovery},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn rel(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn fixture() -> (tempfile::TempDir, VaultRoot) {
    let t = tempfile::tempdir().unwrap();
    fs::write(
        t.path().join("WIKI.md"),
        b"---\nwiki_schema: \"1\"\nwiki_id: Vault.Case\nwiki_kind: vault\ntitle: Offline\n---\n",
    )
    .unwrap();
    let root = VaultRoot::explicit(t.path()).unwrap();
    (t, root)
}
fn app(root: &VaultRoot, dry: bool) -> OfflineApp {
    OfflineApp::new(
        VaultFs::new(root.clone()),
        OperationOptions {
            dry_run: dry,
            offline: true,
            ..Default::default()
        },
    )
    .unwrap()
}
fn page(name: &str, body: &str) -> Vec<u8> {
    format!("---\nwiki_schema: \"1\"\nwiki_id: {name}\nwiki_kind: page\ntitle: {name}\nwiki_status: reviewed\n---\n{body}").into_bytes()
}
fn request(content: &[u8]) -> CaptureRequest {
    CaptureRequest {
        title: "Captured fixture".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "fixture.txt".into(),
        original: content.to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/plain".into()),
    }
}
fn tree(path: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)>) {
        for entry in fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = entry.metadata().unwrap();
            let bytes = if meta.is_dir() {
                vec![]
            } else {
                fs::read(&path).unwrap()
            };
            out.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                (bytes, meta.modified().unwrap()),
            );
            if meta.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(path, path, &mut out);
    out
}
#[test]
fn init_refuses_overwrite_and_preserves_existing_excludes() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("new");
    let before = tree(t.path());
    assert!(
        !init(
            &path,
            "Test",
            OperationOptions {
                dry_run: true,
                ..Default::default()
            }
        )
        .unwrap()
        .created
    );
    assert_eq!(before, tree(t.path()));
    fs::create_dir(&path).unwrap();
    fs::create_dir(path.join(".wiki")).unwrap();
    fs::write(path.join(".wiki/.gitignore"), b"# custom\nkeep").unwrap();
    let created = init(&path, "Test", Default::default()).unwrap();
    assert!(created.created);
    assert!(!path.join(".wiki/cache/index.sqlite").exists());
    assert_eq!(
        fs::read(path.join(".wiki/.gitignore")).unwrap(),
        b"# custom\nkeep\ncache/\n"
    );
    let before = tree(&path);
    assert_eq!(
        init(&path, "Overwrite", Default::default())
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(before, tree(&path));
    let next = t.path().join("other");
    assert!(init(&next, "New", Default::default()).unwrap().created);
}
#[test]
fn cli_discovery_precedence_and_exact_ids() {
    let (t, root) = fixture();
    let nested = t.path().join("child");
    fs::create_dir(&nested).unwrap();
    init(&nested, "Nested", Default::default()).unwrap();
    let inside = nested.join("inside");
    fs::create_dir(&inside).unwrap();
    assert_eq!(
        discovery::resolve(None, &inside, false).unwrap().path(),
        nested.canonicalize().unwrap()
    );
    assert_eq!(
        discovery::resolve(Some(t.path()), &inside, false).unwrap(),
        root
    );
    let a = app(&root, false);
    a.page_put(rel("case.md"), page("Opaque.ID", "Body"), None)
        .unwrap();
    assert_eq!(
        a.read(ReadRequest {
            selector: RecordSelector::Id(id("opaque.id")),
            range: None,
            max_bytes: 100
        })
        .unwrap_err()
        .code,
        ErrorCode::RecordNotFound
    );
    assert_eq!(
        a.read(ReadRequest {
            selector: RecordSelector::Id(id("Opaque.ID")),
            range: None,
            max_bytes: 100
        })
        .unwrap()
        .path,
        rel("case.md")
    );
}
#[test]
fn local_config_precedence_and_portable_trust_boundaries() {
    let (t, root) = fixture();
    let marker=fs::read_to_string(t.path().join("WIKI.md")).unwrap().replace("title: Offline","title: Offline\nlwiki_read_max_bytes: 10\nlwiki_lock_timeout_ms: 50\nlwiki_offline: false\nwiki_profile: portable");
    fs::write(t.path().join("WIKI.md"), marker).unwrap();
    let config = t.path().join("private.json");
    fs::write(&config,serde_json::to_vec(&json!({"schema_version":"1","defaults":{"read_max_bytes":20,"offline":false},"vaults":{"Vault.Case":{"read_max_bytes":30,"profile":"local"}},"allowed_profiles":["local"]})).unwrap()).unwrap();
    let f = VaultFs::new(root.clone());
    let options = PreferenceOptions {
        offline: true,
        read_max_bytes: Some(40),
        ..Default::default()
    };
    let p = local::resolve(&f, &id("Vault.Case"), &options, Some(&config)).unwrap();
    assert_eq!(p.read_max_bytes, 40);
    assert!(p.offline);
    assert_eq!(p.lock_timeout_ms, 50);
    assert_eq!(p.profile.as_deref(), Some("local"));
    assert!(p.profile_permitted);
    let p = local::resolve(&f, &id("Vault.Case"), &Default::default(), None).unwrap();
    assert_eq!(p.read_max_bytes, 10);
    assert!(!p.profile_permitted);
    assert!(!p.warnings.is_empty());
    let marker = fs::read_to_string(t.path().join("WIKI.md"))
        .unwrap()
        .replace(
            "title: Offline",
            "title: Offline\nlwiki_endpoint: https://untrusted.invalid",
        );
    fs::write(t.path().join("WIKI.md"), marker).unwrap();
    assert_eq!(
        local::resolve(&f, &id("Vault.Case"), &Default::default(), None)
            .unwrap_err()
            .code,
        ErrorCode::ConfigInvalid
    );
}
#[test]
fn read_limits_utf8_duplicate_identity_and_captured_frontmatter() {
    let (t, root) = fixture();
    let a = app(&root, false);
    a.page_put(rel("unicode.md"), page("read_id", "é猫x"), None)
        .unwrap();
    let r = a
        .read(ReadRequest {
            selector: RecordSelector::Id(id("read_id")),
            range: None,
            max_bytes: 4,
        })
        .unwrap();
    assert_eq!(r.body, "é");
    assert!(r.truncated);
    assert_eq!(
        a.read(ReadRequest {
            selector: RecordSelector::Id(id("read_id")),
            range: Some(ByteSpan::new(1, 5).unwrap()),
            max_bytes: 10
        })
        .unwrap_err()
        .code,
        ErrorCode::Usage
    );
    fs::write(t.path().join("copy.md"), page("read_id", "copy")).unwrap();
    assert_eq!(
        a.read(ReadRequest {
            selector: RecordSelector::Id(id("read_id")),
            range: None,
            max_bytes: 10
        })
        .unwrap_err()
        .code,
        ErrorCode::ReferenceAmbiguous
    );
    let r = a
        .read(ReadRequest {
            selector: RecordSelector::Path(rel("copy.md")),
            range: None,
            max_bytes: 100,
        })
        .unwrap();
    assert!(r.record.is_none());
    assert!(r.metadata.is_some());
    assert!(!r.diagnostics.is_empty());
    fs::remove_file(t.path().join("copy.md")).unwrap();
    a.source_add(request(&page("payload_id", "Payload")))
        .unwrap();
    let p = scan(a.fs(), a.vault_id()).unwrap();
    let payload = p
        .documents
        .iter()
        .find(|d| d.owner_revision.is_some())
        .unwrap();
    let r = a
        .read(ReadRequest {
            selector: RecordSelector::Path(payload.path.clone()),
            range: None,
            max_bytes: 1000,
        })
        .unwrap();
    assert!(r.record.is_none());
    assert!(r.metadata.is_none());
    assert!(r.body.starts_with("---\n"));
    assert_eq!(
        a.read(ReadRequest {
            selector: RecordSelector::Id(id("payload_id")),
            range: None,
            max_bytes: 100
        })
        .unwrap_err()
        .code,
        ErrorCode::RecordNotFound
    );
}
#[test]
fn expected_hash_page_put_rename_and_incoming_links() {
    let (t, root) = fixture();
    let a = app(&root, false);
    let original = String::from_utf8(page("rename_id", "# Original\n"))
        .unwrap()
        .replace("title: rename_id", "title: rename_id\naliases: [Common]")
        .into_bytes();
    a.page_put(rel("old.md"), original.clone(), None).unwrap();
    assert_eq!(
        a.page_put(rel("old.md"), page("rename_id", "changed"), None)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(
        a.page_put(
            rel("old.md"),
            page("rename_id", "changed"),
            Some(Blake3Hash::digest(b"wrong"))
        )
        .unwrap_err()
        .code,
        ErrorCode::ContentConflict
    );
    let other = String::from_utf8(page("other_id", "Other"))
        .unwrap()
        .replace("title: other_id", "title: other_id\naliases: [Common]")
        .into_bytes();
    a.page_put(rel("other.md"), other, None).unwrap();
    let incoming = "[[Common]] [[old.md#Original|Label]] [Inline](old.md#Original \"Title\") [Ref][r]\n\n[r]: old.md#Original \"Reference title\"\n\n`[[old.md]]`\n```\n[[old.md]]\n```\n";
    a.page_put(rel("incoming.md"), page("incoming", incoming), None)
        .unwrap();
    let result = a
        .page_rename(
            id("rename_id"),
            rel("moved/new.md"),
            Blake3Hash::digest(&original),
        )
        .unwrap();
    assert_eq!(result.status, Some(ChangeStatus::Committed));
    assert!(!t.path().join("old.md").exists());
    assert_eq!(
        parse_note(&fs::read(t.path().join("moved/new.md")).unwrap())
            .canonical
            .unwrap()
            .id(),
        &id("rename_id")
    );
    let text = fs::read_to_string(t.path().join("incoming.md")).unwrap();
    assert!(text.contains("[[moved/new.md#Original|Label]]"));
    assert!(text.contains("[[Common]]"));
    assert!(text.contains("[Inline](moved/new.md#Original \"Title\")"));
    assert!(text.contains("[r]: moved/new.md#Original \"Reference title\""));
    assert!(text.contains("`[[old.md]]`"));
    assert!(text.contains("```\n[[old.md]]\n```"));
}
#[test]
fn rename_updates_known_typed_companions_and_refuses_immutable_incoming() {
    let (t, root) = fixture();
    let entity=b"---\nwiki_schema: \"1\"\nwiki_id: entity_a\nwiki_kind: entity\ntitle: A\nwiki_status: active\nwiki_entity_type: concept\n---\n";
    fs::write(t.path().join("entity.md"), entity).unwrap();
    fs::write(t.path().join("assertion.md"),b"---\nwiki_schema: \"1\"\nwiki_id: assertion_a\nwiki_kind: assertion\ntitle: Value\nwiki_status: proposed\nwiki_subject_id: entity_a\nwiki_subject: '[[entity.md]]' # retained\nwiki_predicate: has_property\nwiki_property: value\nwiki_literal_type: string\nwiki_literal_value: yes\n---\n").unwrap();
    let missing = fs::read_to_string(t.path().join("assertion.md"))
        .unwrap()
        .replace("wiki_id: assertion_a", "wiki_id: assertion_missing")
        .replace("[[entity.md]]", "[[missing.md#Anchor|Retained label]]");
    fs::write(t.path().join("missing_companion.md"), missing).unwrap();
    let a = app(&root, false);
    a.page_rename(
        id("entity_a"),
        rel("renamed.md"),
        Blake3Hash::digest(entity),
    )
    .unwrap();
    let text = fs::read_to_string(t.path().join("assertion.md")).unwrap();
    assert!(text.contains("\"[[renamed.md]]\" # retained"));
    assert!(
        fs::read_to_string(t.path().join("missing_companion.md"))
            .unwrap()
            .contains("[[renamed.md#Anchor|Retained label]]")
    );
    fs::write(t.path().join("run.md"),b"---\nwiki_schema: \"1\"\nwiki_id: run_1\nwiki_kind: run\ntitle: Run\nwiki_status: planned\nwiki_created_at: \"2026-09-28T00:00:00Z\"\n---\n").unwrap();
    fs::write(t.path().join("event.md"),b"---\nwiki_schema: \"1\"\nwiki_id: event_1\nwiki_kind: run_event\ntitle: Event\nwiki_run_id: run_1\nwiki_sequence: 1\nwiki_event_type: started\nwiki_occurred_at: \"2026-09-28T00:00:00Z\"\n---\n[[renamed.md]]\n").unwrap();
    let before = tree(t.path());
    assert_eq!(
        a.page_rename(id("entity_a"), rel("again.md"), Blake3Hash::digest(entity))
            .unwrap_err()
            .code,
        ErrorCode::RecordInvalid
    );
    assert_eq!(before, tree(t.path()));
}
#[test]
fn staged_changes_show_apply_abort_and_guarded_inverse() {
    let (t, root) = fixture();
    let staged = OfflineApp::new(
        VaultFs::new(root.clone()),
        OperationOptions {
            stage_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    let change = staged
        .page_put(rel("stage.md"), page("stage_id", "Stage"), None)
        .unwrap()
        .change
        .unwrap();
    assert!(!t.path().join("stage.md").exists());
    let a = app(&root, false);
    assert_eq!(
        a.changes_show(change.change_id.clone()).unwrap().status,
        ChangeStatus::Prepared
    );
    a.changes_apply(change.change_id.clone()).unwrap();
    assert!(t.path().join("stage.md").exists());
    let inverse = a.changes_rollback(change.change_id).unwrap();
    assert_eq!(inverse.status, Some(ChangeStatus::Prepared));
    assert!(t.path().join("stage.md").exists());
    a.changes_apply(inverse.change.unwrap().change_id).unwrap();
    assert!(!t.path().join("stage.md").exists());
    let next = staged
        .page_put(rel("abort.md"), page("abort_id", "Abort"), None)
        .unwrap()
        .change
        .unwrap();
    assert_eq!(
        a.changes_abort(next.change_id).unwrap().status,
        Some(ChangeStatus::Aborted)
    );
    assert!(!t.path().join("abort.md").exists());
    a.recover().unwrap();
}

#[test]
fn source_add_rollback_explains_retained_history_and_withdraws_safely() {
    let (t, root) = fixture();
    let a = app(&root, false);
    let captured = a.source_add(request(b"Captured source\n")).unwrap();
    let source = captured.allocated_ids["source"].clone();
    let change = captured.change.unwrap();
    let source_folder = t.path().join("sources").join(source.as_str());
    let before = tree(&source_folder);
    let error = a.changes_rollback(change.change_id).unwrap_err();
    assert_eq!(error.code, ErrorCode::RecordInvalid);
    assert!(error.message.contains("source withdraw"), "{error:?}");
    assert_eq!(tree(&source_folder), before);
    let revisions = tree(&source_folder.join("revisions"));
    a.source_withdraw(source.clone(), "Retain captured history")
        .unwrap();
    assert_eq!(tree(&source_folder.join("revisions")), revisions);
    assert_eq!(
        scan(a.fs(), a.vault_id()).unwrap().records[&source].eligibility,
        Eligibility::Withdrawn
    );
}

#[test]
fn edited_source_still_conflicts_before_rollback_history_hint() {
    let (t, root) = fixture();
    let a = app(&root, false);
    let captured = a.source_add(request(b"Captured source\n")).unwrap();
    let source = &captured.allocated_ids["source"];
    let note = t
        .path()
        .join("sources")
        .join(source.as_str())
        .join("source.md");
    let mut bytes = fs::read(&note).unwrap();
    bytes.extend_from_slice(b"\nUnfamiliar author edit.\n");
    fs::write(&note, &bytes).unwrap();
    let error = a
        .changes_rollback(captured.change.unwrap().change_id)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert_eq!(fs::read(note).unwrap(), bytes);
}
#[test]
fn source_refresh_withdrawal_and_revalidation_stage_only() {
    let (t, root) = fixture();
    let a = app(&root, false);
    let captured = a.source_add(request(b"Exact quote\n")).unwrap();
    let p = scan(a.fs(), a.vault_id()).unwrap();
    let source = p
        .records
        .values()
        .find(|r| r.record.kind() == RecordKind::Source)
        .unwrap()
        .record
        .id()
        .clone();
    let revision = p
        .records
        .values()
        .find(|r| r.record.kind() == RecordKind::Revision)
        .unwrap()
        .record
        .id()
        .clone();
    assert_eq!(captured.allocated_ids.get("source"), Some(&source));
    assert_eq!(captured.allocated_ids.get("revision"), Some(&revision));
    let reused = a
        .source_refresh(source.clone(), request(b"Exact quote\n"))
        .unwrap();
    assert!(reused.reused);
    assert_eq!(reused.allocated_ids.get("source"), Some(&source));
    assert_eq!(reused.allocated_ids.get("revision"), Some(&revision));
    fs::write(t.path().join("entity.md"),b"---\nwiki_schema: \"1\"\nwiki_id: subject\nwiki_kind: entity\ntitle: Subject\nwiki_status: active\nwiki_entity_type: concept\n---\n").unwrap();
    fs::write(t.path().join("assertion.md"),b"---\nwiki_schema: \"1\"\nwiki_id: claim\nwiki_kind: assertion\ntitle: Claim\nwiki_status: proposed\nwiki_subject_id: subject\nwiki_predicate: has_property\nwiki_property: value\nwiki_literal_type: string\nwiki_literal_value: quote\n---\n").unwrap();
    let store = SourceStore::new(VaultFs::new(root.clone()));
    let plan = store
        .plan_evidence(EvidenceRequest {
            assertion_id: id("claim"),
            source_id: source.clone(),
            revision_id: revision,
            quote: b"Exact quote".to_vec(),
            window: ByteSpan::new(0, 12).unwrap(),
            stance: EvidenceStance::Supports,
            explanation: "Fixture".into(),
            title: "Evidence".into(),
        })
        .unwrap();
    let evidence = plan.evidence_id.clone();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let w =
        lwiki::vault::WriterPermit::acquire(&root, std::time::Duration::from_millis(100)).unwrap();
    let prepared = engine.prepare(&w, plan.draft).unwrap();
    engine
        .apply(
            &w,
            &prepared.prepared,
            &lwiki::catalog::CatalogGraphValidator,
            &lwiki::catalog::Catalog::new(VaultFs::new(root.clone()), a.vault_id().clone()),
        )
        .unwrap();
    drop(w);
    a.source_refresh(source.clone(), request(b"Prefix Exact quote suffix\n"))
        .unwrap();
    let p = scan(a.fs(), a.vault_id()).unwrap();
    let head = id(p.records[&source]
        .record
        .string("wiki_current_revision")
        .unwrap());
    let hash = p.records[&evidence].hash.clone();
    let staged = a
        .evidence_revalidate(evidence.clone(), head, hash.clone())
        .unwrap();
    assert_eq!(staged.status, Some(ChangeStatus::Prepared));
    assert_eq!(
        scan(a.fs(), a.vault_id()).unwrap().records[&evidence].hash,
        hash
    );
    a.changes_abort(staged.change.unwrap().change_id).unwrap();
    a.source_withdraw(source.clone(), "Fixture withdrawal")
        .unwrap();
    assert_eq!(
        scan(a.fs(), a.vault_id()).unwrap().records[&source].eligibility,
        Eligibility::Withdrawn
    );
}
#[test]
fn dry_run_zero_writes_refresh_helpers_dns_http() {
    let (t, root) = fixture();
    let a = app(&root, false);
    let bytes = page("dry_page", "Dry");
    a.page_put(rel("dry.md"), bytes.clone(), None).unwrap();
    a.source_add(request(b"Original")).unwrap();
    let p = scan(a.fs(), a.vault_id()).unwrap();
    let source = p
        .records
        .values()
        .find(|r| r.record.kind() == RecordKind::Source)
        .unwrap()
        .record
        .id()
        .clone();
    let staged = OfflineApp::new(
        VaultFs::new(root.clone()),
        OperationOptions {
            stage_only: true,
            ..Default::default()
        },
    )
    .unwrap()
    .page_put(rel("stage.md"), page("staged", "Stage"), None)
    .unwrap()
    .change
    .unwrap();
    let before = tree(t.path());
    let dry = app(&root, true);
    dry.page_put(rel("new.md"), page("new_page", "new"), None)
        .unwrap();
    dry.page_put(
        rel("dry.md"),
        page("dry_page", "Replacement"),
        Some(Blake3Hash::digest(&bytes)),
    )
    .unwrap();
    dry.page_rename(
        id("dry_page"),
        rel("renamed.md"),
        Blake3Hash::digest(&bytes),
    )
    .unwrap();
    dry.source_add(request(b"New input")).unwrap();
    dry.source_refresh(source.clone(), request(b"Refreshed"))
        .unwrap();
    dry.source_withdraw(source, "Dry withdraw").unwrap();
    dry.changes_show(staged.change_id.clone()).unwrap();
    dry.changes_apply(staged.change_id.clone()).unwrap();
    dry.changes_abort(staged.change_id).unwrap();
    dry.index_sync(false).unwrap();
    dry.index_sync(true).unwrap();
    assert_eq!(dry.doctor().unwrap().cache_state, "unknown");
    dry.check().unwrap();
    dry.recover().unwrap();
    dry.read(ReadRequest {
        selector: RecordSelector::Id(id("dry_page")),
        range: None,
        max_bytes: 100,
    })
    .unwrap();
    assert_eq!(tree(t.path()), before);
    // No HTTP, DNS, provider or credential-helper handle is available to this layer.
}
#[test]
fn schema_migration_is_explicit_and_guarded() {
    let (t, root) = fixture();
    let original=b"\xef\xbb\xbf---\r\nwiki_schema: '0' # keep\r\nwiki_id: legacy\r\nwiki_kind: page\r\ntitle: Legacy\r\nwiki_status: reviewed\r\ncustom: {nested: [one, two]} # unknown\r\n---\r\n# Body\r\nUnchanged bytes\r\n";
    fs::write(t.path().join("legacy.md"), original).unwrap();
    let a = app(&root, false);
    let hash = Blake3Hash::digest(original);
    let read = a
        .read(ReadRequest {
            selector: RecordSelector::Id(id("legacy")),
            range: None,
            max_bytes: 100,
        })
        .unwrap();
    assert!(read.record.is_none());
    assert!(read.metadata.is_some());
    let before = tree(t.path());
    let dry = app(&root, true);
    dry.migrate(RecordSelector::Id(id("legacy")), hash.clone(), "1")
        .unwrap();
    assert_eq!(before, tree(t.path()));
    assert_eq!(
        a.migrate(
            RecordSelector::Path(rel("legacy.md")),
            Blake3Hash::digest(b"wrong"),
            "1"
        )
        .unwrap_err()
        .code,
        ErrorCode::ContentConflict
    );
    let staged = a
        .migrate(RecordSelector::Id(id("legacy")), hash, "1")
        .unwrap();
    assert_eq!(staged.status, Some(ChangeStatus::Prepared));
    assert_eq!(fs::read(t.path().join("legacy.md")).unwrap(), original);
    a.changes_apply(staged.change.unwrap().change_id).unwrap();
    let result = fs::read(t.path().join("legacy.md")).unwrap();
    let expected = String::from_utf8(original.to_vec())
        .unwrap()
        .replace("wiki_schema: '0'", "wiki_schema: \"1\"");
    assert_eq!(result, expected.as_bytes());
    assert_eq!(parse_note(&result).canonical.unwrap().id(), &id("legacy"));
    fs::write(
        t.path().join("future.md"),
        page("future", "Future").as_slice(),
    )
    .unwrap();
    let future = fs::read_to_string(t.path().join("future.md"))
        .unwrap()
        .replace("wiki_schema: \"1\"", "wiki_schema: \"2\"");
    fs::write(t.path().join("future.md"), future.as_bytes()).unwrap();
    assert_eq!(
        a.migrate(
            RecordSelector::Id(id("future")),
            Blake3Hash::digest(future.as_bytes()),
            "1"
        )
        .unwrap_err()
        .code,
        ErrorCode::CapabilityUnavailable
    );
}
#[test]
fn dry_run_missing_cache_does_not_create_lock_or_cache() {
    let (t, root) = fixture();
    let before = tree(t.path());
    let dry = app(&root, true);
    dry.page_put(rel("new.md"), page("new", "New"), None)
        .unwrap();
    dry.index_sync(false).unwrap();
    dry.doctor().unwrap();
    dry.recover().unwrap();
    assert_eq!(before, tree(t.path()));
    assert!(!t.path().join(".wiki").exists());
}

#[test]
fn read_refuses_private_generated_and_original_payload_paths() {
    let (t, root) = fixture();
    let a = app(&root, false);
    a.source_add(request(b"Captured content")).unwrap();
    fs::create_dir_all(t.path().join(".git")).unwrap();
    fs::write(t.path().join(".git/config"), b"private").unwrap();
    fs::write(t.path().join(".wiki/private.json"), b"private").unwrap();
    fs::write(t.path().join("index.md"), b"generated").unwrap();
    let p = scan(a.fs(), a.vault_id()).unwrap();
    let revision = p
        .records
        .values()
        .find(|r| r.record.kind() == RecordKind::Revision)
        .unwrap();
    let parent = revision.path.as_str().rsplit_once('/').unwrap().0;
    for path in [
        ".git/config".to_owned(),
        ".wiki/private.json".to_owned(),
        "index.md".to_owned(),
        format!(
            "{parent}/{}",
            revision.record.string("wiki_original_path").unwrap()
        ),
    ] {
        assert_eq!(
            a.read(ReadRequest {
                selector: RecordSelector::Path(rel(&path)),
                range: None,
                max_bytes: 100
            })
            .unwrap_err()
            .code,
            ErrorCode::RecordNotFound,
            "{path}"
        );
    }
}

#[test]
fn rename_refuses_companion_identity_conflict_without_writes() {
    let (t, root) = fixture();
    for (path, name) in [("a.md", "entity_a"), ("b.md", "entity_b")] {
        fs::write(t.path().join(path),format!("---\nwiki_schema: \"1\"\nwiki_id: {name}\nwiki_kind: entity\ntitle: Entity\nwiki_status: active\nwiki_entity_type: concept\n---\n")).unwrap();
    }
    fs::write(t.path().join("claim.md"),b"---\nwiki_schema: \"1\"\nwiki_id: claim\nwiki_kind: assertion\ntitle: Claim\nwiki_status: proposed\nwiki_subject_id: entity_a\nwiki_subject: '[[b.md]]'\nwiki_predicate: has_property\nwiki_property: value\nwiki_literal_type: string\nwiki_literal_value: yes\n---\n").unwrap();
    let a = app(&root, false);
    let before = tree(t.path());
    let hash = Blake3Hash::digest(fs::read(t.path().join("a.md")).unwrap());
    assert_eq!(
        a.page_rename(id("entity_a"), rel("renamed.md"), hash)
            .unwrap_err()
            .code,
        ErrorCode::RecordInvalid
    );
    assert_eq!(before, tree(t.path()));
}

#[test]
fn changes_show_exact_payloads_detect_corruption_and_bound_aggregate() {
    let (t, root) = fixture();
    let staged = OfflineApp::new(
        VaultFs::new(root.clone()),
        OperationOptions {
            stage_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    let bytes = page("small", "Exact bytes\n");
    let change = staged
        .page_put(rel("small.md"), bytes.clone(), None)
        .unwrap()
        .change
        .unwrap();
    let shown = staged.changes_show(change.change_id.clone()).unwrap();
    assert_eq!(shown.payloads.len(), 1);
    assert!(shown.omitted_payloads.is_empty());
    assert_eq!(shown.payloads[0].proposed, Some(bytes));
    assert!(shown.payloads[0].before.is_none());
    let small_change = change.clone();
    let retained = shown.manifest.operations[0]
        .after_payload
        .as_ref()
        .unwrap()
        .path
        .clone();
    let mut old = page("large", "");
    old.extend(std::iter::repeat_n(b'a', 8 * 1024 * 1024 + 1));
    fs::write(t.path().join("large.md"), &old).unwrap();
    let mut new = page("large", "");
    new.extend(std::iter::repeat_n(b'b', 8 * 1024 * 1024 + 1));
    let change = staged
        .page_put(rel("large.md"), new.clone(), Some(Blake3Hash::digest(&old)))
        .unwrap()
        .change
        .unwrap();
    let shown = staged.changes_show(change.change_id.clone()).unwrap();
    assert!(shown.payloads.is_empty());
    assert_eq!(shown.omitted_payloads, vec![0]);
    let selected = staged.changes_payload(change.change_id, 0).unwrap();
    assert_eq!(selected.before, Some(old));
    assert_eq!(selected.proposed, Some(new));
    fs::write(
        t.path().join(retained.as_str()),
        b"corrupt retained payload",
    )
    .unwrap();
    assert_eq!(
        staged
            .changes_payload(small_change.change_id, 0)
            .unwrap_err()
            .code,
        ErrorCode::RecordInvalid
    );
}
