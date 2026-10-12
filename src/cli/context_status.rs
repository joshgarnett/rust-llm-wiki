//! Context diagnostics describe scope and missing evidence, not successful work.
use super::dispatch::terminal_text;
use crate::output::Envelope;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{self, Write},
};

fn numeric(warning: &str, prefix: &str, separators: &[&str]) -> bool {
    let Some(mut remaining) = warning.strip_prefix(prefix) else {
        return false;
    };
    for separator in separators {
        let Some((count, rest)) = remaining.split_once(separator) else {
            return false;
        };
        if count.is_empty()
            || !count.bytes().all(|byte| byte.is_ascii_digit())
            || count.parse::<u64>().is_err()
        {
            return false;
        }
        remaining = rest;
    }
    remaining.is_empty()
}

/// Hide only fully recognized successful diagnostics. Unexpected formats stay
/// visible, including a recognized prefix followed by a new failure message.
fn warning(value: &str) -> Option<String> {
    if matches!(
        value,
        "tokens are UTF-8 byte estimates (ceil(bytes/4)); no exact tokenizer accounting"
            | "proof bytes/files meter canonical notes, source/dependency payloads and optional saved-display reads; directory/component/entry counts are logical operations; operational journals/SQLite/recovery/index maintenance are separate local safety work; elapsed checks do not interrupt blocking syscalls"
            | "unverified index snapshot; citations suppressed"
            | "Discovery uses the published generation; selected document dependencies are verified. Global membership, identity uniqueness, completeness and unselected freshness are not verified. Use index sync to discover external edits."
            | "Discovery uses published document eligibility and compact policy-bound unit identities. Compatible vectors are intersected with current descriptors; unselected canonical freshness is unverified and index sync discovers external edits."
            | "eligibility and discovery refer to the published generation; selected source heads and complete original/content bytes were checked, without proving global identity uniqueness, membership, cache completeness or unselected dependency freshness"
    ) || value == crate::retrieval::selected_search::SCOPE_WARNING
    {
        return None;
    }
    if numeric(
        value,
        "source-aware context inspected ",
        &[
            " bytes in ",
            " blocks; query overlap guides passage selection, not answer completeness",
        ],
    ) || numeric(
        value,
        "context retained ranked evidence units and mapped unique structural parents over ",
        &[
            " source bytes and ",
            " blocks; retrieval rank does not establish answer completeness",
        ],
    ) || numeric(
        value,
        "selected semantic scoring: ",
        &[
            " units, ",
            " source bytes, ",
            " decoded vector bytes; selected canonical proof is counted separately",
        ],
    ) || numeric(
        value,
        "Vector read work: ",
        &[
            " reads / ",
            " bytes, ",
            " metadata bytes, ",
            " SQL VM steps; shared across discovery and scoring.",
        ],
    ) || numeric(
        value,
        "Vector read work: ",
        &[
            " reads / ",
            " bytes, ",
            " metadata bytes, ",
            " SQL VM steps; shared across both discovery passes and selected scoring.",
        ],
    ) {
        return None;
    }
    if numeric(
        value,
        "discovery domain: captured-source payloads only; ",
        &[
            " candidate owners and ",
            " selected owners; pages, graph records and other vault content were not searched; omissions outside this domain are not counted",
        ],
    ) {
        return Some("Only captured sources were searched. Authored notes and relationships were excluded; their omissions are not counted.".into());
    }
    // The existing coverage translator preserves missing/corrupt/pending counts.
    // Accept only the full known context format before allowing it to hide a
    // complete-coverage message. A malformed or newly extended suffix survives.
    if semantic_coverage(value) {
        return super::search_status::human_warning(value, false);
    }
    Some(terminal_text(value, false))
}

fn semantic_coverage(value: &str) -> bool {
    numeric(
        value,
        "semantic coverage ",
        &[
            "/",
            " eligible units; ",
            " missing, ",
            " corrupt, ",
            " pending; exact linear scan. Discovery read ",
            " compact descriptors / ",
            " logical field bytes and rendered ",
            " units / ",
            " UTF-8 bytes; catalog work is separate from selected proof.",
        ],
    )
}

fn reason(value: &str) -> &str {
    match value {
        "required_bundle_or_passage_does_not_fit" => {
            "passages or complete relationship evidence exceeded the context limit"
        }
        "document_passage_cap" => "per-note passage limit",
        "merged_passage_exceeds_excerpt_bound" => "combined passage exceeded the excerpt limit",
        "discovery_only_or_empty" => "matching notes had no usable evidence",
        "combined_graph_share_cap" => {
            "relationship evidence reached its share of the context limit"
        }
        other => other,
    }
}

