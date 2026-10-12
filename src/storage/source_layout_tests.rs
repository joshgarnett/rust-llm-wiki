//! Disposable v0.3-style fixtures; no network or owner data.
use super::*;
use crate::{
    app::{OfflineApp, OperationOptions, PageSourceRefs},
    domain::source_presentation::SourcePresentation,
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
};
use std::fs;

const ORIGINAL: &str = "---\r\nsubject: authoritative original\r\n---\r\n# Conditions\r\nThe permit expires on Friday.\r\n";
fn fixture(retained: bool) -> (tempfile::TempDir, OfflineApp) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    for directory in ["pages", "sources", "changes", ".obsidian"] {
        fs::create_dir_all(temp.path().join(directory)).unwrap();
    }
    fs::write(
        temp.path().join(".obsidian/app.json"),
        b"{\"showUnsupportedFiles\":true}\n",
    )
    .unwrap();
    let vault = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    if retained {
        let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
        crate::storage::cleanup(&vault, &writer, &StorageOptions::default()).unwrap();
    }
    let app = OfflineApp::new(
        vault,
        OperationOptions {
            offline: true,
            ..Default::default()
        },
    )
    .unwrap();
    (temp, app)
}
fn request(text: &str) -> CaptureRequest {
    CaptureRequest {
        title: "Field notes # permits".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "author/Permits.md".into(),
        original: text.as_bytes().to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/markdown".into()),
    }
}
fn read(fs: &VaultFs, path: &VaultRelativePath) -> Vec<u8> {
    fs.read_before(path).unwrap().unwrap().bytes
}
fn metadata(fs: &VaultFs, source: &RecordId) -> SourcePresentation {
    let note = parse_note(&read(
        fs,
        &rel(format!("sources/{source}/source.md")).unwrap(),
    ));
    SourcePresentation::from_record(note.canonical.as_ref().unwrap())
        .unwrap()
        .unwrap()
}
fn migrate(app: &OfflineApp) -> SourceLayoutReport {
    let plan = plan_source_layout(app.fs(), &SourceLayoutOptions::default()).unwrap();
    assert!(plan.complete, "{:?}", plan.blockers);
    let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
    migrate_source_layout(
        app.fs(),
        &writer,
        &SourceLayoutOptions {
            expected_plan: Some(plan.plan_hash),
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn migration_preserves_originals_editor_settings_undo_and_authored_links() {
    for retained in [false, true] {
        let (temp, app) = fixture(retained);
        let first = app.source_add(request(ORIGINAL)).unwrap();
        let source = first.allocated_ids["source"].clone();
        let revision = first.allocated_ids["revision"].clone();
        let content = rel(format!("sources/{source}/revisions/{revision}/content.md")).unwrap();
        let source_path = rel(format!("sources/{source}/source.md")).unwrap();
        fs::write(temp.path().join("author.md"), format!("Author prefix\r\n[Heading]({content}#Conditions)\r\n[[{source_path}|Saved source]]\r\nAuthor suffix\r\n")).unwrap();
        let before = super::super::inventory(app.fs(), &StorageOptions::default()).unwrap();
        let originals: BTreeMap<_, _> = before
            .files
            .iter()
            .filter(|file| file.path.as_str() != ".wiki/state/writer.lock")
            .map(|file| {
                (
                    file.path.clone(),
                    fs::read(temp.path().join(file.path.as_str())).unwrap(),
                )
            })
            .collect();
        let report = migrate(&app);
        assert_eq!(report.layout_version, 3);
        assert_eq!(read(app.fs(), &content), ORIGINAL.as_bytes());
        // Follow the published restore instruction in a separate root. The
        // retained vault subtree deliberately has no nested WIKI.md marker.
        let restored = tempfile::tempdir().unwrap();
        let mut restored_marker = None;
        for (path, bytes) in originals {
            let backup = fs::read(
                temp.path()
                    .join(backup_path(&report.operation_id, &path).unwrap().as_str()),
            )
            .unwrap();
            assert_eq!(backup, bytes, "backup {path}");
            if path.as_str() == "WIKI.md" {
                restored_marker = Some(backup);
            } else {
                let destination = restored.path().join(path.as_str());
                fs::create_dir_all(destination.parent().unwrap()).unwrap();
                fs::write(destination, backup).unwrap();
            }
        }
        fs::write(restored.path().join("WIKI.md"), restored_marker.unwrap()).unwrap();
        let restored_fs = VaultFs::new(VaultRoot::explicit(restored.path()).unwrap());
        assert_eq!(
            layout::layout_kind(restored_fs.root()).unwrap(),
            if retained {
                LayoutKind::Retained
            } else {
                LayoutKind::Legacy
            }
        );
        assert_eq!(read(&restored_fs, &content), ORIGINAL.as_bytes());
        assert!(
            SourcePresentation::from_record(
                parse_note(&read(&restored_fs, &source_path))
                    .canonical
                    .as_ref()
                    .unwrap()
            )
            .unwrap()
            .is_none()
        );
        let restored_engine = ChangeEngine::new(restored_fs.clone()).unwrap();
        for change in restored_engine.change_ids().unwrap() {
            restored_engine.inspect_history(&change).unwrap();
        }
        assert_eq!(
            fs::read(temp.path().join(".obsidian/app.json")).unwrap(),
            b"{\"showUnsupportedFiles\":true}\n"
        );
        let presentation = metadata(app.fs(), &source);
        let saved = presentation.published[&revision].as_ref().unwrap();
        assert!(
            String::from_utf8(read(app.fs(), &saved.path))
                .unwrap()
                .contains("# Conditions\r\nThe permit")
        );
        let author = fs::read_to_string(temp.path().join("author.md")).unwrap();
        assert!(author.starts_with("Author prefix\r\n"));
        assert!(author.ends_with("Author suffix\r\n"));
        assert!(author.contains("#Conditions"));
        assert!(!author.contains("/revisions/"));
        assert!(!temp.path().join(source_path.as_str()).exists());
        assert!(
            temp.path()
                .join(".wiki/retained")
                .join(content.as_str())
                .is_file()
        );
        assert!(
            !app.fs()
                .root()
                .scan_markdown()
                .unwrap()
                .iter()
                .any(|path| path.as_str().starts_with("sources/views/"))
        );
        assert!(
            plan_source_layout(app.fs(), &SourceLayoutOptions::default())
                .unwrap()
                .complete
        );
        let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
        crate::storage::cleanup(app.fs(), &writer, &StorageOptions::default()).unwrap();
        assert_eq!(read(app.fs(), &content), ORIGINAL.as_bytes());
    }
}

#[test]
fn migration_preserves_navigation_in_unadopted_index_and_markdown_extensions() {
    let (temp, app) = fixture(false);
    let first = app.source_add(request(ORIGINAL)).unwrap();
    let source = &first.allocated_ids["source"];
    let revision = &first.allocated_ids["revision"];
    let content = rel(format!("sources/{source}/revisions/{revision}/content.md")).unwrap();
    let source_path = format!("sources/{source}/source.md");
    let excluded_frontmatter = format!(
        "---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: {source}\ntitle: Unadopted navigation\nwiki_status: draft\n---\n"
    );
    for name in [
        "index.md",
        "notes/index.md",
        "notes/Guide.MD",
        "notes/Guide.Markdown",
    ] {
        let file = temp.path().join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let prefix = if name.starts_with("notes/") {
            "../"
        } else {
            ""
        };
        let front = if name == "notes/index.md" {
            excluded_frontmatter.as_str()
        } else {
            ""
        };
        fs::write(file, format!("{front}Author intro\r\n[Saved]({prefix}{source_path})\r\n[Heading]({prefix}{content}#Conditions)\r\n[Unknown](unrelated.md#Keep)\r\nAuthor ending\r\n")).unwrap();
    }
    migrate(&app);
    let presentation = metadata(app.fs(), source);
    let saved = presentation.published[revision].as_ref().unwrap();
    for name in [
        "index.md",
        "notes/index.md",
        "notes/Guide.MD",
        "notes/Guide.Markdown",
    ] {
        let file = temp.path().join(name);
        let text = fs::read_to_string(&file).unwrap();
        if name == "notes/index.md" {
            assert!(text.starts_with(&excluded_frontmatter));
        } else {
            assert!(text.starts_with("Author intro\r\n"));
        }
        assert!(text.ends_with("Author ending\r\n"));
        assert!(text.contains("[Unknown](unrelated.md#Keep)\r\n"));
        let base = url::Url::from_file_path(&file).unwrap();
        let links = crate::records::extract_links(&text);
        let current = base.join(&links[0].destination).unwrap();
        assert_eq!(
            fs::canonicalize(current.to_file_path().unwrap()).unwrap(),
            fs::canonicalize(temp.path().join(presentation.current.path.as_str())).unwrap()
        );
        let historical = base.join(&links[1].destination).unwrap();
        assert_eq!(historical.fragment(), Some("Conditions"));
        assert_eq!(
            fs::canonicalize(historical.to_file_path().unwrap()).unwrap(),
            fs::canonicalize(temp.path().join(saved.path.as_str())).unwrap()
        );
        assert!(
            !app.fs()
                .root()
                .scan_markdown()
                .unwrap()
                .contains(&rel(name).unwrap())
        );
    }
    assert_eq!(read(app.fs(), &content), ORIGINAL.as_bytes());
}

#[test]
fn cited_page_migrates_exact_references_then_refresh_keeps_published_capture_route() {
    let (temp, app) = fixture(false);
    let first = app.source_add(request(ORIGINAL)).unwrap();
    let source = first.allocated_ids["source"].clone();
    let revision = first.allocated_ids["revision"].clone();
    let citation = CitationRef::Source(SourceSpanRef {
        source_id: source.clone(),
        source_revision: revision.clone(),
        span: ByteSpan::new(0, ORIGINAL.len() as u64).unwrap(),
        quote_hash: Blake3Hash::digest(ORIGINAL.as_bytes()),
    });
    let references = PageSourceRefs::from_json_slice(
        &serde_json::to_vec(&serde_json::json!({"schema_version":"1", "citations":[citation]}))
            .unwrap(),
    )
    .unwrap();
    let page = rel("pages/Permits.md").unwrap();
    app.page_initialize_with_source_refs(
        Some(page.clone()),
        None,
        "Permits".into(),
        "Keep this authored answer.\n\n```json\n{\"user\":true}\n```\n".into(),
        references.clone(),
    )
    .unwrap();
    migrate(&app);
    let migrated = read(app.fs(), &page);
    assert_eq!(
        crate::app::migration_source_refs(&migrated).unwrap(),
        Some(references)
    );
    assert!(String::from_utf8_lossy(&migrated).contains("```json\n{\"user\":true}\n```"));
    let before = metadata(app.fs(), &source);
    assert!(before.published[&revision].is_none());
    app.source_refresh(
        source.clone(),
        request("# Conditions\nThe permit expires next month.\n"),
    )
    .unwrap();
    let after = metadata(app.fs(), &source);
    assert_eq!(after.current.path, before.current.path);
    let saved = after.published[&revision].as_ref().unwrap();
    assert!(String::from_utf8_lossy(&read(app.fs(), &saved.path)).contains("expires on Friday"));
    assert!(
        String::from_utf8_lossy(&read(app.fs(), &after.current.path))
            .contains(&presentation::capture_anchor(&revision))
    );
    assert_eq!(read(app.fs(), &page), migrated);
    let source_before = read(
        app.fs(),
        &rel(format!("sources/{source}/source.md")).unwrap(),
    );
    fs::write(
        temp.path().join(after.current.path.as_str()),
        b"My edits must survive.\n",
    )
    .unwrap();
    let error = app
        .source_refresh(source.clone(), request("Another update.\n"))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert_eq!(
        read(
            app.fs(),
            &rel(format!("sources/{source}/source.md")).unwrap()
        ),
        source_before
    );
    assert_eq!(
        fs::read(temp.path().join(after.current.path.as_str())).unwrap(),
        b"My edits must survive.\n"
    );
    assert!(
        !plan_source_layout(app.fs(), &SourceLayoutOptions::default())
            .unwrap()
            .complete
    );
}

#[test]
fn exact_pending_migration_replays_each_durable_boundary() {
    for boundary in [
        "receipt",
        "copies",
        "replacements",
        "marker",
        "activation",
        "unlinked",
        "published",
        "completion",
    ] {
        let (_temp, app) = fixture(false);
        let first = app.source_add(request(ORIGINAL)).unwrap();
        let source = first.allocated_ids["source"].clone();
        let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
        let interrupted = with_checkpoint(
            Box::new(move |name, _| {
                if name == boundary {
                    Err(recovery("injected interruption"))
                } else {
                    Ok(())
                }
            }),
            || migrate_source_layout(app.fs(), &writer, &SourceLayoutOptions::default()),
        );
        assert!(interrupted.is_err(), "{boundary}");
        assert_eq!(
            crate::storage::cleanup(app.fs(), &writer, &StorageOptions::default())
                .unwrap_err()
                .code,
            ErrorCode::RecoveryRequired
        );
        assert!(
            layout::raw_read(app.fs().root(), &rel(PENDING).unwrap(), MAX_RECEIPT)
                .unwrap()
                .is_some()
        );
        // Primitive reads are also used by recovery and remain available. Actual
        // ordinary mutation must refuse while the exact migration is pending.
        assert!(
            app.fs()
                .read_before(&rel("WIKI.md").unwrap())
                .unwrap()
                .is_some()
        );
        let blocked = rel("must-not-be-written.md").unwrap();
        assert_eq!(
            app.fs()
                .stage(&blocked, b"blocked", &writer)
                .err()
                .expect("ordinary staging must refuse pending migration")
                .code,
            ErrorCode::RecoveryRequired
        );
        assert!(!app.fs().root().path().join(blocked.as_str()).exists());
        drop(writer);
        let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
        let report = migrate_source_layout(app.fs(), &writer, &SourceLayoutOptions::default())
            .unwrap_or_else(|error| panic!("{boundary}: {error:?}"));
        assert!(report.resumed);
        assert_eq!(metadata(app.fs(), &source).source_id, source);
        assert!(
            layout::raw_read(app.fs().root(), &rel(PENDING).unwrap(), MAX_RECEIPT)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn cleanup_accepts_exact_bootstrap_receipt_but_refuses_unknown_or_changed_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("fresh");
    crate::app::offline::init(
        &root,
        "Fresh saved notes",
        OperationOptions {
            offline: true,
            ..Default::default()
        },
    )
    .unwrap();
    let vault = VaultFs::new(VaultRoot::explicit(&root).unwrap());
    let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
    let report = crate::storage::cleanup(&vault, &writer, &StorageOptions::default()).unwrap();
    assert_eq!(report.layout_version, 3);
    assert_eq!(
        layout::layout_kind(vault.root()).unwrap(),
        LayoutKind::ReadableSources
    );
    let unknown = root.join(".wiki/state/storage/receipts/unknown.plan.json");
    let bytes = b"{\"version\":1,\"bootstrap\":true}";
    fs::write(&unknown, bytes).unwrap();
    assert!(crate::storage::cleanup(&vault, &writer, &StorageOptions::default()).is_err());
    assert_eq!(fs::read(&unknown).unwrap(), bytes);
    fs::remove_file(unknown).unwrap();
    let active: Layout = layout::decode(&fs::read(root.join(layout::ACTIVE)).unwrap()).unwrap();
    let receipt = root.join(archive(&active.migration_id).unwrap().as_str());
    let mut corrupted = fs::read(&receipt).unwrap();
    corrupted.push(b' ');
    fs::write(&receipt, &corrupted).unwrap();
    assert!(crate::storage::cleanup(&vault, &writer, &StorageOptions::default()).is_err());
    assert_eq!(fs::read(receipt).unwrap(), corrupted);
}

#[test]
fn migration_refuses_new_files_before_activation_and_keeps_backup() {
    let (temp, app) = fixture(false);
    app.source_add(request(ORIGINAL)).unwrap();
    let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
    let error = with_checkpoint(
        Box::new(|name, fs| {
            if name == "replacements" {
                std::fs::write(
                    fs.root().path().join("new-author-note.md"),
                    b"Do not lose this note.\n",
                )
                .unwrap();
            }
            Ok(())
        }),
        || migrate_source_layout(app.fs(), &writer, &SourceLayoutOptions::default()),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert_eq!(layout::vault(app.fs().root()).unwrap().1, 1);
    assert_eq!(
        fs::read(temp.path().join("new-author-note.md")).unwrap(),
        b"Do not lose this note.\n"
    );
}

#[test]
fn reserved_collision_and_tiny_bounds_refuse_before_pending_epoch() {
    let (temp, app) = fixture(false);
    app.source_add(request(ORIGINAL)).unwrap();
    fs::create_dir_all(temp.path().join("sources/views")).unwrap();
    fs::write(
        temp.path().join("sources/views/author.md"),
        b"Author file.\n",
    )
    .unwrap();
    assert!(
        !plan_source_layout(app.fs(), &SourceLayoutOptions::default())
            .unwrap()
            .complete
    );
    let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
    assert!(migrate_source_layout(app.fs(), &writer, &SourceLayoutOptions::default()).is_err());
    assert!(
        layout::raw_read(app.fs().root(), &rel(PENDING).unwrap(), MAX_RECEIPT)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fs::read(temp.path().join("sources/views/author.md")).unwrap(),
        b"Author file.\n"
    );
    assert!(
        plan_source_layout(
            app.fs(),
            &SourceLayoutOptions {
                max_files: 1,
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn migration_refuses_oversized_combined_source_and_direct_attachment_links() {
    let (temp, app) = fixture(false);
    let first = app.source_add(request(ORIGINAL)).unwrap();
    let source = &first.allocated_ids["source"];
    let revision = &first.allocated_ids["revision"];
    let revision_path = rel(format!("sources/{source}/revisions/{revision}/revision.md")).unwrap();
    let revision_note = parse_note(&read(app.fs(), &revision_path));
    let original = revision_note
        .canonical
        .as_ref()
        .unwrap()
        .string("wiki_original_path")
        .unwrap();
    fs::write(
        temp.path().join("attachment.md"),
        format!("[Original](sources/{source}/revisions/{revision}/{original})\n"),
    )
    .unwrap();
    let preview = plan_source_layout(app.fs(), &SourceLayoutOptions::default()).unwrap();
    assert!(!preview.complete);
    assert!(
        preview
            .blockers
            .iter()
            .any(|value| value.contains("captured attachment"))
    );
    fs::remove_file(temp.path().join("attachment.md")).unwrap();
    let path = rel(format!("sources/{source}/source.md")).unwrap();
    let mut bytes = read(app.fs(), &path);
    bytes.extend(vec![b'a'; 256 * 1024]);
    fs::write(temp.path().join(path.as_str()), &bytes).unwrap();
    assert_eq!(
        plan_source_layout(app.fs(), &SourceLayoutOptions::default())
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(read(app.fs(), &path), bytes);
    assert!(!temp.path().join(PENDING).exists());
}

#[test]
fn withdrawal_updates_guarded_current_display_and_retains_exact_captured_bytes() {
    let (temp, app) = fixture(false);
    let added = app.source_add(request(ORIGINAL)).unwrap();
    let source = added.allocated_ids["source"].clone();
    let revision = added.allocated_ids["revision"].clone();
    migrate(&app);
    let content = rel(format!("sources/{source}/revisions/{revision}/content.md")).unwrap();
    let display = metadata(app.fs(), &source).current.path;
    app.source_withdraw(source.clone(), "Original instructions were withdrawn")
        .unwrap();
    assert!(
        String::from_utf8_lossy(&read(app.fs(), &display))
            .contains("This source has been withdrawn")
    );
    assert_eq!(read(app.fs(), &content), ORIGINAL.as_bytes());
    let stored = metadata(app.fs(), &source);
    assert_eq!(
        stored.current.generated_hash,
        Blake3Hash::digest(read(app.fs(), &display))
    );
    let fresh = app
        .source_add(request("A second exact saved note.\n"))
        .unwrap();
    let second = fresh.allocated_ids["source"].clone();
    let display = metadata(app.fs(), &second).current.path;
    fs::write(temp.path().join(display.as_str()), b"An author's edit.\n").unwrap();
    assert_eq!(
        app.source_withdraw(second.clone(), "Do not replace author bytes")
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    let head = parse_note(&read(
        app.fs(),
        &rel(format!("sources/{second}/source.md")).unwrap(),
    ))
    .canonical
    .unwrap();
    assert_eq!(head.string("wiki_status"), Some("active"));
}

#[test]
fn recomputed_bootstrap_hash_does_not_authorize_nonempty_or_changed_plan() {
    let marker = include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md");
    let (_after, files) = bootstrap_files(marker).unwrap();
    let receipt = &files[0].1;
    let original: Epoch = layout::decode(receipt).unwrap();
    for mutation in [0, 1, 2] {
        let temp = tempfile::tempdir().unwrap();
        let mut epoch = original.clone();
        match mutation {
            0 => epoch.plan.sources = 1,
            1 => epoch.plan.warnings.push("Not an empty bootstrap".into()),
            _ => epoch.marker_after.push_str("Changed original body.\n"),
        }
        let receipt = layout::encode(&epoch).unwrap();
        let active = layout::encode(&Layout {
            version: 3,
            vault_id: epoch.vault_id.clone(),
            migration_id: epoch.id.clone(),
            migration_hash: Blake3Hash::digest(&receipt),
        })
        .unwrap();
        for (path, bytes) in [
            (files[0].0.clone(), receipt),
            (rel(layout::ACTIVE).unwrap(), active),
            (rel(".wiki/.gitignore").unwrap(), b"cache/\n".to_vec()),
        ] {
            let target = temp.path().join(path.as_str());
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
        }
        let root = VaultRoot::for_initialization(temp.path()).unwrap();
        assert!(resume_bootstrap_marker(&root, None).is_err());
        assert!(!temp.path().join("WIKI.md").exists());
    }
}

// Final migration qualification: authentic local ledgers and native child death.
// The provider fixture below uses an in-memory Transport, never a socket.
#[allow(
    dead_code,
    clippy::duplicate_mod,
    reason = "Independent disposable ledger fixture"
)]
#[path = "../../tests/fixtures/p15/common.rs"]
mod accounting_fixture;
#[allow(
    clippy::duplicate_mod,
    reason = "Independent in-memory embedding transport"
)]
#[path = "../../tests/fixtures/p17/common.rs"]
mod embedding_fixture;
use crate as lwiki;
use crate::{
    catalog::{
        CatalogGraphValidator,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    jobs::{
        AccountingCompleteness, AttemptPhase, BillingDisposition, JobLedger, JobLedgerApi,
        LedgerInspection, RunState,
    },
    retrieval::{render::RenderedUnit, spaces::EmbeddingSettings, vectors::VectorStore},
};
use std::{
    path::Path,
    sync::{Arc, atomic::Ordering},
    time::SystemTime,
};

type QualificationTree = BTreeMap<VaultRelativePath, (Vec<u8>, SystemTime)>;
fn qualification_tree(root: &Path) -> QualificationTree {
    fn visit(root: &Path, directory: &Path, out: &mut QualificationTree, bytes: &mut u64) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            assert!(
                !metadata.file_type().is_symlink(),
                "fixture contains a symlink"
            );
            if metadata.is_dir() {
                visit(root, &entry.path(), out, bytes);
            } else {
                assert!(metadata.is_file());
                *bytes += metadata.len();
                assert!(*bytes <= 64 * 1024 * 1024 && out.len() < 10_000);
                out.insert(
                    rel(entry.path().strip_prefix(root).unwrap().to_str().unwrap()).unwrap(),
                    (
                        fs::read(entry.path()).unwrap(),
                        metadata.modified().unwrap(),
                    ),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out, &mut 0);
    out
}
fn without_writer(mut tree: QualificationTree) -> QualificationTree {
    tree.remove(&rel(".wiki/state/writer.lock").unwrap());
    tree
}
fn qualification_directories(root: &Path) -> BTreeSet<VaultRelativePath> {
    fn visit(root: &Path, directory: &Path, out: &mut BTreeSet<VaultRelativePath>) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            assert!(!metadata.file_type().is_symlink());
            if metadata.is_dir() {
                assert!(out.len() < 10_000);
                out.insert(
                    rel(entry.path().strip_prefix(root).unwrap().to_str().unwrap()).unwrap(),
                );
                visit(root, &entry.path(), out);
            }
        }
    }
    let mut directories = BTreeSet::new();
    visit(root, root, &mut directories);
    directories
}
fn qualification_cli(root: &Path, arguments: &[&str]) -> (serde_json::Value, u8) {
    use clap::Parser;
    let mut argv = vec![
        "lwiki",
        "--no-user-config",
        "--offline",
        "--json",
        "--lock-timeout-ms",
        "1000",
        "--wiki",
        root.to_str().unwrap(),
    ];
    argv.extend_from_slice(arguments);
    let args = crate::cli::Arguments::try_parse_from(argv).unwrap();
    let (envelope, code) = crate::cli::execute(&args);
    let value = serde_json::to_value(envelope).unwrap();
    assert_eq!(value["meta"]["network_used"], false, "{value}");
    (value, code)
}
fn qualification_success(root: &Path, arguments: &[&str]) -> serde_json::Value {
    let (value, code) = qualification_cli(root, arguments);
    assert_eq!(code, 0, "{arguments:?}: {value}");
    assert_eq!(value["ok"], true, "{value}");
    value["data"].clone()
}
fn qualification_ledger(fs: &VaultFs, before: &LedgerInspection) -> JobLedger {
    JobLedger::new(
        fs.clone(),
        before.spec.vault_id.clone(),
        before.spec.run_id.clone(),
        embedding_fixture::options(),
    )
    .unwrap()
}
fn assert_accounting(fs: &VaultFs, before: &LedgerInspection) {
    let history = crate::jobs::checkpoint::storage_run_history(fs, &before.spec.run_id).unwrap();
    let mut replay = crate::jobs::replay_storage_frames(&history.frames).unwrap();
    let mut encoded_journal = Vec::new();
    for frame in &history.frames {
        let encoded = crate::jobs::events::encode(&frame.event).unwrap().1;
        assert!(encoded_journal.len() + encoded.len() <= crate::jobs::JOURNAL_MAX_BYTES as usize);
        encoded_journal.extend_from_slice(&encoded);
    }
    assert_eq!(
        encoded_journal,
        read(
            fs,
            &rel(format!(
                ".wiki/state/jobs/{}/journal.bin",
                before.spec.run_id
            ))
            .unwrap()
        ),
        "complete authenticated frames must reproduce the exact physical journal"
    );
    let journal_bytes = encoded_journal.len() as u64;
    assert_eq!(journal_bytes, before.budget.journal_bytes_used);
    // The frame-only replay API intentionally receives a byte count of zero;
    // that placeholder is not the real journal persistence budget. Supply only
    // the independently verified exact physical length, then compare all fields.
    assert_eq!(replay.budget.journal_bytes_used, 0);
    replay.budget.journal_bytes_used = journal_bytes;
    assert_eq!(replay, *before);
    assert_eq!(
        qualification_ledger(fs, before)
            .inspect_accounting_verified()
            .unwrap(),
        *before
    );
    let status = qualification_success(
        fs.root().path(),
        &["jobs", "status", "--run", before.spec.run_id.as_str()],
    );
    let mut status: LedgerInspection = serde_json::from_value(status).unwrap();
    // Current freshness diagnostics confer no accounting authority. The strict
    // original verifier above must pass without dropping any history proof.
    status.warnings = before.warnings.clone();
    assert_eq!(status, *before);
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalEmbeddingMarker {
    version: u32,
    space: Blake3Hash,
    run_id: RecordId,
    operation: String,
    task_keys: Vec<Blake3Hash>,
    expected_units: Vec<RenderedUnit>,
}
struct TerminalAccounting {
    inspection: LedgerInspection,
    marker_path: VaultRelativePath,
    marker_bytes: Vec<u8>,
    marker: HistoricalEmbeddingMarker,
    vectors: BTreeMap<Blake3Hash, Vec<f32>>,
    responses: Arc<embedding_fixture::Responses>,
}
fn terminal_accounting(app: &OfflineApp, controls: &Path) -> TerminalAccounting {
    let config = controls.join("mock-providers.toml");
    embedding_fixture::private_write(
        &config,
        format!(
            "version=1\n[profiles.primary]\nembedding='embed'\n[services.embed]\nadapter='embeddings-v1'\nurl='https://mock.example/v1/embeddings'\nmodel='test-model'\nrevision='r1'\nmax_batch_items=1\nmax_batch_bytes=262144\n[services.embed.auth]\nkind='static'\nkey='fixture-only'\n[vault_bindings.main]\nroot={}\nwiki_id={}\nallowed_profiles=['primary']\n",
            serde_json::to_string(app.fs().root().path().to_str().unwrap()).unwrap(),
            serde_json::to_string(app.vault_id().as_str()).unwrap()
        ),
    );
    let service = crate::config::providers::ProviderConfig::load(&config)
        .unwrap()
        .authorize(
            app.fs(),
            app.vault_id(),
            "primary",
            crate::jobs::Capability::Embed,
        )
        .unwrap();
    let responses = Arc::new(embedding_fixture::Responses::new());
    let dispatcher = embedding_fixture::dispatcher(app.fs(), responses.clone());
    let mut runtime = embedding_fixture::runtime(&service, &dispatcher);
    runtime.limits.requests = 4;
    let online = OfflineApp::new(app.fs().clone(), OperationOptions::default()).unwrap();
    let report = online
        .embeddings_sync(&EmbeddingSettings::default(), &runtime)
        .unwrap();
    assert!(report.published && report.generated_inputs > 0);
    assert!((1..=4).contains(&responses.calls.load(Ordering::SeqCst)));
    let run_id = report.run_id.unwrap();
    let ledger = JobLedger::new(
        app.fs().clone(),
        app.vault_id().clone(),
        run_id.clone(),
        embedding_fixture::options(),
    )
    .unwrap();
    ledger.complete_run().unwrap();
    let draft = ledger.checkpoint_plan().unwrap();
    let writer = WriterPermit::acquire(app.fs().root(), Duration::from_secs(1)).unwrap();
    let engine = ChangeEngine::new(app.fs().clone()).unwrap();
    let prepared = engine.prepare(&writer, draft).unwrap().prepared;
    engine
        .apply(
            &writer,
            &prepared,
            &CatalogGraphValidator,
            &Catalog::new(app.fs().clone(), app.vault_id().clone()),
        )
        .unwrap();
    drop(writer);
    ledger.checkpoint_committed(&prepared).unwrap();
    let inspection = ledger.inspect_accounting_verified().unwrap();
    assert_eq!(inspection.state, RunState::Completed);
    assert_eq!(
        inspection.completeness,
        AccountingCompleteness::CompleteHistory
    );
    assert!(inspection.budget.dispatched_requests > 0);
    assert!(
        inspection
            .attempts
            .iter()
            .all(|a| a.phase == AttemptPhase::Settled && a.receipt.is_some())
    );
    let markers: Vec<_> = fs::read_dir(app.fs().root().path().join(".wiki/state/embedding-jobs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(markers.len(), 1, "one authentic legacy embedding Run");
    let marker_bytes = fs::read(&markers[0]).unwrap();
    let marker_path = rel(markers[0]
        .strip_prefix(app.fs().root().path())
        .unwrap()
        .to_str()
        .unwrap())
    .unwrap();
    let marker: HistoricalEmbeddingMarker = serde_json::from_slice(&marker_bytes).unwrap();
    assert_eq!(marker.version, 1);
    assert_eq!(marker.run_id, run_id);
    assert_eq!(marker.operation, inspection.spec.scope.operation);
    assert_eq!(
        marker.task_keys,
        inspection
            .spec
            .tasks
            .iter()
            .map(|t| t.key.clone())
            .collect::<Vec<_>>()
    );
    let proof = marker
        .expected_units
        .iter()
        .map(|u| (&u.unit_id, &u.input_hash, &u.dependency_fingerprint))
        .collect::<Vec<_>>();
    assert_eq!(
        inspection.spec.scope.scope_payload_hash,
        Some(Blake3Hash::digest(
            crate::graph::packet::canonical_json(&proof).unwrap()
        ))
    );
    let store = VectorStore::open(app.fs(), None).unwrap();
    let vectors = marker
        .expected_units
        .iter()
        .map(|unit| {
            (
                unit.input_hash.clone(),
                store
                    .vector(&marker.space, &unit.input_hash)
                    .unwrap()
                    .unwrap(),
            )
        })
        .collect();
    drop(store);
    assert_accounting(app.fs(), &inspection);
    TerminalAccounting {
        inspection,
        marker_path,
        marker_bytes,
        marker,
        vectors,
        responses,
    }
}
fn historical_file(path: &VaultRelativePath) -> bool {
    let s = path.as_str();
    s.starts_with("runs/")
        || s.starts_with("changes/")
        || s.starts_with(".wiki/retained/runs/")
        || s.starts_with(".wiki/retained/changes/")
        || s.starts_with(".wiki/state/jobs/")
        || s.starts_with(".wiki/state/embedding-")
}
fn assert_history_bytes(fs: &VaultFs, original: &QualificationTree) {
    for (path, (bytes, _)) in original.iter().filter(|(path, _)| historical_file(path)) {
        assert_eq!(&read(fs, path), bytes, "retained accounting/history {path}");
    }
}
fn historical_keys(tree: &QualificationTree, kind: LayoutKind) -> BTreeSet<VaultRelativePath> {
    let mut keys = BTreeSet::new();
    for path in tree.keys().filter(|path| historical_file(path)) {
        let logical = layout::logical_path_for(kind, path).unwrap_or_else(|| path.clone());
        assert!(
            keys.insert(logical),
            "duplicate physical historical aliases: {path}"
        );
    }
    keys
}
fn restored_backup(
    app: &OfflineApp,
    report: &SourceLayoutReport,
    before: &QualificationTree,
) -> tempfile::TempDir {
    let restored = tempfile::tempdir().unwrap();
    for (path, (bytes, _)) in before {
        if path.as_str() == ".wiki/state/writer.lock" {
            continue;
        }
        let backup = fs::read(
            app.fs()
                .root()
                .path()
                .join(backup_path(&report.operation_id, path).unwrap().as_str()),
        )
        .unwrap();
        assert_eq!(&backup, bytes, "exact backup {path}");
        let destination = restored.path().join(path.as_str());
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(destination, backup).unwrap();
    }
    restored
}
#[test]
fn terminal_accounting_and_embedding_history_survive_migration_and_restore() {
    let (_temp, app) = fixture(false);
    fs::write(
        app.fs().root().path().join("author.md"),
        b"Author note: keep these exact CRLF bytes.\r\n",
    )
    .unwrap();
    let source = app.source_add(request(ORIGINAL)).unwrap().allocated_ids["source"].clone();
    let controls = tempfile::tempdir().unwrap();
    let terminal = terminal_accounting(&app, controls.path());
    let before = qualification_tree(app.fs().root().path());
    let preview =
        qualification_success(app.fs().root().path(), &["storage", "migrate", "--dry-run"]);
    assert_eq!(preview["complete"], true, "{preview}");
    assert_eq!(qualification_tree(app.fs().root().path()), before);
    let data = qualification_success(
        app.fs().root().path(),
        &[
            "storage",
            "migrate",
            "--expected-plan",
            preview["plan_hash"].as_str().unwrap(),
        ],
    );
    let report: SourceLayoutReport = serde_json::from_value(data).unwrap();
    assert_eq!(report.layout_version, 3);
    assert!(!app.fs().root().path().join(PENDING).exists());
    assert_eq!(
        historical_keys(
            &qualification_tree(app.fs().root().path()),
            LayoutKind::ReadableSources
        ),
        historical_keys(&before, LayoutKind::Legacy),
        "migration must not invent accounting files or history"
    );
    assert_history_bytes(app.fs(), &before);
    assert_accounting(app.fs(), &terminal.inspection);
    assert_eq!(read(app.fs(), &terminal.marker_path), terminal.marker_bytes);
    let restored = restored_backup(&app, &report, &before);
    let restored_fs = VaultFs::new(VaultRoot::explicit(restored.path()).unwrap());
    assert_eq!(
        layout::layout_kind(restored_fs.root()).unwrap(),
        LayoutKind::Legacy
    );
    assert_history_bytes(&restored_fs, &before);
    assert_accounting(&restored_fs, &terminal.inspection);
    assert_eq!(
        read(&restored_fs, &terminal.marker_path),
        terminal.marker_bytes
    );
    let before_cache_sync = qualification_tree(app.fs().root().path());
    let dispatched = terminal.responses.calls.load(Ordering::SeqCst);
    let cache = qualification_success(app.fs().root().path(), &["embeddings", "sync"]);
    eprintln!("migration cache-only report: {cache}");
    assert_eq!(cache["generated_inputs"], 0);
    assert_eq!(cache["network_used"], false);
    assert!(cache["reused_inputs"].as_u64().unwrap() > 0, "{cache}");
    assert_eq!(
        cache["space"],
        serde_json::to_value(&terminal.marker.space).unwrap()
    );
    let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let policy = crate::retrieval::unit_inventory_types::RenderPolicyId::for_settings(
        &reader.snapshot().parser_fingerprint,
        &EmbeddingSettings::default(),
    )
    .unwrap();
    let descriptors = reader.unit_descriptors_page(&policy, None, 128).unwrap();
    let saved_inputs: BTreeSet<_> = terminal
        .marker
        .expected_units
        .iter()
        .filter(|unit| {
            unit.owner
                .as_str()
                .starts_with(&format!("sources/{source}/revisions/"))
        })
        .map(|u| u.input_hash.clone())
        .collect();
    assert!(
        !saved_inputs.is_empty(),
        "real captured-text embedding input"
    );
    let current_saved_inputs: BTreeSet<_> = descriptors
        .iter()
        .filter(|unit| saved_inputs.contains(&unit.input_hash))
        .map(|unit| unit.input_hash.clone())
        .collect();
    assert_eq!(
        current_saved_inputs, saved_inputs,
        "every unchanged captured-text input must remain current and compatible: {cache}"
    );
    let store = VectorStore::open(app.fs(), None).unwrap();
    for owner in descriptors
        .iter()
        .filter(|unit| saved_inputs.contains(&unit.input_hash))
        .map(|unit| &unit.owner)
        .collect::<BTreeSet<_>>()
    {
        let binding = reader
            .unit_owner_binding(&policy, owner)
            .unwrap()
            .expect("current captured-text owner binding");
        assert!(
            store
                .owner_binding_ready(&terminal.marker.space, &binding)
                .unwrap(),
            "compatible saved text must have acknowledged current membership, not just a retained vector: {owner}; {cache}"
        );
    }
    for (input, vector) in &terminal.vectors {
        assert_eq!(
            store
                .vector(&terminal.marker.space, input)
                .unwrap()
                .as_ref(),
            Some(vector)
        );
    }
    drop(store);
    drop(reader);
    assert_history_bytes(app.fs(), &before_cache_sync);
    assert_accounting(app.fs(), &terminal.inspection);
    assert_eq!(
        qualification_tree(app.fs().root().path())
            .keys()
            .filter(|p| historical_file(p))
            .cloned()
            .collect::<BTreeSet<_>>(),
        before_cache_sync
            .keys()
            .filter(|p| historical_file(p))
            .cloned()
            .collect()
    );
    assert_eq!(read(app.fs(), &terminal.marker_path), terminal.marker_bytes);
    assert_eq!(terminal.responses.calls.load(Ordering::SeqCst), dispatched);
}

#[test]
fn active_unknown_charge_guard_refuses_migration_without_changes() {
    let (_temp, app) = fixture(false);
    fs::write(
        app.fs().root().path().join("author.md"),
        b"Author note: keep these exact CRLF bytes.\r\n",
    )
    .unwrap();
    app.source_add(request(ORIGINAL)).unwrap();
    let mut spec = accounting_fixture::spec(app.fs(), "run_migration_unknown", 1);
    spec.vault_id = app.vault_id().clone();
    let marker = rel("WIKI.md").unwrap();
    spec.scope.read_preconditions = vec![crate::changes::ReadDependency {
        path: marker.clone(),
        expected: ExpectedState::Hash(Blake3Hash::digest(read(app.fs(), &marker))),
    }];
    spec.input_fingerprint = crate::jobs::tasks::input_fingerprint(&spec).unwrap();
    let clock = Arc::new(accounting_fixture::Clock(
        std::sync::atomic::AtomicI64::new(spec.created_at_utc_ms),
    ));
    let job = JobLedger::new(
        app.fs().clone(),
        app.vault_id().clone(),
        spec.run_id.clone(),
        accounting_fixture::options(clock.clone()),
    )
    .unwrap();
    let writer = WriterPermit::acquire(app.fs().root(), Duration::from_secs(1)).unwrap();
    job.create(&writer, spec.clone()).unwrap();
    drop(writer);
    job.start().unwrap();
    let reserved = job
        .reserve(
            &spec.tasks[0].key,
            accounting_fixture::bound(&spec.tasks[0]),
        )
        .unwrap();
    drop(job.dispatch_intent(reserved).unwrap());
    let inspection = job.inspect_accounting_verified().unwrap();
    assert_eq!(inspection.state, RunState::Running);
    assert_eq!(inspection.attempts[0].phase, AttemptPhase::DispatchIntent);
    assert_eq!(
        inspection.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
    assert_eq!(inspection.budget.remote_inflight, 1);
    assert_eq!(inspection.budget.dispatched_requests, 1);
    assert_eq!(
        inspection.completeness,
        AccountingCompleteness::CompleteHistory
    );
    let before = qualification_tree(app.fs().root().path());
    let directories_before = qualification_directories(app.fs().root().path());
    let preview =
        qualification_success(app.fs().root().path(), &["storage", "migrate", "--dry-run"]);
    assert_eq!(preview["complete"], false);
    let blockers = preview["blockers"].to_string();
    assert!(blockers.contains(spec.run_id.as_str()) && blockers.contains("WIKI.md"));
    assert_eq!(qualification_tree(app.fs().root().path()), before);
    assert_eq!(
        qualification_directories(app.fs().root().path()),
        directories_before
    );
    let (refusal, code) = qualification_cli(app.fs().root().path(), &["storage", "migrate"]);
    assert_ne!(code, 0);
    assert_eq!(refusal["error"]["code"], "RECOVERY_REQUIRED");
    assert!(
        refusal["error"]["message"]
            .as_str()
            .unwrap()
            .contains(spec.run_id.as_str())
    );
    assert!(
        refusal["error"]["message"]
            .as_str()
            .unwrap()
            .contains("WIKI.md")
    );
    assert_eq!(
        without_writer(qualification_tree(app.fs().root().path())),
        without_writer(before)
    );
    assert_eq!(
        qualification_directories(app.fs().root().path()),
        directories_before,
        "refused migration must not leave even empty backup directories"
    );
    assert_eq!(
        layout::layout_kind(app.fs().root()).unwrap(),
        LayoutKind::Legacy
    );
    assert!(!app.fs().root().path().join(PENDING).exists());
    let reopened = JobLedger::new(
        app.fs().clone(),
        app.vault_id().clone(),
        spec.run_id,
        accounting_fixture::options(clock),
    )
    .unwrap();
    assert_eq!(reopened.inspect_accounting_verified().unwrap(), inspection);
    assert_accounting(app.fs(), &inspection);
}

#[cfg(unix)]
mod native_qualification {
    use super::*;
    use crate::vault::{DirectorySync, DurableIo, NativeIo};
    use std::{
        io,
        os::unix::fs::MetadataExt,
        path::PathBuf,
        process::{Child, Command, Stdio},
        sync::{Mutex, atomic::AtomicBool},
        time::Instant,
    };

    const CHILD: &str =
        "storage::source_layout::tests::native_qualification::source_layout_native_child";
    #[derive(Clone, Copy)]
    enum SyncAction {
        Pause,
        Error,
        Unsupported,
    }
    struct MigrationIo {
        target: PathBuf,
        controls: PathBuf,
        action: SyncAction,
        replaced: AtomicBool,
        synced_files: Mutex<BTreeSet<(u64, u64)>>,
        events: Mutex<Vec<&'static str>>,
    }
    impl MigrationIo {
        fn new(root: &Path, controls: &Path, target: &str, action: SyncAction) -> Self {
            Self {
                target: root.join(target),
                controls: controls.to_path_buf(),
                action,
                replaced: AtomicBool::new(false),
                synced_files: Mutex::new(BTreeSet::new()),
                events: Mutex::new(Vec::new()),
            }
        }
    }
    fn pause(controls: &Path, boundary: &str) {
        fs::write(
            controls.join("ready.tmp"),
            serde_json::to_vec(&serde_json::json!({"pid":std::process::id(),"boundary":boundary}))
                .unwrap(),
        )
        .unwrap();
        fs::rename(controls.join("ready.tmp"), controls.join("ready.json")).unwrap();
        loop {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    impl DurableIo for MigrationIo {
        fn create_stage(&self, path: &Path) -> io::Result<fs::File> {
            NativeIo.create_stage(path)
        }
        fn create_private_stage(&self, path: &Path) -> io::Result<fs::File> {
            NativeIo.create_private_stage(path)
        }
        fn create_private_directory(&self, path: &Path) -> io::Result<()> {
            NativeIo.create_private_directory(path)
        }
        fn open_append(&self, path: &Path) -> io::Result<fs::File> {
            NativeIo.open_append(path)
        }
        fn truncate_file(&self, file: &fs::File, length: u64) -> io::Result<()> {
            NativeIo.truncate_file(file, length)
        }
        fn write_stage(&self, file: &mut fs::File, bytes: &[u8]) -> io::Result<()> {
            NativeIo.write_stage(file, bytes)
        }
        fn sync_file(&self, file: &fs::File) -> io::Result<()> {
            NativeIo.sync_file(file)?;
            let metadata = file.metadata()?;
            self.synced_files
                .lock()
                .unwrap()
                .insert((metadata.dev(), metadata.ino()));
            Ok(())
        }
        fn replace(&self, staged: &Path, target: &Path) -> io::Result<()> {
            if target == self.target {
                let metadata = fs::metadata(staged)?;
                assert!(
                    self.synced_files
                        .lock()
                        .unwrap()
                        .contains(&(metadata.dev(), metadata.ino())),
                    "authority stage must be file-synced before rename"
                );
                self.events.lock().unwrap().push("file_synced");
            }
            NativeIo.replace(staged, target)?;
            if target == self.target {
                self.events.lock().unwrap().push("renamed");
                self.replaced.store(true, Ordering::SeqCst);
                if matches!(self.action, SyncAction::Pause) {
                    pause(
                        &self.controls,
                        target.file_name().unwrap().to_str().unwrap(),
                    );
                }
            }
            Ok(())
        }
        fn remove(&self, path: &Path) -> io::Result<()> {
            NativeIo.remove(path)
        }
        fn remove_directory(&self, path: &Path) -> io::Result<()> {
            NativeIo.remove_directory(path)
        }
        fn create_directory(&self, path: &Path) -> io::Result<()> {
            NativeIo.create_directory(path)
        }
        fn sync_directory(&self, path: &Path) -> io::Result<DirectorySync> {
            if path == self.target.parent().unwrap() && self.replaced.swap(false, Ordering::SeqCst)
            {
                self.events.lock().unwrap().push("parent_sync");
                return match self.action {
                    SyncAction::Error => Err(io::Error::other("qualification parent sync failure")),
                    SyncAction::Unsupported => Ok(DirectorySync::Unsupported),
                    SyncAction::Pause => NativeIo.sync_directory(path),
                };
            }
            NativeIo.sync_directory(path)
        }
    }
    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if matches!(self.0.try_wait(), Ok(None)) {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }
    fn child(root: &Path, controls: &Path, mode: &str) -> ChildGuard {
        let log = fs::File::create(controls.join(format!("{mode}.log"))).unwrap();
        ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CHILD, "--ignored", "--nocapture"])
                .env("LWIKI_SOURCE_LAYOUT_ROOT", root)
                .env("LWIKI_SOURCE_LAYOUT_CONTROL", controls)
                .env("LWIKI_SOURCE_LAYOUT_MODE", mode)
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap(),
        )
    }
    fn retry(root: &Path, controls: &Path) -> SourceLayoutReport {
        let mut process = child(root, controls, "retry");
        let start = Instant::now();
        loop {
            if let Some(status) = process.0.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "retry failed: {}",
                    fs::read_to_string(controls.join("retry.log")).unwrap()
                );
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "retry exceeded native test deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        serde_json::from_slice(&fs::read(controls.join("result.json")).unwrap()).unwrap()
    }
    #[test]
    #[ignore = "explicit native source-layout subprocess helper; no production environment hooks"]
    fn source_layout_native_child() {
        let root = PathBuf::from(std::env::var_os("LWIKI_SOURCE_LAYOUT_ROOT").unwrap());
        let controls = PathBuf::from(std::env::var_os("LWIKI_SOURCE_LAYOUT_CONTROL").unwrap());
        assert!(!controls.starts_with(&root));
        let mode = std::env::var("LWIKI_SOURCE_LAYOUT_MODE").unwrap();
        if mode == "retry" {
            let data = qualification_success(&root, &["storage", "migrate"]);
            fs::write(
                controls.join("result.json"),
                serde_json::to_vec(&data).unwrap(),
            )
            .unwrap();
            return;
        }
        let vault_root = VaultRoot::explicit(&root).unwrap();
        let vault = match mode.as_str() {
            "marker" => VaultFs::with_io(
                vault_root,
                Arc::new(MigrationIo::new(
                    &root,
                    &controls,
                    "WIKI.md",
                    SyncAction::Pause,
                )),
            ),
            "activation" => VaultFs::with_io(
                vault_root,
                Arc::new(MigrationIo::new(
                    &root,
                    &controls,
                    layout::ACTIVE,
                    SyncAction::Pause,
                )),
            ),
            "receipt" | "published" => VaultFs::new(vault_root),
            _ => panic!("unknown native migration mode"),
        };
        let writer = WriterPermit::acquire(vault.root(), Duration::from_secs(1)).unwrap();
        with_checkpoint(
            Box::new(move |name, _| {
                if name == mode {
                    pause(&controls, name);
                }
                Ok(())
            }),
            || migrate_source_layout(&vault, &writer, &SourceLayoutOptions::default()),
        )
        .unwrap();
        panic!("native migration boundary was not reached");
    }
    fn assert_completed(
        app: &OfflineApp,
        source: &RecordId,
        report: &SourceLayoutReport,
        before: &QualificationTree,
    ) {
        assert!(report.resumed);
        assert_eq!(report.layout_version, 3);
        assert_eq!(
            layout::layout_kind(app.fs().root()).unwrap(),
            LayoutKind::ReadableSources
        );
        assert!(!app.fs().root().path().join(PENDING).exists());
        assert!(
            Catalog::new(app.fs().clone(), app.vault_id().clone())
                .operation_state()
                .unwrap()
                .is_some()
        );
        let presentation = metadata(app.fs(), source);
        assert!(
            String::from_utf8(read(app.fs(), &presentation.current.path))
                .unwrap()
                .contains("The permit expires on Friday.")
        );
        assert_history_bytes(app.fs(), before);
        for (path, (bytes, _)) in before.iter().filter(|(p, _)| {
            p.as_str().contains("/revisions/")
                || p.as_str().starts_with(".obsidian/")
                || p.as_str() == "author.md"
        }) {
            assert_eq!(
                &read(app.fs(), path),
                bytes,
                "immutable/editor preservation {path}"
            );
        }
        let restored = restored_backup(app, report, before);
        assert_eq!(
            layout::layout_kind(&VaultRoot::explicit(restored.path()).unwrap()).unwrap(),
            LayoutKind::Legacy
        );
    }
    #[test]
    fn native_migration_kills_resume_exact_epoch() {
        for boundary in ["receipt", "marker", "activation", "published"] {
            let (_temp, app) = fixture(false);
            fs::write(
                app.fs().root().path().join("author.md"),
                b"Author note: keep these exact CRLF bytes.\r\n",
            )
            .unwrap();
            let source = app.source_add(request(ORIGINAL)).unwrap().allocated_ids["source"].clone();
            let controls = tempfile::tempdir().unwrap();
            // One actual accounting-bearing kill, not a hand-written ledger.
            let terminal =
                (boundary == "published").then(|| terminal_accounting(&app, controls.path()));
            let before = qualification_tree(app.fs().root().path());
            let mut process = child(app.fs().root().path(), controls.path(), boundary);
            let start = Instant::now();
            while !controls.path().join("ready.json").exists() {
                assert!(
                    start.elapsed() < Duration::from_secs(15),
                    "{boundary}: ready deadline"
                );
                assert!(
                    process.0.try_wait().unwrap().is_none(),
                    "{boundary}: exited before boundary: {}",
                    fs::read_to_string(controls.path().join(format!("{boundary}.log"))).unwrap()
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let ready: serde_json::Value =
                serde_json::from_slice(&fs::read(controls.path().join("ready.json")).unwrap())
                    .unwrap();
            assert_eq!(ready["pid"], process.0.id());
            assert_eq!(
                ready["boundary"],
                match boundary {
                    "marker" => "WIKI.md",
                    "activation" => "layout.json",
                    other => other,
                }
            );
            process.0.kill().unwrap();
            assert!(!process.0.wait().unwrap().success());
            let pending_bytes = fs::read(app.fs().root().path().join(PENDING)).unwrap();
            let pending: Epoch = layout::decode(&pending_bytes).unwrap();
            validate_epoch(&pending).unwrap();
            let report = retry(app.fs().root().path(), controls.path());
            assert_eq!(report.operation_id, pending.id, "same exact retained epoch");
            assert_completed(&app, &source, &report, &before);
            eprintln!(
                "native migration kill boundary={boundary}, child_pid={}, operation={}, resumed={}",
                process.0.id(),
                report.operation_id,
                report.resumed
            );
            if let Some(terminal) = &terminal {
                assert_accounting(app.fs(), &terminal.inspection);
            }
        }
    }
    #[test]
    fn native_migration_sync_failures_preserve_pending_and_retry() {
        for target in ["WIKI.md", layout::ACTIVE] {
            for action in [SyncAction::Error, SyncAction::Unsupported] {
                let (_temp, app) = fixture(false);
                fs::write(
                    app.fs().root().path().join("author.md"),
                    b"Author note: keep these exact CRLF bytes.\r\n",
                )
                .unwrap();
                let source =
                    app.source_add(request(ORIGINAL)).unwrap().allocated_ids["source"].clone();
                let controls = tempfile::tempdir().unwrap();
                let before = qualification_tree(app.fs().root().path());
                let io = Arc::new(MigrationIo::new(
                    app.fs().root().path(),
                    controls.path(),
                    target,
                    action,
                ));
                let faulty = VaultFs::with_io(app.fs().root().clone(), io.clone());
                let writer =
                    WriterPermit::acquire(app.fs().root(), Duration::from_secs(1)).unwrap();
                let error =
                    migrate_source_layout(&faulty, &writer, &SourceLayoutOptions::default())
                        .unwrap_err();
                assert_eq!(
                    *io.events.lock().unwrap(),
                    ["file_synced", "renamed", "parent_sync"]
                );
                match action {
                    SyncAction::Error => assert!(
                        error.message.contains("qualification parent sync failure"),
                        "{error:?}"
                    ),
                    SyncAction::Unsupported => {
                        assert_eq!(error.code, ErrorCode::CapabilityUnavailable)
                    }
                    SyncAction::Pause => unreachable!(),
                }
                if target == layout::ACTIVE {
                    assert_eq!(error.details["migration_committed"], true);
                }
                assert!(
                    error
                        .hint
                        .as_deref()
                        .is_some_and(|hint| hint.contains("storage migrate"))
                );
                let pending: Epoch =
                    layout::decode(&fs::read(app.fs().root().path().join(PENDING)).unwrap())
                        .unwrap();
                validate_epoch(&pending).unwrap();
                drop(writer);
                let report = retry(app.fs().root().path(), controls.path());
                assert_eq!(report.operation_id, pending.id);
                assert_completed(&app, &source, &report, &before);
                assert_eq!(
                    NativeIo.sync_directory(app.fs().root().path()).unwrap(),
                    DirectorySync::Supported
                );
            }
        }
    }
}

#[test]
#[ignore = "explicit bounded disposable export for pinned old/current CLI accounting replay"]
fn export_source_layout_accounting_fixture() {
    let destination = std::path::PathBuf::from(
        std::env::var_os("LWIKI_SOURCE_LAYOUT_EXPORT_DIR").expect("explicit new export directory"),
    );
    assert!(destination.is_absolute());
    // Absent-only destination: this helper never adopts or replaces owner data.
    fs::create_dir(&destination).unwrap();
    let (_temp, app) = fixture(false);
    fs::write(
        app.fs().root().path().join("author.md"),
        b"Author note: keep these exact CRLF bytes.\r\n",
    )
    .unwrap();
    assert!(!destination.starts_with(app.fs().root().path()));
    app.source_add(request(ORIGINAL)).unwrap();
    let controls = tempfile::tempdir().unwrap();
    let terminal = terminal_accounting(&app, controls.path());
    let before = qualification_tree(app.fs().root().path());
    let baseline = destination.join("legacy");
    embedding_fixture::copy(app.fs().root().path(), &baseline);
    let data = qualification_success(app.fs().root().path(), &["storage", "migrate"]);
    let report: SourceLayoutReport = serde_json::from_value(data).unwrap();
    assert_accounting(app.fs(), &terminal.inspection);
    let restored = restored_backup(&app, &report, &before);
    embedding_fixture::copy(restored.path(), &destination.join("restored"));
    embedding_fixture::copy(app.fs().root().path(), &destination.join("migrated"));
    let manifest: BTreeMap<_, _> = before
        .iter()
        .map(|(path, (bytes, _))| {
            (
                path.as_str(),
                serde_json::json!({"bytes":bytes.len(), "blake3":Blake3Hash::digest(bytes)}),
            )
        })
        .collect();
    fs::write(
        destination.join("accounting.json"),
        serde_json::to_vec_pretty(&terminal.inspection).unwrap(),
    )
    .unwrap();
    fs::write(
        destination.join("before-manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(
        destination.join("migration.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!(
        "disposable accounting fixture exported to {}; run={}",
        destination.display(),
        terminal.inspection.spec.run_id
    );
}
