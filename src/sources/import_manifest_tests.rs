use super::*;
use serde_json::json;

fn list(path: &Path, inputs: &[serde_json::Value]) {
    let mut bytes = Vec::new();
    for input in inputs {
        serde_json::to_writer(&mut bytes, input).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn sealed(items: &[ImportManifestItem]) -> Vec<u8> {
    let mut bytes = encoded(&ImportManifestRecord::Header { version: 1 }).unwrap();
    for item in items {
        bytes.extend(encoded(&ImportManifestRecord::Item { item: item.clone() }).unwrap());
    }
    let footer = ImportManifestRecord::Footer {
        items: items.len() as u64,
        body_hash: Blake3Hash::digest(&bytes),
    };
    bytes.extend(encoded(&footer).unwrap());
    bytes
}

fn declared(ordinal: u64, byte_len: u64) -> ImportManifestItem {
    ImportManifestItem {
        ordinal,
        path: "/explicit/import/nonexistent.md".into(),
        title: "Declared input".into(),
        media_type: None,
        original_hash: Blake3Hash::digest(b"declared, not opened"),
        byte_len,
        extraction: ImportExtraction::Utf8Preserve,
    }
}

fn directory_names(path: &Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

#[test]
fn manifest_preparation_resolves_spaced_relative_and_absolute_inputs_and_exact_hashes() {
    let temp = tempfile::tempdir().unwrap();
    let lists = temp.path().join("input lists");
    fs::create_dir(&lists).unwrap();
    let relative = temp.path().join("source bytes.MD");
    let absolute = temp.path().join("other.CSV");
    let original = b"first\r\nlast \xf0\x9f\x8d\xb5";
    fs::write(&relative, original).unwrap();
    fs::write(&absolute, b"one,two\n").unwrap();
    let input = lists.join("explicit.jsonl");
    list(
        &input,
        &[
            json!({"path":"../source bytes.MD","media_type":"text/custom"}),
            json!({"path":absolute.to_str().unwrap(),"title":"Explicit CSV"}),
        ],
    );
    let output = temp.path().join("manifest.jsonl");
    let prepared = prepare_manifest(&input, &output, false).unwrap();
    let bytes = fs::read(&output).unwrap();
    assert_eq!(prepared.manifest_hash, Blake3Hash::digest(&bytes));
    assert_eq!(prepared.manifest_bytes, bytes.len() as u64);
    assert_eq!(prepared.items, 2);
    assert_eq!(prepared.input_bytes, (original.len() + 8) as u64);
    assert!(!prepared.preview);
    let footer_start = bytes[..bytes.len() - 1]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .unwrap()
        + 1;
    assert_eq!(
        record(&bytes[footer_start..]).unwrap(),
        ImportManifestRecord::Footer {
            items: 2,
            body_hash: Blake3Hash::digest(&bytes[..footer_start]),
        }
    );
    let batch = read_manifest_batch(&output, prepared.first_item_offset, 0, 8).unwrap();
    assert!(batch.eof);
    assert_eq!(
        batch.items[0].path,
        fs::canonicalize(relative).unwrap().to_str().unwrap()
    );
    assert_eq!(batch.items[0].title, "source bytes.MD");
    assert_eq!(batch.items[0].media_type.as_deref(), Some("text/custom"));
    assert_eq!(batch.items[0].original_hash, Blake3Hash::digest(original));
    assert_eq!(batch.items[1].title, "Explicit CSV");
    assert!(
        batch
            .items
            .iter()
            .all(|item| item.extraction == ImportExtraction::Utf8Preserve)
    );
    fs::remove_file(absolute).unwrap();
    fs::remove_file(temp.path().join("source bytes.MD")).unwrap();
    let validated = validate_manifest(&output).unwrap();
    assert_eq!(validated.manifest_hash, prepared.manifest_hash);
    assert_eq!(validated.first_item_offset, prepared.first_item_offset);
    assert_eq!(validated.input_bytes, prepared.input_bytes);
    assert_eq!(
        read_manifest_batch(&output, validated.first_item_offset, 0, 8)
            .unwrap()
            .items,
        batch.items
    );
    assert!(
        directory_names(temp.path())
            .iter()
            .all(|name| !name.to_string_lossy().starts_with(".lwiki-import-"))
    );
}

#[test]
fn manifest_seek_is_bounded_ordered_and_requires_explicit_footer() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("manifest.jsonl");
    fs::write(
        &path,
        sealed(&[declared(0, 1), declared(1, 2), declared(2, 3)]),
    )
    .unwrap();
    let prepared = validate_manifest(&path).unwrap();
    let first = read_manifest_batch(&path, prepared.first_item_offset, 0, 2).unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(!first.eof);
    let last = read_manifest_batch(&path, first.next_offset, 2, 2).unwrap();
    assert_eq!(last.items, vec![declared(2, 3)]);
    assert!(last.eof);
    for (offset, ordinal, count) in [
        (prepared.first_item_offset + 1, 0, 2),
        (prepared.first_item_offset, 1, 2),
        (prepared.first_item_offset, 0, 0),
        (prepared.first_item_offset, 0, 9),
        (last.next_offset, 3, 2),
        (0, 0, 2),
    ] {
        assert!(read_manifest_batch(&path, offset, ordinal, count).is_err());
    }
    // An exact full batch leaves the explicit footer for the next bounded call.
    let exact = read_manifest_batch(&path, prepared.first_item_offset, 0, 3).unwrap();
    assert!(!exact.eof);
    let footer = read_manifest_batch(&path, exact.next_offset, 3, 1).unwrap();
    assert!(footer.eof && footer.items.is_empty());
    fs::write(
        &path,
        &fs::read(&path).unwrap()[..exact.next_offset as usize],
    )
    .unwrap();
    assert!(read_manifest_batch(&path, first.next_offset, 2, 2).is_err());
}

#[test]
fn manifest_preserves_empty_unsupported_and_streaming_utf8_capture_classification() {
    let temp = tempfile::tempdir().unwrap();
    let mut crossing = vec![b'a'; BUFFER_BYTES - 1];
    crossing.extend_from_slice("🍵".as_bytes());
    let mut incomplete = crossing.clone();
    incomplete.pop();
    let cases: [(&str, &[u8], ImportExtraction); 6] = [
        ("empty.txt", b"", ImportExtraction::Utf8Preserve),
        ("binary.bin", b"valid text", ImportExtraction::Unsupported),
        ("invalid.md", b"\xff", ImportExtraction::Unsupported),
        ("noextension", b"plain", ImportExtraction::Utf8Preserve),
        (
            "crossing.markdown",
            &crossing,
            ImportExtraction::Utf8Preserve,
        ),
        ("incomplete.md", &incomplete, ImportExtraction::Unsupported),
    ];
    let input = temp.path().join("inputs.jsonl");
    let mut inputs = Vec::new();
    for (name, original, _) in &cases {
        fs::write(temp.path().join(name), original).unwrap();
        inputs.push(json!({"path":name}));
    }
    list(&input, &inputs);
    let output = temp.path().join("manifest.jsonl");
    let prepared = prepare_manifest(&input, &output, false).unwrap();
    let batch = read_manifest_batch(&output, prepared.first_item_offset, 0, 8).unwrap();
    for (item, (_, original, extraction)) in batch.items.iter().zip(&cases) {
        assert_eq!(&item.extraction, extraction);
        assert_eq!(item.original_hash, Blake3Hash::digest(original));
        assert_eq!(item.byte_len, original.len() as u64);
    }
    assert!(batch.eof);
}

#[test]
fn manifest_dry_run_is_deterministic_without_output_or_directory_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.md");
    let input = temp.path().join("inputs.jsonl");
    fs::write(&source, b"exact original\n").unwrap();
    list(&input, &[json!({"path":"source.md"})]);
    let before: Vec<_> = [&source, &input]
        .into_iter()
        .map(|path| {
            (
                fs::read(path).unwrap(),
                fs::metadata(path).unwrap().modified().unwrap(),
            )
        })
        .collect();
    let names = directory_names(temp.path());
    let output = temp.path().join("dry.jsonl");
    let preview = prepare_manifest(&input, &output, true).unwrap();
    let repeated = prepare_manifest(&input, &output, true).unwrap();
    assert!(preview.preview && repeated.preview);
    assert_eq!(preview.manifest_hash, repeated.manifest_hash);
    assert_eq!(directory_names(temp.path()), names);
    for (path, (bytes, modified)) in [&source, &input].into_iter().zip(before) {
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), modified);
    }
    let actual = prepare_manifest(&input, &temp.path().join("actual.jsonl"), false).unwrap();
    assert_eq!(actual.manifest_hash, preview.manifest_hash);
    assert_eq!(actual.manifest_bytes, preview.manifest_bytes);
    let missing = temp.path().join("missing/manifest.jsonl");
    assert!(prepare_manifest(&input, &missing, true).is_err());
    assert!(!temp.path().join("missing").exists());
}