pub fn present_context_status(envelope: &Envelope, output: &mut impl Write) -> io::Result<()> {
    if let Some(selection) = &envelope.meta.selection {
        writeln!(
            output,
            "Wiki: {}",
            terminal_text(selection["root"].as_str().unwrap_or("unknown"), false)
        )?;
    }
    if envelope.ok {
        match envelope.meta.freshness.as_deref() {
            Some("indexed_evidence") => writeln!(
                output,
                "Checked the returned text. Changes elsewhere in the wiki were not checked; results may be incomplete. Run lwiki index sync to find changed notes."
            )?,
            Some("verified_snapshot") => writeln!(
                output,
                "Results checked against the current notes; matching text may still be omitted."
            )?,
            // The human text's index-snapshot header already says unverified.
            Some("index_snapshot") if envelope.context_human_text.is_some() => {}
            Some("index_snapshot") => writeln!(
                output,
                "Saved index used; notes were not checked and source citations are unavailable."
            )?,
            Some(other) => writeln!(output, "Freshness: {}", terminal_text(other, false))?,
            None => {}
        }
        if let Some(packet) = envelope.data["selection_packet"].as_object() {
            writeln!(
                output,
                "Selection task: {} candidate cards. Save the agent's reply, then repeat this request with --selection FILE.",
                packet
                    .get("candidate_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            )?;
        } else if envelope
            .context_human_text
            .as_ref()
            .is_some_and(String::is_empty)
            || envelope.data["text"] == ""
        {
            writeln!(
                output,
                "{}",
                if envelope.meta.partial {
                    "No context fits. Increase --max-bytes and --max-tokens or narrow the query."
                } else {
                    "No matching context. Try fewer query terms or literal search for exact text."
                }
            )?;
        }
    }
    let mut reasons = BTreeMap::<&str, u64>::new();
    if let Some(omissions) = envelope.data["omissions"].as_array() {
        for omission in omissions {
            *reasons
                .entry(omission["reason"].as_str().unwrap_or("unspecified"))
                .or_default() += omission["count"].as_u64().unwrap_or(0);
        }
    }
    for (value, count) in &reasons {
        writeln!(
            output,
            "Omitted {count}: {}",
            terminal_text(reason(value), false)
        )?;
    }
    for value in &envelope.warnings {
        if let Some(value) = warning(value) {
            writeln!(output, "Warning: {value}")?;
        }
    }
    if envelope.meta.partial && reasons.is_empty() {
        writeln!(
            output,
            "Some matching evidence is unavailable. See the warnings and relationship omission counts."
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_complete_recognized_diagnostics_are_hidden() {
        let complete = "Vector read work: 1 reads / 2 bytes, 3 metadata bytes, 4 SQL VM steps; shared across discovery and scoring.";
        assert_eq!(warning(complete), None);
        for value in [
            format!("{complete} failed"),
            complete.replace("2 bytes", "unknown bytes"),
            "Vector read work: source unavailable".into(),
            "new warning: partial source".into(),
        ] {
            assert!(warning(&value).is_some(), "{value}");
        }
        let coverage = "semantic coverage 4/4 eligible units; 0 missing, 0 corrupt, 0 pending; exact linear scan. Discovery read 8 compact descriptors / 9 logical field bytes and rendered 4 units / 10 UTF-8 bytes; catalog work is separate from selected proof.";
        assert_eq!(warning(coverage), None);
        assert!(warning(&format!("{coverage} unexpected omission")).is_some());
        let missing = coverage
            .replace("4/4", "3/4")
            .replace("0 missing", "1 missing");
        assert!(warning(&missing).unwrap().contains("1 missing"));
    }

    #[test]
    fn scope_unknown_omission_and_partial_are_preserved() {
        let mut envelope = Envelope::success(
            "context",
            json!({"text":"answer", "omissions":[{"reason":"new_reason", "count":3}]}),
        );
        envelope.meta.selection =
            Some(json!({"root":"/tmp/wiki", "wiki_id":"wiki_x", "origin":"explicit"}));
        envelope.meta.freshness = Some("indexed_evidence".into());
        envelope.meta.partial = true;
        envelope
            .warnings
            .push("provider coverage incomplete".into());
        let mut output = Vec::new();
        present_context_status(&envelope, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_eq!(output.matches("Wiki:").count(), 1);
        assert!(output.contains("Changes elsewhere in the wiki were not checked"));
        assert!(output.contains("Omitted 3: new_reason"));
        assert!(output.contains("provider coverage incomplete"));
        assert!(!output.contains("wiki_x"));
        assert!(!output.contains("candidate limit"));
    }

    #[test]
    fn empty_plain_evidence_is_reported_despite_a_canonical_scope_header() {
        let mut envelope = Envelope::success(
            "context",
            json!({"text":"[context indexed_documents; scope]\n\n"}),
        );
        envelope.context_human_text = Some(String::new());
        let mut output = Vec::new();
        present_context_status(&envelope, &mut output).unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("No matching context")
        );
    }
}
