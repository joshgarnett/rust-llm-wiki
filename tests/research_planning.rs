use lwiki::{
    changes::ReadDependency,
    config::providers::{ProviderConfig, TrustedService},
    domain::*,
    graph::packet::canonical_json,
    jobs::*,
    providers::{
        public_fetch,
        types::{RemoteInput, RemoteOperation},
        wire,
    },
    research::{
        ResearchLimits, ResearchPassage, ResearchScope, frontier, gaps, plan::*, synthesis,
    },
    vault::{ExpectedState, VaultFs, VaultRoot},
};
use std::{collections::BTreeMap, path::Path, time::UNIX_EPOCH};

fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn hash(value: impl AsRef<[u8]>) -> Blake3Hash {
    Blake3Hash::digest(value)
}

struct Fixture {
    temp: tempfile::TempDir,
    generation: TrustedService,
    search: TrustedService,
}
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: vault_test\nwiki_kind: vault\ntitle: Planning fixture\n---\n").unwrap();
    let fs = VaultFs::new(VaultRoot::explicit(&root).unwrap());
    let path = temp.path().join("providers.toml");
    std::fs::write(&path, format!(
        "version=1\n[profiles.primary]\ngeneration=\"generation\"\nsearch=\"search\"\n[services.generation]\nadapter=\"chat-completions-v1\"\nurl=\"https://provider.example/v1/chat/completions\"\nmodel=\"fixture\"\nrevision=\"r1\"\n[services.generation.auth]\nkind=\"static\"\nkey_env=\"UNUSED_RESEARCH_PLANNING_TOKEN\"\n[services.search]\nadapter=\"brave-web-v1\"\nurl=\"https://api.search.brave.com/res/v1/web/search\"\n[services.search.auth]\nkind=\"static\"\nkey_env=\"UNUSED_RESEARCH_PLANNING_SEARCH_TOKEN\"\nheader=\"X-Subscription-Token\"\nprefix=\"\"\n[vault_bindings.test]\nroot={}\nwiki_id=\"vault_test\"\nallowed_profiles=[\"primary\"]\n",
        serde_json::to_string(root.to_str().unwrap()).unwrap())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let config = ProviderConfig::load(&path).unwrap();
    let generation = config
        .authorize(&fs, &id("vault_test"), "primary", Capability::Generate)
        .unwrap();
    let search = config
        .authorize(&fs, &id("vault_test"), "primary", Capability::Search)
        .unwrap();
    Fixture {
        temp,
        generation,
        search,
    }
}
fn scope() -> ResearchScope {
    ResearchScope {
        version: 1,
        question: "What does this source establish?".into(),
        exclusions: vec!["excluded.invalid".into()],
        explicit_urls: vec!["https://example.org/source?version=1#section".into()],
        generation_profile: "primary".into(),
        search_profile: None,
        limits: ResearchLimits::default(),
        apply: false,
    }
}
fn binding(f: &Fixture, search: bool) -> BindingEpochV1 {
    let mut services = vec![&f.generation];
    if search {
        services.push(&f.search);
    }
    BindingEpochV1 {
        version: 1,
        number: 0,
        config_fingerprint: f.generation.summary().config_fingerprint,
        source_snapshot: None,
        input_records: vec![],
        read_preconditions: vec![],
        services: services
            .into_iter()
            .map(|service| {
                let summary = service.summary();
                ServiceBindingV1 {
                    profile_id: summary.profile_id,
                    capability: summary.capability,
                    profile_fingerprint: summary.profile_fingerprint,
                    endpoint_fingerprint: summary.endpoint_fingerprint,
                }
            })
            .collect(),
    }
}
fn planned(
    f: &Fixture,
    scope: ResearchScope,
    binding: BindingEpochV1,
    passages: &[ResearchPassage],
) -> lwiki::domain::Result<lwiki::research::ResearchPlan> {
    plan(
        scope,
        id("vault_test"),
        id("run_preview"),
        binding,
        &f.generation,
        passages,
        LifetimeLimits::default(),
        1000,
        901000,
    )
}
fn tree(root: &Path) -> Vec<(String, Vec<u8>, u128, bool)> {
    fn walk(root: &Path, path: &Path, output: &mut Vec<(String, Vec<u8>, u128, bool)>) {
        let mut entries = std::fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            let directory = metadata.is_dir();
            output.push((
                path.strip_prefix(root).unwrap().to_str().unwrap().into(),
                if directory {
                    vec![]
                } else {
                    std::fs::read(&path).unwrap()
                },
                metadata
                    .modified()
                    .unwrap()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                directory,
            ));
            if directory {
                walk(root, &path, output);
            }
        }
    }
    let mut output = vec![];
    walk(root, root, &mut output);
    output
}
fn passage() -> ResearchPassage {
    let quote = "A source passage, not accepted truth.";
    ResearchPassage {
        citation: CitationRef::Source(SourceSpanRef {
            source_id: id("source_test"),
            source_revision: id("revision_test"),
            span: ByteSpan::new(10, 10 + quote.len() as u64).unwrap(),
            quote_hash: hash(quote),
        }),
        quote: quote.into(),
        dependencies: vec![ReadDependency {
            path: VaultRelativePath::new("sources/source_test/revisions/revision_test/content.txt")
                .unwrap(),
            expected: ExpectedState::Hash(hash(quote)),
        }],
    }
}

