use lwiki as library;
use lwiki::{config::providers::ProviderConfig, domain::ErrorCode, jobs::Capability};
#[path = "fixtures/p16/common.rs"]
mod common;
use common::*;

#[test]
fn cloned_profile_no_auth_or_helper() {
    let (t, fs, path, trusted) = fixture(STATIC);
    let cloned = vault(&t.path().join("clone"));
    let cfg = ProviderConfig::load(&path).unwrap();
    assert_eq!(
        cfg.authorize(&cloned, &id("vault_test"), "primary", Capability::Embed)
            .err()
            .unwrap()
            .code,
        ErrorCode::ProfileUntrusted
    );
    assert_eq!(
        cfg.authorize(&fs, &id("vault_other"), "primary", Capability::Embed)
            .err()
            .unwrap()
            .code,
        ErrorCode::ProfileUntrusted
    );
    assert!(
        cfg.authorize(&fs, &id("vault_test"), "primary", Capability::Generate)
            .is_err()
    );
    assert!(
        cfg.authorize(&fs, &id("vault_test"), "unknown", Capability::Embed)
            .is_err()
    );
    let summary = serde_json::to_string(&trusted.summary()).unwrap();
    assert!(!summary.contains("test-secret"));
    assert!(!summary.contains("https://"));
}
#[test]
fn strict_private_toml_rejects_duplicate_unknown_and_conflicting_sources() {
    let (_t, fs, path, _) = fixture(STATIC);
    for text in [
        document(
            fs.root().path(),
            "kind = \"static\"\nkey=\"SECRET-MARKER\"\nkey_env=\"OTHER\"",
        ),
        document(
            fs.root().path(),
            "kind=\"static\"\nkey=\"SECRET-MARKER\"\nkey=\"again\"",
        ),
        document(
            fs.root().path(),
            "kind=\"static\"\nkey=\"SECRET-MARKER\"\nunknown=\"PRIVATE-MARKER\"",
        ),
        document(fs.root().path(), STATIC).replace("version = 1", "version = 2"),
        document(fs.root().path(), STATIC).replace(
            "adapter = \"embeddings-v1\"",
            "adapter = \"chat-completions-v1\"",
        ),
    ] {
        private_write(&path, text);
        let error = ProviderConfig::load(&path).err().unwrap();
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        let value = serde_json::to_string(&error).unwrap();
        assert!(!value.contains("SECRET-MARKER"));
        assert!(!value.contains("PRIVATE-MARKER"));
        assert!(!value.contains(path.to_str().unwrap()));
    }
}
#[test]
fn full_url_tls_managed_headers_and_capability_policies_are_exact() {
    let (_t, fs, path, trusted) = fixture(STATIC);
    let original = trusted.summary().endpoint_fingerprint;
    let base = document(fs.root().path(), STATIC);
    for url in [
        "http://gateway.example/embeddings",
        "https://user:SECRET@gateway.example/x",
        "https://gateway.example/x#fragment",
        "https://gateway.example/x?api_key=SECRET",
        "file:///private/key",
    ] {
        private_write(
            &path,
            base.replace("https://gateway.example/v1/embeddings?tenant=one", url),
        );
        assert!(ProviderConfig::load(&path).is_err());
    }
    for line in [
        "headers={ Authorization=\"SECRET\" }",
        "headers={ Content-Type=\"application/json\" }",
        "headers={ X-Test=\"one\", x-test=\"two\" }",
        "instruction_role=\"system\"",
        "tls_verify=false",
        "proxy=\"http://localhost\"",
        "redirects=true",
    ] {
        private_write(
            &path,
            base.replace(
                "[services.embed.auth]",
                &format!("{line}\n[services.embed.auth]"),
            ),
        );
        assert!(ProviderConfig::load(&path).is_err());
    }
    private_write(&path, base.replace("tenant=one", "tenant=two"));
    let new = ProviderConfig::load(&path)
        .unwrap()
        .authorize(&fs, &id("vault_test"), "primary", Capability::Embed)
        .unwrap();
    assert_ne!(new.summary().endpoint_fingerprint, original);
    private_write(
        &path,
        base.replace(
            "https://gateway.example/v1/embeddings?tenant=one",
            "http://127.0.0.1:1234/exact",
        )
        .replace(
            "[services.embed.auth]",
            "allow_loopback_http=true\n[services.embed.auth]",
        ),
    );
    assert!(ProviderConfig::load(&path).is_ok());
    private_write(
        &path,
        base.replace(
            "https://gateway.example/v1/embeddings?tenant=one",
            "http://localhost:1234/exact",
        )
        .replace(
            "[services.embed.auth]",
            "allow_loopback_http=true\n[services.embed.auth]",
        ),
    );
    assert!(ProviderConfig::load(&path).is_err());
}
#[test]
fn credential_rotation_does_not_export_secret_hash_or_change_endpoint_identity() {
    let (_t, fs, path, trusted) = fixture(STATIC);
    let before = trusted.summary();
    private_write(
        &path,
        document(
            fs.root().path(),
            "kind=\"static\"\nkey=\"new-private-token\"",
        ),
    );
    let after = ProviderConfig::load(&path)
        .unwrap()
        .authorize(&fs, &id("vault_test"), "primary", Capability::Embed)
        .unwrap()
        .summary();
    assert_eq!(before.endpoint_fingerprint, after.endpoint_fingerprint);
    assert_eq!(before.profile_fingerprint, after.profile_fingerprint);
    assert_eq!(before.config_fingerprint, after.config_fingerprint);
}
#[test]
fn private_config_symlink_sparse_size_and_permissions_refuse() {
    let (t, _fs, path, _) = fixture(STATIC);
    let huge = t.path().join("huge.toml");
    std::fs::File::create(&huge)
        .unwrap()
        .set_len(65537)
        .unwrap();
    assert!(ProviderConfig::load(&huge).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let link = t.path().join("link.toml");
        symlink(&path, &link).unwrap();
        assert!(ProviderConfig::load(&link).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ProviderConfig::load(&path).is_err());
    }
}
#[test]
fn configured_private_ca_content_is_part_of_endpoint_trust() {
    let (t, fs, path, trusted) = fixture(STATIC);
    let ca = t.path().join("ca.pem");
    private_write(&ca, b"TEST-CA-CONTENT");
    let text = document(fs.root().path(), STATIC).replace(
        "[services.embed.auth]",
        &format!(
            "ca_file={}\n[services.embed.auth]",
            quote(ca.to_str().unwrap())
        ),
    );
    private_write(&path, &text);
    let cfg = ProviderConfig::load(&path).unwrap();
    let authority = cfg
        .authorize(&fs, &id("vault_test"), "primary", Capability::Embed)
        .unwrap();
    assert_ne!(
        authority.summary().endpoint_fingerprint,
        trusted.summary().endpoint_fingerprint
    );
    private_write(&ca, b"CHANGED-CA");
    assert!(
        cfg.authorize(&fs, &id("vault_test"), "primary", Capability::Embed)
            .is_err()
    );
}