#[test]
fn manifest_refuses_foreign_output_and_cleans_only_owned_failed_temporary() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("inputs.jsonl");
    let output = temp.path().join("occupied.jsonl");
    fs::write(&output, b"foreign output").unwrap();
    list(&input, &[]);
    let modified = fs::metadata(&output).unwrap().modified().unwrap();
    for dry_run in [true, false] {
        assert!(prepare_manifest(&input, &output, dry_run).is_err());
        assert_eq!(fs::read(&output).unwrap(), b"foreign output");
        assert_eq!(fs::metadata(&output).unwrap().modified().unwrap(), modified);
    }
    let unrelated = temp.path().join(".lwiki-import-unfamiliar.tmp");
    fs::write(&unrelated, b"preserve this too").unwrap();
    fs::write(&input, b"not JSON\n").unwrap();
    let names = directory_names(temp.path());
    assert!(prepare_manifest(&input, &temp.path().join("failed.jsonl"), false).is_err());
    assert_eq!(directory_names(temp.path()), names);
    assert_eq!(fs::read(unrelated).unwrap(), b"preserve this too");
}

#[test]
fn manifest_rejects_exact_hash_drift_order_truncation_and_trailing_records() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("manifest.jsonl");
    let valid = sealed(&[declared(0, 1)]);
    let mut drift = valid.clone();
    let index = drift
        .windows(b"Declared".len())
        .position(|part| part == b"Declared")
        .unwrap();
    drift[index] = b'X';
    let mut extra = valid.clone();
    extra.extend_from_slice(b"{}\n");
    let mut no_footer = encoded(&ImportManifestRecord::Header { version: 1 }).unwrap();
    no_footer.extend(
        encoded(&ImportManifestRecord::Item {
            item: declared(0, 1),
        })
        .unwrap(),
    );
    let mut wrong_count = encoded(&ImportManifestRecord::Header { version: 1 }).unwrap();
    let hash = Blake3Hash::digest(&wrong_count);
    wrong_count.extend(
        encoded(&ImportManifestRecord::Footer {
            items: 1,
            body_hash: hash,
        })
        .unwrap(),
    );
    for bytes in [
        drift,
        extra,
        valid[..valid.len() - 1].to_vec(),
        no_footer,
        sealed(&[declared(1, 1)]),
        wrong_count,
        b"{\"kind\":\"header\",\"version\":2}\n".to_vec(),
        b"{\"kind\":\"header\",\"version\":1,\"foreign\":true}\n".to_vec(),
    ] {
        fs::write(&path, bytes).unwrap();
        assert!(validate_manifest(&path).is_err());
    }
}