#[test]
fn pure_plan_preserves_complete_fixture_tree_and_lifetime_genesis() {
    let f = fixture();
    let before = tree(f.temp.path());
    let scope = scope();
    let binding = binding(&f, false);
    let result = planned(&f, scope.clone(), binding.clone(), &[]).unwrap();
    assert_eq!(tree(f.temp.path()), before);
    assert_eq!(result.scope, scope);
    assert_eq!(result.scope_bytes, canonical_json(&scope).unwrap());
    assert_eq!(result.spec.limits, LifetimeLimits::default());
    assert_eq!(result.spec.created_at_utc_ms, 1000);
    assert_eq!(result.spec.deadline_utc_ms, 901000);
    assert!(result.spec.scope.profile_fingerprints.is_empty());
    let genesis = result.spec.scope.research.unwrap();
    assert_eq!(genesis.initial_binding, binding);
    assert_eq!(
        genesis.scope.path.as_str(),
        "runs/run_preview/inputs/scope.json"
    );
    assert_eq!(genesis.scope.hash, hash(&result.scope_bytes));
    assert_eq!(genesis.scope.byte_len, result.scope_bytes.len() as u64);
    assert_eq!(genesis.limits.rounds, scope.limits.rounds);
    assert_eq!(genesis.limits.sources, scope.limits.sources);
    assert_eq!(result.descriptors.len(), 2);
}