#[test]
fn new_generation_defaults_to_responses_and_chat_settings_are_explicit() {
    let (_t, fs, path, _) = fixture(STATIC);
    let base = document(fs.root().path(), STATIC)
        .replace("embedding = \"embed\"", "generation = \"embed\"")
        .replace("adapter = \"embeddings-v1\"\n", "")
        .replace("max_batch_items = 32\nmax_batch_bytes = 262144\n", "")
        .replace("/v1/embeddings?tenant=one", "/v1/responses");
    private_write(&path, &base);
    let implicit = ProviderConfig::load(&path)
        .unwrap()
        .authorize(&fs, &id("vault_test"), "primary", Capability::Generate)
        .unwrap()
        .summary();
    let explicit = base.replace(
        "[services.embed]\n",
        "[services.embed]\nadapter = \"responses-v1\"\n",
    );
    private_write(&path, &explicit);
    let summary = ProviderConfig::load(&path)
        .unwrap()
        .authorize(&fs, &id("vault_test"), "primary", Capability::Generate)
        .unwrap()
        .summary();
    assert_eq!(implicit.profile_fingerprint, summary.profile_fingerprint);
    for extra in [
        "instruction_role = \"system\"",
        "output_limit_field = \"max_tokens\"",
    ] {
        private_write(
            &path,
            explicit.replace(
                "[services.embed.auth]",
                &format!("{extra}\n[services.embed.auth]"),
            ),
        );
        assert!(ProviderConfig::load(&path).is_err());
    }
    let chat = explicit.replace("responses-v1", "chat-completions-v1")
        .replace("/v1/responses", "/v1/chat/completions")
        .replace("[services.embed.auth]", "instruction_role = \"system\"\noutput_limit_field = \"max_tokens\"\n[services.embed.auth]");
    private_write(&path, chat);
    assert!(
        ProviderConfig::load(&path)
            .unwrap()
            .authorize(&fs, &id("vault_test"), "primary", Capability::Generate)
            .is_ok()
    );
}
