//! Explicit diagnostic invocation on a frozen, owned public fixture.
use crate::catalog::query_diagnostics;
use clap::Parser;
use serde_json::json;
use std::{fs::OpenOptions, io::Write, path::PathBuf, time::Instant};

#[test]
#[ignore = "requires a frozen owned fixture and explicit diagnostic output path"]
fn frozen_public_access_observations() {
    let wiki = PathBuf::from(std::env::var("LWIKI_NAMED_DIAGNOSTIC_WIKI").unwrap());
    let root = std::env::var("LWIKI_NAMED_DIAGNOSTIC_ENTITY").unwrap();
    let output = PathBuf::from(std::env::var("LWIKI_NAMED_DIAGNOSTIC_OUTPUT").unwrap());
    assert!(
        wiki.is_absolute()
            && wiki
                .to_str()
                .unwrap()
                .contains("/.artifacts/normalized-named-neighbors-001/")
    );
    assert!(
        output.is_absolute()
            && output
                .to_str()
                .unwrap()
                .contains("/.artifacts/normalized-named-neighbors-001/")
    );
    let mut observations = Vec::new();
    for (label, id, expected_status) in [
        ("named", root.as_str(), 0),
        ("absent", "entity_named_neighbors_absent_20261005", 1),
    ] {
        let argv = vec![
            "lwiki",
            "--wiki",
            wiki.to_str().unwrap(),
            "--offline",
            "--json",
            "graph",
            "neighbors",
            id,
        ];
        let request = crate::cli::Arguments::try_parse_from(&argv).unwrap();
        let started_utc = crate::sources::revision::timestamp().unwrap();
        let start = Instant::now();
        query_diagnostics::begin();
        let (envelope, status) = crate::cli::execute(&request);
        let trace = query_diagnostics::end();
        let elapsed = start.elapsed().as_secs_f64();
        observations.push(json!({"label":label,"argv":argv,"started_utc":started_utc,
            "elapsed_monotonic_seconds":elapsed,"exit":status,"expected_status":expected_status,
            "envelope":envelope,"trace":trace,
            "scope":"Actual public parsed CLI coordinator in optimized test executable; observation overhead and EXPLAIN steps are disclosed separately from release CLI latency."}));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .unwrap();
    serde_json::to_writer_pretty(&mut file, &observations).unwrap();
    file.write_all(b"\n").unwrap();
    for observation in observations {
        if observation["expected_status"] == 0 {
            assert_eq!(observation["exit"], 0, "{observation}");
        } else {
            assert_ne!(observation["exit"], 0, "{observation}");
            assert_eq!(observation["envelope"]["error"]["code"], "RECORD_NOT_FOUND");
        }
        assert_eq!(observation["envelope"]["meta"]["network_used"], false);
    }
}