#[test]
fn initial_descriptors_match_production_fingerprints_and_durable_paths() {
    let f = fixture();
    let result = planned(&f, scope(), binding(&f, false), &[]).unwrap();
    assert_eq!(
        result.spec.input_fingerprint,
        lwiki::jobs::tasks::input_fingerprint(&result.spec).unwrap()
    );
    let inspect = &result.descriptors[0];
    assert_eq!(inspect.task.stage, TaskStage::InspectExisting);
    assert_eq!(inspect.task.capability, None);
    let local: ResearchInspectInput = serde_json::from_slice(&inspect.bytes).unwrap();
    assert_eq!(local.scope_hash, hash(&result.scope_bytes));
    let frontier = &result.descriptors[1];
    assert_eq!(frontier.task.dependencies, vec![inspect.task.key.clone()]);
    for descriptor in &result.descriptors {
        assert_eq!(descriptor.task.input.hash, hash(&descriptor.bytes));
        assert_eq!(descriptor.task.input_hash, hash(&descriptor.bytes));
        assert_eq!(
            descriptor.task.input.path.as_str(),
            format!(
                "runs/run_preview/inputs/{}.json",
                descriptor.task.input_hash.hex()
            )
        );
        assert_eq!(
            descriptor.task.input.byte_len,
            descriptor.bytes.len() as u64
        );
        assert_eq!(
            descriptor.task.key,
            lwiki::jobs::tasks::task_key(&descriptor.task).unwrap()
        );
    }
    let input: RemoteInput = serde_json::from_slice(&frontier.bytes).unwrap();
    let fingerprints = wire::task_fingerprints(&f.generation, &input).unwrap();
    assert_eq!(frontier.task.input_hash, fingerprints.input);
    assert_eq!(frontier.task.prompt_hash, fingerprints.prompt);
    assert_eq!(frontier.task.schema_hash, fingerprints.schema);
    assert_eq!(frontier.task.model_hash, Some(fingerprints.model));
    assert_eq!(frontier.task.settings_hash, fingerprints.settings);
    let RemoteOperation::Generate {
        instructions,
        data,
        output_schema,
        ..
    } = input.operation
    else {
        panic!()
    };
    assert!(instructions.contains("untrusted data"));
    assert!(instructions.contains("unassessed"));
    assert_eq!(output_schema, frontier::schema());
    let data: serde_json::Value = serde_json::from_str(&data).unwrap();
    let stage: ResearchStageBinding = serde_json::from_value(data["binding"].clone()).unwrap();
    assert_eq!(stage.scope_hash, hash(&result.scope_bytes));
    assert_eq!(stage.round, 1);
    assert_eq!(stage.stage, TaskStage::PlanFrontier);
    let mut forbidden = serde_json::to_value(stage).unwrap();
    forbidden["apply"] = true.into();
    assert!(serde_json::from_value::<ResearchStageBinding>(forbidden).is_err());
}

#[test]
fn same_profile_binds_generation_and_search_by_exact_role() {
    let f = fixture();
    let mut scope = scope();
    scope.search_profile = Some("primary".into());
    let result = planned(&f, scope.clone(), binding(&f, true), &[]).unwrap();
    let services = &result.spec.scope.research.unwrap().initial_binding.services;
    assert_eq!(services.len(), 2);
    assert_eq!(services[0].profile_id, services[1].profile_id);
    assert_ne!(
        services[0].profile_fingerprint,
        services[1].profile_fingerprint
    );
    let search = search_task(
        &id("run_preview"),
        &scope,
        "source question",
        0,
        &f.search,
        7,
        vec![],
    )
    .unwrap();
    let input: RemoteInput = serde_json::from_slice(&search.bytes).unwrap();
    let proof = wire::task_fingerprints(&f.search, &input).unwrap();
    assert_eq!(search.task.settings_hash, proof.settings);
    assert_eq!(search.task.stage, TaskStage::Discover);
    assert!(
        search_task(
            &id("run_preview"),
            &scope,
            "source",
            0,
            &f.generation,
            0,
            vec![]
        )
        .is_err()
    );
    let mut wrong = binding(&f, true);
    wrong.services[0].profile_fingerprint = wrong.services[1].profile_fingerprint.clone();
    assert!(planned(&f, scope.clone(), wrong, &[]).is_err());
    let mut wrong = binding(&f, true);
    wrong.services[0].endpoint_fingerprint = hash("other endpoint");
    assert!(planned(&f, scope.clone(), wrong, &[]).is_err());
    let mut wrong = binding(&f, true);
    wrong.config_fingerprint = hash("other config");
    assert!(planned(&f, scope, wrong, &[]).is_err());
}