#[test]
fn manifest_enforces_line_item_aggregate_and_manifest_limits_before_unbounded_reads() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("inputs.jsonl");
    let output = temp.path().join("manifest.jsonl");
    fs::write(&input, vec![b'x'; MAX_IMPORT_LINE_BYTES + 1]).unwrap();
    assert!(prepare_manifest(&input, &output, true).is_err());
    assert!(!output.exists());
    let large = temp.path().join("large.bin");
    File::create(&large)
        .unwrap()
        .set_len(MAX_IMPORT_ITEM_BYTES + 1)
        .unwrap();
    list(&input, &[json!({"path":"large.bin"})]);
    assert!(prepare_manifest(&input, &output, true).is_err());
    File::create(&output)
        .unwrap()
        .set_len(MAX_IMPORT_MANIFEST_BYTES + 1)
        .unwrap();
    assert!(validate_manifest(&output).is_err());
    assert!(read_manifest_batch(&output, 1, 0, 1).is_err());
    let count = MAX_IMPORT_INPUT_BYTES / MAX_IMPORT_ITEM_BYTES + 1;
    let items: Vec<_> = (0..count)
        .map(|ordinal| declared(ordinal, MAX_IMPORT_ITEM_BYTES))
        .collect();
    fs::write(&output, sealed(&items)).unwrap();
    assert!(validate_manifest(&output).is_err());
    fs::write(&output, sealed(&[declared(0, MAX_IMPORT_ITEM_BYTES + 1)])).unwrap();
    assert!(validate_manifest(&output).is_err());
    assert!(item_metadata(&declared(MAX_IMPORT_ITEMS, 0), MAX_IMPORT_ITEMS).is_err());
    fs::write(&input, vec![b'x'; MAX_IMPORT_LINE_BYTES + 1]).unwrap();
    assert!(validate_manifest(&input).is_err());
    OpenOptions::new()
        .write(true)
        .open(&input)
        .unwrap()
        .set_len(MAX_IMPORT_MANIFEST_BYTES + 1)
        .unwrap();
    assert!(prepare_manifest(&input, &temp.path().join("absent.jsonl"), true).is_err());
}

