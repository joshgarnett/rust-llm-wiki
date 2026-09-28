use clap::Parser;
use lwiki::{
    cli::{Arguments, OutputFormat, execute, present},
    output::Envelope,
};
use serde_json::json;
use std::io::{self, Write};

fn stream_event(
    output: &mut impl Write,
    invocation: &str,
    sequence: u64,
    event: &str,
    data: serde_json::Value,
) -> io::Result<()> {
    serde_json::to_writer(
        &mut *output,
        &json!({"schema_version":"1","invocation_id":invocation,"sequence":sequence,"event":event,"data":data}),
    )?;
    writeln!(output)?;
    output.flush()
}
fn argument_format() -> OutputFormat {
    let args: Vec<_> = std::env::args_os().collect();
    if args.iter().any(|a| a == "--jsonl" || a == "--format=jsonl")
        || args
            .windows(2)
            .any(|w| w[0] == "--format" && w[1] == "jsonl")
    {
        OutputFormat::Jsonl
    } else if args.iter().any(|a| a == "--json" || a == "--format=json")
        || args
            .windows(2)
            .any(|w| w[0] == "--format" && w[1] == "json")
    {
        OutputFormat::Json
    } else {
        OutputFormat::Human
    }
}
fn main() {
    let args = match Arguments::try_parse() {
        Ok(args) => args,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                error.print().ok();
                return;
            }
            let envelope = Envelope::failure("arguments", "USAGE", error.to_string());
            let format = argument_format();
            if format == OutputFormat::Jsonl {
                let invocation = uuid::Uuid::now_v7().to_string();
                let mut output = io::stdout().lock();
                stream_event(
                    &mut output,
                    &invocation,
                    0,
                    "started",
                    json!({"command":"arguments"}),
                )
                .ok();
                stream_event(
                    &mut output,
                    &invocation,
                    1,
                    "completed",
                    serde_json::to_value(&envelope).expect("serializable envelope"),
                )
                .ok();
            } else if format == OutputFormat::Json {
                present(&envelope, format, &mut io::stdout().lock()).ok();
            } else {
                eprint!("{error}");
            }
            std::process::exit(2);
        }
    };
    let format = args.output_format();
    let invocation = uuid::Uuid::now_v7().to_string();
    if format == OutputFormat::Jsonl
        && stream_event(
            &mut io::stdout().lock(),
            &invocation,
            0,
            "started",
            json!({"command":args.command.name()}),
        )
        .is_err()
    {
        std::process::exit(1);
    }
    let (envelope, exit) = execute(&args);
    if format == OutputFormat::Human && matches!(envelope.command.as_str(), "read" | "context") {
        if let Some(freshness) = &envelope.meta.freshness {
            eprintln!(
                "Freshness: {freshness}{}",
                envelope
                    .meta
                    .verified_at
                    .as_ref()
                    .map(|at| format!(" at {at}"))
                    .unwrap_or_else(|| " (unverified)".into())
            );
        }
        for warning in &envelope.warnings {
            eprintln!("Warning: {warning}");
        }
    }
    let output_result = if format == OutputFormat::Jsonl {
        stream_event(
            &mut io::stdout().lock(),
            &invocation,
            1,
            "completed",
            serde_json::to_value(&envelope).expect("serializable envelope"),
        )
    } else if format == OutputFormat::Human && !envelope.ok {
        present(&envelope, format, &mut io::stderr().lock())
    } else {
        present(&envelope, format, &mut io::stdout().lock())
    };
    if output_result.is_err() {
        std::process::exit(1);
    }
    std::process::exit(i32::from(exit));
}