#[test]
fn explicit_url_plan_needs_no_search_binding_and_capture_is_pure() {
    let f = fixture();
    let before = tree(f.temp.path());
    let scope = scope();
    planned(&f, scope.clone(), binding(&f, false), &[]).unwrap();
    assert!(
        search_task(
            &id("run_preview"),
            &scope,
            "source",
            0,
            &f.search,
            0,
            vec![]
        )
        .is_err()
    );
    let capture = capture_task(
        &id("run_preview"),
        &scope.explicit_urls[0],
        &scope.limits.fetch,
        2,
        vec![hash("parent")],
    )
    .unwrap();
    assert_eq!(tree(f.temp.path()), before);
    assert_eq!(capture.task.capability, Some(Capability::Fetch));
    assert_eq!(
        capture.task.settings_hash,
        public_fetch::settings_fingerprint()
    );
    assert_eq!(capture.task.model_hash, None);
    let input: RemoteInput = serde_json::from_slice(&capture.bytes).unwrap();
    assert_eq!(
        capture.task.input_hash,
        public_fetch::input_fingerprint(&input).unwrap()
    );
    let RemoteOperation::Fetch { url, .. } = input.operation else {
        panic!()
    };
    assert_eq!(url, "https://example.org/source?version=1");
    assert!(
        capture_task(
            &id("run_preview"),
            "http://127.0.0.1/",
            &scope.limits.fetch,
            0,
            vec![]
        )
        .is_err()
    );
}

#[test]
fn source_and_parent_proofs_are_preserved_and_invalid_quotes_fail() {
    let f = fixture();
    let passage = passage();
    let mut binding = binding(&f, false);
    assert!(planned(&f, scope(), binding.clone(), std::slice::from_ref(&passage)).is_err());
    binding.read_preconditions = passage.dependencies.clone();
    let result = planned(&f, scope(), binding.clone(), std::slice::from_ref(&passage)).unwrap();
    for descriptor in &result.descriptors {
        assert_eq!(descriptor.task.source_bindings, passage.dependencies);
    }
    let parent = DurableOutputRef {
        record: RecordRef {
            vault_id: id("vault_test"),
            record_id: id("run_event_parent"),
            expected_kind: RecordKind::RunEvent,
        },
        path: VaultRelativePath::new("runs/run_preview/outputs/run_event_parent.md").unwrap(),
        hash: hash("parent"),
    };
    let generation = generation_task(
        &id("run_preview"),
        &scope(),
        TaskStage::AssessGaps,
        1,
        std::slice::from_ref(&parent),
        std::slice::from_ref(&passage),
        &[],
        &f.generation,
        0,
        vec![],
    )
    .unwrap();
    assert!(generation.task.source_bindings.contains(&ReadDependency {
        path: parent.path,
        expected: ExpectedState::Hash(parent.hash)
    }));
    let mut wrong = passage.clone();
    wrong.quote = "Altered bytes".into();
    assert!(planned(&f, scope(), binding.clone(), &[wrong]).is_err());
    let mut wrong = passage.clone();
    wrong.dependencies.clear();
    assert!(planned(&f, scope(), binding.clone(), &[wrong]).is_err());
    let mut wrong = passage;
    wrong.dependencies[0].expected = ExpectedState::Absent;
    assert!(planned(&f, scope(), binding, &[wrong]).is_err());
}

#[test]
fn caller_scope_and_stage_ceilings_reject_oversize_or_implicit_remote_retrieval() {
    let mut invalid = vec![];
    let mut s = scope();
    s.limits.rounds = 17;
    invalid.push(s);
    let mut s = scope();
    s.limits.sources = 257;
    invalid.push(s);
    let mut s = scope();
    s.limits.stage.max_bytes += 1;
    invalid.push(s);
    let mut s = scope();
    s.limits.fetch.expanded_bytes += 1;
    invalid.push(s);
    let mut s = scope();
    s.limits.extraction.max_mentions = 65;
    invalid.push(s);
    let mut s = scope();
    s.limits.retrieval.limits.candidates = 81;
    invalid.push(s);
    let mut s = scope();
    s.limits.retrieval.mode = lwiki::retrieval::SearchMode::Semantic;
    invalid.push(s);
    let mut s = scope();
    s.question = "x".repeat(4097);
    invalid.push(s);
    let mut s = scope();
    s.explicit_urls = vec!["https://user:secret@example.org/".into()];
    invalid.push(s);
    let mut s = scope();
    s.explicit_urls = vec!["https://example.org/excluded.invalid".into()];
    invalid.push(s);
    let mut s = scope();
    s.explicit_urls = vec!["https://example.org/\n".into()];
    invalid.push(s);
    let mut s = scope();
    s.exclusions = vec!["\t".into()];
    invalid.push(s);
    for scope in invalid {
        assert!(validate_scope(&scope).is_err());
    }
    let mut s = scope();
    s.explicit_urls = vec![
        "https://example.org/?a=1".into(),
        "https://example.org/?a=2".into(),
    ];
    validate_scope(&s).unwrap();
    s.explicit_urls
        .push("https://example.org/?a=1#another".into());
    assert!(validate_scope(&s).is_err());
}

