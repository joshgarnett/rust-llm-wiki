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
        Path::new("/chosen/lwiki/config.toml")
    );
    assert_eq!(
        ambient_path(None, Some(OsStr::new("/home"))).unwrap(),
        Path::new("/home/.config/lwiki/config.toml")
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
    assert_eq!(loaded.document().schema_version, "3");
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

#[test]
fn ambient_toml_wins_legacy_json_only_falls_back_when_toml_is_absent() {
    let (_temp, base) = fixture();
    let json = config(&base, v1());
    let original = fs::read(&json).unwrap();
    let toml = base.join("config.toml");
    let legacy = load_ambient(&toml).unwrap();
    assert_eq!(legacy.path(), Some(json.as_path()));
    assert_eq!(legacy.document().defaults.read_max_bytes, Some(123));
    fs::write(
        &toml,
        "schema_version = \"3\"\n[defaults]\nread_max_bytes = 456\n",
    )
    .unwrap();
    let loaded = load_ambient(&toml).unwrap();
    assert_eq!(loaded.path(), Some(toml.as_path()));
    assert_eq!(loaded.document().defaults.read_max_bytes, Some(456));
    assert_eq!(loaded.summary().format, Some(UserConfigFormat::Toml));
    assert_eq!(loaded.summary().shadowed_legacy_path, Some(json.clone()));
    assert_eq!(loaded.summary().warnings.len(), 1);
    fs::write(
        &toml,
        "schema_version = \"3\"\n[defaults]\nread_max_bytes = 0\n",
    )
    .unwrap();
    assert_eq!(
        load_ambient(&toml).unwrap_err().code,
        ErrorCode::ConfigInvalid
    );
    assert_eq!(fs::read(json).unwrap(), original);
}

#[test]
fn legacy_json_setter_stays_v2_and_extended_fields_require_v3() {
    let (_temp, base) = fixture();
    for version in ["1", "2"] {
        for field in ["providers_config", "embeddings", "remote"] {
            let path = base.join("config.json");
            // Build a real dynamic property, including empty tables old schemas cannot contain.
            let value = serde_json::json!({"schema_version":version});
            let mut value = value.as_object().unwrap().clone();
            value.insert(
                field.into(),
                if field == "providers_config" {
                    serde_json::Value::Null
                } else {
                    serde_json::json!({})
                },
            );
            let bytes = serde_json::to_vec(&value).unwrap();
            fs::write(&path, &bytes).unwrap();
            assert!(load_user_config(Some(&path), false).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
    let path = config(&base, v1());
    let loaded = load_user_config(Some(&path), false).unwrap();
    update_default(&loaded, None, false, 5000).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["schema_version"], "2");
    for field in ["providers_config", "embeddings", "remote"] {
        assert!(value.get(field).is_none());
    }
    assert_eq!(
        load_user_config(Some(&path), false)
            .unwrap()
            .document()
            .schema_version,
        "2"
    );
}

#[test]
fn supported_json_migration_preserves_all_preferences_and_original_bytes() {
    for version in ["1", "2"] {
        let (_temp, base) = fixture();
        let mut value = v1();
        value["schema_version"] = version.into();
        let path = config(&base, value);
        let original = fs::read(&path).unwrap();
        let loaded = load_user_config(Some(&path), false).unwrap();
        let target = loaded.toml_migration_target().unwrap();
        let mut proposed = loaded.document().clone();
        proposed.providers_config = Some(base.join("providers.toml"));
        proposed.embeddings.query_prefix = Some("検索: ".into());
        proposed.embeddings.max_input_bytes = Some(16000);
        proposed.remote.max_requests = Some(7);
        proposed.remote.max_input_units = Some(0);
        proposed.remote.max_cost = Some("0.125".into());
        let preview =
            update_document(&loaded, proposed.clone(), Some(&target), true, 5000).unwrap();
        assert!(preview.changed && !preview.applied && !preview.durable);
        assert!(!target.exists());
        assert!(!path.with_file_name("config.json.lock").exists());
        assert!(!preview.preserved_original_path.as_ref().unwrap().exists());
        let applied =
            update_document(&loaded, proposed.clone(), Some(&target), false, 5000).unwrap();
        assert!(applied.applied && applied.durable && applied.failure.is_none());
        assert_eq!(
            fs::read(applied.preserved_original_path.unwrap()).unwrap(),
            original
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        let migrated = load_user_config(Some(&target), false).unwrap();
        assert_eq!(migrated.document().schema_version, "3");
        assert_eq!(migrated.document().defaults.read_max_bytes, Some(123));
        assert_eq!(
            migrated.document().vaults[&RecordId::new("Vault.First").unwrap()].lock_timeout_ms,
            Some(42)
        );
        assert_eq!(migrated.document().allowed_profiles, ["local"]);
        assert_eq!(
            migrated.document().embeddings.query_prefix.as_deref(),
            Some("検索: ")
        );
        let before = fs::read(&target).unwrap();
        let retry = update_document(&loaded, proposed, Some(&target), false, 5000).unwrap();
        assert!(retry.applied && retry.durable && retry.failure.is_none());
        assert_eq!(fs::read(&target).unwrap(), before);
        assert_eq!(
            load_ambient(&target)
                .unwrap()
                .summary()
                .shadowed_legacy_path,
            Some(path)
        );
    }
}

#[test]
fn migration_destination_and_backup_conflicts_refuse_before_any_write() {
    for backup_collision in [false, true] {
        let (_temp, base) = fixture();
        let path = config(&base, v1());
        let original = fs::read(&path).unwrap();
        let loaded = load_user_config(Some(&path), false).unwrap();
        let target = base.join("config.toml");
        let collision = if backup_collision {
            base.join("config.json.v1.bak")
        } else {
            target.clone()
        };
        fs::write(&collision, b"author's original configuration").unwrap();
        for dry_run in [true, false] {
            let error = update_document(
                &loaded,
                loaded.document().clone(),
                Some(&target),
                dry_run,
                5000,
            )
            .unwrap_err();
            assert_eq!(error.code, ErrorCode::ContentConflict);
            assert_eq!(fs::read(&path).unwrap(), original);
            assert_eq!(
                fs::read(&collision).unwrap(),
                b"author's original configuration"
            );
            assert!(!base.join("config.json.lock").exists());
            assert!(!base.join("config.toml.lock").exists());
        }
    }
}

#[test]
fn explicit_setup_destination_is_missing_without_creating_anything() {
    let (_temp, base) = fixture();
    let path = base.join("new/settings.toml");
    assert!(load_user_config(Some(&path), false).is_err());
    let loaded = load_user_config_for_setup(Some(&path), false).unwrap();
    assert!(!loaded.summary().present);
    assert_eq!(loaded.summary().origin, UserConfigOrigin::Explicit);
    let mut proposed = loaded.document().clone();
    proposed.embeddings.document_prefix = Some("document: ".into());
    assert!(
        !update_document(&loaded, proposed.clone(), None, true, 5000)
            .unwrap()
            .applied
    );
    assert!(!base.join("new").exists());
    let result = update_document(&loaded, proposed, None, false, 5000).unwrap();
    assert!(result.applied && result.durable);
    assert_eq!(
        load_user_config(Some(&path), false)
            .unwrap()
            .document()
            .embeddings
            .document_prefix
            .as_deref(),
        Some("document: ")
    );
}

#[test]
fn extended_toml_validation_is_strict_bounded_and_redacts_parse_input() {
    let (_temp, base) = fixture();
    let path = base.join("config.toml");
    for text in [
        "schema_version = \"1\"\n",
        "schema_version = \"2\"\n",
        "schema_version = \"99\"\n",
        "schema_version = \"3\"\nschema_version = \"3\"\n",
        "schema_version = \"3\"\nproviders_config = \"relative.toml\"\n",
        "schema_version = \"3\"\n[defaults]\nproviders_config = \"/private/provider.toml\"\n",
        "schema_version = \"3\"\n[vaults.test]\nremote = {}\n",
        "schema_version = \"3\"\n[embeddings]\nmax_input_bytes = 0\n",
        "schema_version = \"3\"\n[embeddings]\nmax_input_bytes = 131073\n",
        "schema_version = \"3\"\n[embeddings]\nmax_input_bytes = 20\nquality_target_bytes = 21\n",
        "schema_version = \"3\"\n[remote]\nmax_requests = 0\n",
        "schema_version = \"3\"\n[remote]\nconcurrency = 0\n",
        "schema_version = \"3\"\n[remote]\ndeadline_ms = 86400001\n",
        "schema_version = \"3\"\n[remote]\ncurrency = \"usd\"\n",
        "schema_version = \"3\"\n[remote]\nmax_cost = \"-1\"\n",
        "schema_version = \"3\"\n[remote]\nmax_cost = \"0.0000000001\"\n",
        "schema_version = \"3\"\n[remote]\nretry_uncertain = true\n",
        "schema_version = \"3\"\nsecret-sentinel-not-for-errors = \"synthetic-key\"\n",
    ] {
        fs::write(&path, text).unwrap();
        let error = load_user_config(Some(&path), false).unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert!(!format!("{error:?}").contains("secret-sentinel-not-for-errors"));
        assert!(!format!("{error:?}").contains("synthetic-key"));
        assert_eq!(fs::read(&path).unwrap(), text.as_bytes());
    }
    fs::write(&path, br#"{"schema_version":"3"}"#).unwrap();
    assert!(load_user_config(Some(&path), false).is_err());
    let json = base.join("config.json");
    fs::write(&json, "schema_version = \"3\"\n").unwrap();
    assert!(load_user_config(Some(&json), false).is_err());
}

#[test]
fn document_report_records_publication_before_a_later_failure() {
    let (_temp, base) = fixture();
    let path = base.join("config.toml");
    let loaded = load_user_config_for_setup(Some(&path), false).unwrap();
    let mut proposed = loaded.document().clone();
    proposed.remote.max_requests = Some(4);
    CHECKPOINT_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|stage, _| {
            if stage == "published" {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "injected later failure",
                ))
            } else {
                Ok(())
            }
        }))
    });
    let report = update_document(&loaded, proposed, None, false, 5000).unwrap();
    CHECKPOINT_HOOK.with(|hook| *hook.borrow_mut() = None);
    assert!(report.applied && report.durable && report.failure.is_some());
    assert_eq!(
        load_user_config(Some(&path), false)
            .unwrap()
            .document()
            .remote
            .max_requests,
        Some(4)
    );
}

