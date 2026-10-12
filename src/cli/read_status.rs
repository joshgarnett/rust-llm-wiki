//! Plain read diagnostics keep byte ranges and continuation, without edit guards.
use super::dispatch::terminal_text;
use crate::output::Envelope;
use std::io::{self, Write};

fn source_continuation(envelope: &Envelope, warning: &str) -> Option<String> {
    if !envelope.ok || envelope.command != "source read" {
        return None;
    }
    let data = &envelope.data;
    let source = crate::domain::RecordId::new(data["source_id"].as_str()?).ok()?;
    let revision = crate::domain::RecordId::new(data["revision_id"].as_str()?).ok()?;
    let read = &data["read"];
    let start = read["continuation"]["start"].as_u64()?;
    let end = read["continuation"]["end"].as_u64()?;
    if read["truncated"] != true || read["range"]["end"].as_u64()? != start || start >= end {
        return None;
    }
    // Recognize the complete existing generated warning. Keep unknown wording,
    // malformed selectors and unusual bounds visible without guessing at them.
    let cap = warning.strip_suffix('.')?.rsplit_once(" --max-bytes ")?.1;
    let max_bytes = cap.parse::<usize>().ok()?;
    if max_bytes == 0 || max_bytes > 16 * 1024 * 1024 || max_bytes.to_string() != cap {
        return None;
    }
    let expected = format!(
        "More text is available. Continue with source read {source} --revision {revision} --start {start} --max-bytes {max_bytes}."
    );
    if warning != expected {
        return None;
    }
    Some(format!(
        "Read: lwiki source read {source} --revision {revision} --start {start} --end {end} --max-bytes {max_bytes}"
    ))
}

