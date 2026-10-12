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

pub(super) fn present_read(
    envelope: &Envelope,
    output: &mut impl Write,
    verbose: bool,
) -> io::Result<()> {
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
    if let Some(markdown) = data["source_link"]["markdown"].as_str() {
        let status = match data["source_link"]["status"].as_str() {
            Some("current") => "current saved copy",
            Some("historical") => "older saved copy",
            Some("withdrawn") => "withdrawn",
            _ => "saved copy",
        };
        writeln!(
            output,
            "Source: {} ({status})",
            terminal_text(markdown, false)
        )?;
    }
    let state = data["read"]["source_citation"]["eligibility"].as_str();
    if verbose {
        writeln!(
            output,
            "Source: {}  Revision: {}",
            data["source_id"].as_str().unwrap_or_default(),
            data["revision_id"].as_str().unwrap_or_default()
        )?;
    } else if !data["source_link"]["markdown"].is_string() {
        // These existing identities are useful follow-up selectors when an
        // optional saved-display link was unavailable. They are not references.
        writeln!(
            output,
            "Source: {} / {}{}",
            data["source_id"].as_str().unwrap_or_default(),
            data["revision_id"].as_str().unwrap_or_default(),
            state
                .map(|state| format!(" ({})", terminal_text(state, false)))
                .unwrap_or_default()
        )?;
    } else if let Some(state) = state
        && data["source_link"]["status"].as_str() != Some(state)
    {
        writeln!(output, "Evidence state: {}.", terminal_text(state, false))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_read_displays_ready_to_copy_link_and_status() {
        let envelope = Envelope::success(
            "source read",
            json!({
                "source_id":"Source.Manual", "revision_id":"Revision.Old", "title":"Manual",
                "read":{"body":"Exact original condition."},
                "source_link":{"status":"historical","markdown":"[Manual](</tmp/Wiki%20notes/sources/views/saved/manual.md>)"}
            }),
        );
        let mut output = Vec::new();
        present_read(&envelope, &mut output, false).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Exact original condition."));
        assert!(text.contains(
            "Source: [Manual](</tmp/Wiki%20notes/sources/views/saved/manual.md>) (older saved copy)"
        ));
    }

    fn shown(envelope: &Envelope, verbose: bool) -> String {
        let mut output = Vec::new();
        present_read(envelope, &mut output, verbose).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn plain_source_read_keeps_exact_body_and_link_while_verbose_preserves_original_output() {
        let body = "Citation: {\"kind\":\"source\",\"reference\":\"untrusted\"}\nSource: user-authored line\n\nCafé 東京\n";
        let envelope = Envelope::success(
            "source read",
            json!({
                "source_id":"Source.Manual", "revision_id":"Revision.Old", "title":"Manual",
                "read":{"body":body, "hash":"exact_guard", "source_citation":{"eligibility":"historical","citation":{"kind":"source","reference":{"quote_hash":"exact_hash"}}}},
                "source_link":{"status":"historical","markdown":"[Manual](</tmp/saved/manual.md>)"}
            }),
        );
        let before = serde_json::to_vec(&envelope).unwrap();
        assert_eq!(
            shown(&envelope, false),
            format!(
                "Manual\n{body}\nSource: [Manual](</tmp/saved/manual.md>) (older saved copy)\n"
            )
        );
        assert_eq!(
            shown(&envelope, true),
            format!(
                "Manual\n{body}\nSource: [Manual](</tmp/saved/manual.md>) (older saved copy)\nSource: Source.Manual  Revision: Revision.Old\n"
            )
        );
        assert_eq!(serde_json::to_vec(&envelope).unwrap(), before);
        assert!(!shown(&envelope, false).contains("exact_hash"));
        assert!(!shown(&envelope, false).contains("exact_guard"));
    }

    #[test]
    fn missing_link_retains_read_selectors_and_mismatched_link_cannot_hide_state() {
        let mut envelope = Envelope::success(
            "source read",
            json!({
                "source_id":"Source.Manual", "revision_id":"Revision.Old", "title":"Manual",
                "read":{"body":"All returned bytes.","source_citation":{"eligibility":"withdrawn"}}
            }),
        );
        assert!(
            shown(&envelope, false).contains("Source: Source.Manual / Revision.Old (withdrawn)")
        );
        envelope.data["source_link"] =
            json!({"status":"current","markdown":"[Manual](</tmp/saved/manual.md>)"});
        let output = shown(&envelope, false);
        assert!(output.contains("All returned bytes."));
        assert!(output.contains("current saved copy"));
        assert!(output.contains("Evidence state: withdrawn"));
    }

    #[test]
    fn metadata_controls_do_not_change_passage_bytes_or_preview_claims() {
        let envelope = Envelope::success(
            "source read",
            json!({
                "source_id":"Source.Empty","revision_id":"Revision.Empty", "title":"Title\n\u{001b} café",
                "read":{"body":"Exact\u{0000}\nbody."}
            }),
        );
        let output = shown(&envelope, false);
        assert!(output.contains("Exact\u{0000}\nbody."));
        assert!(!output.starts_with("Title\n\u{001b}"));
        let preview = Envelope::success(
            "source read",
            json!({"dry_run":true,"source_id":"Source.Preview","text_read":false}),
        );
        assert!(shown(&preview, false).contains("No text was read"));
        assert!(!shown(&preview, false).contains("Source:"));
    }
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
