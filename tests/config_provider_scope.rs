//! Public-command regression for lasting wiki-scoped provider repair.
#![cfg(unix)]
#[path = "../test_support/paths.rs"]
mod test_paths;
use lwiki::config;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

fn private_write(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn command(base: &Path, argv: &[String]) -> Value {
    let output = Command::new(test_paths::binary(env!("CARGO_BIN_EXE_lwiki")))
        .args(argv)
        .current_dir(base)
        .env("HOME", base)
        .env("XDG_CONFIG_HOME", base)
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid CLI JSON: {error}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(
        output.status.success(),
        value["ok"].as_bool().unwrap(),
        "{value}"
    );
    assert!(
        output.stderr.is_empty(),
        "JSON command wrote prose: {:?}",
        output.stderr
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-mock-token"));
    value
}
fn argv(preferences: &Path, wiki: &Path, tail: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "--preferences".into(),
        preferences.to_str().unwrap().into(),
        "--no-user-config".into(),
        "--wiki".into(),
        wiki.to_str().unwrap().into(),
        "--json".into(),
    ];
    argv.extend(tail.iter().map(|word| (*word).into()));
    argv
}
fn snapshot(path: &Path) -> (Vec<u8>, std::time::SystemTime) {
    (
        fs::read(path).unwrap(),
        fs::metadata(path).unwrap().modified().unwrap(),
    )
}
fn provider_document(
    wiki: &config::DefaultWiki,
    model: &str,
    cap: u64,
    endpoint: &str,
    auth: &str,
) -> String {
    format!(
        r#"# Retained owner definitions.
version = 1
[profiles.primary]
embedding = "embed"
generation = "generate"
[services.embed]
adapter = "embeddings-v1"
url = "https://embedding.example/v1/embeddings"
model = "retained-embedding"
dimensions = 8
[services.embed.auth]
kind = "static"
key_env = "UNUSED_SCOPE_EMBEDDING_KEY"
[services.generate]
adapter = "responses-v1"
url = {endpoint}
model = {model}
max_output_tokens = {cap}
timeout_seconds = 1
allow_loopback_http = true
[services.generate.auth]
{auth}
[vault_bindings.selected]
root = {root}
wiki_id = {id}
allowed_profiles = ["primary"]
"#,
        endpoint = json!(endpoint),
        model = json!(model),
        root = json!(wiki.path),
        id = json!(wiki.wiki_id)
    )
}
fn serve(listener: std::net::TcpListener) -> Vec<Value> {
    listener.set_nonblocking(true).unwrap();
    let mut requests = Vec::new();
    for incomplete in [true, false] {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "bounded B probe never reached mock"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("mock accept: {error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let (start, length) = loop {
            let mut buffer = [0; 4096];
            let count = socket.read(&mut buffer).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            assert!(bytes.len() < 65536);
            if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
                assert!(headers.starts_with("POST /b-generation HTTP/1.1"));
                let length = headers
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
        while bytes.len() < start + length {
            let mut buffer = [0; 4096];
            let count = socket.read(&mut buffer).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
        }
        requests.push(serde_json::from_slice(&bytes[start..start + length]).unwrap());
        let usage = json!({"input_tokens":24,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},"output_tokens":16,"output_tokens_details":{"reasoning_tokens":if incomplete {16} else {0}},"total_tokens":40,"cost":null});
        let response = if incomplete {
            json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"error":null,"usage":usage})
        } else {
            json!({"object":"response","status":"completed","model":"wiki-b-model","incomplete_details":null,"error":null,"output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"{\"ok\":true}"}]}],"usage":usage})
        };
        let response = serde_json::to_vec(&response).unwrap();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
        socket.write_all(&response).unwrap();
        socket.flush().unwrap();
    }
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    requests
}

