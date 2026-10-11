use super::*;
use clap::Parser;
use std::{io::Cursor, time::Duration};
#[derive(Debug, Parser)]
struct Harness {
    #[command(flatten)]
    args: SourceRelationsArguments,
}
fn until() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
#[test]
fn prepare_collects_repeated_sources_and_trusted_remote_options() {
    let parsed = Harness::try_parse_from([
        "relations",
        "prepare",
        "--source-id",
        "source_one",
        "--source-id",
        "source_two",
        "--max-output-tokens",
        "1024",
        "--max-requests",
        "4",
        "--quote",
    ])
    .unwrap();
    let SourceRelationsCommand::Prepare(args) = parsed.args.command else {
        panic!("wrong command")
    };
    assert_eq!(args.source_ids.len(), 2);
    assert_eq!(args.max_output_tokens, 1024);
    assert_eq!(args.remote.max_requests, Some(4));
    assert!(args.quote);
    assert!(Harness::try_parse_from(["relations", "prepare"]).is_err());
}
#[test]
fn run_defaults_to_sixteen_tasks_and_preserves_manifest_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("manifest.json");
    let exact = b"{\n  \"version\": 1\n}\n";
    std::fs::write(&file, exact).unwrap();
    let parsed = Harness::try_parse_from([
        "relations",
        "run",
        "--file",
        file.to_str().unwrap(),
        "--run",
        "run_fictional",
    ])
    .unwrap();
    let SourceRelationsCommand::Run(args) = parsed.args.command else {
        panic!("wrong command")
    };
    assert_eq!(args.max_tasks, 16);
    assert_eq!(args.manifest_bytes(until()).unwrap(), exact);
    assert!(args.manifest_bytes(Instant::now()).is_err());
}
#[test]
fn reader_refuses_empty_oversized_nonregular_and_invalid_task_allowance() {
    assert!(bounded_input(Cursor::new(Vec::<u8>::new()), until()).is_err());
    assert!(bounded_input(Cursor::new(vec![b'x'; MANIFEST_MAX_BYTES + 1]), until()).is_err());
    let temp = tempfile::tempdir().unwrap();
    let parsed = Harness::try_parse_from([
        "relations",
        "run",
        "--file",
        temp.path().to_str().unwrap(),
        "--run",
        "run_fictional",
    ])
    .unwrap();
    let SourceRelationsCommand::Run(mut args) = parsed.args.command else {
        panic!("wrong command")
    };
    assert!(args.manifest_bytes(until()).is_err());
    let file = temp.path().join("oversize.json");
    std::fs::File::create(&file)
        .unwrap()
        .set_len(MANIFEST_MAX_BYTES as u64 + 1)
        .unwrap();
    args.file = file;
    assert!(args.manifest_bytes(until()).is_err());
    args.max_tasks = 0;
    assert_eq!(
        args.manifest_bytes(until()).err().unwrap().code,
        ErrorCode::Usage
    );
}

#[test]
fn full_arguments_use_one_global_profile_for_both_relations_subcommands() {
    use crate::cli::Arguments;
    for tokens in [
        vec![
            "lwiki",
            "--profile",
            "fictional",
            "relations",
            "prepare",
            "--source-id",
            "source_one",
        ],
        vec![
            "lwiki",
            "relations",
            "prepare",
            "--source-id",
            "source_one",
            "--profile",
            "fictional",
        ],
        vec![
            "lwiki",
            "--profile",
            "fictional",
            "relations",
            "run",
            "--file",
            "manifest.json",
            "--run",
            "run_one",
        ],
        vec![
            "lwiki",
            "relations",
            "run",
            "--file",
            "manifest.json",
            "--run",
            "run_one",
            "--profile",
            "fictional",
        ],
    ] {
        let parsed = Arguments::try_parse_from(tokens).unwrap();
        assert_eq!(parsed.profile.as_deref(), Some("fictional"));
    }
    for tokens in [
        vec!["lwiki", "relations", "prepare", "--source-id", "source_one"],
        vec![
            "lwiki",
            "relations",
            "run",
            "--file",
            "manifest.json",
            "--run",
            "run_one",
        ],
    ] {
        assert_eq!(Arguments::try_parse_from(tokens).unwrap().profile, None);
    }
    // A standalone leaf has no competing flag or implicit profile selection.
    assert!(
        Harness::try_parse_from([
            "relations",
            "prepare",
            "--source-id",
            "source_one",
            "--profile",
            "fictional"
        ])
        .is_err()
    );
    assert!(
        Harness::try_parse_from([
            "relations",
            "run",
            "--file",
            "manifest.json",
            "--run",
            "run_one",
            "--profile",
            "fictional"
        ])
        .is_err()
    );
}
