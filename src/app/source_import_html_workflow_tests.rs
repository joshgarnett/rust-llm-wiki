//! Integrated HTML import/lifecycle checks; cross-version binary admission is separate.
use super::*;
use crate::domain::Eligibility;
use crate::sources::import_manifest_types::{
    ImportExtraction, ImportManifestItem, ImportManifestRecord,
};

fn readable_packet(
    fixture: &Fixture,
    source: &RecordId,
    revision: &RecordId,
    raw: &[u8],
    needle: &str,
    condition: &str,
) -> Vec<(String, Vec<CitationRef>, Eligibility)> {
    let app = fixture.app();
    let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
    let plan = QueryPlan {
        filters: SearchFilters {
            source_ids: vec![source.clone()],
            ..Default::default()
        },
        ..Default::default()
    };
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let hits = search_indexed_sources(&reader, needle, &plan).unwrap();
    assert!(hits.hits.iter().any(|hit| {
        hit.source_id.as_ref() == Some(source) && hit.owner_revision.as_ref() == Some(revision)
    }));
    for hidden in [
        "AttributeGhost",
        "CommentGhost",
        "ScriptGhost",
        "StyleGhost",
    ] {
        assert!(
            search_indexed_sources(&reader, hidden, &plan)
                .unwrap()
                .hits
                .is_empty(),
            "{hidden}"
        );
    }
    drop(reader);
    let packet = verification::context(
        &catalog,
        None,
        needle,
        &ContextRequest {
            scope: ContextScope::IndexedDocuments,
            documents: plan,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!packet.network_used);
    let mut quoted = Vec::new();
    for passage in &packet.passages {
        for citation in &passage.citations {
            let CitationRef::Source(reference) = citation else {
                panic!("HTML evidence must carry a captured Source citation");
            };
            assert_eq!(&reference.source_id, source);
            assert_eq!(&reference.source_revision, revision);
            let quote = &raw[reference.span.start() as usize..reference.span.end() as usize];
            assert_eq!(passage.text.as_bytes(), quote);
            assert_eq!(reference.quote_hash, Blake3Hash::digest(quote));
            assert_eq!(passage.eligibility, Eligibility::Current);
        }
        if !passage.citations.is_empty() {
            quoted.push((
                passage.text.clone(),
                passage.citations.clone(),
                passage.eligibility,
            ));
        }
    }
    assert!(
        quoted
            .iter()
            .any(|(text, _, _)| text.contains(needle) && text.contains(condition)),
        "fact and qualification must occur in actual returned exact evidence"
    );
    quoted
}

#[test]
fn html_manifest_run_resume_preserves_bytes_and_returns_readable_qualified_evidence() {
    for migrated in [false, true] {
        let fixture = Fixture::with_inputs(migrated, vec![
            ("manual.html".into(), br#"<h1>Harbor manual</h1>
<p data-note="AttributeGhost > ignored">HarborLamp emits seven amber pulses only after the safety latch closes.</p>
<!-- CommentGhost --><script>ScriptGhost</script><style>StyleGhost</style>
"#.to_vec()),
            ("second.HTM".into(), "<h2>Dock manual</h2>\n<p>DockBeacon emits nine blue pulses only while the gate remains locked. café 東京.</p>\n".as_bytes().to_vec()),
            ("control.md".into(), b"# Markdown control\nControlLamp emits three green pulses only after inspection.\n".to_vec()),
        ]);
        let hash = fixture.prepare();
        let first = fixture
            .app()
            .source_import_run(&fixture.manifest, KEY, 1, 1)
            .unwrap();
        assert_eq!(first.manifest_hash, hash);
        assert_eq!(
            (
                first.imported_items,
                first.groups_committed,
                first.total_items
            ),
            (1, 1, 3)
        );
        assert!(!first.completed);
        assert_eq!(first.pending_change, None);
        let mut all = mapping(first.last_group.as_ref().unwrap());
        let cut = fixture.app().source_import_status(KEY).unwrap();
        assert_eq!((cut.imported_items, cut.groups_committed), (1, 1));
        assert_eq!(mapping(cut.last_group.as_ref().unwrap()), all);
        for expected in [2, 3] {
            let resumed = fixture.app().source_import_resume(KEY, 1).unwrap();
            assert_eq!(resumed.manifest_hash, hash);
            assert_eq!(
                (resumed.imported_items, resumed.groups_committed),
                (expected, expected)
            );
            assert_eq!(resumed.completed, expected == 3);
            assert_eq!(resumed.pending_change, None);
            all.extend(mapping(resumed.last_group.as_ref().unwrap()));
        }
        assert_eq!(
            all.iter().map(|item| item.0).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            all.iter()
                .map(|item| &item.1)
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
        assert_eq!(
            all.iter()
                .map(|item| &item.2)
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
        for (ordinal, source, revision) in &all {
            fixture.assert_item(*ordinal, source, revision, true);
            let (needle, condition) = match ordinal {
                0 => ("HarborLamp", "only after the safety latch closes"),
                1 => ("DockBeacon", "only while the gate remains locked"),
                _ => ("ControlLamp", "only after inspection"),
            };
            readable_packet(
                &fixture,
                source,
                revision,
                &fixture.bytes[*ordinal as usize],
                needle,
                condition,
            );
        }
        let status = fixture.app().source_import_status(KEY).unwrap();
        assert!(status.completed);
        assert_eq!(status.groups_committed, 3);
        assert_eq!(fixture.app().check().unwrap().error_count, 0);
    }
}

#[test]
fn frozen_unsupported_html_resume_and_refresh_preserve_original_only_history() {
    for migrated in [false, true] {
        let raw = b"<p>ArchiveLamp emits five silver pulses only after review.</p>\n";
        let fixture = Fixture::with_inputs(
            migrated,
            vec![
                ("archived.html".into(), raw.to_vec()),
                (
                    "another.HTM".into(),
                    b"<p>OtherArchive stays sealed.</p>\n".to_vec(),
                ),
            ],
        );
        // A valid frozen legacy policy, not a forged parser proof or old-binary claim.
        let mut sealed = Vec::new();
        serde_json::to_writer(&mut sealed, &ImportManifestRecord::Header { version: 1 }).unwrap();
        sealed.push(b'\n');
        for ordinal in 0..2 {
            let item = ImportManifestItem {
                ordinal: ordinal as u64,
                path: fs::canonicalize(&fixture.inputs[ordinal])
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into(),
                title: format!("Imported source {ordinal}"),
                media_type: Some("text/html".into()),
                original_hash: Blake3Hash::digest(&fixture.bytes[ordinal]),
                byte_len: fixture.bytes[ordinal].len() as u64,
                extraction: ImportExtraction::Unsupported,
            };
            serde_json::to_writer(&mut sealed, &ImportManifestRecord::Item { item }).unwrap();
            sealed.push(b'\n');
        }
        let body_hash = Blake3Hash::digest(&sealed);
        serde_json::to_writer(
            &mut sealed,
            &ImportManifestRecord::Footer {
                items: 2,
                body_hash,
            },
        )
        .unwrap();
        sealed.push(b'\n');
        fs::write(&fixture.manifest, &sealed).unwrap();
        let first = fixture
            .app()
            .source_import_run(&fixture.manifest, KEY, 1, 1)
            .unwrap();
        assert!(!first.completed);
        let old = mapping(first.last_group.as_ref().unwrap());
        fixture.assert_item(0, &old[0].1, &old[0].2, false);
        let old_tree =
            fixture.physical(&rel(format!("sources/{}/revisions/{}", old[0].1, old[0].2)));
        let old_bytes = snapshot(&old_tree);
        let resumed = fixture.app().source_import_resume(KEY, 1).unwrap();
        assert!(resumed.completed);
        assert_eq!((resumed.imported_items, resumed.groups_committed), (2, 2));
        let second = mapping(resumed.last_group.as_ref().unwrap());
        fixture.assert_item(1, &second[0].1, &second[0].2, false);
        assert_eq!(fs::read(&fixture.manifest).unwrap(), sealed);
        assert_eq!(snapshot(&old_tree), old_bytes);
        let refreshed = fixture
            .app()
            .source_refresh(
                old[0].1.clone(),
                CaptureRequest {
                    title: "Imported source 0".into(),
                    origin_kind: SourceOrigin::LocalFile,
                    origin: fixture.inputs[0].to_str().unwrap().into(),
                    original: raw.to_vec(),
                    extraction: ExtractionInput::Utf8Preserve,
                    media_type: Some("text/html".into()),
                },
            )
            .unwrap();
        let current = &refreshed.allocated_ids["revision"];
        assert_ne!(current, &old[0].2);
        fixture.assert_item(0, &old[0].1, current, true);
        assert_eq!(snapshot(&old_tree), old_bytes);
        let before = readable_packet(
            &fixture,
            &old[0].1,
            current,
            raw,
            "ArchiveLamp",
            "only after review",
        );
        let replay = fixture.app().source_import_resume(KEY, 1).unwrap();
        assert_eq!(mapping(replay.last_group.as_ref().unwrap()), second);
        assert_eq!(snapshot(&old_tree), old_bytes);
        assert_eq!(
            readable_packet(
                &fixture,
                &old[0].1,
                current,
                raw,
                "ArchiveLamp",
                "only after review"
            ),
            before
        );
        // A second complete revision makes historical/current eligibility observable
        // without inventing content for the original unsupported revision.
        let complete_tree =
            fixture.physical(&rel(format!("sources/{}/revisions/{current}", old[0].1)));
        let complete_bytes = snapshot(&complete_tree);
        let next_raw =
            b"<p>RevisedArchiveLamp emits six silver pulses only after a second review.</p>\n";
        let next = fixture
            .app()
            .source_refresh(
                old[0].1.clone(),
                CaptureRequest {
                    title: "Imported source 0".into(),
                    origin_kind: SourceOrigin::LocalFile,
                    origin: fixture.inputs[0].to_str().unwrap().into(),
                    original: next_raw.to_vec(),
                    extraction: ExtractionInput::Utf8Preserve,
                    media_type: Some("text/html".into()),
                },
            )
            .unwrap();
        let latest = &next.allocated_ids["revision"];
        assert_ne!(latest, current);
        let app = fixture.app();
        let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
        let view = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
        for (revision, eligibility) in [
            (current, Eligibility::Historical),
            (latest, Eligibility::Current),
        ] {
            let document = view
                .document(&rel(format!(
                    "sources/{}/revisions/{revision}/content.md",
                    old[0].1
                )))
                .unwrap()
                .expect("complete revision must remain discoverable with explicit eligibility");
            assert_eq!(document.source_id.as_ref(), Some(&old[0].1));
            assert_eq!(document.owner_revision.as_ref(), Some(revision));
            assert_eq!(document.eligibility, eligibility);
        }
        drop(view);
        let latest_packet = readable_packet(
            &fixture,
            &old[0].1,
            latest,
            next_raw,
            "RevisedArchiveLamp",
            "only after a second review",
        );
        assert!(!latest_packet.is_empty());
        let retained_cache = fixture.temp.path().join("retained-html-cache");
        fs::rename(fixture.root.join(".wiki/cache"), &retained_cache).unwrap();
        let retained_cache_bytes = snapshot(&retained_cache);
        fixture.app().index_rebuild_normalized().unwrap();
        assert_eq!(
            readable_packet(
                &fixture,
                &old[0].1,
                latest,
                next_raw,
                "RevisedArchiveLamp",
                "only after a second review"
            ),
            latest_packet
        );
        let rebuilt_app = fixture.app();
        let rebuilt_catalog =
            Catalog::new(rebuilt_app.fs().clone(), rebuilt_app.vault_id().clone());
        let rebuilt = rebuilt_catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        for (revision, eligibility) in [
            (current, Eligibility::Historical),
            (latest, Eligibility::Current),
        ] {
            let document = rebuilt
                .document(&rel(format!(
                    "sources/{}/revisions/{revision}/content.md",
                    old[0].1
                )))
                .unwrap()
                .expect("cache rebuild must retain complete historical/current revisions");
            assert_eq!(document.source_id.as_ref(), Some(&old[0].1));
            assert_eq!(document.owner_revision.as_ref(), Some(revision));
            assert_eq!(document.eligibility, eligibility);
        }
        drop(rebuilt);
        assert_eq!(snapshot(&retained_cache), retained_cache_bytes);
        assert_eq!(snapshot(&old_tree), old_bytes);
        assert_eq!(snapshot(&complete_tree), complete_bytes);
        assert_eq!(fixture.app().check().unwrap().error_count, 0);
    }
}
