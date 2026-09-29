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
use lwiki::cli::{Arguments, execute};
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
    native_api_cli_case(false);
}
#[test]
fn native_api_cli_failed_paid_response_reports_actual_network_activity() {
    native_api_cli_case(true);
}
fn native_api_cli_case(invalid_response: bool) {
    let f = Fixture::new();
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let mock = Mock::response(f.response());
    let response = if invalid_response {
        b"{}".to_vec()
    } else {
        serde_json::to_vec(mock.body.lock().unwrap().as_ref().unwrap()).unwrap()
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
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
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

#[path = "fixtures/p17/common.rs"]
mod embedding_fixture;
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
                "usage":{"prompt_tokens":2,"total_tokens":2}}))
            .unwrap();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",response.len()).unwrap();
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
