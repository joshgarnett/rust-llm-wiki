//! Ignored setup for one root-owned canonical copy; not public import/rebuild.
use super::refresh_fixture_export::{ExportResult, create_file, create_owned_parents, stream_hash};
use crate::{
    catalog::{
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        scan, selector,
    },
    domain::{Blake3Hash, CanonicalRecord, RecordId, RecordKind, VaultRelativePath},
    records::parse_note,
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    env, fs, io,
    path::Path,
    time::{Duration, Instant},
};
const VAULT: &str = "vault_refresh_fixture";
const SUBJECT: &str = "entity_general_identity";
const OBJECT: &str = "entity_general_stale";
const CLAIM: &str = "assertion_general_supported";
const PROPOSED: &str = "assertion_general_proposed";
fn invalid(message: &str) -> Box<dyn std::error::Error> {
    io::Error::new(io::ErrorKind::InvalidInput, message).into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    source_id: RecordId,
    revision_id: RecordId,
    marker: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    version: u32,
    kind: String,
    vault_path: String,
    original_seed_manifest_sha256: String,
    source_count: usize,
    first_source: Source,
    wiki_blake3: Blake3Hash,
}
fn note(kind: &str, id: &str, title: &str, extra: Value, body: &str) -> ExportResult<Vec<u8>> {
    let mut fields = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!(id)),
        ("wiki_kind".into(), json!(kind)),
        ("title".into(), json!(title)),
    ]);
    fields.extend(
        extra
            .as_object()
            .ok_or_else(|| invalid("overlay extra must be object"))?
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    CanonicalRecord::new(fields.clone())?;
    let mut text = "---\n".to_owned();
    for (key, value) in fields {
        text.push_str(&format!("{key}: {value}\n"));
    }
    text.push_str("---\n");
    text.push_str(body);
    let bytes = text.into_bytes();
    if parse_note(&bytes).canonical.is_none() {
        return Err(invalid("overlay failed real envelope parser"));
    }
    Ok(bytes)
}
fn overlay(source: &Source, quote: &[u8]) -> ExportResult<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for index in 0..128 {
        let mut extra = json!({"wiki_status":if index==127 {"draft"}else{"reviewed"},"tags":[if index==126 {"late-eligible"}else{"general-overlay"}]});
        if (32..64).contains(&index) {
            extra["aliases"] = json!(["GeneralSharedAlias"]);
        }
        if index == 2 {
            extra["aliases"] = json!(["Éclair 東京"]);
            extra["tags"] = json!(["café", "東京"]);
        }
        if (32..48).contains(&index) {
            extra["benchmark_metadata"] = json!("valid bounded metadata ".repeat(2400));
        }
        let title = if (32..64).contains(&index) {
            "General shared title".to_owned()
        } else {
            format!("General authored {index:03}")
        };
        let body = match index {
            0 => format!("# General workflow\n\ngeneralworkflow blue: choose the azure cluster in Café 東京.\n\n{}# Retry\n\ngeneralworkflow retry: wait exactly 37 seconds.\n", "Ordinary background describes the maintenance procedure.\n\n".repeat(90)),
            1 => "# Exception\n\ngeneralworkflow exception: require the violet permit from the complementary authored document.\n".into(),
            32..48 => "# Source filter work\n\nsourceworkneedle Synthetic capture unassociated authored metadata.\n".into(),
            _ => format!("# General authored {index:03}\n\nSynthetic capture generalpopular authored fact {index:03}.\n"),
        };
        files.push((
            format!("pages/general/{index:03}.md"),
            note(
                "page",
                &format!("page_general_{index:03}"),
                &title,
                extra,
                &body,
            )?,
        ));
    }
    files.push(("entities/general/identity.md".into(), note("entity", SUBJECT, "General Identity", json!({"wiki_status":"active","wiki_entity_type":"concept","aliases":["GeneralIdentityAlias"]}), "UnsupportedIdentityDescription\n")?));
    files.push(("entities/general/stale.md".into(), note("entity", OBJECT, "General Stale Identity", json!({"wiki_status":"active","wiki_entity_type":"concept","wiki_depends_on_ids":[PROPOSED]}), "StaleIdentityDescription\n")?));
    for (id, status, body) in [
        (
            CLAIM,
            "accepted",
            "sourceworkneedle Synthetic capture accepted supported assertion.\n",
        ),
        (
            PROPOSED,
            "proposed",
            "Proposed assertion keeps entity description stale.\n",
        ),
    ] {
        files.push((format!("assertions/general/{id}.md"),note("assertion",id,id,json!({"wiki_status":status,"wiki_subject_id":SUBJECT,"wiki_predicate":"uses","wiki_object_id":OBJECT}),body)?));
    }
    files.push(("evidence/general/seed.md".into(),note("evidence","evidence_general_seed","General captured support",json!({"wiki_status":"active","wiki_assertion_id":CLAIM,"wiki_source_id":source.source_id,"wiki_source_revision":source.revision_id,"wiki_stance":"supports","wiki_locator_kind":"utf8-bytes","wiki_span_start":0,"wiki_span_end":quote.len(),"wiki_quote_hash":Blake3Hash::digest(quote)}),&format!("```text\n{}\n```\n\nsourceworkneedle exact captured header support.\n",std::str::from_utf8(quote)?))?));
    Ok(files)
}
fn cases(source: &Source) -> Value {
    let captured_path = format!(
        "sources/{}/revisions/{}/content.md",
        source.source_id, source.revision_id
    );
    json!([
        {"name":"exact_id","args":["search","page_general_126","--no-sync"],"expected":{"hit_ids_all":["page_general_126"],"first_reason":"exact_id"}},
        {"name":"exact_title_bucket","args":["search","General shared title","--no-sync"],"expected":{"hit_ids_all":["page_general_032","page_general_033"],"first_reason":"exact_title"}},
        {"name":"exact_alias_bucket","args":["search","GeneralSharedAlias","--no-sync"],"expected":{"hit_ids_all":["page_general_032","page_general_033"],"first_reason":"exact_alias"}},
        {"name":"unicode_alias","args":["search","Éclair 東京","--no-sync"],"expected":{"hit_ids_all":["page_general_002"],"first_reason":"exact_alias"}},
        {"name":"rare_capture","args":["search",source.marker,"--no-sync"],"expected":{"hit_paths_all":[captured_path]}},
        {"name":"popular","args":["search","Synthetic capture","--no-sync"],"expected":{"nonempty_hits":true}},
        {"name":"selective_late","args":["search","Synthetic capture","--no-sync","--kind","page","--status","reviewed","--tag","late-eligible","--path-prefix","pages/general/126"],"expected":{"hit_ids_all":["page_general_126"]}},
        {"name":"selective_empty","args":["search","Synthetic capture","--no-sync","--tag","no-general-survivors"],"expected":{"empty_hits":true}},
        {"name":"source_long_metadata_positive","args":["search","sourceworkneedle","--no-sync","--source-id",source.source_id],"expected":{"hit_ids_all":[CLAIM]}},
        {"name":"source_long_metadata_negative","args":["search","sourceworkneedle","--no-sync","--source-id","source_general_absent"],"expected":{"empty_hits":true}},
        {"name":"stale_entity_identity","args":["search","General Stale Identity","--no-sync","--kind","entity"],"expected":{"hit_ids_all":[OBJECT],"stale_identity_only":true}},
        {"name":"stale_description_hidden","args":["search","StaleIdentityDescription","--no-sync"],"expected":{"empty_hits":true}},
        {"name":"authored_context","args":["context","generalworkflow blue retry exception","--scope","snapshot","--target","documents","--no-sync"],"expected":{"snapshot_context":true,"text_all":["azure cluster","37 seconds","violet permit"]}},
        {"name":"mixed_context","args":["context",format!("generalworkflow {}",source.marker),"--scope","snapshot","--target","documents","--no-sync"],"expected":{"snapshot_context":true,"text_all":["azure cluster","37 seconds","violet permit",source.marker]}}
    ])
}
fn prepare(setup: &Path) -> ExportResult<Value> {
    let started = Instant::now();
    if !setup.is_absolute() || fs::canonicalize(setup)? != setup {
        return Err(invalid(
            "setup must name an owned canonical absolute directory",
        ));
    }
    let ownership_path = setup.join("ownership.json");
    let (descriptor_bytes, descriptor_hash) = stream_hash(&ownership_path, true)?;
    if descriptor_bytes > 16 * 1024 {
        return Err(invalid("ownership descriptor exceeds16KiB"));
    }
    let owned: Ownership = serde_json::from_slice(&fs::read(&ownership_path)?)?;
    let root = setup.join("vault");
    if owned.version != 1
        || owned.kind != "general-query-owned-copy"
        || owned.vault_path != root.to_str().unwrap_or_default()
        || fs::canonicalize(&root)? != root
        || ![1, 1000, 10000].contains(&owned.source_count)
        || owned.original_seed_manifest_sha256.len() != 64
        || !owned
            .original_seed_manifest_sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(invalid("ownership descriptor binding or limits invalid"));
    }
    for relative in [".wiki", "pages", "entities", "assertions", "evidence"] {
        if fs::symlink_metadata(root.join(relative)).is_ok() {
            return Err(invalid(
                "setup refuses preexisting cache or authored overlay",
            ));
        }
    }
    let (_, wiki_hash) = stream_hash(&root.join("WIKI.md"), true)?;
    if wiki_hash != owned.wiki_blake3 {
        return Err(invalid(
            "copied WIKI hash differs from ownership descriptor",
        ));
    }
    let wiki = fs::read(root.join("WIKI.md"))?;
    let record = parse_note(&wiki)
        .canonical
        .ok_or_else(|| invalid("copied WIKI not canonical"))?;
    if record.kind() != RecordKind::Vault || record.id().as_str() != VAULT {
        return Err(invalid("copied WIKI identity invalid"));
    }
    let fs_handle = VaultFs::new(VaultRoot::explicit(&root)?);
    let content = fs_handle
        .read_before(&VaultRelativePath::new(format!(
            "sources/{}/revisions/{}/content.md",
            owned.first_source.source_id, owned.first_source.revision_id
        ))?)?
        .ok_or_else(|| invalid("seed content missing"))?
        .bytes;
    let quote_end = content
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| invalid("seed content missing header newline"))?
        + 1;
    if quote_end > 256
        || !std::str::from_utf8(&content[..content.len().min(256)])?
            .contains(&owned.first_source.marker)
    {
        return Err(invalid("seed header/marker invalid"));
    }
    let files = overlay(&owned.first_source, &content[..quote_end])?;
    let overlay_hashes=files.iter().map(|(path,bytes)|json!({"path":path,"bytes":bytes.len(),"blake3":Blake3Hash::digest(bytes)})).collect::<Vec<_>>();
    for (path, bytes) in &files {
        create_file(
            &create_owned_parents(&root, &VaultRelativePath::new(path)?)?,
            bytes,
        )?;
    }
    let writer = WriterPermit::acquire(fs_handle.root(), Duration::ZERO)?;
    let identity = BuildIdentity {
        selection: CatalogSelection::new(RecordId::new(VAULT)?, 1)?,
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: false,
    };
    selector::prepare(&fs_handle, &writer, &identity.selection)?;
    let mut builder =
        NormalizedBuilder::begin(&fs_handle, &writer, identity, BuildLimits::default())?;
    let input = scan::scan_input(&fs_handle, &RecordId::new(VAULT)?)?;
    let projected = scan::project_normalized_with_sink(&fs_handle, &input, false, &mut builder)?;
    if files.iter().any(|(_, bytes)| {
        let record = parse_note(bytes).canonical.unwrap();
        !projected.validation.records.contains_key(record.id())
    }) || projected
        .validation
        .records
        .get(&RecordId::new(CLAIM)?)
        .is_none_or(|row| row.eligibility != crate::domain::Eligibility::Current)
        || projected
            .validation
            .records
            .get(&RecordId::new(OBJECT)?)
            .is_none_or(|row| {
                row.identity_eligibility != Some(crate::domain::Eligibility::Current)
                    || row.description_eligibility != Some(crate::domain::Eligibility::Stale)
            })
    {
        return Err(invalid(
            "overlay projection failed adoption/support/stale identity oracle",
        ));
    }
    if projected
        .validation
        .records
        .values()
        .filter(|row| row.record.kind() == RecordKind::Source)
        .count()
        != owned.source_count
    {
        return Err(invalid(
            "copied source count differs from ownership descriptor",
        ));
    }
    let completed = builder.finish_normalized(&projected)?;
    selector::publish(
        &fs_handle,
        &writer,
        &completed.identity.selection,
        Duration::ZERO,
    )?;
    let result = json!({"version":1,"kind":"general-query-fixture-setup","status":"published","vault_path":root,"original_seed_manifest_sha256":owned.original_seed_manifest_sha256,"ownership_blake3":descriptor_hash,"source_count":owned.source_count,"overlay_version":1,"overlay_files":overlay_hashes,"overlay_pages":128,"overlay_records":files.len(),"snapshot":completed.snapshot,"selection":completed.identity.selection,"proof_layout_version":2,"revision_ownership_version":1,"stats":completed.stats,"setup_seconds":started.elapsed().as_secs_f64(),"cases":cases(&owned.first_source),"interpretation":"Explicit disposable setup: real one fullbuild; root owns copy inventory, manifest and resource supervision. No public normalized rebuild/import or query capacity claim."});
    create_file(
        &setup.join("setup-result.json"),
        &serde_json::to_vec_pretty(&result)?,
    )?;
    Ok(result)
}
#[test]
#[ignore = "requires new root-owned canonical copy and native resource supervisor"]
fn prepare_general_query_fixture() {
    let setup =
        env::var_os("LWIKI_GENERAL_QUERY_SETUP").expect("LWIKI_GENERAL_QUERY_SETUP required");
    let result = prepare(Path::new(&setup)).expect("general-query fixture setup");
    println!(
        "{}",
        json!({"status":"published","result":Path::new(&setup).join("setup-result.json"),"snapshot":result["snapshot"]})
    );
}

