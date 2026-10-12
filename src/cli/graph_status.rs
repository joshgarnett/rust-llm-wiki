//! Short summaries for proposed graph changes. Agent packets keep their JSON output.
use crate::output::Envelope;
use serde_json::Value;
use std::io::{self, Write};

pub(super) fn present(
    envelope: &Envelope,
    output: &mut impl Write,
    command_prefix: &str,
) -> io::Result<()> {
    let data = if envelope.command == "graph extract" && envelope.data["import"].is_object() {
        &envelope.data["import"]
    } else {
        &envelope.data
    };
    if data["dry_run"] == true || envelope.data["dry_run"] == true {
        writeln!(output, "Preview only. No graph changes were saved.")?;
    } else {
        let status = match data["status"].as_str() {
            Some("prepared") => "Graph changes are ready for review and have not been applied.",
            Some("committed") => "These graph changes have already been applied.",
            Some("conflict") => "These graph changes conflict with newer edits.",
            Some("aborted") => "These graph changes were cancelled.",
            Some("applying" | "files_applied" | "indexed") => {
                "These graph changes still need to finish applying."
            }
            _ if data["disposition"] == "canonical_restored" => {
                "These graph records are already saved."
            }
            _ => "Graph preparation finished. No new change is ready to apply.",
        };
        writeln!(output, "{status}")?;
    }
    match envelope.command.as_str() {
        "graph import" | "graph extract" => {
            if let Some(assertions) = data["allocations"]["assertions"].as_object() {
                writeln!(output, "Proposed facts: {}", assertions.len())?;
            } else if let Some(count) = data["assertions"].as_u64() {
                writeln!(output, "Proposed facts: {count}")?;
            }
        }
        "graph resolve" => {
            counts(
                output,
                &data["summary"],
                &[
                    ("resolved_mentions", "Names matched"),
                    ("create_entities", "New people or things"),
                    ("pending_mentions", "Names still to review"),
                ],
            )?;
        }
        "graph review" => {
            counts(
                output,
                &data["summary"],
                &[
                    ("accept_assertions", "Facts accepted"),
                    ("reject_assertions", "Facts rejected"),
                    ("retract_evidence", "Citations removed"),
                ],
            )?;
        }
        _ => {}
    }
    if let Some(change) = data["prepared"]["change_id"].as_str() {
        let change = super::dispatch::terminal_text(change, false);
        writeln!(output, "Inspect: {command_prefix} changes show {change}")?;
        if matches!(
            data["status"].as_str(),
            Some("prepared" | "applying" | "files_applied" | "indexed")
        ) {
            let expected = envelope
                .meta
                .selection
                .as_ref()
                .and_then(|selection| selection["wiki_id"].as_str())
                .map(|id| format!(" --expect-wiki-id '{}'", id.replace('\'', "'\\''")))
                .unwrap_or_default();
            writeln!(
                output,
                "Apply: {command_prefix}{expected} changes apply {change}"
            )?;
        }
    }
    Ok(())
}

fn counts(output: &mut impl Write, summary: &Value, fields: &[(&str, &str)]) -> io::Result<()> {
    for (field, label) in fields {
        if let Some(count) = summary[*field].as_u64() {
            writeln!(output, "{label}: {count}")?;
        }
    }
    Ok(())
}
