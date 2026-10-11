use super::*;
use std::fs;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    (temp, base)
}
fn wiki(base: &Path, name: &str, id: &str) -> DefaultWiki {
    let path = base.join(name);
    fs::create_dir(&path).unwrap();
    fs::write(
        path.join("WIKI.md"),
        format!("---\nwiki_schema: \"1\"\nwiki_id: {id}\nwiki_kind: vault\ntitle: Test\n---\n"),
    )
    .unwrap();
    crate::vault::discovery::default_from_path(&path).unwrap()
}
fn config(base: &Path, value: serde_json::Value) -> PathBuf {
    let path = base.join("config.json");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    path
}
fn v1() -> serde_json::Value {
    serde_json::json!({"schema_version":"1","defaults":{"read_max_bytes":123},"vaults":{"Vault.First":{"lock_timeout_ms":42}},"allowed_profiles":["local"]})
}

#[test]
fn ambient_locations_are_absolute_and_explicit_disabled_skip_environment() {
    assert_eq!(
        ambient_path(Some(OsStr::new("/chosen")), Some(OsStr::new("/home"))).unwrap(),
        Path::new("/chosen/lwiki/config.json")
    );
    assert_eq!(
        ambient_path(None, Some(OsStr::new("/home"))).unwrap(),
        Path::new("/home/.config/lwiki/config.json")
    );
    let error = ambient_path(Some(OsStr::new("relative")), Some(OsStr::new("/home"))).unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigInvalid);
    assert!(error.hint.unwrap().contains("absolute"));
    assert!(ambient_path(None, None).is_err());
    assert!(ambient_path(None, Some(OsStr::new("relative"))).is_err());
    let (_temp, base) = fixture();
    let path = config(&base, v1());
    let loaded = load_user_config(Some(&path), true).unwrap();
    assert_eq!(loaded.summary().origin, UserConfigOrigin::Explicit);
    let disabled = load_user_config(None, true).unwrap();
    assert_eq!(disabled.summary().origin, UserConfigOrigin::Disabled);
    assert!(disabled.path().is_none());
    assert!(require_write_target(&disabled).is_err());
}

#[test]
fn missing_ambient_uses_defaults_but_explicit_missing_names_path() {
    let (_temp, base) = fixture();
    let path = base.join("missing/lwiki/config.json");
    let loaded = load_path(Some(&path), UserConfigOrigin::Ambient).unwrap();
    assert!(!loaded.summary().present);
    assert_eq!(loaded.document().schema_version, "2");
    assert!(!base.join("missing").exists());
    let error = load_user_config(Some(&path), false).unwrap_err();
    assert!(error.message.contains(path.to_str().unwrap()));
    assert!(error.hint.unwrap().contains("--no-user-config"));
}

