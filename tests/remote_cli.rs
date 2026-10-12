#[path = "../test_support/paths.rs"]
mod test_paths;
use lwiki as library;
#[path = "fixtures/p18/common.rs"]
mod common;
#[allow(dead_code)]
#[path = "fixtures/p16c/common.rs"]
mod provider;
use clap::Parser;
use common::*;
use lwiki::{
    cli::{Arguments, execute},
    domain::RecordId,
    jobs::JobLedgerApi,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    time::{Duration, Instant},
};

fn files(root: &std::path::Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                todo.push(entry.path());
            } else {
                found.insert(
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    found
}
#[test]
fn remote_cli_dry_run_needs_no_provider_and_preserves_vault_bytes() {
    let f = Fixture::new();
    let wiki = f.temp.path().to_str().unwrap();
    let before = files(f.temp.path());
    let source = f.request.export.source_id.as_str();
    for command in [
        vec!["embeddings", "sync"],
        vec!["embeddings", "check", "--probe"],
        vec!["search", "fixture", "--mode", "semantic"],
        vec![
            "search", "fixture", "--mode", "hybrid", "--graph", "entities",
        ],
        vec!["graph", "query", "fixture", "--seed", "semantic"],
        vec![
            "context", "fixture", "--mode", "hybrid", "--target", "combined", "--seed", "semantic",
        ],
        vec![
            "graph",
            "extract",
            "--source-id",
            source,
            "--executor",
            "api",
        ],
    ] {
        let mut argv = vec!["lwiki", "--wiki", wiki, "--json", "--dry-run", "--offline"];
        argv.extend(command);
        let args = Arguments::try_parse_from(argv).unwrap();
        let (envelope, exit) = execute(&args);
        assert_eq!(exit, 0, "{}: {:?}", envelope.command, envelope.error);
        assert!(!envelope.meta.network_used);
        assert_eq!(
            files(f.temp.path()),
            before,
            "{} changed vault",
            envelope.command
        );
    }
}
#[test]
fn native_api_cli_stages_proposals_and_offline_reuses_paid_output() {
    native_api_cli_case(false, false, false);
}
#[test]
fn native_api_cli_failed_paid_response_reports_actual_network_activity() {
    native_api_cli_case(true, false, false);
}
#[test]
fn native_responses_api_cli_stages_and_reuses_projected_schema_output() {
    native_api_cli_case(false, true, false);
}
#[test]
fn saved_generation_configuration_resumes_without_amending_retained_limits() {
    native_api_cli_case(false, true, true);
}
fn native_api_cli_case(invalid_response: bool, responses: bool, saved: bool) {
    let f = Fixture::new();
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let mock = Mock::response(f.response());
    let mut response_value = mock.body.lock().unwrap().as_ref().unwrap().clone();
    if responses {
        let content = response_value["choices"][0]["message"]["content"].clone();
        response_value = json!({"object":"response","status":"completed","model":"test-model",
            "output":[{"type":"reasoning","encrypted_content":"ignored"},
                {"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":content}]}],
            "usage":{"input_tokens":10,"output_tokens":8,"total_tokens":18,
                "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},"output_tokens_details":{"reasoning_tokens":0},"cost":null}});
    }
    let response = if invalid_response {
        b"{}".to_vec()
    } else {
        serde_json::to_vec(&response_value).unwrap()
    };
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "CLI never reached native mock");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => panic!("mock accept: {e}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut request = Vec::new();
        let (header_end, length) = loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buf[..n]);
            assert!(request.len() < 512 * 1024);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let header = String::from_utf8(request[..end].to_vec()).unwrap();
                let length = header
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                assert!(header.starts_with("POST /exact-generation?retained=yes HTTP/1.1"));
                break (end + 4, length);
            }
        };
        while request.len() < header_end + length {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buf[..n]);
        }
        let body: Value =
            serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
        assert_eq!(body["model"], "test-model");
        if responses {
            assert!(body.get("messages").is_none());
            assert!(body.get("tools").is_none());
            assert_eq!(body["store"], false);
            assert_eq!(body["input"][0]["role"], "user");
            assert_eq!(body["text"]["format"]["type"], "json_schema");
            assert!(body["instructions"].is_string());
        } else {
            assert_eq!(body["messages"][0]["role"], "system");
            assert_eq!(body["messages"][1]["role"], "user");
        }
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nllm_provider-x-amzn-requestid: synthetic-id\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
        socket.write_all(&response).unwrap();
        socket.flush().unwrap();
    });
    let config = f.temp.path().join("providers.toml");
    let content = std::fs::read_to_string(&config)
        .unwrap()
        .replace(
            "https://gateway.example/chat",
            &format!("http://{address}/exact-generation?retained=yes"),
        )
        .replace(
            "model=\"test-model\"",
            "allow_loopback_http=true\nmodel=\"test-model\"",
        );
    let content = if responses {
        content.replace(
            "adapter=\"chat-completions-v1\"",
            "adapter=\"responses-v1\"\nresponse_mode=\"json-schema\"",
        )
    } else {
        content
    };
    let content = if saved {
        // The saved lifetime must admit the complete configured attempt timeout.
        // This loopback fixture responds immediately and needs no 120s default.
        content.replace(
            "model=\"test-model\"",
            "timeout_seconds=5\nmodel=\"test-model\"",
        )
    } else {
        content
    };
    std::fs::write(&config, content).unwrap();
    let preferences = f.temp.path().join("saved-config.toml");
    let save_preferences = |requests| {
        let mut document = lwiki::config::LocalConfigDocument {
            providers_config: Some(config.clone()),
            ..Default::default()
        };
        document.defaults.profile = Some("primary".into());
        document.allowed_profiles = vec!["primary".into()];
        document.remote.max_requests = Some(requests);
        document.remote.deadline_ms = Some(20000 + requests * 100);
        embedding_fixture::private_write(&preferences, toml::to_string(&document).unwrap());
    };
    if saved {
        save_preferences(3);
    }
    let invoke = |offline: bool| {
        let mut command =
            std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
        command.args(["--wiki", f.temp.path().to_str().unwrap(), "--json"]);
        if saved {
            command.args(["--config", preferences.to_str().unwrap()]);
        } else {
            command.args(["--no-user-config", "--profile", "primary"]);
        }
        if offline {
            command.arg("--offline");
        }
        command.args([
            "graph",
            "extract",
            "--source-id",
            f.request.export.source_id.as_str(),
            "--executor",
            "api",
            "--run",
            "run_cli",
        ]);
        if !saved {
            command.args(["--providers-config", config.to_str().unwrap()]);
        }
        command.env("EXPLICIT_FIXTURE_TOKEN", "synthetic-cli-secret");
        let result = command.output().unwrap();
        let value: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(result.status.success(), !invalid_response, "{value}");
        assert!(!String::from_utf8_lossy(&result.stdout).contains("synthetic-cli-secret"));
        value
    };
    if invalid_response {
        let refusal = invoke(true);
        assert_eq!(refusal["meta"]["network_used"], false);
        assert_eq!(refusal["error"]["code"], "OFFLINE_UNAVAILABLE");
    }
    let first = invoke(false);
    server.join().unwrap();
    assert_eq!(first["meta"]["network_used"], true);
    if invalid_response {
        assert_eq!(first["error"]["code"], "PROVIDER_RESPONSE");
        assert!(first["error"].get("network_used").is_none());
        return;
    }
    assert!(first["data"]["import"]["prepared"].is_object());
    if saved {
        save_preferences(9);
    }
    let second = invoke(true);
    if saved {
        let ledger = lwiki::jobs::JobLedger::new(
            f.fs.clone(),
            f.app.vault_id().clone(),
            id("run_cli"),
            f.options.clone(),
        )
        .unwrap();
        let retained = ledger.inspect().unwrap();
        assert_eq!(retained.spec.limits.requests, 3);
        assert_eq!(retained.effective_limits.requests, 3);
        assert_eq!(
            retained.spec.deadline_utc_ms - retained.spec.created_at_utc_ms,
            20_300
        );
        assert_eq!(
            retained.effective_deadline_utc_ms,
            retained.spec.deadline_utc_ms
        );
        assert_eq!(retained.budget.dispatched_requests, 1);
    }
    assert_eq!(second["meta"]["network_used"], false);
    assert_eq!(second["data"]["reused"], true);
    assert_eq!(second["data"]["output"], first["data"]["output"]);
    assert_eq!(
        second["data"]["import"]["allocations"],
        first["data"]["import"]["allocations"]
    );
    assert_eq!(
        second["data"]["import"]["coverage"]["materialized_assertions"],
        json!(0)
    );
}