#[test]
fn suggested_probe_repair_preserves_default_a_and_persists_b_file_profile_for_readback_retry_and_edits()
 {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let a_path = base.join("Default A notes");
    let b_path = base.join("B's notes");
    for path in [&a_path, &b_path] {
        lwiki::app::offline::init(path, "My notes", lwiki::app::OperationOptions::default())
            .unwrap();
    }
    let a = lwiki::vault::discovery::default_from_path(&a_path).unwrap();
    let b = lwiki::vault::discovery::default_from_path(&b_path).unwrap();
    let preferences = base.join("Owner's preferences.toml");
    let a_providers = base.join("A providers.toml");
    let b_providers = base.join("B's providers.toml");
    let helper = base.join("credential helper");
    let sentinel = base.join("helper-calls");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nprintf 'called\\n' >> '{}'\nprintf 'private-mock-token\\n'\n",
            sentinel.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let a_bytes = provider_document(
        &a,
        "wiki-a-model",
        32,
        "http://127.0.0.1:9/a-generation",
        "kind = \"static\"\nkey_env = \"UNUSED_SCOPE_A_KEY\"",
    );
    let b_bytes = provider_document(
        &b,
        "wiki-b-model",
        64,
        &format!("http://{address}/b-generation"),
        &format!(
            "kind = \"command\"\ncommand = [{}]\noutput = \"text\"\ntimeout_seconds = 1\nttl_seconds = 300\nrefresh_skew_seconds = 60",
            json!(helper)
        ),
    );
    private_write(&a_providers, &a_bytes);
    private_write(&b_providers, &b_bytes);
    let mut document = config::LocalConfigDocument {
        default_wiki: Some(a.clone()),
        providers_config: Some(a_providers.clone()),
        ..Default::default()
    };
    document.defaults.profile = Some("primary".into());
    document.vaults.insert(
        a.wiki_id.clone(),
        config::LocalPreferences {
            profile: Some("primary".into()),
            read_max_bytes: Some(1234),
            ..Default::default()
        },
    );
    document.vaults.insert(
        b.wiki_id.clone(),
        config::LocalPreferences {
            profile: Some("primary".into()),
            read_max_bytes: Some(4321),
            ..Default::default()
        },
    );
    document.remote.max_requests = Some(1);
    document.remote.attempts_per_task = Some(1);
    document.remote.concurrency = Some(1);
    document.remote.deadline_ms = Some(5000);
    document.remote.max_request_bytes = Some(65536);
    document.remote.max_response_bytes = Some(lwiki::jobs::GENERATION_SPOOL_MAX_BYTES);
    private_write(&preferences, toml::to_string(&document).unwrap());
    let a_preferences = serde_json::to_value(&document.vaults[&a.wiki_id]).unwrap();
    let before_a = snapshot(&a_providers);
    let server = std::thread::spawn(move || serve(listener));
    let failed = command(
        &base,
        &argv(
            &preferences,
            &b_path,
            &[
                "--profile",
                "primary",
                "doctor",
                "--probe",
                "--role",
                "generate",
                "--providers-config",
                b_providers.to_str().unwrap(),
            ],
        ),
    );
    assert_eq!(failed["ok"], false, "{failed}");
    assert_eq!(
        failed["error"]["details"]["cause"]["reason"],
        "incomplete_max_output_tokens"
    );
    assert_eq!(fs::read_to_string(&sentinel).unwrap().lines().count(), 1);
    let repair = failed["error"]["details"]["repair_command"]
        .as_str()
        .unwrap();
    let mut repair: Vec<String> = shlex::split(repair)
        .unwrap()
        .into_iter()
        .skip(1)
        .map(|word| if word == "TOKENS" { "128".into() } else { word })
        .collect();
    repair.push("--json".into());
    let repaired = command(&base, &repair);
    assert_eq!(repaired["ok"], true, "{repaired}");
    assert_eq!(repaired["meta"]["network_used"], false);
    assert_eq!(fs::read_to_string(&sentinel).unwrap().lines().count(), 1);
    assert_eq!(snapshot(&a_providers), before_a);
    assert!(
        fs::read(&b_providers)
            .unwrap()
            .starts_with(b_bytes.as_bytes())
    );
    let saved = config::local::load_user_config(Some(&preferences), true).unwrap();
    assert_eq!(
        saved.document().providers_config.as_ref(),
        Some(&a_providers)
    );
    assert_eq!(saved.default_wiki(), Some(&a));
    assert_eq!(
        serde_json::to_value(&saved.document().vaults[&a.wiki_id]).unwrap(),
        a_preferences
    );
    assert_eq!(
        saved.document().vaults[&b.wiki_id]
            .providers_config
            .as_ref(),
        Some(&b_providers)
    );
    assert_ne!(
        saved.document().vaults[&b.wiki_id].profile.as_deref(),
        Some("primary")
    );
    for (wiki, file, model, cap, origin) in [
        (&a_path, &a_providers, "wiki-a-model", 32, "user_config"),
        (&b_path, &b_providers, "wiki-b-model", 128, "user_vault"),
    ] {
        let shown = command(&base, &argv(&preferences, wiki, &["config", "show"]));
        assert_eq!(shown["data"]["provider_configuration"]["path"], json!(file));
        assert_eq!(shown["data"]["provider_configuration"]["origin"], origin);
        assert_eq!(shown["data"]["provider"]["generation"]["model"], model);
        assert_eq!(
            shown["data"]["provider"]["generation"]["effective_output_token_limit"],
            cap
        );
        assert_eq!(
            shown["data"]["provider"]["generation"]["permitted_for_selected_wiki"],
            true
        );
    }
    let next = repaired["data"]["next_step"].as_str().unwrap();
    let mut readback = shlex::split(
        next.strip_prefix("Run ")
            .unwrap()
            .strip_suffix(" to inspect both models.")
            .unwrap(),
    )
    .unwrap()
    .into_iter()
    .skip(1)
    .collect::<Vec<_>>();
    readback.push("--json".into());
    assert_eq!(
        command(&base, &readback)["data"]["provider"]["generation"]["effective_output_token_limit"],
        128
    );
    // Keep the finite saved limits and omit the stale explicit profile/file.
    let retried = command(
        &base,
        &argv(
            &preferences,
            &b_path,
            &["doctor", "--probe", "--role", "generate"],
        ),
    );
    assert_eq!(retried["ok"], true, "{retried}");
    assert_eq!(retried["data"]["probe"]["validated"], true);
    assert_eq!(
        retried["data"]["probe"]["inspection"]["budget"]["dispatched_requests"],
        1
    );
    let retry_limits = &retried["data"]["probe"]["inspection"]["effective_limits"];
    assert_eq!(retry_limits["requests"], 1);
    assert_eq!(retry_limits["attempts_per_task"], 1);
    assert_eq!(retry_limits["concurrency"], 1);
    assert_eq!(retry_limits["request_bytes"], 65536);
    assert_eq!(
        retry_limits["response_bytes"],
        lwiki::jobs::GENERATION_SPOOL_MAX_BYTES
    );
    assert_eq!(fs::read_to_string(&sentinel).unwrap().lines().count(), 2);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    for (request, cap) in requests.iter().zip([64, 128]) {
        assert_eq!(request["model"], "wiki-b-model");
        assert_eq!(request["max_output_tokens"], cap);
    }
    let accounting = failed["error"]["details"]["accounting_command"]
        .as_str()
        .unwrap();
    let mut accounting = shlex::split(accounting)
        .unwrap()
        .into_iter()
        .skip(1)
        .collect::<Vec<_>>();
    accounting.push("--json".into());
    let previous = command(&base, &accounting);
    assert_eq!(previous["data"]["state"], "paused");
    assert_eq!(previous["data"]["attempts"].as_array().unwrap().len(), 1);
    assert_eq!(
        previous["data"]["attempts"][0]["billing"],
        "unknown_reserved"
    );
    let edit = argv(
        &preferences,
        &b_path,
        &[
            "config",
            "set",
            "generation",
            "--max-output-tokens",
            "256",
            "--grant-provider",
            "--non-interactive",
        ],
    );
    assert_eq!(command(&base, &edit)["ok"], true);
    let pinned_preferences = snapshot(&preferences);
    let pinned_providers = snapshot(&b_providers);
    for explicit_file in [false, true] {
        let mut repeat = argv(
            &preferences,
            &b_path,
            &[
                "config",
                "set",
                "generation",
                "--max-output-tokens",
                "256",
                "--non-interactive",
            ],
        );
        if explicit_file {
            repeat.extend([
                "--providers-config".into(),
                b_providers.to_str().unwrap().into(),
            ]);
        }
        assert_eq!(command(&base, &repeat)["ok"], true);
        assert_eq!(snapshot(&preferences), pinned_preferences);
        assert_eq!(snapshot(&b_providers), pinned_providers);
    }
    assert_eq!(snapshot(&a_providers), before_a);
    assert_eq!(fs::read_to_string(&sentinel).unwrap().lines().count(), 2);
    let saved = config::local::load_user_config(Some(&preferences), true).unwrap();
    assert_eq!(
        saved.document().remote.max_requests,
        document.remote.max_requests
    );
    assert_eq!(
        serde_json::to_value(&saved.document().remote).unwrap(),
        serde_json::to_value(&document.remote).unwrap()
    );
    assert_eq!(
        saved.document().vaults[&b.wiki_id].read_max_bytes,
        Some(4321)
    );
    let final_show = command(&base, &argv(&preferences, &b_path, &["config", "show"]));
    assert_eq!(
        final_show["data"]["provider"]["generation"]["effective_output_token_limit"],
        256
    );
    assert_eq!(
        final_show["data"]["provider"]["embedding"]["model"],
        "retained-embedding"
    );
}