#[test]
fn manifest_hashes_exact_whitespace_and_refuses_incompatible_item_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("manifest.jsonl");
    let mut bytes = b" {\"version\":1,\"kind\":\"header\"}\r\n".to_vec();
    let mut item = encoded(&ImportManifestRecord::Item {
        item: declared(0, 0),
    })
    .unwrap();
    item.pop();
    item.extend_from_slice(b"\r\n");
    bytes.extend(item);
    let body_hash = Blake3Hash::digest(&bytes);
    bytes.extend(
        encoded(&ImportManifestRecord::Footer {
            items: 1,
            body_hash,
        })
        .unwrap(),
    );
    fs::write(&path, &bytes).unwrap();
    assert_eq!(
        validate_manifest(&path).unwrap().manifest_hash,
        Blake3Hash::digest(&bytes)
    );
    for (name, title, extraction) in [
        ("relative.md", "Title", ImportExtraction::Utf8Preserve),
        (
            "/absolute/../alias.md",
            "Title",
            ImportExtraction::Utf8Preserve,
        ),
        ("/absolute/input.md", "  ", ImportExtraction::Utf8Preserve),
        (
            "/absolute/input.bin",
            "Title",
            ImportExtraction::Utf8Preserve,
        ),
    ] {
        let mut item = declared(0, 0);
        item.path = name.into();
        item.title = title.into();
        item.extraction = extraction;
        fs::write(&path, sealed(&[item])).unwrap();
        assert!(validate_manifest(&path).is_err());
    }
}

#[test]
fn manifest_handles_empty_list_final_input_without_newline_and_serialized_line_expansion() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("inputs.jsonl");
    let empty = temp.path().join("empty.jsonl");
    fs::write(&input, b"").unwrap();
    let prepared = prepare_manifest(&input, &empty, false).unwrap();
    assert_eq!(prepared.items, 0);
    assert_eq!(prepared.input_bytes, 0);
    let batch = read_manifest_batch(&empty, prepared.first_item_offset, 0, 8).unwrap();
    assert!(batch.eof && batch.items.is_empty());
    fs::write(temp.path().join("x.txt"), b"x").unwrap();
    fs::write(&input, b"{\"path\":\"x.txt\"}").unwrap();
    assert_eq!(
        prepare_manifest(&input, &temp.path().join("one.jsonl"), false)
            .unwrap()
            .items,
        1
    );
    list(
        &input,
        &[json!({"path":"x.txt","title":"x".repeat(MAX_IMPORT_LINE_BYTES - 100)})],
    );
    assert!(fs::metadata(&input).unwrap().len() <= MAX_IMPORT_LINE_BYTES as u64);
    assert!(prepare_manifest(&input, &temp.path().join("expanded.jsonl"), false).is_err());
    assert!(!temp.path().join("expanded.jsonl").exists());
}

#[cfg(unix)]
#[test]
fn manifest_refuses_symlink_inputs_outputs_and_preserves_foreign_targets() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.md");
    fs::write(&source, b"foreign source").unwrap();
    symlink(&source, temp.path().join("link.md")).unwrap();
    let input = temp.path().join("inputs.jsonl");
    list(&input, &[json!({"path":"link.md"})]);
    assert!(prepare_manifest(&input, &temp.path().join("new.jsonl"), false).is_err());
    list(&input, &[json!({"path":"source.md"})]);
    let output = temp.path().join("output.jsonl");
    symlink(&source, &output).unwrap();
    assert!(prepare_manifest(&input, &output, false).is_err());
    assert!(validate_manifest(&output).is_err());
    assert_eq!(fs::read(source).unwrap(), b"foreign source");
    assert!(
        fs::symlink_metadata(output)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
