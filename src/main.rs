use clap::Parser;
use lwiki::{
    cli::{Arguments, OutputFormat, execute, present, present_with_wiki},
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
    let (envelope, exit) = match lwiki::cli::interrupt::install() {
        Ok(()) => execute(&args),
        Err(error) => (
            Envelope::failure(args.command.name(), &error.code.to_string(), error.message),
            1,
        ),
    };
    if format == OutputFormat::Human {
        if envelope.ok
            && envelope.command == "read"
            && let Some(hash) = envelope.data["hash"].as_str()
        {
            eprintln!(
                "Record: {}\nHash (for --if-match): {hash}",
                envelope.data["path"].as_str().unwrap_or_default()
            );
            if let Some(citation) = envelope.data["source_citation"].as_object() {
                eprintln!(
                    "Source citation ({}): {}",
                    citation
                        .get("eligibility")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown"),
                    citation.get("citation").unwrap_or(&serde_json::Value::Null)
                );
            }
        }
        if envelope.ok && envelope.command == "context" && envelope.data["text"].is_string() {
            if let Some(packet) = envelope.data["selection_packet"].as_object() {
                eprintln!(
                    "Selection task: {} candidate cards, not final context. Save the host agent's ID-only reply, then repeat this request with --selection FILE.",
                    packet
                        .get("candidate_count")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0)
                );
            } else if envelope.data["text"] == "" {
                eprintln!(
                    "{}",
                    if envelope.meta.partial {
                        "No context fits the requested bounds. Increase --max-bytes / --max-tokens or narrow the query."
                    } else {
                        "No matching context. Try fewer query terms, literal search for exact text, or semantic search after embeddings sync."
                    }
                );
            }
            if let Some(omissions) = envelope.data["omissions"].as_array() {
                let mut reasons = std::collections::BTreeMap::<&str, u64>::new();
                for omission in omissions {
                    *reasons
                        .entry(omission["reason"].as_str().unwrap_or("unspecified"))
                        .or_default() += omission["count"].as_u64().unwrap_or(0);
                }
                for (reason, count) in reasons {
                    eprintln!("Omitted {count}: {reason}");
                }
            }
        }
        if matches!(envelope.command.as_str(), "read" | "context")
            && let Some(freshness) = &envelope.meta.freshness
        {
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
        if envelope.meta.partial && !matches!(envelope.command.as_str(), "search" | "read") {
            eprintln!(
                "Partial output: inspect the reported omissions and retained work before continuing."
            );
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
        present_with_wiki(
            &envelope,
            format,
            &mut io::stderr().lock(),
            args.wiki.as_deref(),
        )
    } else {
        present_with_wiki(
            &envelope,
            format,
            &mut io::stdout().lock(),
            args.wiki.as_deref(),
        )
    };
    if output_result.is_err() {
        std::process::exit(1);
    }
    std::process::exit(i32::from(exit));
}