/// Serialized public CLI mechanics with one tiny fictional Source and one
/// unpriced loopback response. This does not establish relation entailment,
/// distant-evidence relevance, live-provider compatibility or quality.
#[test]
fn native_source_relations_cli_prepares_retains_and_rebuilds_offline() {
    use lwiki::{
        domain::{Blake3Hash, ByteSpan, CitationRef, ReadSnapshot, RecordKind},
        jobs::{AttemptPhase, BillingDisposition, LedgerInspection, TaskState},
        records::parse_note,
    };
    fn same_rendered_across_publication(before: &Value, after: &Value) {
        let header = |generation| {
            format!(
                "[context indexed_documents; discovery generation {generation}; selected document dependencies verified; global membership, identity uniqueness and completeness not verified]\n\n"
            )
        };
        let mut snapshots = Vec::new();
        let mut remaining = Vec::new();
        for result in [before, after] {
            let snapshot: ReadSnapshot =
                serde_json::from_value(result["data"]["snapshot"].clone()).unwrap();
            assert_eq!(result["meta"]["index_generation"], snapshot.generation);
            let exact_header = header(snapshot.generation);
            remaining.push(
                result["data"]["text"]
                    .as_str()
                    .unwrap()
                    .strip_prefix(&exact_header)
                    .unwrap(),
            );
            snapshots.push(snapshot);
        }
        assert_ne!(snapshots[0], snapshots[1]);
        assert_eq!(remaining[0].as_bytes(), remaining[1].as_bytes());
        assert_eq!(before["data"]["passages"], after["data"]["passages"]);
    }
    fn artifact(bytes: &[u8], fence: &str) -> Value {
        let note = parse_note(bytes);
        assert_eq!(
            note.canonical.as_ref().unwrap().kind(),
            RecordKind::RunEvent
        );
        let prefix = format!("\n```{fence}\n");
        let body = std::str::from_utf8(note.body()).unwrap();
        serde_json::from_str(
            body.strip_prefix(&prefix)
                .unwrap()
                .strip_suffix("\n```\n")
                .unwrap(),
        )
        .unwrap()
    }
    let f = Fixture::new();
    let source = &f.request.export.source_id;
    let revision = f.request.export.revision_id.as_ref().unwrap();
    let content_path = format!("sources/{source}/revisions/{revision}/content.md");
    let original = std::fs::read_to_string(f.temp.path().join(&content_path)).unwrap();
    assert_eq!(original, f.packet.windows[0].text);
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let config = f.temp.path().join("providers.toml");
    let configured = std::fs::read_to_string(&config)
        .unwrap()
        .replace(
            "https://gateway.example/chat",
            &format!("http://{address}/exact-generation?retained=yes"),
        )
        .replace(
            "model=\"test-model\"",
            "allow_loopback_http=true\ntimeout_seconds=10\nmodel=\"test-model\"",
        )
        .replace(
            "adapter=\"chat-completions-v1\"",
            "adapter=\"responses-v1\"\nresponse_mode=\"json-schema\"",
        );
    std::fs::write(&config, configured).unwrap();
    let invoke = |args: &[&str], offline: bool, remote: bool| {
        let mut command =
            std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
        command.args(["--wiki", f.temp.path().to_str().unwrap(), "--json"]);
        command.env_remove("EXPLICIT_FIXTURE_TOKEN");
        if offline {
            command.arg("--offline");
        }
        if remote {
            command.args(["--profile", "primary"]);
        }
        command.args(args);
        if remote {
            command.args([
                "--providers-config",
                config.to_str().unwrap(),
                "--max-requests",
                "1",
                "--attempts-per-task",
                "1",
                "--concurrency",
                "1",
                "--deadline-ms",
                "20000",
            ]);
            if !offline {
                command.env("EXPLICIT_FIXTURE_TOKEN", "synthetic-cli-secret");
            }
        }
        let output = command.output().unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {value}; {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(value.get("error").is_none_or(Value::is_null), "{value}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-cli-secret"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-cli-secret"));
        if offline {
            assert_eq!(value["meta"]["network_used"], false);
        }
        value
    };
    let context_args = [
        "context",
        "Ada",
        "--mode",
        "lexical",
        "--scope",
        "indexed-documents",
        "--excerpt-bytes",
        "1024",
        "--max-bytes",
        "12000",
        "--max-tokens",
        "3000",
    ];
    let mut experimental_args = context_args.to_vec();
    experimental_args.push("--experimental-source-relations");
    invoke(&["index", "rebuild", "--normalized"], true, false);
    let baseline = invoke(&context_args, true, false);
    let absent = invoke(&experimental_args, true, false);
    assert_eq!(baseline["data"]["text"], absent["data"]["text"]);
    assert_eq!(baseline["data"]["passages"], absent["data"]["passages"]);
    let before_prepare = files(f.temp.path());
    let planned = invoke(
        &[
            "relations",
            "prepare",
            "--source-id",
            source.as_str(),
            "--max-output-tokens",
            "1024",
        ],
        true,
        true,
    );
    assert_eq!(planned["data"]["sources"], 1);
    assert_eq!(planned["data"]["provider_calls"], 0);
    assert_eq!(planned["data"]["persisted"], false);
    assert_eq!(files(f.temp.path()), before_prepare);
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    let manifest_bytes = planned["data"]["manifest_json"]
        .as_str()
        .unwrap()
        .as_bytes();
    let manifest_hash = Blake3Hash::digest(manifest_bytes);
    assert_eq!(planned["data"]["manifest_hash"], json!(manifest_hash));
    let manifest: Value = serde_json::from_slice(manifest_bytes).unwrap();
    assert_eq!(manifest["entries"].as_array().unwrap().len(), 1);
    assert_eq!(manifest["max_output_tokens"], 1024);
    assert_eq!(manifest["entries"][0]["source"]["source_id"], json!(source));
    assert_eq!(
        manifest["entries"][0]["source"]["revision_id"],
        json!(revision)
    );
    assert_eq!(
        manifest["entries"][0]["source"]["content_hash"],
        json!(Blake3Hash::digest(&original))
    );
    let manifest_path = f.temp.path().join("relations-manifest.json");
    std::fs::write(&manifest_path, manifest_bytes).unwrap();
    assert_eq!(std::fs::read(&manifest_path).unwrap(), manifest_bytes);
    let response_text = serde_json::to_string(&json!({
        "schema":"lwiki.source-relation-response.v1",
        "nodes":[{"id":"ada","kind":"rule","scope":"Ada Tool","span_id":"span_000000"},
            {"id":"tool","kind":"condition","scope":"Ada Tool","span_id":"span_000000"}],
        "edges":[{"from":"ada","to":"tool","kind":"qualifies","evidence_span_id":"span_000000"}]
    }))
    .unwrap();
    let response = serde_json::to_vec(&json!({"object":"response","status":"completed","model":"test-model",
        "output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":response_text}]}],
        "usage":{"input_tokens":10,"output_tokens":8,"total_tokens":18,
            "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},"output_tokens_details":{"reasoning_tokens":0},"cost":null}})).unwrap();
    let expected_original = original.clone();
    let server = std::thread::spawn(move || {
        let (mut socket, header, body) = accept_cli_json_request(&listener);
        assert!(header.starts_with("POST /exact-generation?retained=yes HTTP/1.1"));
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["max_output_tokens"], 1024);
        assert_eq!(body["store"], false);
        assert_eq!(body["input"][0]["role"], "user");
        assert!(body.get("messages").is_none());
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert!(
            body["instructions"]
                .as_str()
                .unwrap()
                .contains("unassessed navigation metadata")
        );
        let input: Value =
            serde_json::from_str(body["input"][0]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["schema"], "lwiki.source-relation-input.v1");
        assert_eq!(
            input["fragments"],
            json!([{"id":"span_000000","text":expected_original}])
        );
        assert!(input.get("query").is_none());
        socket
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
        socket.write_all(&response).unwrap();
        socket.flush().unwrap();
        (listener, body)
    });
    let run = "run_relation_cli";
    let run_args = [
        "relations",
        "run",
        "--file",
        manifest_path.to_str().unwrap(),
        "--run",
        run,
        "--max-tasks",
        "1",
    ];
    let first = invoke(&run_args, false, true);
    let (listener, sent) = server.join().unwrap();
    assert_eq!(first["meta"]["network_used"], true);
    let assert_report = |result: &Value| {
        assert_eq!(result["data"]["run_id"], run);
        assert_eq!(result["data"]["manifest_hash"], json!(manifest_hash));
        for key in ["manifest_sources", "admitted_tasks", "completed"] {
            assert_eq!(result["data"][key], 1);
        }
        for key in ["failed", "pending"] {
            assert_eq!(result["data"][key], 0);
        }
        assert_eq!(result["data"]["dry_run"], false);
        assert_eq!(result["meta"]["partial"], false);
    };
    assert_report(&first);
    let before: LedgerInspection = serde_json::from_value(native_probe_status(&f, run)).unwrap();
    assert_eq!(
        before.spec.scope.scope_payload_hash.as_ref(),
        Some(&manifest_hash)
    );
    assert_eq!(before.attempts.len(), 1);
    assert_eq!(before.attempts[0].phase, AttemptPhase::Settled);
    assert_eq!(before.attempts[0].bound.timeout_ms, 10_000);
    assert_eq!(
        before.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
    assert_eq!(before.budget.dispatched_requests, 1);
    assert_eq!(before.budget.outstanding.requests, 1);
    assert_eq!(before.budget.settled.requests, 0);
    assert_eq!(before.budget.unknown_attempts.len(), 1);
    assert_eq!(before.effective_limits.requests, 1);
    assert_eq!(before.effective_limits.concurrency, 1);
    assert_eq!(before.effective_limits.attempts_per_task, 1);
    let task = before.tasks.values().next().unwrap();
    assert_eq!(before.tasks.len(), 1);
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(json!(task.spec), manifest["entries"][0]["task"]);
    assert_eq!(task.outputs.len(), 2);
    assert_eq!(before.attempts[0].outputs, task.outputs);
    let mut retained = BTreeMap::new();
    for reference in task.outputs.iter().chain(std::iter::once(
        before.attempts[0].receipt.as_ref().unwrap(),
    )) {
        assert_eq!(reference.record.expected_kind, RecordKind::RunEvent);
        assert_eq!(
            reference.path.as_str(),
            format!("runs/{run}/events/{}.md", reference.record.record_id)
        );
        let bytes = std::fs::read(f.fs.root().resolve(&reference.path).unwrap()).unwrap();
        assert_eq!(Blake3Hash::digest(&bytes), reference.hash);
        assert_eq!(
            parse_note(&bytes)
                .canonical
                .as_ref()
                .unwrap()
                .string("wiki_run_id"),
            Some(run)
        );
        assert!(retained.insert(reference.path.clone(), bytes).is_none());
    }
    assert_eq!(retained.len(), 3);
    let raw = artifact(
        &retained[&task.outputs[0].path],
        "lwiki-source-relation-generation-v1",
    );
    let annotation = artifact(
        &retained[&task.outputs[1].path],
        "lwiki-source-relation-annotation-v1",
    );
    assert_eq!(raw["task"], manifest["entries"][0]["task"]);
    assert_eq!(raw["response"], response_text);
    assert_eq!(
        raw["response_hash"],
        json!(Blake3Hash::digest(&response_text))
    );
    assert_eq!(annotation["raw"], json!(task.outputs[0]));
    assert_eq!(
        annotation["envelope"]["source"]["raw_hash"],
        json!(Blake3Hash::digest(&original))
    );
    assert_eq!(annotation["envelope"]["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(annotation["envelope"]["edges"].as_array().unwrap().len(), 1);
    let descriptor = raw["descriptor"].as_str().unwrap();
    assert_eq!(
        raw["descriptor_hash"],
        json!(Blake3Hash::digest(descriptor))
    );
    assert_eq!(task.spec.input_hash, Blake3Hash::digest(descriptor));
    let input: lwiki::providers::types::RemoteInput = serde_json::from_str(descriptor).unwrap();
    let lwiki::providers::types::RemoteOperation::Generate {
        instructions,
        data,
        max_output_tokens,
        ..
    } = input.operation
    else {
        panic!("generation descriptor required")
    };
    assert_eq!(sent["instructions"], instructions);
    assert_eq!(sent["input"][0]["content"], data);
    assert_eq!(max_output_tokens, 1024);
    let second = invoke(&run_args, true, true);
    assert_report(&second);
    assert_eq!(first["data"], second["data"]);
    let disabled = invoke(&context_args, true, false);
    same_rendered_across_publication(&baseline, &disabled);
    std::fs::remove_file(&config).unwrap();
    let prepared = invoke(&experimental_args, true, false);
    assert_eq!(
        prepared["meta"]["evidence_strategy"],
        "experimental_source_relations"
    );
    assert!(
        prepared["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("unassessed"))
    );
    let check_originals = |result: &Value| {
        let passages = result["data"]["passages"].as_array().unwrap();
        assert!(!passages.is_empty());
        assert!(passages.len() <= 4);
        for passage in passages {
            let span: ByteSpan = serde_json::from_value(passage["span"].clone()).unwrap();
            assert!(span.len() <= 1024);
            assert_eq!(passage["text"], span.slice(&original).unwrap());
            assert_eq!(passage["locator"]["path"], content_path);
            assert_eq!(passage["label"], "captured_source");
            assert_eq!(passage["eligibility"], "current");
            let citations = passage["citations"].as_array().unwrap();
            assert!(!citations.is_empty());
            for citation in citations {
                let CitationRef::Source(citation) =
                    serde_json::from_value(citation.clone()).unwrap()
                else {
                    panic!("exact source citation required")
                };
                assert_eq!(
                    (&citation.source_id, &citation.source_revision),
                    (source, revision)
                );
                assert_eq!(citation.span, span);
                assert_eq!(
                    citation.quote_hash,
                    Blake3Hash::digest(span.slice(&original).unwrap())
                );
            }
        }
        let usage = &result["data"]["usage"];
        assert_eq!(
            usage["rendered_bytes"],
            result["data"]["text"].as_str().unwrap().len()
        );
        for (field, ceiling) in [
            ("rendered_bytes", 12000),
            ("estimated_tokens", 3000),
            ("verification_bytes", 67108864),
            ("verification_files", 4096),
            ("verification_entries", 16384),
        ] {
            assert!(
                usage[field].as_u64().unwrap() <= ceiling,
                "{field}: {usage}"
            );
        }
    };
    check_originals(&baseline);
    check_originals(&prepared);
    std::fs::remove_dir_all(f.temp.path().join(".wiki/cache")).unwrap();
    assert!(!config.exists());
    invoke(&["index", "rebuild", "--normalized"], true, false);
    let rebuilt = invoke(&experimental_args, true, false);
    same_rendered_across_publication(&prepared, &rebuilt);
    check_originals(&rebuilt);
    assert_eq!(std::fs::read(&manifest_path).unwrap(), manifest_bytes);
    for (path, bytes) in retained {
        assert_eq!(
            std::fs::read(f.fs.root().resolve(&path).unwrap()).unwrap(),
            bytes
        );
    }
    let after: LedgerInspection = serde_json::from_value(native_probe_status(&f, run)).unwrap();
    assert_eq!(after.attempts, before.attempts);
    assert_eq!(after.tasks, before.tasks);
    assert_eq!(after.budget, before.budget);
    assert_eq!(after.effective_limits, before.effective_limits);
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "only one HTTP request is permitted across the lifecycle"
    );
}

#[test]
fn native_responses_doctor_probe_uses_default_and_service_capped_output() {
    native_generation_probe_case(true, None, false);
    native_generation_probe_case(true, Some(64), false);
    native_generation_probe_case(true, Some(8192), false);
}

#[test]
fn native_chat_doctor_probe_keeps_legacy_wire_with_service_output_limit() {
    native_generation_probe_case(false, None, false);
}

#[test]
fn native_incomplete_responses_probe_retains_one_paid_attempt_and_fixed_reason() {
    native_generation_probe_case(true, None, true);
    native_generation_probe_case(true, Some(8192), true);
    native_generation_probe_auth_case(true, Some(8192), true, false, true);
}

#[test]
fn doctor_generation_zero_output_allowance_refuses_before_provider_or_run_writes() {
    let f = Fixture::new();
    let config = f.temp.path().join("providers.toml");
    let before = files(f.temp.path());
    for extraction_schema in [false, true] {
        let mut argv = vec![
            "lwiki",
            "--wiki",
            f.temp.path().to_str().unwrap(),
            "--no-user-config",
            "--json",
            "--profile",
            "primary",
            "doctor",
            "--probe",
            "--role",
            "generate",
            "--providers-config",
            config.to_str().unwrap(),
            "--max-output-units",
            "0",
        ];
        if extraction_schema {
            argv.push("--extraction-schema");
        }
        let args = Arguments::try_parse_from(argv).unwrap();
        let (envelope, exit) = execute(&args);
        assert_ne!(exit, 0);
        assert!(!envelope.meta.network_used);
        assert_eq!(envelope.error.unwrap().code, "BUDGET_EXCEEDED");
        assert_eq!(files(f.temp.path()), before);
    }
}

fn native_generation_probe_case(responses: bool, service_cap: Option<u64>, incomplete: bool) {
    native_generation_probe_auth_case(responses, service_cap, incomplete, false, false);
}

#[test]
fn native_generation_probe_accepts_private_credential_file_with_terminal_newline() {
    native_generation_probe_auth_case(false, None, false, true, false);
}

fn native_generation_probe_auth_case(
    responses: bool,
    service_cap: Option<u64>,
    incomplete: bool,
    file_auth: bool,
    extraction_schema: bool,
) {
    let f = Fixture::new();
    let expected = service_cap.unwrap_or(4096);
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let body = if incomplete {
        json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"error":null,
            "usage":{"input_tokens":24,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},
                "output_tokens":16,"output_tokens_details":{"reasoning_tokens":16},"total_tokens":40,"cost":null}})
    } else if responses {
        json!({"object":"response","status":"completed","model":"test-model","incomplete_details":null,"error":null,
            "output":[{"type":"reasoning","encrypted_content":"ignored"},
                {"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"{\"ok\":true}"}]}],
            "usage":{"input_tokens":24,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},
                "output_tokens":16,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":40,"cost":null}})
    } else {
        json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop",
            "message":{"role":"assistant","content":"{\"ok\":true}"}}],
            "usage":{"prompt_tokens":24,"prompt_tokens_details":{"cached_tokens":0},
                "completion_tokens":16,"completion_tokens_details":{"reasoning_tokens":0},"total_tokens":40}})
    };
    let reply = serde_json::to_vec(&body).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "doctor probe never reached mock");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("doctor mock accept: {error}"),
            }
        };
        // Accepted sockets can inherit the listener's nonblocking mode on macOS.
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut request = Vec::new();
        let (header_end, length) = loop {
            let mut chunk = [0; 4096];
            let size = socket.read(&mut chunk).unwrap();
            assert!(size > 0);
            request.extend_from_slice(&chunk[..size]);
            assert!(request.len() < 512 * 1024);
            if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                let header = std::str::from_utf8(&request[..end]).unwrap();
                let length = header
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                break (end + 4, length);
            }
        };
        while request.len() < header_end + length {
            let mut chunk = [0; 4096];
            let size = socket.read(&mut chunk).unwrap();
            assert!(size > 0);
            request.extend_from_slice(&chunk[..size]);
        }
        let request: Value =
            serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",reply.len()).unwrap();
        socket.write_all(&reply).unwrap();
        socket.flush().unwrap();
        request
    });
    let config = f.temp.path().join("providers.toml");
    let content = std::fs::read_to_string(&config)
        .unwrap()
        .replace(
            "https://gateway.example/chat",
            &format!("http://{address}/probe"),
        )
        .replace(
            "model=\"test-model\"",
            "allow_loopback_http=true\nmodel=\"test-model\"",
        );
    let content = if responses {
        content.replace(
            "adapter=\"chat-completions-v1\"",
            "adapter=\"responses-v1\"\nresponse_mode=\"json-schema\"",
        )
    } else {
        content
    };
    let content = if let Some(cap) = service_cap {
        content.replace(
            "revision=\"r1\"",
            &format!("revision=\"r1\"\nmax_output_tokens={cap}"),
        )
    } else {
        content
    };
    let credential_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(credential_file.path(), b"synthetic-cli-secret\n").unwrap();
    let content = if file_auth {
        content.replace(
            "key_env=\"EXPLICIT_FIXTURE_TOKEN\"",
            &format!(
                "key_file={}",
                provider::quote(credential_file.path().to_str().unwrap())
            ),
        )
    } else {
        content
    };
    std::fs::write(&config, content).unwrap();
    let selection_temp = tempfile::tempdir().unwrap();
    let selection_base = std::fs::canonicalize(selection_temp.path()).unwrap();
    let default_a = selection_base.join("Default A notes");
    std::fs::create_dir(&default_a).unwrap();
    std::fs::write(default_a.join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: Vault.Default.A\nwiki_kind: vault\ntitle: Default A\n---\n").unwrap();
    let preferences = selection_base.join("Owner's probe preferences.toml");
    let mut document = lwiki::config::LocalConfigDocument {
        default_wiki: Some(lwiki::config::DefaultWiki {
            path: default_a.clone(),
            wiki_id: RecordId::new("Vault.Default.A").unwrap(),
        }),
        providers_config: Some(selection_base.join("Unused A providers.toml")),
        ..Default::default()
    };
    document.defaults.profile = Some("unused-default-profile".into());
    std::fs::write(&preferences, toml::to_string(&document).unwrap()).unwrap();
    let result = std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .args([
            "--wiki",
            f.temp.path().to_str().unwrap(),
            "--preferences",
            preferences.to_str().unwrap(),
            "--no-user-config",
            "--json",
            "--profile",
            "primary",
            "doctor",
            "--probe",
            "--role",
            "generate",
            "--providers-config",
            config.to_str().unwrap(),
            "--max-requests",
            "1",
            "--attempts-per-task",
            "1",
        ])
        .args(extraction_schema.then_some("--extraction-schema"))
        .env("EXPLICIT_FIXTURE_TOKEN", "synthetic-cli-secret")
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(
        result.stderr.is_empty(),
        "JSON probe must have no prose on stderr"
    );
    // Present the actual CLI payload, including real retained accounting, rather
    // than a hand-written approximation of the provider outcome.
    let mut rendered_envelope = lwiki::output::Envelope::success("doctor", value["data"].clone());
    rendered_envelope.ok = value["ok"].as_bool().unwrap();
    if incomplete {
        let error = &value["error"];
        rendered_envelope.error = Some(lwiki::output::ErrorOutput {
            code: error["code"].as_str().unwrap().into(),
            message: error["message"].as_str().unwrap().into(),
            retryable: error["retryable"].as_bool().unwrap(),
            hint: error["hint"].as_str().map(str::to_owned),
            details: error["details"].clone(),
        });
    }
    let mut human = Vec::new();
    lwiki::cli::present(
        &rendered_envelope,
        lwiki::cli::OutputFormat::Human,
        &mut human,
    )
    .unwrap();
    let human = String::from_utf8(human).unwrap();
    assert!(human.contains(&format!("http://{address}")), "{human}");
    assert!(human.contains("test-model"), "{human}");
    assert!(!human.contains("synthetic-cli-secret"), "{human}");
    assert!(!human.contains("\"attempts\""), "{human}");
    if incomplete {
        assert!(
            human.contains("Provider probe (generate): failed"),
            "{human}"
        );
        assert!(human.contains("usage: unknown"), "{human}");
        assert!(
            human.contains(&format!("{expected}-token output limit")),
            "{human}"
        );
        assert!(human.contains("Reasoning tokens"), "{human}");
        assert!(
            human.contains("config set generation")
                && human.contains("--max-output-tokens TOKENS --grant-provider --non-interactive"),
            "{human}"
        );
        assert!(!human.contains("--max-output-units"), "{human}");
        assert!(
            human.contains("repeat your original doctor command"),
            "{human}"
        );
        assert_eq!(human.contains("--extraction-schema"), extraction_schema);
    } else {
        assert!(
            human.contains("Provider probe (generate): succeeded"),
            "{human}"
        );
        assert!(human.contains("Requests dispatched: 1"), "{human}");
        assert!(human.contains("Cost: unknown"), "{human}");
    }
    let request = server.join().unwrap();
    if responses {
        assert_eq!(request["max_output_tokens"], expected);
        assert!(request.get("messages").is_none());
        assert_eq!(request["store"], false);
    } else {
        assert_eq!(request["max_completion_tokens"], expected);
        assert!(request.get("input").is_none());
    }
    assert_eq!(result.status.success(), !incomplete, "{value}");
    assert_eq!(value["meta"]["network_used"], true);
    if incomplete {
        assert_eq!(value["error"]["code"], "PROVIDER_RESPONSE");
        assert_eq!(
            value["error"]["details"]["cause"]["reason"],
            "incomplete_max_output_tokens"
        );
        assert_eq!(
            value["error"]["details"]["cause"]["max_output_tokens"],
            expected
        );
        assert_eq!(
            value["error"]["details"]["cause"]["effective_service_cap"],
            expected
        );
        assert!(
            value["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("--max-output-tokens TOKENS")
        );
        let repair = value["error"]["details"]["repair_command"]
            .as_str()
            .unwrap();
        let words = shlex::split(repair).unwrap().into_iter().map(|word| {
            if word == "TOKENS" {
                (expected * 2).to_string()
            } else {
                word
            }
        });
        let repair_args = Arguments::try_parse_from(words).unwrap();
        assert_eq!(repair_args.wiki.as_deref(), Some(f.fs.root().path()));
        assert_ne!(repair_args.wiki.as_ref(), Some(&default_a));
        assert_eq!(repair_args.preferences.as_ref(), Some(&preferences));
        assert_eq!(repair_args.expect_wiki_id.as_ref(), Some(f.app.vault_id()));
        assert_eq!(repair_args.profile.as_deref(), Some("primary"));
        let lwiki::cli::Command::Config {
            command: lwiki::cli::arguments::ConfigCommand::Set(settings),
        } = repair_args.command
        else {
            panic!("repair is not config set");
        };
        assert_eq!(settings.providers_config.as_ref(), Some(&config));
        assert!(
            value["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("Omit its old --profile")
        );
        let accounting = Arguments::try_parse_from(
            shlex::split(
                value["error"]["details"]["accounting_command"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(accounting.wiki.as_deref(), Some(f.fs.root().path()));
        assert_eq!(accounting.preferences.as_ref(), Some(&preferences));
        let run = value["error"]["details"]["run_id"].as_str().unwrap();
        let projection =
            lwiki::catalog::scan::scan(&f.fs, &lwiki::domain::RecordId::new("vault_test").unwrap())
                .unwrap();
        let receipts: Vec<_> = projection
            .records
            .values()
            .filter(|row| {
                row.record.kind() == lwiki::domain::RecordKind::RunEvent
                    && row.record.string("wiki_run_id") == Some(run)
                    && row.record.string("wiki_event_type") == Some("usage_receipt")
            })
            .collect();
        assert_eq!(receipts.len(), 1);
        let bytes = std::fs::read(f.temp.path().join(receipts[0].path.as_str())).unwrap();
        let retained = String::from_utf8(bytes).unwrap();
        assert!(retained.contains("incomplete_max_output_tokens"));
        assert!(retained.contains("\"billable_units\""));
        let inspection = native_probe_status(&f, run);
        assert_eq!(inspection["state"], "paused");
        assert_eq!(inspection["attempts"].as_array().unwrap().len(), 1);
        assert_eq!(inspection["budget"]["dispatched_requests"], 1);
        assert_eq!(inspection["attempts"][0]["phase"], "settled");
        // The fixture has no trusted rate card: pausing must retain the paid
        // attempt's unknown billing rather than mark it released or free.
        assert_eq!(inspection["attempts"][0]["billing"], "unknown_reserved");
    } else {
        assert_eq!(value["data"]["probe"]["validated"], true);
        assert_eq!(
            value["data"]["probe"]["inspection"]["attempts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

fn native_probe_status(f: &Fixture, run: &str) -> Value {
    let output = std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .args([
            "--wiki",
            f.temp.path().to_str().unwrap(),
            "--json",
            "--offline",
            "jobs",
            "status",
            "--run",
            run,
        ])
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success(), "{value}");
    value["data"].clone()
}

#[test]
fn native_embedding_probe_small_response_budget_is_actionable_and_amendable() {
    native_probe_setup_failure(false);
}

#[cfg(unix)]
#[test]
fn native_probe_insecure_credential_file_is_actionable_and_never_sent() {
    native_probe_setup_failure(true);
}

fn native_probe_setup_failure(insecure_file: bool) {
    let f = Fixture::new();
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = f.temp.path().join("providers.toml");
    let credential_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(credential_file.path(), b"must-not-leak-fixture-secret\n").unwrap();
    #[cfg(unix)]
    if insecure_file {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            credential_file.path(),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
    }
    let mut content = std::fs::read_to_string(&config)
        .unwrap()
        .replace("generation=\"service\"", "embedding=\"service\"")
        .replace(
            "adapter=\"chat-completions-v1\"",
            "adapter=\"embeddings-v1\"",
        )
        .replace(
            "https://gateway.example/chat",
            &format!("http://{}/probe", listener.local_addr().unwrap()),
        )
        .replace(
            "model=\"test-model\"",
            "allow_loopback_http=true\nmodel=\"test-model\"",
        );
    if insecure_file {
        content = content.replace(
            "key_env=\"EXPLICIT_FIXTURE_TOKEN\"",
            &format!(
                "key_file={}",
                provider::quote(credential_file.path().to_str().unwrap())
            ),
        );
    }
    std::fs::write(&config, content).unwrap();
    let mut command = std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
    command.args([
        "--wiki",
        f.temp.path().to_str().unwrap(),
        "--json",
        "--profile",
        "primary",
        "doctor",
        "--probe",
        "--role",
        "embed",
        "--providers-config",
        config.to_str().unwrap(),
        "--max-requests",
        "1",
        "--attempts-per-task",
        "1",
    ]);
    if !insecure_file {
        command.args(["--max-response-bytes", "262144"]);
    }
    let output = command
        .env_remove("EXPLICIT_FIXTURE_TOKEN")
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!output.status.success(), "{value}");
    assert_eq!(value["meta"]["network_used"], false);
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("must-not-leak-fixture-secret"));
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains(credential_file.path().to_str().unwrap())
    );
    let expected = if insecure_file {
        "credential_file_unavailable_or_invalid"
    } else {
        "response_byte_budget_exceeded"
    };
    assert_eq!(value["error"]["details"]["cause"]["reason"], expected);
    let hint = value["error"]["details"]["cause"]["next_action"]
        .as_str()
        .unwrap();
    assert!(hint.contains(if insecure_file { "0600" } else { "8388608" }));
    let run = value["error"]["details"]["run_id"].as_str().unwrap();
    let inspection = native_probe_status(&f, run);
    assert_eq!(inspection["state"], "paused");
    let attempts = inspection["attempts"].as_array().unwrap();
    if insecure_file {
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["billing"], "released_not_sent");
    } else {
        assert!(attempts.is_empty());
        let amended = std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
            .args([
                "--wiki",
                f.temp.path().to_str().unwrap(),
                "--json",
                "--offline",
                "jobs",
                "amend",
                "--run",
                run,
                "--reason",
                "explicit fixture response allowance",
                "--max-response-bytes",
                "8388608",
            ])
            .output()
            .unwrap();
        let result: Value = serde_json::from_slice(&amended.stdout).unwrap();
        assert!(amended.status.success(), "{result}");
        assert_eq!(
            result["data"]["inspection"]["effective_limits"]["response_bytes"],
            8388608
        );
        assert_eq!(
            result["data"]["inspection"]["attempts"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }
}

#[test]
fn native_generation_probe_unknown_unit_bound_refuses_before_send() {
    let f = Fixture::new();
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = f.temp.path().join("providers.toml");
    let content = std::fs::read_to_string(&config)
        .unwrap()
        .replace(
            "https://gateway.example/chat",
            &format!("http://{address}/probe"),
        )
        .replace(
            "model=\"test-model\"",
            "allow_loopback_http=true\nmodel=\"test-model\"",
        )
        .replace(
            "adapter=\"chat-completions-v1\"",
            "adapter=\"responses-v1\"\nresponse_mode=\"json-schema\"",
        );
    std::fs::write(&config, content).unwrap();
    let result = std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .args([
            "--wiki",
            f.temp.path().to_str().unwrap(),
            "--json",
            "--profile",
            "primary",
            "doctor",
            "--probe",
            "--role",
            "generate",
            "--providers-config",
            config.to_str().unwrap(),
            "--max-requests",
            "1",
            "--attempts-per-task",
            "1",
            "--max-output-units",
            "64",
        ])
        .env("EXPLICIT_FIXTURE_TOKEN", "synthetic-cli-secret")
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(!result.status.success(), "{value}");
    assert_eq!(value["error"]["code"], "CAPABILITY_UNAVAILABLE");
    assert_eq!(value["meta"]["network_used"], false);
    let run = value["error"]["details"]["run_id"].as_str().unwrap();
    let input: lwiki::providers::types::RemoteInput = serde_json::from_slice(
        &std::fs::read(f.temp.path().join(format!("runs/{run}/inputs/probe.json"))).unwrap(),
    )
    .unwrap();
    let lwiki::providers::types::RemoteOperation::Generate {
        max_output_tokens, ..
    } = input.operation
    else {
        panic!("generation probe retained a non-generation request");
    };
    assert_eq!(max_output_tokens, 64);
    let inspection = native_probe_status(&f, run);
    assert!(inspection["attempts"].as_array().unwrap().is_empty());
    assert_eq!(inspection["budget"]["dispatched_requests"], 0);
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[path = "fixtures/p17/common.rs"]
mod embedding_fixture;
fn accept_cli_json_request(
    listener: &std::net::TcpListener,
) -> (std::net::TcpStream, String, Value) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut socket = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "CLI mock request timeout");
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("embedding mock accept: {error}"),
        }
    };
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = Vec::new();
    let (start, length, header) = loop {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).unwrap();
        assert!(count > 0);
        request.extend_from_slice(&buffer[..count]);
        assert!(request.len() < 512 * 1024);
        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            let header = std::str::from_utf8(&request[..end]).unwrap();
            let length = header
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            assert!(end + 4 + length < 512 * 1024);
            break (end + 4, length, header.to_owned());
        }
    };
    while request.len() < start + length {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).unwrap();
        assert!(count > 0);
        request.extend_from_slice(&buffer[..count]);
    }
    let body: Value = serde_json::from_slice(&request[start..start + length]).unwrap();
    (socket, header, body)
}
fn accept_embedding_request(listener: &std::net::TcpListener) -> std::net::TcpStream {
    let (socket, _, body) = accept_cli_json_request(listener);
    assert_eq!(body["model"], "test-model");
    socket
}
fn send_embedding_response(socket: &mut std::net::TcpStream, invalid_usage: bool) {
    let usage = if invalid_usage {
        json!({"prompt_tokens":"bad","total_tokens":2})
    } else {
        json!({"prompt_tokens":2,"total_tokens":2,"completion_tokens":0})
    };
    let response = serde_json::to_vec(&json!({"model":"test-model",
        "data":[{"index":0,"embedding":[1.0,0.0]}],"usage":usage}))
    .unwrap();
    write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",response.len()).unwrap();
    socket.write_all(&response).unwrap();
    socket.flush().unwrap();
}
fn embedding_cli(f: &embedding_fixture::Fixture, args: &[&str]) -> (Value, bool) {
    let output = std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .args([
            "--wiki",
            f.fs.root().path().to_str().unwrap(),
            "--json",
            "--profile",
            "primary",
        ])
        .args(args)
        .args(["--providers-config", f.config.to_str().unwrap()])
        .output()
        .unwrap();
    (
        serde_json::from_slice(&output.stdout).unwrap(),
        output.status.success(),
    )
}
fn embedding_mock_config(f: &embedding_fixture::Fixture, endpoint: std::net::SocketAddr) {
    let content = std::fs::read_to_string(&f.config).unwrap().replace(
        "url='https://mock.example/v1/embeddings'",
        &format!("url='http://{endpoint}/embedding'\nallow_loopback_http=true"),
    );
    embedding_fixture::private_write(&f.config, content);
}
#[test]
fn rejected_probe_retains_accounting_and_same_space_can_probe_then_sync() {
    let f = embedding_fixture::Fixture::new();
    f.page("one", "Alpha");
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    embedding_mock_config(&f, listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_calls = calls.clone();
    let server = std::thread::spawn(move || {
        for invalid in [true, false, false] {
            let mut socket = accept_embedding_request(&listener);
            observed_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            send_embedding_response(&mut socket, invalid);
        }
    });
    let (first, success) = embedding_cli(&f, &["embeddings", "check", "--probe"]);
    assert!(!success, "{first}");
    assert_eq!(first["error"]["code"], "PROVIDER_RESPONSE");
    assert_eq!(first["error"]["details"]["reason"], "usage_invalid");
    let old_run: RecordId =
        serde_json::from_value(first["error"]["details"]["run_id"].clone()).unwrap();
    let ledger = lwiki::jobs::JobLedger::new(
        f.fs.clone(),
        f.app.vault_id().clone(),
        old_run.clone(),
        embedding_fixture::options(),
    )
    .unwrap();
    let before = ledger.inspect().unwrap();
    assert_eq!(before.attempts.len(), 1);
    let original = before.attempts[0].clone();
    assert_eq!(original.phase, lwiki::jobs::AttemptPhase::Settled);
    assert_eq!(
        original.billing,
        lwiki::jobs::BillingDisposition::UnknownReserved
    );
    let receipt_path =
        f.fs.root()
            .resolve(&original.receipt.as_ref().unwrap().path)
            .unwrap();
    let receipt_bytes = std::fs::read(&receipt_path).unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let (second, success) = embedding_cli(&f, &["embeddings", "check", "--probe"]);
    assert!(success, "{second}");
    let (third, success) = embedding_cli(&f, &["embeddings", "sync"]);
    assert!(success, "{third}");
    assert_eq!(third["data"]["published"], true);
    assert!(
        third["data"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .is_some_and(|warning| warning.contains("Rejected embedding probe"))),
        "{third}"
    );
    server.join().unwrap();
    let ledger = lwiki::jobs::JobLedger::new(
        f.fs.clone(),
        f.app.vault_id().clone(),
        old_run,
        embedding_fixture::options(),
    )
    .unwrap();
    let inspection = ledger.inspect().unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_eq!(inspection.attempts[0], original);
    assert_eq!(inspection.budget.outstanding, before.budget.outstanding);
    assert_eq!(
        inspection.budget.unknown_attempts,
        before.budget.unknown_attempts
    );
    assert_eq!(inspection.effective_limits, before.effective_limits);
    assert_eq!(
        inspection.effective_deadline_utc_ms,
        before.effective_deadline_utc_ms
    );
    assert_eq!(inspection.state, before.state);
    assert_eq!(std::fs::read(&receipt_path).unwrap(), receipt_bytes);
    assert_eq!(inspection.attempts.len(), 1);
    assert_eq!(
        inspection.attempts[0].phase,
        lwiki::jobs::AttemptPhase::Settled
    );
    assert!(inspection.attempts[0].receipt.is_some());
    assert_eq!(inspection.budget.dispatched_requests, 1);
}
#[test]
fn uncertain_embedding_retry_requires_opt_in_and_keeps_original_hold() {
    let f = embedding_fixture::Fixture::new();
    f.page("one", "Alpha");
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    embedding_mock_config(&f, listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        drop(accept_embedding_request(&listener));
        let mut socket = accept_embedding_request(&listener);
        send_embedding_response(&mut socket, false);
    });
    let (first, success) = embedding_cli(&f, &["embeddings", "sync"]);
    assert!(!success, "{first}");
    let run: RecordId =
        serde_json::from_value(first["error"]["details"]["run_id"].clone()).unwrap();
    let (denied, success) = embedding_cli(&f, &["embeddings", "sync"]);
    assert!(!success, "{denied}");
    assert_eq!(denied["error"]["code"], "RECOVERY_REQUIRED");
    let (amended, success) = embedding_cli(
        &f,
        &[
            "jobs",
            "amend",
            "--run",
            run.as_str(),
            "--reason",
            "retain unknown attempt and raise allowance",
            "--max-requests",
            "100",
        ],
    );
    assert!(success, "{amended}");
    assert_eq!(
        amended["data"]["inspection"]["budget"]["dispatched_requests"],
        1
    );
    assert_eq!(
        amended["data"]["inspection"]["budget"]["unknown_attempts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        amended["data"]["inspection"]["effective_limits"]["requests"],
        100
    );
    let (changed, success) = embedding_cli(
        &f,
        &[
            "embeddings",
            "sync",
            "--retry-uncertain",
            "--max-requests",
            "60",
        ],
    );
    assert!(!success, "{changed}");
    assert_eq!(changed["error"]["code"], "USAGE");
    assert_eq!(
        changed["error"]["details"]["reason"],
        "retained_limits_require_amendment"
    );
    let (retried, success) = embedding_cli(&f, &["embeddings", "sync", "--retry-uncertain"]);
    assert!(success, "{retried}");
    assert_eq!(retried["data"]["published"], true);
    server.join().unwrap();
    let ledger = lwiki::jobs::JobLedger::new(
        f.fs.clone(),
        f.app.vault_id().clone(),
        run,
        embedding_fixture::options(),
    )
    .unwrap();
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.attempts.len(), 2);
    assert_eq!(inspection.budget.dispatched_requests, 2);
    // The retry's valid response has token usage but no cost/rate card; its
    // billing hold is retained as well as the original unknown outcome.
    assert_eq!(inspection.budget.unknown_attempts.len(), 2);
    assert_eq!(
        inspection.attempts[1].phase,
        lwiki::jobs::AttemptPhase::Settled
    );
    assert_eq!(
        inspection.attempts[1].billing,
        lwiki::jobs::BillingDisposition::UnknownReserved
    );
    assert_eq!(
        inspection.attempts[0].billing,
        lwiki::jobs::BillingDisposition::UnknownReserved
    );
}
#[test]
fn uncertain_embedding_retry_respects_concurrency_and_final_request_slot() {
    for bound in [["--concurrency", "1"], ["--max-requests", "1"]] {
        let f = embedding_fixture::Fixture::new();
        f.page("one", "Alpha");
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        embedding_mock_config(&f, listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || drop(accept_embedding_request(&listener)));
        let first_args = ["embeddings", "sync", bound[0], bound[1]];
        let (first, success) = embedding_cli(&f, &first_args);
        assert!(!success, "{first}");
        server.join().unwrap();
        let retry_args = [
            "embeddings",
            "sync",
            bound[0],
            bound[1],
            "--retry-uncertain",
        ];
        let (retry, success) = embedding_cli(&f, &retry_args);
        assert!(!success, "{retry}");
        assert_eq!(retry["error"]["code"], "BUDGET_EXCEEDED", "{retry}");
        let run: RecordId =
            serde_json::from_value(first["error"]["details"]["run_id"].clone()).unwrap();
        let ledger = lwiki::jobs::JobLedger::new(
            f.fs.clone(),
            f.app.vault_id().clone(),
            run,
            embedding_fixture::options(),
        )
        .unwrap();
        let inspection = ledger.inspect().unwrap();
        assert_eq!(inspection.attempts.len(), 1);
        assert_eq!(inspection.budget.unknown_attempts.len(), 1);
    }
}
#[test]
fn expired_planned_embedding_job_can_be_amended_without_resetting_history() {
    let f = embedding_fixture::Fixture::new();
    f.page("one", "Alpha");
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    embedding_mock_config(&f, listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let (expired, success) = embedding_cli(&f, &["embeddings", "sync", "--deadline-ms", "1"]);
    assert!(!success, "{expired}");
    assert_eq!(expired["error"]["code"], "BUDGET_EXCEEDED");
    let marker_dir = f.fs.root().path().join(".wiki/state/embedding-jobs");
    let marker = std::fs::read_dir(marker_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let marker: Value = serde_json::from_slice(&std::fs::read(marker).unwrap()).unwrap();
    let run = marker["run_id"].as_str().unwrap();
    let before = files(f.fs.root().path());
    let (preview, success) = embedding_cli(
        &f,
        &[
            "--dry-run",
            "jobs",
            "amend",
            "--run",
            run,
            "--reason",
            "resume expired plan",
            "--max-requests",
            "100",
            "--deadline-ms",
            "900000",
        ],
    );
    assert!(success, "{preview}");
    assert_eq!(preview["data"]["applied"], false);
    assert_eq!(files(f.fs.root().path()), before);
    let (lowered, success) = embedding_cli(
        &f,
        &[
            "jobs",
            "amend",
            "--run",
            run,
            "--reason",
            "invalid reduction",
            "--max-requests",
            "1",
            "--deadline-ms",
            "900000",
        ],
    );
    assert!(!success, "{lowered}");
    assert_eq!(files(f.fs.root().path()), before);
    let (amended, success) = embedding_cli(
        &f,
        &[
            "jobs",
            "amend",
            "--run",
            run,
            "--reason",
            "resume expired plan",
            "--max-requests",
            "100",
            "--deadline-ms",
            "900000",
        ],
    );
    assert!(success, "{amended}");
    assert_eq!(amended["data"]["inspection"]["state"], "planned");
    assert_eq!(
        amended["data"]["inspection"]["effective_limits"]["requests"],
        100
    );
    assert_eq!(
        amended["data"]["inspection"]["budget"]["dispatched_requests"],
        0
    );
    let server = std::thread::spawn(move || {
        let mut socket = accept_embedding_request(&listener);
        send_embedding_response(&mut socket, false);
    });
    let (resumed, success) = embedding_cli(&f, &["embeddings", "sync"]);
    assert!(success, "{resumed}");
    assert_eq!(resumed["data"]["published"], true);
    server.join().unwrap();
    let ledger = lwiki::jobs::JobLedger::new(
        f.fs.clone(),
        f.app.vault_id().clone(),
        RecordId::new(run).unwrap(),
        embedding_fixture::options(),
    )
    .unwrap();
    let inspection = ledger.inspect().unwrap();
    assert_eq!(inspection.spec.limits.requests, 60);
    assert_eq!(inspection.effective_limits.requests, 100);
    assert_eq!(inspection.budget.dispatched_requests, 1);
}
#[test]
fn native_embedding_cli_sync_query_graph_context_and_cached_offline_reuse() {
    let f = embedding_fixture::Fixture::new();
    f.page("one", "Alpha");
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        for request_number in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "native CLI mock request timeout");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => panic!("mock accept: {e}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                assert!(bytes.len() < 512 * 1024);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = std::str::from_utf8(&bytes[..end]).unwrap();
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    assert!(header.starts_with("POST /exact-embedding?retained=yes HTTP/1.1"));
                    if bytes.len() >= end + 4 + length {
                        let request: Value =
                            serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                        assert_eq!(request["encoding_format"], "float");
                        assert_eq!(request["input"].as_array().unwrap().len(), 1);
                        assert_eq!(request["model"], "test-model");
                        assert!(request["input"][0].as_str().unwrap().starts_with(
                            if request_number == 0 {
                                "document: "
                            } else {
                                "query: "
                            }
                        ));
                        break;
                    }
                }
            }
            let response = serde_json::to_vec(&json!({"model":"test-model",
                "data":[{"index":0,"embedding":[1.0,0.0]}],
                "usage":{"prompt_tokens":2,"total_tokens":2,"completion_tokens":0,"prompt_tokens_details":null,"completion_tokens_details":null}}))
            .unwrap();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nllm_provider-x-amzn-requestid: synthetic-id\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",response.len()).unwrap();
            socket.write_all(&response).unwrap();
            socket.flush().unwrap();
        }
    });
    let content = std::fs::read_to_string(&f.config).unwrap().replace(
        "url='https://mock.example/v1/embeddings'",
        &format!("url='http://{address}/exact-embedding?retained=yes'\nallow_loopback_http=true"),
    );
    embedding_fixture::private_write(&f.config, content);
    let preferences = f.fs.root().path().join("saved-config.toml");
    let mut document = lwiki::config::LocalConfigDocument {
        providers_config: Some(f.config.clone()),
        ..Default::default()
    };
    document.defaults.profile = Some("primary".into());
    document.allowed_profiles = vec!["primary".into()];
    document.embeddings.document_prefix = Some("document: ".into());
    document.embeddings.query_prefix = Some("query: ".into());
    document.embeddings.max_input_bytes = Some(4096);
    document.remote.max_requests = Some(3);
    embedding_fixture::private_write(&preferences, toml::to_string(&document).unwrap());
    let invoke = |command: &[&str], offline: bool| {
        let mut process =
            std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
        process.args([
            "--wiki",
            f.fs.root().path().to_str().unwrap(),
            "--json",
            "--config",
            preferences.to_str().unwrap(),
        ]);
        if offline {
            process.arg("--offline");
        }
        process.args(command);
        let result = process.output().unwrap();
        let value: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert!(result.status.success(), "{value}");
        assert!(!String::from_utf8_lossy(&result.stdout).contains("mock-secret"));
        value
    };
    let sync = invoke(&["embeddings", "sync"], false);
    assert_eq!(sync["meta"]["network_used"], true);
    assert_eq!(sync["data"]["published"], true);
    let search = invoke(&["search", "question", "--mode", "semantic"], false);
    assert_eq!(search["meta"]["network_used"], true);
    assert_eq!(search["data"]["hits"].as_array().unwrap().len(), 1);
    server.join().unwrap();
    // Cached queries need their retained space and current source proof, even
    // when the now-unused private provider file is unavailable.
    std::fs::remove_file(&f.config).unwrap();
    let offline = invoke(&["search", "question", "--mode", "semantic"], true);
    assert_eq!(offline["meta"]["network_used"], false);
    assert_eq!(offline["data"]["hits"], search["data"]["hits"]);
    let graph = invoke(&["graph", "query", "question", "--seed", "semantic"], false);
    assert_eq!(graph["meta"]["network_used"], false);
    let context = invoke(
        &[
            "context", "question", "--mode", "hybrid", "--target", "combined", "--seed", "semantic",
        ],
        false,
    );
    assert_eq!(context["meta"]["network_used"], false);
    assert!(context["data"]["text"].as_str().unwrap().contains("Alpha"));
    let check = invoke(&["embeddings", "check"], true);
    assert_eq!(check["meta"]["network_used"], false);
}

