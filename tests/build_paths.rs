#[path = "../test_support/paths.rs"]
mod test_paths;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
use test_paths::{Environment, resolve};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "lwiki test paths {} {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn file(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, name).unwrap();
        path
    }
    fn environment(&self) -> Environment {
        Environment {
            roots: vec![],
            workspace: "wiki_workspace".into(),
            manifest: None,
            cwd: self.0.clone(),
        }
    }
    fn manifest(&self, entries: &str) -> PathBuf {
        let path = self.0.join("MANIFEST");
        fs::write(&path, entries).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn escaped(value: &str) -> String {
    value
        .replace('\\', "\\b")
        .replace(' ', "\\s")
        .replace('\n', "\\n")
}

#[test]
fn cargo_absolute_path_is_canonical_and_missing_path_cannot_fall_back() {
    let fixture = Fixture::new();
    let binary = fixture.file("cargo target/debug/lwiki");
    let environment = fixture.environment();
    assert_eq!(
        resolve(&binary, &environment).unwrap(),
        fs::canonicalize(binary).unwrap()
    );
    fixture.file("lwiki");
    assert!(resolve(&fixture.0.join("missing/lwiki"), &environment).is_err());
}

#[test]
fn runfiles_workspace_precedes_stale_cwd_and_supports_second_root() {
    let fixture = Fixture::new();
    fixture.file("lwiki");
    let binary = fixture.file("runfiles/wiki_workspace/lwiki");
    let mut environment = fixture.environment();
    environment.roots = vec![
        fixture.0.join("missing-runfiles"),
        fixture.0.join("runfiles"),
    ];
    assert_eq!(
        resolve(Path::new("lwiki"), &environment).unwrap(),
        fs::canonicalize(binary).unwrap()
    );
    let file = fixture.file("runfiles/wiki_workspace/tests/ca.pem");
    assert_eq!(
        resolve(Path::new("./tests/ca.pem"), &environment).unwrap(),
        fs::canonicalize(file).unwrap()
    );
}

#[test]
fn manifest_handles_escaped_keys_destinations_and_unescaped_destination_spaces() {
    let fixture = Fixture::new();
    let binary = fixture.file("physical source/binary candidate");
    let ca = fixture.file("physical source/ca.pem");
    let mut environment = fixture.environment();
    environment.manifest = Some(fixture.manifest(&format!(
        " {} {}\nwiki_workspace/tests/ca.pem {}\n",
        escaped("wiki_workspace/bin/lwiki candidate"),
        escaped(binary.to_str().unwrap()),
        ca.display()
    )));
    assert_eq!(
        resolve(Path::new("bin/lwiki candidate"), &environment).unwrap(),
        fs::canonicalize(binary).unwrap()
    );
    assert_eq!(
        resolve(Path::new("tests/ca.pem"), &environment).unwrap(),
        fs::canonicalize(ca).unwrap()
    );
}

#[test]
fn manifest_directory_entry_resolves_child_file() {
    let fixture = Fixture::new();
    let ca = fixture.file("physical fixtures/ca.pem");
    let mut environment = fixture.environment();
    environment.manifest = Some(fixture.manifest(&format!(
        "wiki_workspace/tests/fixtures {}\n",
        ca.parent().unwrap().display()
    )));
    assert_eq!(
        resolve(Path::new("tests/fixtures/ca.pem"), &environment).unwrap(),
        fs::canonicalize(ca).unwrap()
    );
}

#[test]
fn manifest_file_entries_recover_consistent_fixture_directory_only() {
    let fixture = Fixture::new();
    let first = fixture.file("physical fixtures/vault/WIKI.md");
    let second = fixture.file("physical fixtures/vault/nested/page.md");
    let mut environment = fixture.environment();
    environment.manifest = Some(fixture.manifest(&format!(
        "wiki_workspace/tests/fixtures/vault/WIKI.md {}\nwiki_workspace/tests/fixtures/vault/nested/page.md {}\n",
        first.display(), second.display()
    )));
    assert_eq!(
        resolve(Path::new("tests/fixtures/vault"), &environment).unwrap(),
        fs::canonicalize(first.parent().unwrap()).unwrap()
    );
    let elsewhere = fixture.file("different tree/page.md");
    environment.manifest = Some(fixture.manifest(&format!(
        "wiki_workspace/tests/fixtures/vault/WIKI.md {}\nwiki_workspace/tests/fixtures/vault/nested/page.md {}\n",
        first.display(), elsewhere.display()
    )));
    assert!(resolve(Path::new("tests/fixtures/vault"), &environment).is_err());
}

#[test]
fn resolved_binary_remains_usable_after_command_changes_directory() {
    let fixture = Fixture::new();
    let binary = std::env::current_exe().unwrap();
    let mut environment = fixture.environment();
    environment.manifest = Some(fixture.manifest(&format!(
        " {} {}\n",
        escaped("wiki_workspace/lwiki"),
        escaped(binary.to_str().unwrap())
    )));
    let resolved = resolve(Path::new("lwiki"), &environment).unwrap();
    assert!(resolved.is_absolute());
    let output = std::process::Command::new(resolved)
        .current_dir(&fixture.0)
        .arg("--list")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn relative_cargo_manifest_fixture_uses_cwd_fallback() {
    let fixture = Fixture::new();
    let ca = fixture.file("tests/fixtures/ca.pem");
    assert_eq!(
        resolve(Path::new("./tests/fixtures/ca.pem"), &fixture.environment()).unwrap(),
        fs::canonicalize(ca).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn runfiles_fixture_tree_copies_symlinked_files_and_directories() {
    let fixture = Fixture::new();
    let original = fixture.file("source/vault/WIKI.md");
    let nested = fixture.file("source/vault/pages/page.md");
    let tree = fixture.0.join("runfiles/wiki_workspace/tests/vault");
    fs::create_dir_all(&tree).unwrap();
    std::os::unix::fs::symlink(&original, tree.join("WIKI.md")).unwrap();
    std::os::unix::fs::symlink(nested.parent().unwrap(), tree.join("pages")).unwrap();
    let mut environment = fixture.environment();
    environment.roots = vec![fixture.0.join("runfiles")];
    let root = resolve(Path::new("tests/vault"), &environment).unwrap();
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let path = entry.unwrap().path();
            let destination = to.join(path.file_name().unwrap());
            if path.is_dir() {
                copy(&path, &destination);
            } else {
                fs::copy(path, destination).unwrap();
            }
        }
    }
    let copy_to = fixture.0.join("copy");
    copy(&root, &copy_to);
    assert_eq!(
        fs::read(copy_to.join("WIKI.md")).unwrap(),
        fs::read(original).unwrap()
    );
    assert_eq!(
        fs::read(copy_to.join("pages/page.md")).unwrap(),
        fs::read(nested).unwrap()
    );
}
