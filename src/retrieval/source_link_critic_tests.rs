//! Independently authored navigation acceptance cases. Root registers this sibling module.
use super::{
    ContextBudget, ContextCheckpoint, ContextFault, ContextOptions, ContextRequest, ContextScope,
    SearchMode,
    context_selection_packet::{SelectionAction, SelectionReply},
    verification,
};
use crate::{
    app::{OfflineApp, OperationOptions, SourceCaptureRequest, SourceReadRequest, offline::init},
    catalog::Catalog,
    domain::RecordId,
    vault::{VaultFs, VaultRoot},
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

const NOTE: &str =
    "criticnavigation: cobalt opens the north cabinet; silver opens the south cabinet.\n";

fn fixture(normalized: bool) -> (tempfile::TempDir, OfflineApp, RecordId) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("Separate wiki é %23 # (links)");
    let options = OperationOptions {
        offline: true,
        ..Default::default()
    };
    init(&root, "Critic notes", options).unwrap();
    let app = OfflineApp::new(VaultFs::new(VaultRoot::explicit(root).unwrap()), options).unwrap();
    if normalized {
        app.index_rebuild_normalized().unwrap();
    }
    let original = temp.path().join("Navigation [cabinets].md");
    fs::write(&original, NOTE).unwrap();
    let capture = app
        .capture_sources(SourceCaptureRequest {
            paths: vec![original],
            files_from: None,
            markdown_only: true,
        })
        .unwrap();
    let source = capture.items[0].source_id.clone().unwrap();
    (temp, app, source)
}
fn read(app: &OfflineApp, source: &RecordId) -> crate::app::SourceReadOutcome {
    app.read_source(SourceReadRequest {
        source_id: source.clone(),
        revision_id: None,
        range: None,
        from: None,
        max_bytes: 1024,
    })
    .unwrap()
}
fn destination(markdown: &str) -> PathBuf {
    let encoded = markdown
        .rsplit_once("](<")
        .unwrap()
        .1
        .strip_suffix(">)")
        .unwrap();
    let mut bytes = Vec::new();
    let mut remaining = encoded.as_bytes();
    while let Some((&first, rest)) = remaining.split_first() {
        if first == b'%' {
            bytes.push(u8::from_str_radix(std::str::from_utf8(&rest[..2]).unwrap(), 16).unwrap());
            remaining = &rest[2..];
        } else {
            bytes.push(first);
            remaining = rest;
        }
    }
    PathBuf::from(String::from_utf8(bytes).unwrap())
}
fn request(scope: ContextScope) -> ContextRequest {
    let mut value = ContextRequest {
        scope,
        budget: ContextBudget {
            max_bytes: 6000,
            max_tokens: 1500,
            ..Default::default()
        },
        ..Default::default()
    };
    value.documents.mode = SearchMode::Lexical;
    value.verification_budget.max_elapsed_ms = 30_000;
    value
}
fn catalog(app: &OfflineApp) -> Catalog {
    Catalog::new(app.fs().clone(), app.vault_id().clone())
}