#[test]
fn tiny_owned_copy_adopts_overlay_and_passes_frozen_command_expectations() {
    use crate::sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore};
    use clap::Parser;
    let temp = tempfile::tempdir().unwrap();
    let setup = temp.path().canonicalize().unwrap();
    let root = setup.join("vault");
    fs::create_dir(&root).unwrap();
    let wiki = b"---\nwiki_schema: '1'\nwiki_kind: vault\nwiki_id: vault_refresh_fixture\ntitle: Tiny general fixture\n---\n";
    create_file(&root.join("WIKI.md"), wiki).unwrap();
    let fs_handle = VaultFs::new(VaultRoot::explicit(&root).unwrap());
    let marker = "refreshprobe000000v000000";
    let seed = SourceStore::new(fs_handle)
        .plan_capture(CaptureRequest {
            title: "Synthetic capture 000000".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture-000000.md".into(),
            original: format!("# Synthetic capture\n\n{marker}: café 東京 🦀 field observation.\n")
                .into_bytes(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    for operation in seed.draft.unwrap().operations {
        let path = create_owned_parents(&root, &operation.target).unwrap();
        create_file(&path, &operation.proposed.unwrap()).unwrap();
    }
    create_file(&setup.join("ownership.json"),&serde_json::to_vec(&json!({
        "version":1,"kind":"general-query-owned-copy","vault_path":root,
        "original_seed_manifest_sha256":"0".repeat(64),"source_count":1,
        "first_source":{"source_id":seed.source_id,"revision_id":seed.revision_id,"marker":marker},
        "wiki_blake3":Blake3Hash::digest(wiki),
    })).unwrap()).unwrap();
    let result = prepare(&setup).unwrap();
    assert_eq!(result["overlay_records"], 133);
    for case in result["cases"].as_array().unwrap() {
        let mut args = vec![
            "lwiki".to_owned(),
            "--wiki".into(),
            root.to_str().unwrap().into(),
            "--json".into(),
            "--offline".into(),
        ];
        args.extend(
            case["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| arg.as_str().unwrap().to_owned()),
        );
        let parsed = crate::cli::Arguments::try_parse_from(args).unwrap();
        let (envelope, status) = crate::cli::execute(&parsed);
        assert_eq!(status, 0, "{}: {:?}", case["name"], envelope.error);
        let expected = &case["expected"];
        if expected["snapshot_context"] == true {
            let text = envelope.data["text"].as_str().unwrap();
            for facet in expected["text_all"].as_array().unwrap() {
                assert!(
                    text.contains(facet.as_str().unwrap()),
                    "{} missing {facet}: {text}",
                    case["name"]
                );
            }
            assert_eq!(envelope.data["verification"]["mode"], "index_snapshot");
            assert!(
                envelope.data["passages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|p| p["citations"].as_array().unwrap().is_empty())
            );
        } else {
            let hits = envelope.data["hits"].as_array().unwrap();
            if expected["empty_hits"] == true {
                assert!(hits.is_empty(), "{}: {hits:?}", case["name"]);
            }
            if expected["nonempty_hits"] == true {
                assert!(!hits.is_empty());
            }
            for (field, target) in [("hit_ids_all", "record_ref"), ("hit_paths_all", "path")] {
                if let Some(values) = expected[field].as_array() {
                    for value in values {
                        assert!(
                            hits.iter().any(|hit| if target == "record_ref" {
                                &hit[target]["record_id"] == value
                            } else {
                                &hit[target] == value
                            }),
                            "{} missing {value}: {hits:?}",
                            case["name"]
                        );
                    }
                }
            }
            if let Some(reason) = expected["first_reason"].as_str() {
                assert!(
                    hits[0]["reasons"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|r| r == reason)
                );
            }
            if expected["stale_identity_only"] == true {
                let stale = hits
                    .iter()
                    .find(|hit| hit["record_ref"]["record_id"] == OBJECT)
                    .unwrap();
                assert_eq!(stale["identity_eligibility"], "current");
                assert_eq!(stale["eligibility"], "stale");
                assert!(
                    stale["reasons"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|reason| reason == "identity")
                );
                assert_eq!(stale["excerpt"]["text"], "");
                assert!(
                    stale["secondary_excerpts"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|excerpt| excerpt["text"] == "")
                );
            }
        }
    }
    assert!(
        prepare(&setup).is_err(),
        "setup must refuse reused fixtures"
    );
}