#[test]
fn identical_document_retry_syncs_existing_file_and_parent_after_failed_publication_sync() {
    let (_temp, base) = fixture();
    let path = base.join("config.toml");
    let loaded = load_user_config_for_setup(Some(&path), false).unwrap();
    let mut proposed = loaded.document().clone();
    proposed.remote.max_requests = Some(3);
    super::super::local_io::SYNC_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|phase, _| {
            if phase == "publication_directory" {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "injected publication sync failure",
                ))
            } else {
                Ok(())
            }
        }))
    });
    let published = update_document(&loaded, proposed.clone(), None, false, 5000);
    super::super::local_io::SYNC_HOOK.with(|hook| *hook.borrow_mut() = None);
    let published = published.unwrap();
    assert!(published.applied && !published.durable && published.failure.is_some());
    let original = fs::read(&path).unwrap();
    let loaded = load_user_config(Some(&path), false).unwrap();
    let phases = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded = phases.clone();
    super::super::local_io::SYNC_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |phase, _| {
            recorded.borrow_mut().push(phase.to_string());
            Ok(())
        }))
    });
    let retry = update_document(&loaded, proposed, None, false, 5000);
    super::super::local_io::SYNC_HOOK.with(|hook| *hook.borrow_mut() = None);
    let retry = retry.unwrap();
    assert!(!retry.changed && !retry.applied && retry.durable && retry.failure.is_none());
    assert_eq!(*phases.borrow(), ["existing_file", "directory"]);
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn unchanged_document_sync_failure_is_an_explicit_failed_report_and_preview_never_syncs() {
    let (_temp, base) = fixture();
    let path = base.join("config.toml");
    let loaded = load_user_config_for_setup(Some(&path), false).unwrap();
    update_document(&loaded, loaded.document().clone(), None, false, 5000).unwrap();
    let loaded = load_user_config(Some(&path), false).unwrap();
    super::super::local_io::SYNC_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|_, _| {
            Err(WikiError::new(
                ErrorCode::Internal,
                "injected existing sync failure",
            ))
        }))
    });
    let preview = update_document(&loaded, loaded.document().clone(), None, true, 5000).unwrap();
    assert!(!preview.applied && !preview.durable && preview.failure.is_none());
    let result = update_document(&loaded, loaded.document().clone(), None, false, 5000);
    super::super::local_io::SYNC_HOOK.with(|hook| *hook.borrow_mut() = None);
    let result = result.unwrap();
    assert!(!result.applied && !result.durable && result.failure.is_some());
}