// Every path, content digest, mtime and mode is retained. Unix also records file
// identity. Atime is absent because reads may legitimately update it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SnapshotEntry {
    directory: bool,
    length: u64,
    content: Option<crate::domain::Blake3Hash>,
    modified: SystemTime,
    read_only: bool,
    #[cfg(unix)]
    identity_and_mode: (u64, u64, u32),
}
type Tree = BTreeMap<PathBuf, SnapshotEntry>;
fn snapshot(root: &Path) -> Tree {
    fn visit(root: &Path, path: &Path, out: &mut Tree) {
        let meta = fs::symlink_metadata(path).unwrap();
        assert!(!meta.file_type().is_symlink());
        #[cfg(unix)]
        let identity_and_mode = {
            use std::os::unix::fs::MetadataExt;
            (meta.dev(), meta.ino(), meta.mode())
        };
        out.insert(
            path.strip_prefix(root).unwrap().to_owned(),
            SnapshotEntry {
                directory: meta.is_dir(),
                length: meta.len(),
                content: meta
                    .is_file()
                    .then(|| crate::domain::Blake3Hash::digest(fs::read(path).unwrap())),
                modified: meta.modified().unwrap(),
                read_only: meta.permissions().readonly(),
                #[cfg(unix)]
                identity_and_mode,
            },
        );
        if meta.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), out);
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn assert_snapshot_unchanged(before: &Tree, after: &Tree, cached_shm: Option<&Path>) {
    let paths = before
        .keys()
        .chain(after.keys())
        .collect::<std::collections::BTreeSet<_>>();
    let mut differences = Vec::new();
    for path in paths {
        let unchanged = match (before.get(path), after.get(path)) {
            (Some(old), Some(new)) if cached_shm == Some(path.as_path()) => {
                // Existing explicit cached APIs coordinate using SQLite read
                // marks. Only SHM content and mtime may vary; never membership,
                // size, identity or mode. DB/WAL/authority remain fully checked.
                let mut normalized = new.clone();
                normalized.content = old.content.clone();
                normalized.modified = old.modified;
                old == &normalized
            }
            (Some(old), Some(new)) => old == new,
            _ => false,
        };
        if !unchanged {
            differences.push(format!(
                "{}: before={:?}; after={:?}",
                path.display(),
                before.get(path),
                after.get(path)
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} unexpected snapshot differences:\n{}",
        differences.len(),
        differences
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn assert_cached_snapshot_unchanged(before: &Tree, after: &Tree) {
    // These fixtures publish exactly one normalized catalog. Pin its existing
    // sidecar rather than ignoring arbitrary SHM names or the whole cache tree.
    let sidecars = before
        .iter()
        .filter_map(|(path, entry)| {
            if entry.directory || path.parent() != Some(Path::new(".wiki/cache/catalogs")) {
                return None;
            }
            let filename = path.file_name()?.to_str()?;
            let database_name = filename.strip_suffix("-shm")?;
            if !database_name.ends_with(".sqlite") {
                return None;
            }
            let database = before.get(&path.with_file_name(database_name))?;
            (!database.directory).then_some(path)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sidecars.len(),
        1,
        "fixture must identify one existing normalized catalog SHM"
    );
    assert_snapshot_unchanged(before, after, Some(sidecars[0]));
}

#[test]
fn critic_all_files_and_mtimes_survive_optional_absence_and_edit() {
    for normalized in [false, true] {
        let (_temp, app, source) = fixture(normalized);
        let first = read(&app, &source);
        let target = destination(&first.source_link.as_ref().unwrap().markdown);
        for replacement in [Some("A user edited this optional copy.\n"), None] {
            if let Some(text) = replacement {
                fs::write(&target, text).unwrap();
            } else {
                fs::remove_file(&target).unwrap();
            }
            let before = snapshot(app.fs().root().path());
            let output = read(&app, &source);
            assert_eq!(output.read.body, NOTE);
            assert!(output.source_link.is_none());
            assert_eq!(
                serde_json::to_value(output.read.source_citation).unwrap(),
                serde_json::to_value(&first.read.source_citation).unwrap()
            );
            assert_snapshot_unchanged(&before, &snapshot(app.fs().root().path()), None);
        }
    }
}

#[test]
fn critic_indexed_routes_keep_evidence_at_the_measured_mandatory_budget() {
    for scope in [
        ContextScope::IndexedDocuments,
        ContextScope::IndexedEvidence,
    ] {
        let (_temp, app, source) = fixture(true);
        let target = destination(&read(&app, &source).source_link.unwrap().markdown);
        let saved = fs::read(&target).unwrap();
        fs::remove_file(&target).unwrap();
        let catalog = catalog(&app);
        let mut request = request(scope);
        let baseline = verification::context(&catalog, None, "criticnavigation", &request).unwrap();
        assert!(!baseline.passages().is_empty());
        request.verification_budget.max_bytes = baseline.usage().verification_bytes;
        request.verification_budget.max_files = baseline.usage().verification_files;
        request.verification_budget.max_entries = baseline.usage().verification_entries;
        fs::write(&target, saved).unwrap();
        let before = snapshot(app.fs().root().path());
        let actual = verification::context(&catalog, None, "criticnavigation", &request).unwrap();
        assert_eq!(actual.passages().len(), baseline.passages().len());
        for (a, b) in actual.passages().iter().zip(baseline.passages()) {
            assert_eq!(a.text, b.text);
            assert_eq!(a.citations, b.citations);
            assert!(a.source_links.is_empty());
        }
        assert!(actual.usage().verification_bytes <= request.verification_budget.max_bytes);
        assert!(actual.usage().verification_files <= request.verification_budget.max_files);
        assert!(actual.usage().verification_entries <= request.verification_budget.max_entries);
        assert_cached_snapshot_unchanged(&before, &snapshot(app.fs().root().path()));
    }
}

struct RemoveOptional(PathBuf);
impl ContextFault for RemoveOptional {
    fn check(&self, _: ContextCheckpoint) -> crate::domain::Result<()> {
        fs::remove_file(&self.0).unwrap();
        Ok(())
    }
}

#[test]
fn critic_prepare_is_evidence_only_and_replays_after_optional_removal() {
    let (_temp, app, source) = fixture(true);
    let target = destination(&read(&app, &source).source_link.unwrap().markdown);
    let catalog = catalog(&app);
    let request = request(ContextScope::IndexedDocuments);
    crate::catalog::query_diagnostics::begin();
    let initial = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Prepare,
            ..Default::default()
        },
    )
    .unwrap();
    let initial_reads = crate::catalog::query_diagnostics::end();
    assert!(
        initial_reads
            .reads
            .iter()
            .all(|read| Path::new(&read.path) != target)
    );
    crate::catalog::query_diagnostics::begin();
    let changed = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Prepare,
            fault: Some(Arc::new(RemoveOptional(target.clone()))),
            ..Default::default()
        },
    )
    .unwrap();
    let changed_reads = crate::catalog::query_diagnostics::end();
    assert!(
        changed_reads
            .reads
            .iter()
            .all(|read| Path::new(&read.path) != target)
    );
    let before = initial.selection_packet().unwrap();
    let after = changed.selection_packet().unwrap();
    assert_eq!(after.fingerprint, before.fingerprint);
    assert_eq!(after.selector_input, before.selector_input);
    assert_eq!(after.cards, before.cards);
    assert!(
        after
            .selector_input
            .contains("without optional source links")
    );
    assert!(after.selector_input.contains("may omit selected passages"));
    assert_eq!(after.cards.len(), before.cards.len());
    assert!(!after.cards.is_empty());
    assert_eq!(after.candidate_count, before.candidate_count);
    assert_eq!(after.omitted_candidates, before.omitted_candidates);
    assert_eq!(after.input_bytes, after.selector_input.len());
    assert_eq!(
        after.estimated_tokens,
        after.selector_input.len().div_ceil(4)
    );
    for (a, b) in after.cards.iter().zip(&before.cards) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.passage.text, b.passage.text);
        assert_eq!(a.passage.citations, b.passage.citations);
        assert!(b.passage.source_links.is_empty());
        assert!(a.passage.source_links.is_empty());
        assert_eq!(a.rendered_bytes, b.rendered_bytes);
    }
    let reply = SelectionReply {
        packet_fingerprint: after.fingerprint.clone(),
        ordered_ids: vec![after.cards[0].id.clone()],
    };
    let output = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Apply(reply),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!output.passages().is_empty());
    assert!(output.passages().iter().all(|p| p.source_links.is_empty()));
    assert_eq!(output.usage().rendered_bytes, output.text().len());
    assert!(output.text().len() <= 6000 && output.usage().estimated_tokens <= 1500);
}

