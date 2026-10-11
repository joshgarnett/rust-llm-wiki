#[path = "../test_support/paths.rs"]
mod test_paths;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    _temp: tempfile::TempDir,
    base: PathBuf,
    home: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let home = base.join("home");
        fs::create_dir(&home).unwrap();
        Self {
            _temp: temp,
            base,
            home,
        }
    }
    fn command(&self, cwd: &Path, xdg: Option<&str>) -> Command {
        let mut command = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env_remove("XDG_CONFIG_HOME")
            .args(["--json", "--offline"]);
        if let Some(xdg) = xdg {
            command.env("XDG_CONFIG_HOME", xdg);
        }
        command
    }
    fn raw(&self, cwd: &Path, args: &[&str], xdg: Option<&str>) -> Output {
        self.command(cwd, xdg).args(args).output().unwrap()
    }
    fn invoke(&self, cwd: &Path, args: &[&str], xdg: Option<&str>) -> (i32, Value) {
        let output = self.raw(cwd, args, xdg);
        assert!(
            output.stderr.is_empty(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.contains(&0x1b));
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?}: {error}; {}",
                String::from_utf8_lossy(&output.stdout)
            )
        });
        let schema: Value =
            serde_json::from_str(include_str!("../schemas/output-v2.json")).unwrap();
        jsonschema::validator_for(&schema)
            .unwrap()
            .validate(&envelope)
            .unwrap();
        assert_eq!(envelope["meta"]["network_used"], false);
        (output.status.code().unwrap(), envelope)
    }
    fn ok(&self, cwd: &Path, args: &[&str], xdg: Option<&str>) -> Value {
        let (status, value) = self.invoke(cwd, args, xdg);
        assert_eq!(status, 0, "{args:?}: {value}");
        assert_eq!(value["ok"], true);
        value
    }
    fn init(&self, name: &str, set_default: bool) -> (PathBuf, String) {
        let path = self.base.join(name);
        let mut args = vec!["init", path.to_str().unwrap()];
        if set_default {
            args.push("--set-default");
        }
        let result = self.ok(&self.base, &args, None);
        (path, result["data"]["id"].as_str().unwrap().to_owned())
    }
    fn config(&self) -> PathBuf {
        self.home.join(".config/lwiki/config.toml")
    }
}
fn snapshot(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let key = path.strip_prefix(base).unwrap().to_path_buf();
            if entry.file_type().unwrap().is_symlink() {
                result.insert(
                    key,
                    fs::read_link(&path)
                        .unwrap()
                        .to_string_lossy()
                        .as_bytes()
                        .to_vec(),
                );
            } else if path.is_dir() {
                result.insert(key, Vec::new());
                visit(base, &path, result);
            } else {
                result.insert(key, fs::read(path).unwrap());
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(path, path, &mut result);
    result
}

#[test]
fn fresh_process_default_selection_and_nearest_explicit_precedence() {
    let fixture = Fixture::new();
    let target = fixture.base.join("Personal Wiki 猫");
    let before = snapshot(&fixture.base);
    let preview = fixture.ok(
        &fixture.base,
        &[
            "init",
            target.to_str().unwrap(),
            "--set-default",
            "--dry-run",
        ],
        None,
    );
    assert_eq!(preview["data"]["created"], false);
    assert_eq!(preview["data"]["default_registration"]["applied"], false);
    assert_eq!(snapshot(&fixture.base), before);
    let (personal, personal_id) = fixture.init("Personal Wiki 猫", true);
    let shown = fixture.ok(&fixture.base, &["wiki", "show"], None);
    assert_eq!(shown["data"]["root"], json!(personal));
    assert_eq!(shown["data"]["wiki_id"], personal_id);
    assert_eq!(shown["data"]["origin"], "default");
    let (work, work_id) = fixture.init("Work Wiki", false);
    let nested = work.join("nested");
    let nested_init = fixture.ok(&fixture.base, &["init", nested.to_str().unwrap()], None);
    let nested_id = nested_init["data"]["id"].as_str().unwrap();
    let shown = fixture.ok(&nested, &["wiki", "show"], None);
    assert_eq!(shown["data"]["wiki_id"], nested_id);
    assert_eq!(shown["data"]["origin"], "ancestor");
    let shown = fixture.ok(
        &nested,
        &["--wiki", work.to_str().unwrap(), "wiki", "show"],
        None,
    );
    assert_eq!(shown["data"]["wiki_id"], work_id);
    assert_eq!(shown["data"]["origin"], "explicit");
    let (exit, _) = fixture.invoke(
        &nested,
        &["--wiki", fixture.base.to_str().unwrap(), "wiki", "show"],
        None,
    );
    assert_ne!(exit, 0);
    fs::write(nested.join("WIKI.md"), b"invalid nearer marker").unwrap();
    let (exit, _) = fixture.invoke(&nested, &["wiki", "show"], None);
    assert_ne!(exit, 0);
    let shown = fixture.ok(&fixture.base, &["wiki", "show"], None);
    assert_eq!(shown["data"]["wiki_id"], personal_id);
}

#[test]
fn expected_identity_and_replaced_or_moved_default_never_write_wrong_target() {
    let fixture = Fixture::new();
    let (personal, id) = fixture.init("personal", true);
    let body = fixture.base.join("body.md");
    fs::write(&body, b"new author bytes").unwrap();
    let before = snapshot(&fixture.base);
    let (exit, error) = fixture.invoke(
        &fixture.base,
        &[
            "--expect-wiki-id",
            "Wrong.Wiki",
            "page",
            "init",
            "--file",
            body.to_str().unwrap(),
            "--title",
            "Wrong",
            "--path",
            "pages/wrong.md",
        ],
        None,
    );
    assert_eq!(exit, 4);
    assert_eq!(error["error"]["code"], "CONTENT_CONFLICT");
    assert_eq!(snapshot(&fixture.base), before);
    let marker = fs::read_to_string(personal.join("WIKI.md"))
        .unwrap()
        .replace(&id, "Vault.Replacement");
    fs::write(personal.join("WIKI.md"), marker).unwrap();
    let before = snapshot(&fixture.base);
    let (exit, error) = fixture.invoke(
        &fixture.base,
        &[
            "page",
            "init",
            "--file",
            body.to_str().unwrap(),
            "--title",
            "Wrong",
            "--path",
            "pages/wrong.md",
        ],
        None,
    );
    assert_eq!(exit, 4);
    assert_eq!(error["error"]["code"], "CONTENT_CONFLICT");
    assert_eq!(snapshot(&fixture.base), before);
    fs::rename(&personal, fixture.base.join("moved-personal")).unwrap();
    assert_ne!(fixture.invoke(&fixture.base, &["wiki", "show"], None).0, 0);
    let (other, _) = fixture.init("other", false);
    fixture.ok(
        &fixture.base,
        &["--wiki", other.to_str().unwrap(), "wiki", "show"],
        None,
    );
    fixture.ok(&other, &["wiki", "show"], None);
}

#[test]
fn ambient_xdg_explicit_preferences_and_disabled_lookup_are_distinct() {
    let fixture = Fixture::new();
    let (personal, id) = fixture.init("personal", true);
    let (work, work_id) = fixture.init("work", false);
    let xdg = fixture.base.join("xdg");
    let directory = xdg.join("lwiki");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("config.json"),
        serde_json::to_vec(
            &json!({"schema_version":"2","default_wiki":{"path":work,"wiki_id":work_id}}),
        )
        .unwrap(),
    )
    .unwrap();
    let shown = fixture.ok(
        &fixture.base,
        &["wiki", "show"],
        Some(xdg.to_str().unwrap()),
    );
    assert_eq!(shown["data"]["wiki_id"], work_id);
    let (exit, error) = fixture.invoke(
        &fixture.base,
        &["--wiki", personal.to_str().unwrap(), "wiki", "show"],
        Some("relative"),
    );
    assert_eq!(exit, 2);
    assert!(
        error["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("absolute")
    );
    let shown = fixture.ok(
        &fixture.base,
        &[
            "--preferences",
            fixture.config().to_str().unwrap(),
            "--no-user-config",
            "wiki",
            "show",
        ],
        Some("relative"),
    );
    assert_eq!(shown["data"]["wiki_id"], id);
    assert_eq!(shown["data"]["configuration"]["origin"], "explicit");
    assert_ne!(
        fixture
            .invoke(&fixture.base, &["--no-user-config", "wiki", "show"], None)
            .0,
        0
    );
    let shown = fixture.ok(
        &personal,
        &["--no-user-config", "wiki", "show"],
        Some("relative"),
    );
    assert_eq!(shown["data"]["configuration"]["origin"], "disabled");
    assert_eq!(shown["data"]["origin"], "ancestor");
    // An intentionally unsupported location override never changes selection.
    let output = fixture
        .command(&fixture.base, None)
        .env("LWIKI_WIKI", &work)
        .args(["wiki", "show"])
        .output()
        .unwrap();
    let shown: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(shown["data"]["wiki_id"], id);
}

#[test]
fn malformed_unknown_and_symlinked_configuration_preserve_bytes_and_offer_bypass() {
    let fixture = Fixture::new();
    let (personal, _) = fixture.init("personal", true);
    for bytes in [
        b"malformed configuration".as_slice(),
        b"schema_version = \"99\"\n".as_slice(),
    ] {
        fs::write(fixture.config(), bytes).unwrap();
        let (exit, error) = fixture.invoke(
            &fixture.base,
            &["--wiki", personal.to_str().unwrap(), "wiki", "show"],
            None,
        );
        assert_eq!(exit, 2);
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains(fixture.config().to_str().unwrap())
        );
        assert!(
            error["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("--no-user-config")
        );
        fixture.ok(
            &fixture.base,
            &[
                "--wiki",
                personal.to_str().unwrap(),
                "--no-user-config",
                "wiki",
                "show",
            ],
            None,
        );
        for flag in ["--help", "--version"] {
            assert!(
                fixture
                    .raw(&fixture.base, &[flag], Some("relative"))
                    .status
                    .success()
            );
        }
        assert_eq!(fs::read(fixture.config()).unwrap(), bytes);
    }
    #[cfg(unix)]
    {
        let target = fixture.base.join("foreign.json");
        fs::write(&target, b"author-owned config").unwrap();
        fs::remove_file(fixture.config()).unwrap();
        std::os::unix::fs::symlink(&target, fixture.config()).unwrap();
        assert_eq!(
            fixture
                .invoke(
                    &fixture.base,
                    &["--wiki", personal.to_str().unwrap(), "wiki", "show"],
                    None
                )
                .0,
            2
        );
        assert_eq!(fs::read(target).unwrap(), b"author-owned config");
    }
}

#[test]
fn explicit_default_set_migrates_v1_copy_and_clear_is_persistent() {
    let fixture = Fixture::new();
    let legacy_config = fixture.config().with_extension("json");
    let (personal, id) = fixture.init("personal", false);
    fs::create_dir_all(legacy_config.parent().unwrap()).unwrap();
    let v1=br#"{"schema_version":"1","defaults":{"offline":true,"read_max_bytes":512},"vaults":{},"allowed_profiles":["local"]}"#;
    fs::write(&legacy_config, v1).unwrap();
    let before = snapshot(&fixture.base);
    let preview = fixture.ok(
        &fixture.base,
        &[
            "wiki",
            "default",
            "set",
            personal.to_str().unwrap(),
            "--dry-run",
        ],
        None,
    );
    assert_eq!(preview["data"]["previous"], Value::Null);
    assert_eq!(preview["data"]["proposed"]["wiki_id"], id);
    assert_eq!(snapshot(&fixture.base), before);
    fixture.ok(
        &fixture.base,
        &["wiki", "default", "set", personal.to_str().unwrap()],
        None,
    );
    assert_eq!(
        fs::read(legacy_config.with_file_name("config.json.v1.bak")).unwrap(),
        v1
    );
    let shown = fixture.ok(&fixture.base, &["wiki", "show"], None);
    assert_eq!(shown["data"]["wiki_id"], id);
    assert_eq!(
        shown["data"]["preference_origins"]["offline"],
        "command_line"
    );
    assert_eq!(
        shown["data"]["preference_origins"]["read_max_bytes"],
        "user_defaults"
    );
    fixture.ok(&fixture.base, &["wiki", "default", "clear"], None);
    assert_ne!(fixture.invoke(&fixture.base, &["wiki", "show"], None).0, 0);
    fixture.ok(&personal, &["wiki", "show"], None);
}

#[test]
fn existing_editor_folder_requires_intent_and_preserves_author_owned_bytes() {
    let fixture = Fixture::new();
    let root = fixture.base.join("Obsidian Vault");
    fs::create_dir_all(root.join(".obsidian")).unwrap();
    let original = b"---\ncustom: keep exactly\n---\nAuthor text\n";
    fs::write(root.join("note.md"), original).unwrap();
    fs::write(root.join(".obsidian/app.json"), b"{\"theme\":\"dark\"}").unwrap();
    let before = snapshot(&fixture.base);
    assert_ne!(
        fixture
            .invoke(&fixture.base, &["init", root.to_str().unwrap()], None)
            .0,
        0
    );
    assert_eq!(snapshot(&fixture.base), before);
    fixture.ok(
        &fixture.base,
        &[
            "init",
            root.to_str().unwrap(),
            "--existing",
            "--set-default",
            "--dry-run",
        ],
        None,
    );
    assert_eq!(snapshot(&fixture.base), before);
    fixture.ok(
        &fixture.base,
        &[
            "init",
            root.to_str().unwrap(),
            "--existing",
            "--set-default",
        ],
        None,
    );
    assert_eq!(fs::read(root.join("note.md")).unwrap(), original);
    assert_eq!(
        fs::read(root.join(".obsidian/app.json")).unwrap(),
        b"{\"theme\":\"dark\"}"
    );
    let landing = fs::read_to_string(root.join("WIKI.md")).unwrap();
    assert!(landing.contains("source add"));
    assert!(landing.contains("pages"));
    let collision = fixture.base.join("collision");
    fs::create_dir(&collision).unwrap();
    fs::write(collision.join("pages"), b"occupied author file").unwrap();
    let before = snapshot(&fixture.base);
    assert_ne!(
        fixture
            .invoke(
                &fixture.base,
                &["init", collision.to_str().unwrap(), "--existing"],
                None
            )
            .0,
        0
    );
    assert_eq!(snapshot(&fixture.base), before);
}

#[cfg(unix)]
#[test]
fn default_registration_failure_reports_initialized_wiki_and_exact_retry() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let directory = fixture.config().parent().unwrap().to_path_buf();
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
    let root = fixture.base.join("initialized-but-not-default");
    let (exit, error) = fixture.invoke(
        &fixture.base,
        &["init", root.to_str().unwrap(), "--set-default"],
        None,
    );
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert_ne!(exit, 0);
    assert_eq!(error["error"]["details"]["init"]["created"], true);
    assert_eq!(error["error"]["details"]["default_registered"], false);
    assert!(
        error["error"]["details"]["retry"]
            .as_str()
            .unwrap()
            .contains("wiki default set")
    );
    assert!(root.join("WIKI.md").is_file());
    assert!(!fixture.config().exists());
    fixture.ok(
        &fixture.base,
        &["wiki", "default", "set", root.to_str().unwrap()],
        None,
    );
    fixture.ok(&fixture.base, &["wiki", "show"], None);
}