pub fn present_read_status(envelope: &Envelope, output: &mut impl Write) -> io::Result<()> {
    if let Some(selection) = &envelope.meta.selection {
        writeln!(
            output,
            "Wiki: {}",
            terminal_text(selection["root"].as_str().unwrap_or("unknown"), false)
        )?;
    }
    let source = envelope.command == "source read";
    let read = if source {
        &envelope.data["read"]
    } else {
        &envelope.data
    };
    if envelope.ok {
        if !source && let Some(path) = read["path"].as_str() {
            writeln!(output, "Note: {}", terminal_text(path, false))?;
        }
        match envelope.meta.freshness.as_deref() {
            Some("verified_snapshot") => writeln!(output, "Returned text checked.")?,
            Some("indexed_evidence") => writeln!(
                output,
                "Checked the returned text. Changes elsewhere in the wiki were not checked."
            )?,
            Some("index_snapshot") => writeln!(
                output,
                "Saved index used; this text is unverified and has no source citation."
            )?,
            Some(other) => writeln!(output, "Freshness: {}", terminal_text(other, false))?,
            None => {}
        }
        if !source && let Some(state) = read["source_citation"]["eligibility"].as_str() {
            writeln!(output, "Captured text ({}).", terminal_text(state, false))?;
        }
        if let (Some(start), Some(end)) = (
            read["range"]["start"].as_u64(),
            read["range"]["end"].as_u64(),
        ) {
            if let Some(truncated) = read["truncated"].as_bool() {
                writeln!(
                    output,
                    "Read bytes {start}..{end}. {}",
                    if truncated {
                        "More requested text remains."
                    } else {
                        "The requested text was returned in full."
                    }
                )?;
            } else {
                writeln!(output, "Read bytes {start}..{end}.")?;
            }
        }
        if source && envelope.data["capture_state"] == "empty" {
            writeln!(output, "No captured text is available.")?;
        }
    }
    if read["truncated"] == true && read["continuation"].is_object() {
        writeln!(
            output,
            "Keep the same --wiki and configuration options for the continuation command."
        )?;
    }
    for warning in &envelope.warnings {
        if warning
            == "Read verifies selected document dependencies against this discovery generation; global membership and identity uniqueness are not verified. Use index sync to discover external edits."
            && envelope.meta.freshness.as_deref() == Some("indexed_evidence")
        {
            continue;
        }
        if let Some(command) = source_continuation(envelope, warning) {
            writeln!(output, "{command}")?;
            continue;
        }
        // Generic-read continuations already carry the exact path and bounds.
        // Every unknown warning, including unknown continuation wording, stays.
        writeln!(output, "Warning: {}", terminal_text(warning, false))?;
    }
    if envelope.meta.partial && read["truncated"] != true {
        writeln!(
            output,
            "Some requested text is unavailable. See the warnings."
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn status(envelope: &Envelope) -> String {
        let mut output = Vec::new();
        present_read_status(envelope, &mut output).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn ordinary_read_retains_selector_scope_state_range_and_continuation() {
        let mut envelope = Envelope::success(
            "read",
            json!({
                "path":"notes/Café.md", "hash":"guard", "body":"unmodified body",
                "range":{"start":20,"end":60}, "truncated":true, "continuation":{"start":60,"end":80},
                "source_citation":{"eligibility":"historical", "citation":{"kind":"source"}}
            }),
        );
        envelope.meta.selection =
            Some(json!({"root":"/tmp/wiki", "wiki_id":"wiki_secret", "origin":"explicit"}));
        envelope.meta.freshness = Some("indexed_evidence".into());
        envelope.meta.partial = true;
        let follow = "Read is truncated; continue with: lwiki --wiki '/tmp/wiki' read --path 'notes/Café.md' --start 60 --end 80 --max-bytes 20";
        envelope.warnings.push(follow.into());
        let before = serde_json::to_vec(&envelope).unwrap();
        let output = status(&envelope);
        assert_eq!(serde_json::to_vec(&envelope).unwrap(), before);
        for required in [
            "Wiki: /tmp/wiki",
            "Note: notes/Café.md",
            "Captured text (historical)",
            "Read bytes 20..60",
            "More requested text remains",
            follow,
            "Changes elsewhere in the wiki were not checked",
        ] {
            assert!(output.contains(required), "{required}");
        }
        assert!(!output.contains("guard"));
        assert!(!output.contains("wiki_secret"));
        assert!(!output.contains("\"kind\""));
        assert!(!output.contains("returned in full"));
    }

    #[test]
    fn complete_subrange_does_not_claim_whole_source_eof_and_unknown_warnings_survive() {
        let mut envelope = Envelope::success(
            "source read",
            json!({
                "read":{"body":"selected bytes", "range":{"start":20,"end":30}, "truncated":false}
            }),
        );
        envelope.meta.freshness = Some("verified_snapshot".into());
        envelope
            .warnings
            .push("new warning: source route incomplete".into());
        let output = status(&envelope);
        assert!(output.contains("Read bytes 20..30"));
        assert!(output.contains("The requested text was returned in full"));
        assert!(output.contains("new warning: source route incomplete"));
        assert!(!output.contains("EOF"));
        assert!(!output.contains("whole source"));
    }

    #[test]
    fn cached_empty_preview_and_nontruncation_partial_remain_honest() {
        let mut cached = Envelope::success(
            "read",
            json!({"body":"cached", "range":{"start":0,"end":6},"truncated":false}),
        );
        cached.meta.freshness = Some("index_snapshot".into());
        assert!(status(&cached).contains("unverified and has no source citation"));
        let mut empty = Envelope::success(
            "source read",
            json!({"capture_state":"empty","read":{"body":"","range":{"start":0,"end":0},"truncated":false}}),
        );
        assert!(status(&empty).contains("No captured text is available"));
        empty.meta.partial = true;
        assert!(status(&empty).contains("Some requested text is unavailable"));
        let preview = Envelope::success("source read", json!({"dry_run":true,"text_read":false}));
        assert!(!status(&preview).contains("Returned text checked"));
        assert!(!status(&preview).contains("returned in full"));
    }

    #[test]
    fn linked_source_continuation_preserves_the_returned_revision_and_next_range() {
        let mut envelope = Envelope::success(
            "source read",
            json!({
                "source_id":"Source.Manual", "revision_id":"Revision.Old",
                "source_link":{"status":"historical","markdown":"[Manual](</tmp/old.md>)"},
                "read":{"body":"exact range", "range":{"start":10,"end":21}, "truncated":true,
                    "continuation":{"start":21,"end":100}}
            }),
        );
        envelope.meta.partial = true;
        let follow = "More text is available. Continue with source read Source.Manual --revision Revision.Old --start 21 --max-bytes 40.";
        envelope.warnings.push(follow.into());
        let output = status(&envelope);
        assert!(output.contains("Read bytes 10..21"));
        let before = serde_json::to_vec(&envelope).unwrap();
        assert!(output.contains("Read: lwiki source read Source.Manual --revision Revision.Old --start 21 --end 100 --max-bytes 40"));
        assert!(output.contains("Keep the same --wiki and configuration options"));
        assert!(!output.contains(follow));
        assert_eq!(serde_json::to_vec(&envelope).unwrap(), before);
        assert!(!output.contains("returned in full"));
    }
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn malformed_or_unknown_continuations_are_not_rewritten() {
        let mut envelope = Envelope::success(
            "source read",
            json!({
                "source_id":"S", "revision_id":"R", "read": {
                    "range":{"start":0,"end":4}, "truncated":true,
                    "continuation":{"start":4,"end":9}
                }
            }),
        );
        let valid = "More text is available. Continue with source read S --revision R --start 4 --max-bytes 3.";
        assert_eq!(
            source_continuation(&envelope, valid).unwrap(),
            "Read: lwiki source read S --revision R --start 4 --end 9 --max-bytes 3"
        );
        for value in [
            valid.replace("--max-bytes 3", "--max-bytes 03"),
            valid.replace("--max-bytes 3", "--max-bytes 0"),
            valid.replace("--revision R", "--revision Other"),
            valid.replace("--start 4", "--start 5"),
            format!("{valid} extra warning"),
        ] {
            assert!(source_continuation(&envelope, &value).is_none());
        }
        envelope.data["read"]["continuation"]["start"] = json!(5);
        assert!(source_continuation(&envelope, valid).is_none());
    }

    #[test]
    fn source_continuation_parses_with_retained_selection_configuration_and_verbose() {
        use crate::cli::{Arguments, Command, arguments::SourceCommand};
        use clap::Parser;
        let wiki = "/tmp/wiki with apostrophe's % 東京";
        let config = "/tmp/config file's.toml";
        let arguments = Arguments::try_parse_from([
            "lwiki",
            "--wiki",
            wiki,
            "--preferences",
            config,
            "--offline",
            "--verbose",
            "source",
            "read",
            "Source.Old",
            "--revision",
            "Revision.Old",
            "--start",
            "21",
            "--end",
            "100",
            "--max-bytes",
            "40",
        ])
        .unwrap();
        assert_eq!(arguments.wiki.unwrap().to_str(), Some(wiki));
        assert_eq!(arguments.preferences.unwrap().to_str(), Some(config));
        assert!(arguments.offline && arguments.verbose);
        match arguments.command {
            Command::Source {
                command:
                    SourceCommand::Read {
                        id,
                        revision,
                        start,
                        end,
                        max_bytes,
                    },
            } => {
                assert_eq!(id.as_str(), "Source.Old");
                assert_eq!(revision.unwrap().as_str(), "Revision.Old");
                assert_eq!((start, end, max_bytes), (Some(21), Some(100), Some(40)));
            }
            _ => panic!("supplied source command changed selectors"),
        }
    }
}