#[test]
fn critic_final_drop_during_host_apply_preserves_the_selected_exact_citations() {
    let (_temp, app, source) = fixture(true);
    let target = destination(&read(&app, &source).source_link.unwrap().markdown);
    let catalog = catalog(&app);
    let request = request(ContextScope::IndexedDocuments);
    let prepared = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Prepare,
            ..Default::default()
        },
    )
    .unwrap();
    let packet = prepared.selection_packet().unwrap();
    let reply = SelectionReply {
        packet_fingerprint: packet.fingerprint.clone(),
        ordered_ids: vec![packet.cards[0].id.clone()],
    };
    let normal = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Apply(reply.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        packet
            .cards
            .iter()
            .all(|card| card.passage.source_links.is_empty())
    );
    assert!(
        normal
            .passages()
            .iter()
            .any(|passage| !passage.source_links.is_empty())
    );
    assert_eq!(normal.usage().rendered_bytes, normal.text().len());
    assert_eq!(
        normal.usage().estimated_tokens,
        normal.text().len().div_ceil(4)
    );
    for passage in normal.passages() {
        for link in &passage.source_links {
            assert!(normal.text().contains(&link.markdown));
            assert!(
                fs::read_to_string(destination(&link.markdown))
                    .unwrap()
                    .contains(NOTE)
            );
        }
    }
    let changed = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Apply(reply),
            fault: Some(Arc::new(RemoveOptional(target))),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!changed.passages().is_empty());
    assert_eq!(changed.passages().len(), normal.passages().len());
    for (a, b) in changed.passages().iter().zip(normal.passages()) {
        assert_eq!(a.citations, b.citations);
        assert_eq!(a.text, b.text);
        assert_eq!(a.span, b.span);
        assert!(a.source_links.is_empty());
    }
    assert_eq!(changed.usage().rendered_bytes, changed.text().len());
    assert_eq!(
        changed.usage().estimated_tokens,
        changed.text().len().div_ceil(4)
    );
    assert!(changed.text().len() < normal.text().len());
}