#[test]
fn scoped_provider_paths_round_trip_resolve_and_migrate_without_affecting_other_wikis() {
    let (_temp, base) = fixture();
    let a = RecordId::new("Vault.A").unwrap();
    let b = RecordId::new("Vault.B").unwrap();
    let mut document = LocalConfigDocument {
        providers_config: Some(base.join("A providers.toml")),
        ..Default::default()
    };
    document.vaults.insert(
        b.clone(),
        LocalPreferences {
            profile: Some("primary".into()),
            providers_config: Some(base.join("B providers.toml")),
            read_max_bytes: Some(234),
            ..Default::default()
        },
    );
    for extension in ["toml", "json"] {
        let path = base.join(format!("scoped.{extension}"));
        let bytes = serialize_document(&path, &document).unwrap();
        fs::write(&path, &bytes).unwrap();
        let loaded = load_user_config(Some(&path), true).unwrap();
        let saved = loaded.document();
        assert_eq!(
            saved.saved_provider_config(Some(&b)),
            Some((base.join("B providers.toml").as_path(), "user_vault"))
        );
        assert_eq!(
            saved.saved_provider_config(Some(&a)),
            Some((base.join("A providers.toml").as_path(), "user_config"))
        );
        assert_eq!(
            saved.saved_provider_config(None),
            saved.saved_provider_config(Some(&a))
        );
        if extension == "json" {
            let target = loaded.toml_migration_target().unwrap();
            update_document(&loaded, saved.clone(), Some(&target), false, 5000).unwrap();
            let migrated = load_user_config(Some(&target), false).unwrap();
            assert_eq!(
                serde_json::to_value(migrated.document()).unwrap(),
                serde_json::to_value(&document).unwrap()
            );
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert_eq!(
                fs::read(path.with_file_name("scoped.json.v3.bak")).unwrap(),
                bytes
            );
        }
    }
    let absent =
        serialize_document(&base.join("absent.json"), &LocalConfigDocument::default()).unwrap();
    let absent: serde_json::Value = serde_json::from_slice(&absent).unwrap();
    assert!(absent["defaults"].get("providers_config").is_none());
}