#[test]
fn v1_is_read_unchanged_and_migration_preserves_original_exactly() {
    let (_temp, base) = fixture();
    let path = config(&base, v1());
    let original = fs::read(&path).unwrap();
    let selected = wiki(&base, "Personal Wiki 猫", "Vault.First");
    let loaded = load_user_config(Some(&path), false).unwrap();
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(loaded.summary().migration_required);
    let preview = update_default(&loaded, Some(selected.clone()), true, 5000).unwrap();
    assert!(preview.changed);
    assert!(!preview.applied);
    assert_eq!(preview.previous, None);
    assert_eq!(preview.proposed, Some(selected.clone()));
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(!path.with_file_name("config.json.lock").exists());
    let result = update_default(&loaded, Some(selected.clone()), false, 5000).unwrap();
    assert!(result.applied);
    assert_eq!(
        fs::read(result.preserved_v1_path.unwrap()).unwrap(),
        original
    );
    let reloaded = load_user_config(Some(&path), false).unwrap();
    assert_eq!(reloaded.default_wiki(), Some(&selected));
    assert_eq!(reloaded.document().defaults.read_max_bytes, Some(123));
    assert_eq!(
        reloaded.document().vaults[&RecordId::new("Vault.First").unwrap()].lock_timeout_ms,
        Some(42)
    );
    assert_eq!(reloaded.document().allowed_profiles, ["local"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["config.json", "config.json.lock", "config.json.v1.bak"] {
            assert_eq!(
                fs::metadata(base.join(name)).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}

#[test]
fn schema_validation_refuses_unknown_versions_duplicates_and_invalid_defaults() {
    let (_temp, base) = fixture();
    for bytes in [
        br#"{"schema_version":"9"}"#.as_slice(),
        br#"{"schema_version":"2","schema_version":"2"}"#.as_slice(),
        br#"{"schema_version":"2","unknown":true}"#.as_slice(),
        br#"{"schema_version":"1","default_wiki":null}"#.as_slice(),
        br#"{"schema_version":"2","default_wiki":{"path":"relative","wiki_id":"valid"}}"#
            .as_slice(),
        br#"{"schema_version":"2","default_wiki":{"path":"/a/../b","wiki_id":"valid"}}"#.as_slice(),
        br#"{"schema_version":"2","default_wiki":{"path":"/a","wiki_id":"bad id"}}"#.as_slice(),
        br#"{"schema_version":"2","defaults":{"read_max_bytes":0}}"#.as_slice(),
        br#"{"schema_version":"2","vaults":{"unused":{"lock_timeout_ms":30001}}}"#.as_slice(),
    ] {
        let path = base.join("config.json");
        fs::write(&path, bytes).unwrap();
        let error = load_user_config(Some(&path), false).unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert!(error.message.contains(path.to_str().unwrap()));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn default_filesystem_is_not_validated_by_loading_configuration() {
    let (_temp, base) = fixture();
    let path = config(
        &base,
        serde_json::json!({"schema_version":"2","default_wiki":{"path":base.join("moved"),"wiki_id":"Vault.Moved"}}),
    );
    let loaded = load_user_config(Some(&path), false).unwrap();
    assert_eq!(
        loaded.default_wiki().unwrap().wiki_id.as_str(),
        "Vault.Moved"
    );
}

#[test]
fn preferences_use_the_loaded_document_after_its_file_changes() {
    let (_temp, base) = fixture();
    let selected = wiki(&base, "wiki", "Vault.First");
    let path = config(&base, v1());
    let loaded = load_user_config(Some(&path), false).unwrap();
    fs::write(&path, b"malformed later edit").unwrap();
    let fs = VaultFs::new(crate::vault::VaultRoot::explicit(&selected.path).unwrap());
    let preferences = resolve_loaded(&fs, &selected.wiki_id, &Default::default(), &loaded).unwrap();
    assert_eq!(preferences.read_max_bytes, 123);
    assert_eq!(preferences.lock_timeout_ms, 42);
    assert_eq!(preferences.origins["read_max_bytes"], "user_defaults");
    assert_eq!(preferences.origins["lock_timeout_ms"], "user_vault");
    assert_eq!(
        update_default(&loaded, Some(selected), false, 5000)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"malformed later edit");
}

#[test]
fn new_configuration_and_clear_are_private_guarded_and_idempotent() {
    let (_temp, base) = fixture();
    let selected = wiki(&base, "wiki", "Vault.First");
    let path = base.join(".config/lwiki/config.json");
    let loaded = load_path(Some(&path), UserConfigOrigin::Ambient).unwrap();
    update_default(&loaded, Some(selected.clone()), false, 5000).unwrap();
    let loaded = load_path(Some(&path), UserConfigOrigin::Ambient).unwrap();
    assert!(
        !update_default(&loaded, Some(selected), false, 5000)
            .unwrap()
            .changed
    );
    let cleared = update_default(&loaded, None, false, 5000).unwrap();
    assert!(cleared.changed && cleared.applied);
    let loaded = load_path(Some(&path), UserConfigOrigin::Ambient).unwrap();
    assert!(loaded.default_wiki().is_none());
    assert!(!update_default(&loaded, None, false, 5000).unwrap().changed);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[test]
fn competing_config_writers_and_preservation_collision_refuse_without_overwrite() {
    let (_temp, base) = fixture();
    let first = wiki(&base, "first", "Vault.First");
    let second = wiki(&base, "second", "Vault.Second");
    let path = config(&base, v1());
    let before = fs::read(&path).unwrap();
    let loaded = load_user_config(Some(&path), false).unwrap();
    fs::write(
        path.with_file_name("config.json.v1.bak"),
        b"unknown author bytes",
    )
    .unwrap();
    assert_eq!(
        update_default(&loaded, Some(first.clone()), false, 5000)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        fs::read(path.with_file_name("config.json.v1.bak")).unwrap(),
        b"unknown author bytes"
    );
    fs::remove_file(path.with_file_name("config.json.v1.bak")).unwrap();
    update_default(&loaded, Some(first), false, 5000).unwrap();
    assert_eq!(
        update_default(&loaded, Some(second), false, 5000)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn staged_and_preserved_failures_leave_old_config_and_allow_retry() {
    for boundary in ["staged", "preserved"] {
        let (_temp, base) = fixture();
        let selected = wiki(&base, "wiki", "Vault.First");
        let path = config(&base, v1());
        let original = fs::read(&path).unwrap();
        let loaded = load_user_config(Some(&path), false).unwrap();
        CHECKPOINT_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |stage, _| {
                if stage == boundary {
                    Err(WikiError::new(
                        ErrorCode::Internal,
                        "injected interruption before publication",
                    ))
                } else {
                    Ok(())
                }
            }))
        });
        assert!(update_default(&loaded, Some(selected.clone()), false, 5000).is_err());
        CHECKPOINT_HOOK.with(|hook| *hook.borrow_mut() = None);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!fs::read_dir(&base).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".stage-")
        }));
        update_default(&loaded, Some(selected), false, 5000).unwrap();
        assert_eq!(
            fs::read(path.with_file_name("config.json.v1.bak")).unwrap(),
            original
        );
    }
}

#[test]
fn post_publication_failure_remains_readable_and_retry_is_idempotent() {
    let (_temp, base) = fixture();
    let selected = wiki(&base, "wiki", "Vault.First");
    let path = config(&base, v1());
    let loaded = load_user_config(Some(&path), false).unwrap();
    CHECKPOINT_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|stage, _| {
            if stage == "published" {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "injected post-publication interruption",
                ))
            } else {
                Ok(())
            }
        }))
    });
    assert!(update_default(&loaded, Some(selected.clone()), false, 5000).is_err());
    CHECKPOINT_HOOK.with(|hook| *hook.borrow_mut() = None);
    let loaded = load_user_config(Some(&path), false).unwrap();
    assert_eq!(loaded.default_wiki(), Some(&selected));
    assert!(
        !update_default(&loaded, Some(selected), false, 5000)
            .unwrap()
            .changed
    );
}

