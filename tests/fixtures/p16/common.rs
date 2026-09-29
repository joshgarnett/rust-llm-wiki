use super::library::{
    config::providers::{ProviderConfig, TrustedService},
    domain::*,
    jobs::Capability,
    vault::{VaultFs, VaultRoot},
};
use std::path::{Path, PathBuf};
pub fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
pub fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}
pub fn private_write(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}
pub fn vault(path: &Path) -> VaultFs {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("WIKI.md"),b"---\nwiki_schema: \"1\"\nwiki_id: vault_test\nwiki_kind: vault\ntitle: Provider tests\n---\n").unwrap();
    VaultFs::new(VaultRoot::explicit(path).unwrap())
}
pub fn document(root: &Path, auth: &str) -> String {
    format!(
        "version = 1\n[profiles.primary]\nembedding = \"embed\"\n[services.embed]\nadapter = \"embeddings-v1\"\nurl = \"https://gateway.example/v1/embeddings?tenant=one\"\nmodel = \"test-model\"\nrevision = \"r1\"\nmax_batch_items = 32\nmax_batch_bytes = 262144\n[services.embed.auth]\n{auth}\n[vault_bindings.main]\nroot = {}\nwiki_id = \"vault_test\"\nallowed_profiles = [\"primary\"]\n",
        quote(root.to_str().unwrap())
    )
}
pub fn fixture(auth: &str) -> (tempfile::TempDir, VaultFs, PathBuf, TrustedService) {
    let t = tempfile::tempdir().unwrap();
    let fs = vault(&t.path().join("vault"));
    let config = t.path().join("providers.toml");
    private_write(&config, document(fs.root().path(), auth));
    let trusted = ProviderConfig::load(&config)
        .unwrap()
        .authorize(&fs, &id("vault_test"), "primary", Capability::Embed)
        .unwrap();
    (t, fs, config, trusted)
}
pub const STATIC: &str = "kind = \"static\"\nkey = \"test-secret\"";
