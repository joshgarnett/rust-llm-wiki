use super::dispatch::terminal_text;
use crate::output::Envelope;
use std::io::{self, Write};

pub(super) fn present_capture(envelope: &Envelope, output: &mut impl Write) -> io::Result<()> {
    let data = &envelope.data;
    if !data["totals"].is_object() {
        if let Some(error) = &envelope.error {
            writeln!(output, "{}", terminal_text(&error.message, false))?;
            if let Some(hint) = &error.hint {
                writeln!(output, "{}", terminal_text(hint, false))?;
            }
        }
        return Ok(());
    }
    let totals = &data["totals"];
    if data["markdown_only"] == true {
        writeln!(output, "Capture scope: Markdown files (.md, .markdown).")?;
    }
    if data["dry_run"] == true {
        writeln!(
            output,
            "Preview: {} files to capture. No files were changed.",
            totals["planned"]
        )?;
    } else {
        writeln!(
            output,
            "Captured {}; refreshed {}; unchanged {}; skipped {}; failed {}.",
            totals["captured"],
            totals["refreshed"],
            totals["unchanged"],
            totals["skipped"],
            totals["failed"]
        )?;
    }
    let mut shown = 0;
    let mut hidden = 0;
    if let Some(items) = data["items"].as_array() {
        for item in items {
            let action = item["action"].as_str().unwrap_or("unknown");
            let issue = !item["error"].is_null()
                || item["capture_state"] == "unsupported"
                || item["capture_state"] == "empty"
                || matches!(action, "skipped" | "not_attempted" | "failed");
            if action == "unchanged" && !issue {
                continue;
            }
            if !issue && shown >= 3 {
                hidden += 1;
                continue;
            }
            if !issue {
                shown += 1;
            }
            write!(
                output,
                "{}: {}",
                terminal_text(item["input"].as_str().unwrap_or("file"), false),
                action.replace('_', " ")
            )?;
            if let Some(id) = item["source_id"].as_str() {
                write!(output, "  {id}")?;
            }
            if !item["citation"].is_null() {
                write!(output, "  ready to cite")?;
            } else if item["capture_state"] == "empty" {
                write!(output, "  empty file; no text to cite")?;
            } else if item["capture_state"] == "unsupported" {
                write!(output, "  original saved; no readable text")?;
            }
            writeln!(output)?;
            if item["capture_state"] == "unsupported" {
                writeln!(
                    output,
                    "  Export its text as UTF-8 Markdown or plain text, then capture that file to search and cite it."
                )?;
            }
            if let Some(reason) = item["error"]["message"]
                .as_str()
                .or(item["reason"].as_str())
            {
                writeln!(output, "  {}", terminal_text(reason, false))?;
            }
            if let Some(hint) = item["error"]["hint"].as_str() {
                writeln!(output, "  {}", terminal_text(hint, false))?;
            }
        }
    }
    if hidden > 0 {
        if data["dry_run"] == true {
            writeln!(
                output,
                "{hidden} more files planned. No files were changed."
            )?;
        } else {
            writeln!(
                output,
                "{hidden} more files saved. Ask a question with lwiki context to read matching text and citations."
            )?;
        }
    }
    if totals["not_attempted"].as_u64().unwrap_or(0) > 0 {
        writeln!(
            output,
            "{} files were not attempted. Fix the reported problem and repeat the command.",
            totals["not_attempted"]
        )?;
    }
    Ok(())
}

pub(super) fn present_read(envelope: &Envelope, output: &mut impl Write) -> io::Result<()> {
    let data = &envelope.data;
    if data["dry_run"] == true {
        return writeln!(
            output,
            "Would read source {}. No text was read.",
            data["source_id"]
        );
    }
    writeln!(
        output,
        "{}",
        terminal_text(data["title"].as_str().unwrap_or("Source"), false)
    )?;
    writeln!(
        output,
        "{}",
        data["read"]["body"].as_str().unwrap_or_default()
    )?;
    writeln!(
        output,
        "Source: {}  Revision: {}",
        data["source_id"].as_str().unwrap_or_default(),
        data["revision_id"].as_str().unwrap_or_default()
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preview_omits_clean_rows_without_claiming_saved_files_or_hiding_issues() {
        let mut items: Vec<_> = (0..7)
            .map(|index| json!({"input":format!("note-{index}.md"),"action":"planned"}))
            .collect();
        items.push(json!({"input":"unreadable.md","action":"failed","error":{"message":"cannot read input"}}));
        let envelope = Envelope::success(
            "source capture",
            json!({
                "dry_run":true,"markdown_only":true,"totals":{"planned":7},"items":items
            }),
        );
        let mut output = Vec::new();
        present_capture(&envelope, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("4 more files planned. No files were changed."));
        assert!(!text.contains("files saved"));
        assert!(!text.contains("ready to cite"));
        assert!(text.contains("unreadable.md: failed\n  cannot read input"));
        assert!(text.contains("Capture scope: Markdown files"));
    }
}