#[cfg(unix)]
#[test]
fn config_file_directory_and_during_update_symlink_swaps_are_refused() {
    use std::os::unix::fs::symlink;
    let (_temp, base) = fixture();
    let selected = wiki(&base, "wiki", "Vault.First");
    let original = config(&base, v1());
    let file_alias = base.join("alias.json");
    symlink(&original, &file_alias).unwrap();
    assert!(load_user_config(Some(&file_alias), false).is_err());
    let parent_alias = base.join("alias");
    symlink(&base, &parent_alias).unwrap();
    assert!(load_user_config(Some(&parent_alias.join("config.json")), false).is_err());
    let directory = base.join("settings");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("config.json"), fs::read(&original).unwrap()).unwrap();
    // An existing ancestor alias is allowed and its effective location disclosed.
    let through_ancestor = parent_alias.join("settings/config.json");
    assert_eq!(
        load_user_config(Some(&through_ancestor), false)
            .unwrap()
            .path()
            .unwrap(),
        directory.join("config.json")
    );
    let path = directory.join("config.json");
    let loaded = load_user_config(Some(&path), false).unwrap();
    let original_bytes = fs::read(&path).unwrap();
    let target = base.join("target.json");
    fs::write(&target, b"unrelated author bytes").unwrap();
    let target_hook = target.clone();
    CHECKPOINT_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |stage, path| {
            if stage == "preserved" {
                fs::remove_file(path).unwrap();
                symlink(&target_hook, path).unwrap();
            }
            Ok(())
        }))
    });
    assert!(update_default(&loaded, Some(selected), false, 5000).is_err());
    CHECKPOINT_HOOK.with(|hook| *hook.borrow_mut() = None);
    assert_eq!(fs::read(&target).unwrap(), b"unrelated author bytes");
    assert_eq!(
        fs::read(path.with_file_name("config.json.v1.bak")).unwrap(),
        original_bytes
    );
}