#[test]
fn native_cli_successful_fallback_keeps_prior_paid_network_activity() {
    let mut f = embedding_fixture::Fixture::new();
    f.page("one", "Alpha");
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let content = std::fs::read_to_string(&f.config).unwrap().replace(
        "url='https://mock.example/v1/embeddings'",
        &format!("url='http://{address}/embedding'\nallow_loopback_http=true"),
    );
    embedding_fixture::private_write(&f.config, &content);
    f.service = lwiki::config::providers::ProviderConfig::load(&f.config)
        .unwrap()
        .authorize(
            &f.fs,
            f.app.vault_id(),
            "primary",
            lwiki::jobs::Capability::Embed,
        )
        .unwrap();
    let units = embedding_fixture::corpus(&f);
    f.seed(&f.spec(), &units, true);
    let config = f.config.clone();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "fallback mock request timeout");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("mock accept: {error}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut request = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let count = socket.read(&mut buf).unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buf[..count]);
            assert!(request.len() < 512 * 1024);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let length = std::str::from_utf8(&request[..end])
                    .unwrap()
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        // Invalidate the original trusted snapshot after entry, before retry.
        embedding_fixture::private_write(
            &config,
            format!("{content}\n# changed after first send\n"),
        );
        socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 2\r\nRetry-After: 0\r\nConnection: close\r\n\r\n{}").unwrap();
        socket.flush().unwrap();
    });
    let result = std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .args([
            "--wiki",
            f.fs.root().path().to_str().unwrap(),
            "--json",
            "--profile",
            "primary",
            "search",
            "Alpha",
            "--mode",
            "semantic",
            "--lexical-fallback",
            "--providers-config",
            f.config.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    server.join().unwrap();
    let envelope: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(result.status.success(), "{envelope}");
    assert_eq!(envelope["meta"]["network_used"], true);
    assert_eq!(envelope["data"]["network_used"], true);
    assert_eq!(envelope["data"]["hits"].as_array().unwrap().len(), 1);
    assert!(
        envelope["warnings"].to_string().contains("fallback"),
        "{envelope}"
    );
}

#[test]
fn rejected_probe_corrupt_receipt_and_unresolved_probe_refuse_before_corpus_payment() {
    for unresolved in [false, true] {
        let f = embedding_fixture::Fixture::new();
        f.page("one", "Alpha");
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        embedding_mock_config(&f, listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let server = std::thread::spawn(move || {
            let mut socket = accept_embedding_request(&listener);
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if !unresolved {
                send_embedding_response(&mut socket, true);
            }
        });
        let (first, success) = embedding_cli(&f, &["embeddings", "check", "--probe"]);
        assert!(!success, "{first}");
        server.join().unwrap();
        let run: RecordId =
            serde_json::from_value(first["error"]["details"]["run_id"].clone()).unwrap();
        let ledger = lwiki::jobs::JobLedger::new(
            f.fs.clone(),
            f.app.vault_id().clone(),
            run,
            embedding_fixture::options(),
        )
        .unwrap();
        let before = ledger.inspect().unwrap();
        assert_eq!(before.attempts.len(), 1);
        if unresolved {
            assert_eq!(
                before.attempts[0].phase,
                lwiki::jobs::AttemptPhase::DispatchIntent
            );
            assert_eq!(
                before.attempts[0].remote_exposure,
                lwiki::jobs::RemoteExposure::PossiblyInFlight
            );
        } else {
            assert_eq!(before.attempts[0].phase, lwiki::jobs::AttemptPhase::Settled);
            let path =
                f.fs.root()
                    .resolve(&before.attempts[0].receipt.as_ref().unwrap().path)
                    .unwrap();
            std::fs::write(path, b"substituted receipt without its authenticated hash").unwrap();
        }
        let marker_path = f.fs.root().path().join(".wiki/state/embedding-jobs");
        let markers = files(&marker_path);
        let (refused, success) = embedding_cli(&f, &["embeddings", "sync"]);
        assert!(!success, "{refused}");
        assert_eq!(refused["meta"]["network_used"], false);
        if unresolved {
            assert_eq!(refused["error"]["code"], "RECOVERY_REQUIRED");
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            files(&marker_path),
            markers,
            "refusal must not create fresh funded collection scope"
        );
        let after = ledger.inspect().unwrap();
        assert_eq!(after.attempts, before.attempts);
        assert_eq!(after.budget.outstanding, before.budget.outstanding);
        assert_eq!(
            after.budget.unknown_attempts,
            before.budget.unknown_attempts
        );
        assert_eq!(after.budget.dispatched_requests, 1);
    }
}

#[test]
fn native_source_relations_offline_quote_seals_actual_input_without_writes_or_credentials() {
    use lwiki::{domain::Blake3Hash, jobs::*};
    let f = Fixture::new();
    let config_dir = tempfile::tempdir().unwrap();
    let config = config_dir.path().join("providers.toml");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut card = RateCard {
        id: "native-mini-quote-fixture".into(),
        version: 1,
        fingerprint: provider::hash([]),
        currency: Currency::new("USD").unwrap(),
        validity: PriceValidity::DispatchLocked {
            valid_from_utc_ms: now - 1000,
            valid_until_utc_ms: now + 86_400_000,
        },
        request_fee_nanounits: 0,
        rates: BTreeMap::from(
            [
                (BillableClass::Input, 400_000_000),
                (BillableClass::CachedInput, 100_000_000),
                (BillableClass::Output, 1_600_000_000),
                (BillableClass::Reasoning, 0),
            ]
            .map(|(class, price_nanounits)| {
                (
                    class,
                    Rate {
                        price_nanounits,
                        per_units: std::num::NonZeroU64::new(1_000_000).unwrap(),
                    },
                )
            }),
        ),
    };
    card.fingerprint = budgets::rate_card_fingerprint(&card).unwrap();
    let mut prices = format!(
        "[services.mini.rate_card]\nid={}\nversion=1\nfingerprint={}\ncurrency=\"USD\"\nrequest_fee_nanounits=0\n[services.mini.rate_card.validity]\npricing=\"dispatch_locked\"\nvalid_from_utc_ms={}\nvalid_until_utc_ms={}\n",
        provider::quote(&card.id),
        provider::quote(card.fingerprint.as_str()),
        now - 1000,
        now + 86_400_000
    );
    for (class, rate) in &card.rates {
        let name = match class {
            BillableClass::Input => "input",
            BillableClass::CachedInput => "cached_input",
            BillableClass::Output => "output",
            BillableClass::Reasoning => "reasoning",
        };
        prices.push_str(&format!(
            "[services.mini.rate_card.rates.{name}]\nprice_nanounits={}\nper_units={}\n",
            rate.price_nanounits, rate.per_units
        ));
    }
    let write_config = |model: &str, priced: bool| {
        provider::private(
            &config,
            format!(
                "version=1\n[profiles.primary]\ngeneration=\"mini\"\n[services.mini]\nadapter=\"responses-v1\"\nurl=\"https://api.openai.com/v1/responses\"\nmodel={}\nrevision=\"native-mini-quote-v1\"\nresponse_mode=\"json-schema\"\nmax_output_tokens=4096\ntimeout_seconds=10\n[services.mini.auth]\nkind=\"static\"\nkey_env=\"EXPLICIT_FIXTURE_TOKEN\"\n{}\n[vault_bindings.test]\nroot={}\nwiki_id=\"vault_test\"\nallowed_profiles=[\"primary\"]\n",
                provider::quote(model),
                if priced { prices.as_str() } else { "" },
                provider::quote(f.temp.path().to_str().unwrap())
            ),
        );
    };
    write_config("gpt-4.1-mini-2025-04-14", true);
    let invoke = |args: &[&str], remote: bool, expect_success: bool| -> Value {
        let mut command =
            std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
        command.args([
            "--wiki",
            f.temp.path().to_str().unwrap(),
            "--json",
            "--offline",
        ]);
        command.env_remove("EXPLICIT_FIXTURE_TOKEN");
        if remote {
            command.args(["--profile", "primary"]);
        }
        command.args(args);
        if remote {
            command.args([
                "--providers-config",
                config.to_str().unwrap(),
                "--max-requests",
                "1",
                "--attempts-per-task",
                "1",
                "--concurrency",
                "1",
                "--deadline-ms",
                "20000",
            ]);
        }
        let output = command.output().unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            output.status.success(),
            expect_success,
            "{args:?}: {value}; {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(value["meta"]["network_used"], false);
        if !expect_success {
            assert!(!value["error"].is_null(), "{value}");
            assert!(
                value["data"]["quote"].is_null(),
                "partial quote leaked: {value}"
            );
        }
        value
    };
    invoke(&["index", "rebuild", "--normalized"], false, true);
    let before = files(f.temp.path());
    let source = f.request.export.source_id.as_str();
    let base = [
        "relations",
        "prepare",
        "--source-id",
        source,
        "--max-output-tokens",
        "4096",
        "--max-cost",
        "10",
    ];
    let ordinary = invoke(&base, true, true);
    assert_eq!(files(f.temp.path()), before);
    let mut quoted_args = base.to_vec();
    quoted_args.push("--quote");
    let quoted = invoke(&quoted_args, true, true);
    assert_eq!(files(f.temp.path()), before);
    assert_eq!(
        quoted["data"]["manifest_json"],
        ordinary["data"]["manifest_json"]
    );
    assert_eq!(
        quoted["data"]["manifest_hash"],
        ordinary["data"]["manifest_hash"]
    );
    assert_eq!(quoted["data"]["sources"], 1);
    assert_eq!(quoted["data"]["provider_calls"], 0);
    assert_eq!(quoted["data"]["persisted"], false);
    let manifest: Value =
        serde_json::from_str(quoted["data"]["manifest_json"].as_str().unwrap()).unwrap();
    let quote = &quoted["data"]["quote"];
    assert_eq!(
        quote["manifest_hash"],
        json!(Blake3Hash::digest(
            quoted["data"]["manifest_json"].as_str().unwrap().as_bytes()
        ))
    );
    assert_eq!(quote["entries"].as_array().unwrap().len(), 1);
    let entry = &quote["entries"][0];
    let task = &manifest["entries"][0]["task"];
    assert_eq!(entry["source_id"], json!(source));
    assert_eq!(entry["task_key"], task["key"]);
    assert_eq!(entry["input_descriptor"], task["input"]);
    assert_eq!(entry["bound"]["input_hash"], task["input_hash"]);
    assert_eq!(
        entry["bound"]["config_fingerprint"],
        manifest["config_fingerprint"]
    );
    let bound: AttemptBound = serde_json::from_value(entry["bound"].clone()).unwrap();
    assert_eq!(
        bound.bounds_fingerprint,
        budgets::bound_fingerprint(&bound).unwrap()
    );
    assert_ne!(bound.wire_hash, provider::hash([]));
    assert_eq!(
        bound.requested_model.as_deref(),
        Some("gpt-4.1-mini-2025-04-14")
    );
    assert!(bound.request_bytes > 0 && bound.request_bytes <= 256 * 1024);
    assert_eq!(bound.response_bytes, GENERATION_SPOOL_MAX_BYTES);
    assert_eq!(bound.timeout_ms, 10_000);
    assert_eq!(
        bound.quoted_allowance.as_ref().unwrap().nanounits(),
        530_341_600
    );
    assert_eq!(entry["allowance"]["cost"]["nanounits"], 530_341_600);
    assert_eq!(quote["aggregate_worst_case"]["requests"], 1);
    assert_eq!(
        quote["aggregate_worst_case"]["request_bytes"],
        bound.request_bytes
    );
    assert_eq!(
        quote["aggregate_worst_case"]["response_bytes"],
        GENERATION_SPOOL_MAX_BYTES
    );
    assert_eq!(
        quote["aggregate_worst_case"]["cost"]["nanounits"],
        530_341_600
    );
    assert_eq!(quote["complete_acquisition_guaranteed"], false);
    assert_eq!(quote["bound_identity_stage"], "prepared_wire");
    assert_eq!(quote["aggregate_fits_lifetime_limits"], true);
    let mut low_cap = quoted_args.clone();
    let cap = low_cap.iter().position(|v| *v == "--max-cost").unwrap() + 1;
    low_cap[cap] = "0.5";
    invoke(&low_cap, true, false);
    assert_eq!(files(f.temp.path()), before);
    write_config("gpt-4.1-mini", true);
    invoke(&quoted_args, true, false);
    assert_eq!(files(f.temp.path()), before);
    write_config("gpt-4.1-mini-2025-04-14", false);
    invoke(&quoted_args, true, false);
    assert_eq!(files(f.temp.path()), before);
    write_config("gpt-4.1-mini-2025-04-14", true);
    let mut missing = quoted_args.clone();
    missing.extend(["--source-id", "source_missing"]);
    invoke(&missing, true, false);
    assert_eq!(files(f.temp.path()), before);
}
