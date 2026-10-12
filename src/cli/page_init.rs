//! Compact completion for an ordinary new draft; JSON retains the full plan.
use super::dispatch::terminal_text;
use crate::output::Envelope;
use std::io::{self, Write};

pub(super) fn present(
    envelope: &Envelope,
    output: &mut impl Write,
    command_prefix: &str,
    verbose: bool,
) -> io::Result<()> {
    let data = &envelope.data;
    let status = data["status"].as_str().unwrap_or("preview");
    writeln!(
        output,
        "{}",
        match status {
            "committed" if envelope.ok => "Draft saved.",
            "prepared" => "Draft staged; no page saved yet.",
            "preview" => "Preview; no page saved.",
            _ => "Page change is not confirmed saved.",
        }
    )?;
    if let Some(ids) = data["allocated_ids"].as_object() {
        for (path, id) in ids {
            if let Some(id) = id.as_str() {
                writeln!(output, "Page path: {}", terminal_text(path, false))?;
                writeln!(output, "Page ID: {id}")?;
            }
        }
    }
    if let Some(count) = data["source_citations"]["reference_count"].as_u64() {
        writeln!(
            output,
            "Source references: {count}; verified for this proposal: {}. Prose support is not checked.",
            data["source_citations"]["verification_performed"] == true
        )?;
    }
    if let Some(change) = data["change"]["change_id"].as_str() {
        if status == "prepared" {
            writeln!(output, "Change: {change}")?;
            writeln!(
                output,
                "Inspect with: {command_prefix} changes show {change} --summary\nApply with: {command_prefix} changes apply {change}"
            )?;
        } else if verbose {
            writeln!(output, "Change: {change}")?;
        }
    }
    if verbose || !envelope.ok || !matches!(status, "committed" | "prepared" | "preview") {
        writeln!(output, "Change status: {status}")?;
        if let Some(operations) = data["plan"]["operations"].as_array() {
            for operation in operations {
                writeln!(
                    output,
                    "  {} ({} bytes)",
                    terminal_text(operation["path"].as_str().unwrap_or_default(), false),
                    operation["byte_len"]
                )?;
            }
        }
    }
    if let Some(error) = &envelope.error {
        writeln!(
            output,
            "{}: {}",
            error.code,
            terminal_text(&error.message, false)
        )?;
        if let Some(hint) = &error.hint {
            writeln!(output, "{}", terminal_text(hint, false))?;
        }
    }
    if data["reused"] == true {
        writeln!(output, "Reused the existing change.")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn completion_distinguishes_saved_staged_and_preview_and_preserves_actual_destination() {
        for (status, expected, verified) in [
            (json!("committed"), "Draft saved.", true),
            (json!("prepared"), "Draft staged; no page saved yet.", true),
            (serde_json::Value::Null, "Preview; no page saved.", false),
        ] {
            let mut envelope = Envelope::success("page init", json!({}));
            envelope.data = json!({
                "status":status,
                "allocated_ids":{"pages/actual.md":"custom_destination"},
                "source_citations":{"reference_count":1,"verification_performed":verified},
                "change":{"change_id":"Change.actual"},
                "plan":{"operations":[{"path":"sources/support.md","byte_len":900}]}
            });
            let mut output = Vec::new();
            present(&envelope, &mut output, "lwiki --wiki '/chosen wiki'", false).unwrap();
            let text = String::from_utf8(output).unwrap();
            assert!(text.starts_with(expected));
            assert!(text.contains("Page path: pages/actual.md\nPage ID: custom_destination"));
            assert!(text.contains(&format!("verified for this proposal: {verified}")));
            assert!(!text.contains("sources/support.md"));
            if envelope.data["status"] == "prepared" {
                assert!(text.contains("lwiki --wiki '/chosen wiki' changes apply Change.actual"));
            } else {
                assert!(!text.contains("Apply with:"));
            }
        }
    }
}