#[test]
fn scoped_provider_paths_reject_legacy_presence_default_scope_and_invalid_paths() {
    let (_temp, base) = fixture();
    let path = base.join("scope.json");
    for version in ["1", "2"] {
        for value in [
            serde_json::Value::Null,
            serde_json::json!(base.join("private.toml")),
        ] {
            let bytes = serde_json::to_vec(&serde_json::json!({"schema_version":version,"vaults":{"Vault.B":{"providers_config":value}}})).unwrap();
            fs::write(&path, &bytes).unwrap();
            assert!(load_user_config(Some(&path), false).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
    for invalid in [
        "relative.toml".to_owned(),
        base.join("../private.toml").to_str().unwrap().to_owned(),
        format!("/{}", "x".repeat(4096)),
        format!("/{}file.toml", "x/".repeat(65)),
    ] {
        fs::write(&path, serde_json::to_vec(&serde_json::json!({"schema_version":"3","vaults":{"Vault.B":{"providers_config":invalid}}})).unwrap()).unwrap();
        assert!(load_user_config(Some(&path), false).is_err());
    }
    for value in [
        serde_json::Value::Null,
        serde_json::json!(base.join("private.toml")),
    ] {
        fs::write(
            &path,
            serde_json::to_vec(
                &serde_json::json!({"schema_version":"3","defaults":{"providers_config":value}}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(load_user_config(Some(&path), false).is_err());
    }
    let old = config(&base, serde_json::json!({"schema_version":"2"}));
    let loaded = load_user_config(Some(&old), false).unwrap();
    let mut proposed = loaded.document().clone();
    proposed
        .vaults
        .entry(RecordId::new("Vault.B").unwrap())
        .or_default()
        .providers_config = Some(base.join("private.toml"));
    assert!(update_document(&loaded, proposed.clone(), None, false, 5000).is_err());
    assert!(serialize_document(&old, &proposed).is_err());
    let target = loaded.toml_migration_target().unwrap();
    update_document(&loaded, proposed, Some(&target), false, 5000).unwrap();
    assert!(
        load_user_config(Some(&target), false)
            .unwrap()
            .document()
            .saved_provider_config(Some(&RecordId::new("Vault.B").unwrap()))
            .is_some()
    );
}

#[test]
fn portable_preferences_cannot_select_a_private_provider_file() {
    let (_temp, base) = fixture();
    let selected = wiki(&base, "portable", "Vault.Portable");
    let marker = selected.path.join("WIKI.md");
    let bytes = fs::read_to_string(&marker).unwrap().replace(
        "wiki_kind: vault",
        "wiki_kind: vault\nlwiki_providers_config: /private/provider.toml",
    );
    fs::write(&marker, bytes).unwrap();
    let fs = VaultFs::new(crate::vault::VaultRoot::explicit(&selected.path).unwrap());
    let loaded = load_user_config(None, true).unwrap();
    assert!(
        resolve_loaded(
            &fs,
            &selected.wiki_id,
            &PreferenceOptions::default(),
            &loaded
        )
        .is_err()
    );
}
