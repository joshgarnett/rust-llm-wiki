//! Missing Page-specific application-boundary acceptance over disposable vaults.
//! Root registers this module; no production provider or user vault is accessed.
use crate::app::{OfflineApp, OperationOptions, PageBatchRequest, PageUpdate, offline::init};
use crate::{
    catalog::{
        Catalog,
        normalized_delta::CatalogDelta,
        query_types::{QueryCatalog, QueryReadLimits},
        source_projection::RefreshProjectionLimits,
        write_projection,
    },
    changes::{ChangeEngine, ReadDependency, indexed_refresh::IndexedRefreshProof},
    domain::{Blake3Hash, ErrorCode, ReadSnapshot, VaultRelativePath},
    vault::{ExpectedState, VaultFs, VaultRoot, WriterPermit},
};
use clap::Parser;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn options() -> OperationOptions {
    OperationOptions {
        offline: true,
        lock_timeout_ms: 200,
        ..Default::default()
    }
}
fn page(id: &str, body: &str) -> String {
    format!(
        "---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: {id}\ntitle: {id}\nwiki_status: reviewed\n---\n{body}\n"
    )
}
struct PageAppFixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    app: OfflineApp,
}
struct PageAppFailureEvidence(PathBuf);
impl Drop for PageAppFailureEvidence {
    fn drop(&mut self) {
        if std::thread::panicking() && self.0.exists() {
            let preserved = self.0.with_extension("page-app-failed");
            match fs::rename(&self.0, &preserved) {
                Ok(()) => eprintln!("PAGE_FAILED_FIXTURE {}", preserved.display()),
                Err(error) => eprintln!("PAGE_FAILED_FIXTURE preservation failed: {error}"),
            }
        }
    }
}
impl PageAppFixture {
    fn new(retained: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Page acceptance vault with spaces");
        init(&root, "Page acceptance", options()).unwrap();
        let handle = VaultFs::new(VaultRoot::explicit(&root).unwrap());
        if retained {
            let writer = WriterPermit::acquire(handle.root(), Duration::ZERO).unwrap();
            crate::storage::cleanup(&handle, &writer, &crate::storage::StorageOptions::default())
                .unwrap();
        }
        fs::write(
            root.join("control.md"),
            page("Page.Control", "unchanged control body"),
        )
        .unwrap();
        fs::write(
            root.join("write.md"),
            page("Page.Write", "original author body"),
        )
        .unwrap();
        let app = OfflineApp::new(handle, options()).unwrap();
        app.index_rebuild_normalized().unwrap();
        Self {
            _temp: temp,
            root,
            app,
        }
    }
    fn catalog(&self) -> Catalog {
        Catalog::new(self.app.fs().clone(), self.app.vault_id().clone())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PreservedEntry {
    bytes: Vec<u8>,
    modified: SystemTime,
    directory: bool,
}
fn preserved_tree(root: &Path) -> BTreeMap<PathBuf, PreservedEntry> {
    fn visit(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, PreservedEntry>) {
        for entry in fs::read_dir(at).unwrap() {
            let p = entry.unwrap().path();
            let meta = fs::symlink_metadata(&p).unwrap();
            assert!(!meta.file_type().is_symlink());
            let directory = meta.is_dir();
            out.insert(
                p.strip_prefix(root).unwrap().to_path_buf(),
                PreservedEntry {
                    bytes: if directory {
                        vec![]
                    } else {
                        fs::read(&p).unwrap()
                    },
                    modified: meta.modified().unwrap(),
                    directory,
                },
            );
            if directory {
                visit(root, &p, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

const PAGE_FORBIDDEN_PROVIDER_ACCESS: [&str; 4] = [
    "credential_input",
    "credential_helper",
    "provider_dispatch",
    "provider_send",
];
const PAGE_FORBIDDEN_DRY_ACCESS: [&str; 8] = [
    "canonical_scan",
    "writer_acquire",
    "catalog_maintenance",
    "catalog_open",
    "credential_input",
    "credential_helper",
    "provider_dispatch",
    "provider_send",
];

fn index_arguments(root: &Path, dry: bool, rebuild: bool) -> crate::cli::Arguments {
    let mut argv = vec![
        "lwiki".to_owned(),
        "--offline".into(),
        "--json".into(),
        "--lock-timeout-ms".into(),
        "200".into(),
        "--wiki".into(),
        root.to_str().unwrap().into(),
    ];
    if dry {
        argv.push("--dry-run".into());
    }
    argv.push("index".into());
    argv.push(if rebuild { "rebuild" } else { "sync" }.into());
    if rebuild {
        argv.push("--normalized".into());
    }
    crate::cli::Arguments::try_parse_from(argv).unwrap()
}

fn assert_real_access_boundary(boundary: &'static str, action: impl FnOnce()) {
    let guard = crate::catalog::query_diagnostics::forbid_access(&[boundary]);
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action));
    let attempted = guard.finish();
    assert!(
        stopped.is_err(),
        "real boundary {boundary} did not abort before action"
    );
    assert_eq!(
        attempted.last(),
        Some(&boundary),
        "real {boundary} wiring was not observed"
    );
    let payload = stopped.unwrap_err();
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("");
    assert!(
        message.contains(&format!("forbidden maintenance access: {boundary}")),
        "positive control stopped for a different reason"
    );
}

#[test]
fn external_page_access_boundary_positive_controls() {
    use crate::providers::credentials::{
        HelperInvocation, HelperLimits, HelperRunner, NativeCredentialClock, NativeHelperRunner,
        NativeSecretInputs, SecretInputs,
    };
    let f = PageAppFixture::new(false);
    let _failure_evidence = PageAppFailureEvidence(f._temp.path().to_path_buf());
    assert_real_access_boundary("canonical_scan", || {
        let _ = f.app.fs().root().scan_markdown();
    });
    assert_real_access_boundary("writer_acquire", || {
        let _ = WriterPermit::acquire(f.app.fs().root(), Duration::ZERO);
    });
    let writer = WriterPermit::acquire(f.app.fs().root(), Duration::ZERO).unwrap();
    assert_real_access_boundary("catalog_maintenance", || {
        let _ = f.catalog().sync_normalized(&writer);
    });
    drop(writer);
    assert_real_access_boundary("catalog_open", || {
        let _ = f
            .catalog()
            .cached_query_snapshot(QueryReadLimits::default());
    });
    // Both real credential input branches abort BEFORE reading environment/file.
    assert_real_access_boundary("credential_input", || {
        let _ = NativeSecretInputs.environment("LWIKI_PAGE_FORBIDDEN_CREDENTIAL_INPUT", 64);
    });
    assert_real_access_boundary("credential_input", || {
        let _ = NativeSecretInputs.file(&f.root.join("must-not-open-credential"), 64);
    });
    let invocation = HelperInvocation::new(
        vec![
            f.root
                .join("must-not-spawn-helper")
                .to_str()
                .unwrap()
                .into(),
        ],
        f.root.clone(),
    )
    .unwrap();
    let limits = HelperLimits {
        timeout_ms: 1,
        max_stdout_bytes: 64,
        deadline_utc_ms: i64::MAX,
    };
    assert_real_access_boundary("credential_helper", || {
        let _ = NativeHelperRunner.run(
            &invocation,
            &limits,
            &NativeCredentialClock::default(),
            &crate::jobs::CancellationToken::default(),
        );
    });
    // Root separately owns the private authentic provider_dispatch/provider_send
    // positive controls; this module does not fabricate their wiring evidence.
}

#[test]
fn external_page_sync_dry_run_forbidden_access_both_layouts() {
    for retained in [false, true] {
        let f = PageAppFixture::new(retained);
        let _failure_evidence = PageAppFailureEvidence(f._temp.path().to_path_buf());
        fs::write(
            f.root.join("write.md"),
            page("Page.Write", "externally changed pending Page body"),
        )
        .unwrap();
        let catalog = f.catalog();
        let selected = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap()
            .snapshot()
            .clone();
        let authority = catalog.operation_state().unwrap().unwrap();
        assert!(authority.active().is_none());
        for rebuild in [false, true] {
            let before = preserved_tree(&f.root);
            let args = index_arguments(&f.root, true, rebuild);
            let guard =
                crate::catalog::query_diagnostics::forbid_access(&PAGE_FORBIDDEN_DRY_ACCESS);
            let (envelope, exit) = crate::cli::execute(&args);
            let attempted = guard.finish();
            assert_eq!(exit, 0, "dry index returned error: {envelope:?}");
            assert!(envelope.ok);
            assert!(!envelope.meta.network_used);
            assert!(
                attempted.is_empty(),
                "dry-run entered named forbidden graph: {attempted:?}"
            );
            assert_eq!(envelope.data["dry_run"], true);
            assert_eq!(envelope.data["cache_state_unknown"], true);
            assert_eq!(
                envelope.data["maintenance"]["canonical_scan_performed"],
                false
            );
            assert!(envelope.data["report"].is_null());
            // Check bytes/mtime first: subsequent reader acquisition can itself
            // update WAL coordination, and must not contaminate this observation.
            assert!(
                preserved_tree(&f.root) == before,
                "dry-run changed full vault bytes or mtime"
            );
            assert_eq!(catalog.operation_state().unwrap().unwrap(), authority);
            assert_eq!(
                catalog
                    .cached_query_snapshot(QueryReadLimits::default())
                    .unwrap()
                    .snapshot(),
                &selected
            );
            println!(
                "PAGE_MODE {}",
                json!({"layout":if retained {"retained"} else {"original"},
                "mode":if rebuild {"dry_rebuild_normalized"} else {"dry_sync"},
                "named_access_attempts":attempted,"full_byte_mtime_preserved":true,
                "authority_snapshot_pending_preserved":true,"envelope":envelope})
            );
        }
    }
}

#[test]
fn external_page_offline_provider_and_credential_sentinel_both_layouts() {
    for retained in [false, true] {
        let f = PageAppFixture::new(retained);
        let _failure_evidence = PageAppFailureEvidence(f._temp.path().to_path_buf());
        let catalog = f.catalog();
        let initial = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap()
            .snapshot()
            .clone();
        let authority_before = catalog.operation_state().unwrap().unwrap();
        assert!(authority_before.active().is_none());
        let proposed = page("Page.Write", "ordinary offline externally changed body");
        fs::write(f.root.join("write.md"), &proposed).unwrap();
        let before = preserved_tree(&f.root);
        let args = index_arguments(&f.root, false, false);
        let guard =
            crate::catalog::query_diagnostics::forbid_access(&PAGE_FORBIDDEN_PROVIDER_ACCESS);
        let (envelope, exit) = crate::cli::execute(&args);
        let attempted = guard.finish();
        assert_eq!(exit, 0, "offline Page sync returned error: {envelope:?}");
        assert!(envelope.ok);
        assert!(!envelope.meta.network_used);
        assert!(
            PAGE_FORBIDDEN_PROVIDER_ACCESS
                .iter()
                .all(|name| !attempted.contains(name))
        );
        assert!(attempted.contains(&"writer_acquire"));
        assert!(attempted.contains(&"catalog_maintenance"));
        assert!(attempted.contains(&"catalog_open"));
        assert_eq!(envelope.data["dry_run"], false);
        assert!(
            envelope.data["maintenance"]["page_sync"].is_object(),
            "must execute actual selected Page reconciliation"
        );
        let after = preserved_tree(&f.root);
        // Changes cannot be fabricated for an external Page reconciliation;
        // all canonical and existing retained Change byte/mtime entries remain.
        let preserved = |tree: BTreeMap<PathBuf, PreservedEntry>| {
            tree.into_iter()
                .filter(|(p, _)| !p.starts_with(".wiki"))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(preserved(after), preserved(before));
        assert_eq!(
            fs::read(f.root.join("write.md")).unwrap(),
            proposed.as_bytes()
        );
        let authority_after = catalog.operation_state().unwrap().unwrap();
        assert!(authority_after.active().is_none());
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        assert!(reader.snapshot().generation > initial.generation);
        assert_eq!(
            reader.snapshot().generation,
            authority_after.publication().epoch
        );
        assert_eq!(
            reader.snapshot().publication().unwrap().file_id,
            authority_after.publication().file_id
        );
        assert!(
            reader
                .document(&path("write.md"))
                .unwrap()
                .unwrap()
                .raw_text
                .contains("ordinary offline externally changed body")
        );
        assert_eq!(
            envelope.data["report"]["snapshot"],
            serde_json::to_value(reader.snapshot()).unwrap()
        );
        println!(
            "PAGE_MODE {}",
            json!({"layout":if retained {"retained"} else {"original"},
            "mode":"offline_sync","named_access_attempts":attempted,"provider_helper_calls":0,
            "canonical_change_bytes_mtime_preserved":true,"no_pending_operation":true,"envelope":envelope})
        );
    }
}

#[test]
fn external_page_public_apply_managed_delete_refuses_both_layouts() {
    for retained in [false, true] {
        let f = PageAppFixture::new(retained);
        let _failure_evidence = PageAppFailureEvidence(f._temp.path().to_path_buf());
        let catalog = f.catalog();
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let selected = reader.snapshot().clone();
        let control = reader.document(&path("control.md")).unwrap().unwrap();
        let retirement = write_projection::project_external_pages(
            f.app.fs(),
            &reader,
            &[(path("control.md"), Some(control.clone()), None)],
            &RefreshProjectionLimits::default(),
        )
        .unwrap();
        retirement.validate().unwrap();
        drop(reader);
        let staged = OfflineApp::new(
            f.app.fs().clone(),
            OperationOptions {
                stage_only: true,
                ..options()
            },
        )
        .unwrap()
        .page_batch(PageBatchRequest {
            title: "Legitimate retained Page edit".into(),
            pages: vec![PageUpdate {
                path: path("write.md"),
                markdown: page("Page.Write", "intended author body"),
                if_match: Some(Blake3Hash::digest(
                    fs::read(f.root.join("write.md")).unwrap(),
                )),
            }],
            read_preconditions: vec![ReadDependency {
                path: path("control.md"),
                expected: ExpectedState::Hash(control.hash),
            }],
        })
        .unwrap();
        let change = staged.change.unwrap();
        let engine = ChangeEngine::new(f.app.fs().clone()).unwrap();
        let mut proof: IndexedRefreshProof =
            engine.load_indexed_refresh_proof(&change).unwrap().unwrap();
        let delta_path = crate::catalog::source_refresh::delta_path(&change).unwrap();
        let delta_file = f.app.fs().root().resolve(&delta_path).unwrap();
        let mut envelope: Value = serde_json::from_slice(&fs::read(&delta_file).unwrap()).unwrap();
        let mut rows: CatalogDelta = serde_json::from_value(envelope["rows"].clone()).unwrap();
        rows.documents.extend(retirement.documents);
        rows.claims.extend(retirement.claims);
        rows.diagnostics.extend(retirement.diagnostics);
        rows.links.extend(retirement.links);
        let retired_facts = retirement.facts.unwrap();
        let facts = rows.facts.as_mut().unwrap();
        facts.links.extend(retired_facts.links);
        facts
            .policy
            .as_mut()
            .unwrap()
            .retired_owners
            .extend(retired_facts.policy.unwrap().retired_owners);
        rows.validate().unwrap();
        envelope["rows"] = serde_json::to_value(rows).unwrap();
        let bytes = serde_json::to_vec(&envelope).unwrap();
        proof.delta_hash = Blake3Hash::digest(&bytes);
        let version = envelope["version"].as_u64().unwrap();
        let intended_hash = Blake3Hash::digest(
            serde_json::to_vec(&(
                if version <= 2 {
                    "lwiki.source-refresh-publication.v1"
                } else {
                    "lwiki.normalized-write-publication.v1"
                },
                &proof.base,
                &proof.change,
                &proof.delta_hash,
            ))
            .unwrap(),
        );
        proof.intended = ReadSnapshot::published(
            proof.base.generation + 1,
            proof.base.parser_fingerprint.clone(),
            proof.base.publication().unwrap().file_id.clone(),
            intended_hash,
        )
        .unwrap();
        let baseline = crate::changes::indexed_refresh::baseline_path(&change).unwrap();
        let baseline_file = f.app.fs().root().resolve(&baseline).unwrap();
        let checksum = Blake3Hash::digest(serde_json::to_vec(&proof).unwrap());
        fs::write(&delta_file, bytes).unwrap();
        fs::write(
            &baseline_file,
            serde_json::to_vec(&json!({"proof":proof,"checksum":checksum})).unwrap(),
        )
        .unwrap();
        // Checksum-authenticated, exact retained baseline/manifest: refusal must
        // reach the operation-specific guard, not fail an unrelated byte hash.
        let authenticated = engine.load_indexed_refresh_proof(&change).unwrap().unwrap();
        assert_eq!(
            authenticated.delta_hash,
            Blake3Hash::digest(fs::read(&delta_file).unwrap())
        );
        let before = preserved_tree(&f.root);
        let authority_before = catalog.operation_state().unwrap();
        let args = crate::cli::Arguments::try_parse_from([
            "lwiki",
            "--offline",
            "--json",
            "--lock-timeout-ms",
            "200",
            "--wiki",
            f.root.to_str().unwrap(),
            "changes",
            "apply",
            change.change_id.as_str(),
        ])
        .unwrap();
        let (envelope, exit) = crate::cli::execute(&args);
        // Capture before observer reads can update SQLite coordination state.
        let after = preserved_tree(&f.root);
        assert_eq!(exit, ErrorCode::RecoveryRequired.exit_code());
        assert!(!envelope.ok);
        assert!(!envelope.meta.network_used);
        let error = envelope.error.unwrap();
        assert_eq!(error.code, ErrorCode::RecoveryRequired.to_string());
        assert_eq!(error.message, "Page deletion is maintenance-only");
        assert_eq!(catalog.operation_state().unwrap(), authority_before);
        assert_eq!(
            catalog
                .cached_query_snapshot(QueryReadLimits::default())
                .unwrap()
                .snapshot(),
            &selected
        );
        let summarize = |entry: Option<&PreservedEntry>| {
            entry.map(|entry| {
                json!({"bytes":entry.bytes.len(),"hash":Blake3Hash::digest(&entry.bytes),
                    "directory":entry.directory,
                    "mtime_unix_ns":entry.modified.duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap().as_nanos().to_string()})
            })
        };
        let differences = before
            .keys()
            .chain(after.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|p| before.get(*p) != after.get(*p))
            .map(|p| {
                json!({"path":p,"before":summarize(before.get(p)),
                "after":summarize(after.get(p))})
            })
            .collect::<Vec<_>>();
        println!(
            "PAGE_MANAGED_REFUSAL {}",
            json!({"layout":if retained {"retained"} else {"original"},
                "exit":exit,"error":error,"authority_preserved":true,
                "selected_snapshot_preserved":true,"difference_count":differences.len(),
                "raw_differences":differences.iter().take(64).collect::<Vec<_>>()})
        );
        assert!(
            before.keys().eq(after.keys())
                && before.iter().all(|(p, entry)| {
                    let observed = &after[p];
                    if p == Path::new(".wiki/state/writer.lock") {
                        // Writer acquisition refreshes this diagnostic's mtime;
                        // its bytes and every other file field remain exact.
                        !entry.directory && !observed.directory && entry.bytes == observed.bytes
                    } else {
                        entry == observed
                    }
                }),
            "public APPLY refusal changed membership, bytes or non-lock mtime"
        );
    }
}