#[test]
fn critic_optional_edit_between_prepare_and_apply_does_not_invalidate_exact_evidence() {
    let (_temp, app, source) = fixture(true);
    let target = destination(&read(&app, &source).source_link.unwrap().markdown);
    let catalog = catalog(&app);
    let request = request(ContextScope::IndexedDocuments);
    let prepared = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Prepare,
            ..Default::default()
        },
    )
    .unwrap();
    let packet = prepared.selection_packet().unwrap();
    let card = &packet.cards[0];
    let reply = SelectionReply {
        packet_fingerprint: packet.fingerprint.clone(),
        ordered_ids: vec![card.id.clone()],
    };
    fs::write(target, "User edits only the optional readable copy.\n").unwrap();
    let before = snapshot(app.fs().root().path());
    let output = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Apply(reply),
            ..Default::default()
        },
    )
    .expect("an optional display edit must not invalidate unchanged selected evidence");
    assert_eq!(output.passages().len(), 1);
    assert_eq!(output.passages()[0].citations, card.passage.citations);
    assert_eq!(output.passages()[0].text, card.passage.text);
    assert!(output.passages()[0].source_links.is_empty());
    assert_cached_snapshot_unchanged(&before, &snapshot(app.fs().root().path()));
}

// Native Windows filesystem/path parsing coverage only. This does not establish
// that a particular Markdown host opens a drive or UNC target correctly.
#[cfg(windows)]
#[test]
fn critic_windows_canonical_root_does_not_leak_device_prefix_into_markdown() {
    use std::path::{Component, Prefix};
    let (_temp, app, source) = fixture(true);
    let output = read(&app, &source);
    let link = output
        .source_link
        .expect("supported local Windows drive link");
    let encoded = link.markdown.rsplit_once("](<").unwrap().1;
    assert!(!encoded.starts_with("//%3F/"), "{encoded}");
    let target = destination(&link.markdown);
    match target.components().next().unwrap() {
        Component::Prefix(prefix) => assert!(
            matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(_, _)),
            "extended/device prefixes must not escape to host Markdown: {target:?}"
        ),
        other => panic!("expected an absolute Windows drive or UNC target: {other:?}"),
    }
    assert!(target.is_absolute());
    assert!(fs::read_to_string(target).unwrap().contains(NOTE));
}