#[test]
fn generation_stage_schema_and_proof_changes_change_identity() {
    let f = fixture();
    let scope = scope();
    let mut keys = BTreeMap::new();
    for (name, stage, schema) in [
        ("frontier", TaskStage::PlanFrontier, frontier::schema()),
        ("gaps", TaskStage::AssessGaps, gaps::schema()),
        ("synthesis", TaskStage::Synthesize, synthesis::schema()),
    ] {
        let descriptor = generation_task(
            &id("run_preview"),
            &scope,
            stage,
            1,
            &[],
            &[],
            &[],
            &f.generation,
            0,
            vec![],
        )
        .unwrap();
        let input: RemoteInput = serde_json::from_slice(&descriptor.bytes).unwrap();
        let RemoteOperation::Generate { output_schema, .. } = input.operation else {
            panic!()
        };
        assert_eq!(output_schema, schema);
        assert_eq!(
            descriptor.task.schema_hash,
            Some(hash(canonical_json(&schema).unwrap()))
        );
        keys.insert(name, descriptor.task.key);
    }
    assert_ne!(keys["frontier"], keys["gaps"]);
    assert_ne!(keys["gaps"], keys["synthesis"]);
    assert!(
        generation_task(
            &id("run_preview"),
            &scope,
            TaskStage::Extract,
            1,
            &[],
            &[],
            &[],
            &f.generation,
            0,
            vec![]
        )
        .is_err()
    );
    let old = planned(&f, scope.clone(), binding(&f, false), &[]).unwrap();
    let mut changed = scope;
    changed.question.push_str(" More detail?");
    let new = planned(&f, changed, binding(&f, false), &[]).unwrap();
    assert_ne!(old.descriptors[1].task.key, new.descriptors[1].task.key);
    assert_ne!(old.spec.input_fingerprint, new.spec.input_fingerprint);
}

#[test]
fn oversized_combined_generation_data_and_invalid_initial_bindings_fail() {
    let f = fixture();
    let scope = scope();
    let mut passages = vec![];
    for index in 0..8 {
        let quote = "x".repeat(40_000);
        passages.push(ResearchPassage {
            citation: CitationRef::Source(SourceSpanRef {
                source_id: id(&format!("source_{index}")),
                source_revision: id(&format!("revision_{index}")),
                span: ByteSpan::new(0, quote.len() as u64).unwrap(),
                quote_hash: hash(&quote),
            }),
            quote,
            dependencies: vec![ReadDependency {
                path: VaultRelativePath::new(format!("sources/{index}/content.txt")).unwrap(),
                expected: ExpectedState::Hash(hash("content")),
            }],
        });
    }
    assert!(
        generation_task(
            &id("run_preview"),
            &scope,
            TaskStage::Synthesize,
            1,
            &[],
            &passages,
            &[],
            &f.generation,
            0,
            vec![]
        )
        .is_err()
    );
    let mut wrong = binding(&f, false);
    wrong.number = 1;
    assert!(planned(&f, scope.clone(), wrong, &[]).is_err());
    let mut wrong = binding(&f, true);
    wrong.services.reverse();
    assert!(planned(&f, scope.clone(), wrong, &[]).is_err());
    let mut wrong = binding(&f, false);
    wrong.services.push(wrong.services[0].clone());
    assert!(planned(&f, scope.clone(), wrong, &[]).is_err());
    assert!(
        plan(
            scope,
            id("vault_test"),
            id("run_preview"),
            binding(&f, false),
            &f.generation,
            &[],
            LifetimeLimits::default(),
            1000,
            1000
        )
        .is_err()
    );
}
