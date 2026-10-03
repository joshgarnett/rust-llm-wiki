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
    native_api_cli_case(false, false);
}
#[test]
fn native_api_cli_failed_paid_response_reports_actual_network_activity() {
    native_api_cli_case(true, false);
}
#[test]
fn native_responses_api_cli_stages_and_reuses_projected_schema_output() {
    native_api_cli_case(false, true);
}
fn native_api_cli_case(invalid_response: bool, responses: bool) {
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
    std::fs::write(&config, content).unwrap();
    let invoke = |offline: bool| {
        let mut command =
            std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
        command.args([
            "--wiki",
            f.temp.path().to_str().unwrap(),
            "--json",
            "--profile",
            "primary",
        ]);
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
            "--providers-config",
            config.to_str().unwrap(),
        ]);
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
    let second = invoke(true);
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

#[test]
fn native_responses_doctor_probe_uses_default_and_service_capped_output() {
    native_generation_probe_case(true, None, false);
    native_generation_probe_case(true, Some(64), false);
}

#[test]
fn native_chat_doctor_probe_keeps_legacy_output_limit() {
    native_generation_probe_case(false, None, false);
}

#[test]
fn native_incomplete_responses_probe_retains_one_paid_attempt_and_fixed_reason() {
    native_generation_probe_case(true, None, true);
}

fn native_generation_probe_case(responses: bool, service_cap: Option<u64>, incomplete: bool) {
    native_generation_probe_auth_case(responses, service_cap, incomplete, false);
}

#[test]
fn native_generation_probe_accepts_private_credential_file_with_terminal_newline() {
    native_generation_probe_auth_case(false, None, false, true);
}

fn native_generation_probe_auth_case(
    responses: bool,
    service_cap: Option<u64>,
    incomplete: bool,
    file_auth: bool,
) {
    let f = Fixture::new();
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
        ])
        .env("EXPLICIT_FIXTURE_TOKEN", "synthetic-cli-secret")
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    let request = server.join().unwrap();
    let expected = service_cap.unwrap_or(256);
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
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
}

#[path = "fixtures/p17/common.rs"]
mod embedding_fixture;
fn accept_embedding_request(listener: &std::net::TcpListener) -> std::net::TcpStream {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut socket = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "embedding mock request timeout");
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
    let (start, length) = loop {
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
            break (end + 4, length);
        }
    };
    while request.len() < start + length {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).unwrap();
        assert!(count > 0);
        request.extend_from_slice(&buffer[..count]);
    }
    let body: Value = serde_json::from_slice(&request[start..start + length]).unwrap();
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
    let server = std::thread::spawn(move || {
        for invalid in [true, false, false] {
            let mut socket = accept_embedding_request(&listener);
            send_embedding_response(&mut socket, invalid);
        }
    });
    let (first, success) = embedding_cli(&f, &["embeddings", "check", "--probe"]);
    assert!(!success, "{first}");
    assert_eq!(first["error"]["code"], "PROVIDER_RESPONSE");
    assert_eq!(first["error"]["details"]["reason"], "usage_invalid");
    let old_run: RecordId =
        serde_json::from_value(first["error"]["details"]["run_id"].clone()).unwrap();
    let (second, success) = embedding_cli(&f, &["embeddings", "check", "--probe"]);
    assert!(success, "{second}");
    let (third, success) = embedding_cli(&f, &["embeddings", "sync"]);
    assert!(success, "{third}");
    assert_eq!(third["data"]["published"], true);
    server.join().unwrap();
    let ledger = lwiki::jobs::JobLedger::new(
        f.fs.clone(),
        f.app.vault_id().clone(),
        old_run,
        embedding_fixture::options(),
    )
    .unwrap();
    let inspection = ledger.inspect().unwrap();
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
        for _ in 0..2 {
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
    let invoke = |command: &[&str], offline: bool| {
        let mut process =
            std::process::Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")));
        process.args([
            "--wiki",
            f.fs.root().path().to_str().unwrap(),
            "--json",
            "--profile",
            "primary",
        ]);
        if offline {
            process.arg("--offline");
        }
        process
            .args(command)
            .args(["--providers-config", f.config.to_str().unwrap()]);
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
