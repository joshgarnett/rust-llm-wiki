use super::*;
use std::fs;

fn observe(paths: Vec<PathBuf>, wiki: &Path) -> CaptureInputs {
    observe_inputs(&paths, None, wiki, CaptureInputLimits::default()).unwrap()
}

#[test]
fn deterministic_overlap_dedup_hidden_types_and_lexical_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let notes = temp.path().join("notes with spaces");
    fs::create_dir_all(notes.join(".private")).unwrap();
    fs::create_dir_all(notes.join("nested")).unwrap();
    fs::write(notes.join("z.md"), "z").unwrap();
    fs::write(notes.join("nested/é's note.markdown"), "é").unwrap();
    fs::write(notes.join("a.txt"), "a").unwrap();
    fs::write(notes.join("image.bin"), [0, 255]).unwrap();
    fs::write(notes.join(".private/hidden.md"), "hidden").unwrap();
    let inputs = observe(
        vec![notes.clone(), notes.join("nested/../z.md")],
        &temp.path().join("wiki"),
    );
    assert_eq!(inputs.files.len(), 3);
    assert!(
        inputs
            .files
            .windows(2)
            .all(|pair| pair[0].origin < pair[1].origin)
    );
    assert!(inputs.files.iter().all(|input| input.origin.is_absolute()));
    assert_eq!(
        inputs
            .issues
            .iter()
            .filter(|issue| issue.reason == "hidden")
            .count(),
        1
    );
    assert_eq!(
        inputs
            .issues
            .iter()
            .filter(|issue| issue.reason == "file_type")
            .count(),
        1
    );
    let dot = observe(vec![notes.join(".")], &temp.path().join("wiki"));
    assert_eq!(dot.files.len(), 3);
}

#[test]
fn selected_unsupported_file_is_retained_but_managed_wiki_is_excluded() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = temp.path().join("wiki");
    fs::create_dir_all(wiki.join("sources/old/revisions/head")).unwrap();
    fs::create_dir_all(wiki.join("pages")).unwrap();
    fs::create_dir_all(wiki.join(".obsidian")).unwrap();
    fs::write(wiki.join("WIKI.md"), "managed marker").unwrap();
    fs::write(wiki.join("notes.md"), "personal note").unwrap();
    fs::write(
        wiki.join("sources/old/revisions/head/content.md"),
        "managed copy",
    )
    .unwrap();
    fs::write(wiki.join("pages/saved.md"), "managed page").unwrap();
    fs::write(wiki.join(".obsidian/settings.json"), "editor").unwrap();
    let binary = temp.path().join("binary.bin");
    fs::write(&binary, [0, 255]).unwrap();
    let wiki = fs::canonicalize(wiki).unwrap();
    let inputs = observe(vec![wiki.clone(), binary], &wiki);
    assert_eq!(inputs.files.len(), 2);
    assert!(inputs.files.iter().any(|input| matches!(
        input.request.extraction,
        ExtractionInput::Unsupported { .. }
    )));
    assert_eq!(
        inputs
            .issues
            .iter()
            .filter(|issue| issue.reason == "managed_wiki")
            .count(),
        3
    );
    assert_eq!(
        fs::read(wiki.join(".obsidian/settings.json")).unwrap(),
        b"editor"
    );
}

#[test]
fn path_list_resolves_from_its_folder_and_input_recheck_rejects_changed_bytes() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("own note.md"), "first").unwrap();
    let list = temp.path().join("files.txt");
    fs::write(&list, "own note.md\n").unwrap();
    let inputs = observe_inputs(
        &[],
        Some(&list),
        &temp.path().join("wiki"),
        CaptureInputLimits::default(),
    )
    .unwrap();
    assert_eq!(inputs.files.len(), 1);
    assert_eq!(inputs.files[0].request.original, b"first");
    fs::write(temp.path().join("own note.md"), "other").unwrap();
    assert_eq!(
        inputs.files[0].recheck().unwrap_err().code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn selection_limits_refuse_before_returning_any_publishable_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let notes = temp.path().join("notes");
    fs::create_dir(&notes).unwrap();
    fs::write(notes.join("one.md"), "first").unwrap();
    fs::write(notes.join("two.md"), "second").unwrap();
    let limits = CaptureInputLimits {
        max_files: 1,
        ..Default::default()
    };
    assert_eq!(
        observe_inputs(
            std::slice::from_ref(&notes),
            None,
            &temp.path().join("wiki"),
            limits
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::BudgetExceeded
    );
    let limits = CaptureInputLimits {
        max_total_bytes: 5,
        ..Default::default()
    };
    assert_eq!(
        observe_inputs(
            std::slice::from_ref(&notes),
            None,
            &temp.path().join("wiki"),
            limits
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::BudgetExceeded
    );
}

#[cfg(unix)]
#[test]
fn directory_symlinks_are_never_followed_and_explicit_links_fail() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let notes = temp.path().join("notes");
    let outside = temp.path().join("outside");
    fs::create_dir(&notes).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.md"), "outside").unwrap();
    symlink(&outside, notes.join("linked")).unwrap();
    symlink(outside.join("missing.md"), notes.join("broken.md")).unwrap();
    let inputs = observe(vec![notes.clone()], &temp.path().join("wiki"));
    assert!(inputs.files.is_empty());
    assert_eq!(
        inputs
            .issues
            .iter()
            .filter(|issue| issue.reason == "symlink")
            .count(),
        2
    );
    let explicit = observe(vec![notes.join("linked")], &temp.path().join("wiki"));
    assert_eq!(explicit.issues.len(), 1);
    assert!(!explicit.issues[0].skipped);
    assert!(explicit.issues[0].error.is_some());
}