#[test]
fn setup_for_existing_b_preserves_default_a_and_subsequent_setup_reads_b_saved_file() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let a_path = base.join("Default A notes");
    let b_path = base.join("B's notes");
    for path in [&a_path, &b_path] {
        lwiki::app::offline::init(path, "My notes", lwiki::app::OperationOptions::default())
            .unwrap();
    }
    let a = lwiki::vault::discovery::default_from_path(&a_path).unwrap();
    let b = lwiki::vault::discovery::default_from_path(&b_path).unwrap();
    let preferences = base.join("Owner's preferences.toml");
    let a_providers = base.join("A providers.toml");
    let b_providers = base.join("B's providers.toml");
    let auth = "kind = \"static\"\nkey_env = \"UNUSED_SCOPE_KEY\"";
    private_write(
        &a_providers,
        provider_document(
            &a,
            "wiki-a-model",
            32,
            "http://127.0.0.1:9/a-generation",
            auth,
        ),
    );
    private_write(
        &b_providers,
        provider_document(
            &b,
            "wiki-b-model",
            64,
            "http://127.0.0.1:9/b-generation",
            auth,
        ),
    );
    let mut document = config::LocalConfigDocument {
        default_wiki: Some(a.clone()),
        providers_config: Some(a_providers.clone()),
        ..Default::default()
    };
    document.defaults.profile = Some("primary".into());
    document.vaults.insert(
        a.wiki_id.clone(),
        config::LocalPreferences {
            read_max_bytes: Some(1234),
            ..Default::default()
        },
    );
    private_write(&preferences, toml::to_string(&document).unwrap());
    let before_a = snapshot(&a_providers);
    let a_preferences = serde_json::to_value(&document.vaults[&a.wiki_id]).unwrap();
    let setup = argv(
        &preferences,
        &b_path,
        &[
            "setup",
            b_path.to_str().unwrap(),
            "--select",
            "--no-default",
            "--providers-file",
            b_providers.to_str().unwrap(),
            "--generation-max-output-tokens",
            "128",
            "--grant-provider",
            "--non-interactive",
        ],
    );
    let configured = command(&base, &setup);
    assert_eq!(configured["ok"], true, "{configured}");
    assert_eq!(configured["meta"]["network_used"], false);
    let saved = config::local::load_user_config(Some(&preferences), true).unwrap();
    assert_eq!(saved.default_wiki(), Some(&a));
    assert_eq!(
        saved.document().providers_config.as_ref(),
        Some(&a_providers)
    );
    assert_eq!(
        serde_json::to_value(&saved.document().vaults[&a.wiki_id]).unwrap(),
        a_preferences
    );
    assert_eq!(
        saved.document().vaults[&b.wiki_id]
            .providers_config
            .as_ref(),
        Some(&b_providers)
    );
    let preferences_before = snapshot(&preferences);
    let b_before = snapshot(&b_providers);
    let repeat = argv(
        &preferences,
        &b_path,
        &[
            "setup",
            b_path.to_str().unwrap(),
            "--select",
            "--no-default",
            "--generation-max-output-tokens",
            "128",
            "--non-interactive",
        ],
    );
    let repeated = command(&base, &repeat);
    assert_eq!(repeated["ok"], true, "{repeated}");
    assert_eq!(
        repeated["data"]["plan"]["provider_configuration"],
        json!(b_providers)
    );
    assert_eq!(
        repeated["data"]["plan"]["generation"]["model"],
        "wiki-b-model"
    );
    assert_eq!(snapshot(&preferences), preferences_before);
    assert_eq!(snapshot(&b_providers), b_before);
    let inspected = command(
        &base,
        &argv(
            &preferences,
            &b_path,
            &[
                "setup",
                b_path.to_str().unwrap(),
                "--select",
                "--no-default",
                "--non-interactive",
            ],
        ),
    );
    assert_eq!(inspected["ok"], true, "{inspected}");
    assert_eq!(
        inspected["data"]["plan"]["generation"]["model"],
        "wiki-b-model"
    );
    assert_eq!(snapshot(&a_providers), before_a);
    assert_eq!(snapshot(&preferences), preferences_before);
    assert_eq!(snapshot(&b_providers), b_before);
}