#[test]
fn critic_canonical_withdrawal_still_rejects_a_prepared_host_reply() {
    let (_temp, app, source) = fixture(true);
    let catalog = catalog(&app);
    let request = request(ContextScope::IndexedDocuments);
    let prepared = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Prepare,
            ..Default::default()
        },
    )
    .unwrap();
    let packet = prepared.selection_packet().unwrap();
    let reply = SelectionReply {
        packet_fingerprint: packet.fingerprint.clone(),
        ordered_ids: vec![packet.cards[0].id.clone()],
    };
    app.source_withdraw(source, "Independent canonical staleness test")
        .unwrap();
    let before = snapshot(app.fs().root().path());
    let error = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Apply(reply),
            ..Default::default()
        },
    )
    .expect_err("canonical changes must still invalidate the prepared evidence packet");
    assert_eq!(error.code, crate::domain::ErrorCode::FreshnessConflict);
    assert_cached_snapshot_unchanged(&before, &snapshot(app.fs().root().path()));
}

#[test]
fn critic_host_apply_counts_links_even_when_the_evidence_only_estimate_fits() {
    let (_temp, app, source) = fixture(true);
    let target = destination(&read(&app, &source).source_link.unwrap().markdown);
    let saved = fs::read(&target).unwrap();
    fs::remove_file(&target).unwrap();
    let catalog = catalog(&app);
    let mut request = request(ContextScope::IndexedDocuments);
    let prepared = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Prepare,
            ..Default::default()
        },
    )
    .unwrap();
    let packet = prepared.selection_packet().unwrap();
    let reply = SelectionReply {
        packet_fingerprint: packet.fingerprint.clone(),
        ordered_ids: vec![packet.cards[0].id.clone()],
    };
    let no_link = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Apply(reply),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(no_link.passages().len(), 1);
    request.budget.max_bytes = no_link.text().len();
    request.budget.max_tokens = no_link.text().len().div_ceil(4);
    fs::write(&target, saved).unwrap();
    let tight = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Prepare,
            ..Default::default()
        },
    )
    .unwrap();
    let packet = tight.selection_packet().unwrap();
    assert!(!packet.cards.is_empty());
    assert!(packet.cards[0].rendered_bytes <= request.budget.max_bytes);
    let reply = SelectionReply {
        packet_fingerprint: packet.fingerprint.clone(),
        ordered_ids: vec![packet.cards[0].id.clone()],
    };
    let with_link = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Apply(reply.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        with_link.passages().is_empty(),
        "full link must participate in final packing"
    );
    assert!(with_link.text().len() <= request.budget.max_bytes);
    assert_eq!(with_link.usage().rendered_bytes, with_link.text().len());
    assert_eq!(
        with_link.usage().estimated_tokens,
        with_link.text().len().div_ceil(4)
    );
    assert!(!with_link.omissions().is_empty());
    fs::remove_file(target).unwrap();
    let exact = verification::context_with_options(
        &catalog,
        None,
        "criticnavigation",
        &request,
        &ContextOptions {
            selection: SelectionAction::Apply(reply),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(exact.passages().len(), 1);
    assert_eq!(
        exact.passages()[0].citations,
        no_link.passages()[0].citations
    );
    assert_eq!(exact.text().len(), request.budget.max_bytes);
}