#[cfg(unix)]
#[test]
fn replaced_parent_directory_after_loading_cannot_receive_a_write() {
    let (_temp, base) = fixture();
    let selected = wiki(&base, "wiki", "Vault.First");
    let directory = base.join("settings");
    fs::create_dir(&directory).unwrap();
    let path = config(&directory, v1());
    let original = fs::read(&path).unwrap();
    let loaded = load_user_config(Some(&path), false).unwrap();
    fs::rename(&directory, base.join("retained-settings")).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::write(&path, &original).unwrap();
    assert!(update_default(&loaded, Some(selected), false, 5000).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(!directory.join("config.json.lock").exists());
}

#[cfg(unix)]
#[test]
fn configuration_lock_contention_is_bounded_and_never_deletes_lock() {
    let (_temp, base) = fixture();
    let selected = wiki(&base, "wiki", "Vault.First");
    let path = config(&base, v1());
    let loaded = load_user_config(Some(&path), false).unwrap();
    let directory = super::super::local_io::Directory::open(&path, false)
        .unwrap()
        .unwrap();
    let held = directory.lock(0).unwrap();
    let error = update_default(&loaded, Some(selected), false, 0).unwrap_err();
    assert_eq!(error.code, ErrorCode::LockTimeout);
    let name = path.with_file_name("config.json.lock");
    assert!(name.exists());
    drop(held);
    assert!(name.exists());
}

#[cfg(unix)]
#[test]
fn shared_writable_config_directory_cannot_publish_or_create_lock() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, base) = fixture();
    let selected = wiki(&base, "wiki", "Vault.First");
    let path = config(&base, v1());
    let original = fs::read(&path).unwrap();
    let loaded = load_user_config(Some(&path), false).unwrap();
    fs::set_permissions(&base, fs::Permissions::from_mode(0o770)).unwrap();
    let result = update_default(&loaded, Some(selected), false, 5000);
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(result.unwrap_err().code, ErrorCode::ConfigInvalid);
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(!base.join("config.json.lock").exists());
}

#[cfg(unix)]
#[test]
fn substituted_or_edited_staged_configuration_is_never_published() {
    for replace in [false, true] {
        let (_temp, base) = fixture();
        let selected = wiki(&base, "wiki", "Vault.First");
        let path = config(&base, v1());
        let original = fs::read(&path).unwrap();
        let loaded = load_user_config(Some(&path), false).unwrap();
        CHECKPOINT_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |stage, path| {
                if stage == "preserved" {
                    let staged = fs::read_dir(path.parent().unwrap())
                        .unwrap()
                        .map(|entry| entry.unwrap().path())
                        .find(|entry| {
                            entry
                                .file_name()
                                .unwrap()
                                .to_string_lossy()
                                .starts_with("config.json.stage-")
                        })
                        .unwrap();
                    if replace {
                        fs::remove_file(&staged).unwrap();
                    }
                    fs::write(&staged, b"unrelated replacement bytes").unwrap();
                }
                Ok(())
            }));
        });
        let result = update_default(&loaded, Some(selected), false, 5000);
        CHECKPOINT_HOOK.with(|hook| *hook.borrow_mut() = None);
        assert_eq!(result.unwrap_err().code, ErrorCode::ConfigInvalid);
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read(base.join("config.json.v1.bak")).unwrap(), original);
        let retained = fs::read_dir(&base)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| {
                entry
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("config.json.stage-")
            });
        if replace {
            assert_eq!(
                fs::read(retained.unwrap()).unwrap(),
                b"unrelated replacement bytes"
            );
        } else {
            assert!(retained.is_none());
        }
    }
}
