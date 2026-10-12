//! Human search cards preserve the machine result and its evidence authority.
use super::dispatch::terminal_text;
use crate::{domain::Blake3Hash, output::Envelope};
use serde_json::{Value, json};
use std::io::{self, Write};

/// Keep actionable warnings visible; detailed work counters stay in JSON and
/// can also be requested in human output with --verbose.
pub fn human_warning(warning: &str, verbose: bool) -> Option<String> {
    let message = if verbose {
        warning.to_owned()
    } else if warning.starts_with("Vector read work: ") {
        return None;
    } else if let Some(coverage) = coverage_warning(warning) {
        return coverage.map(|message| terminal_text(&message, false));
    } else if warning.starts_with("Discovery uses published document eligibility") {
        "Results use the saved index. New or edited notes may be missing; run lwiki index sync to update it.".into()
    } else if warning == crate::retrieval::selected_search::SCOPE_WARNING {
        "The displayed notes were checked. New or edited notes elsewhere may be missing; run lwiki index sync to update the saved index.".into()
    } else if warning.starts_with("candidate_cap_reached;") {
        "Search reached its candidate limit. Increase --candidates or narrow the query to find more notes.".into()
    } else if warning.starts_with("complete_query_preference_work_limit;") {
        "Search reached a work limit; results keep the existing order.".into()
    } else {
        warning.to_owned()
    };
    Some(terminal_text(&message, false))
}

// Unknown or malformed coverage formats remain visible rather than silently
// hiding a newly introduced failure or omission.
fn coverage_warning(warning: &str) -> Option<Option<String>> {
    let rest = warning.strip_prefix("semantic coverage ")?;
    let (ratio, rest) = rest.split_once(" eligible units; ")?;
    let (available, eligible) = ratio.split_once('/')?;
    let available: usize = available.parse().ok()?;
    let eligible: usize = eligible.parse().ok()?;
    let counts = rest.split(';').next()?;
    let mut missing = None;
    let mut corrupt = None;
    let mut pending = 0;
    for count in counts.split(',') {
        let (count, label) = count.trim().split_once(' ')?;
        let count: usize = count.parse().ok()?;
        match label {
            "missing" => missing = Some(count),
            "corrupt" => corrupt = Some(count),
            "pending" => pending = count,
            _ => return None,
        }
    }
    let (missing, corrupt) = (missing?, corrupt?);
    if available == eligible && missing == 0 && corrupt == 0 && pending == 0 {
        Some(None)
    } else {
        Some(Some(format!(
            "Similarity search covers {available}/{eligible} passages: {missing} missing, {corrupt} unreadable, {pending} awaiting preparation. Local text results may still be available."
        )))
    }
}

pub(super) fn present(
    envelope: &Envelope,
    output: &mut impl Write,
    verbose: bool,
) -> io::Result<()> {
    let Some(hits) = envelope.data["hits"].as_array() else {
        if envelope.data["dry_run"] == true {
            writeln!(
                output,
                "Search preview. Results and note freshness were not checked."
            )?;
            if let Some(query) = envelope.data["query"].as_str() {
                writeln!(output, "Query: {}", terminal_text(query, false))?;
            }
            return Ok(());
        }
        serde_json::to_writer_pretty(&mut *output, &envelope.data)?;
        return writeln!(output);
    };
    present_freshness(envelope, output)?;
    let groups = grouping(hits);
    let mut shown = vec![false; hits.len()];
    for index in 0..hits.len() {
        // Keep a related card at its first result position, even when the
        // original source ranks above the Page that contains the same text.
        let page_index = groups[index].unwrap_or(index);
        if shown[page_index] {
            continue;
        }
        shown[page_index] = true;
        let hit = &hits[page_index];
        writeln!(
            output,
            "{}",
            terminal_text(hit["title"].as_str().unwrap_or("Untitled note"), false)
        )?;
        present_locator(hit, envelope, output, "", verbose)?;
        let mut displayed = false;
        for excerpt in excerpts(hit) {
            if let Some(text) = excerpt["text"].as_str().filter(|text| !text.is_empty()) {
                writeln!(output, "{}", terminal_text(text, true))?;
                displayed = true;
            }
            if verbose && !excerpt["citation"].is_null() {
                writeln!(output, "Exact citation: {}", excerpt["citation"])?;
            }
        }
        if !displayed {
            writeln!(output, "(No note text.)")?;
        }
        for (source_index, grouped_page) in groups.iter().enumerate() {
            if *grouped_page == Some(page_index) {
                let source = &hits[source_index];
                writeln!(
                    output,
                    "The same displayed text appears in captured source: {}",
                    terminal_text(source["title"].as_str().unwrap_or("Untitled source"), false)
                )?;
                present_locator(source, envelope, output, "  ", verbose)?;
                if verbose {
                    for excerpt in excerpts(source) {
                        if !excerpt["citation"].is_null() {
                            writeln!(output, "  Exact citation: {}", excerpt["citation"])?;
                        }
                    }
                }
            }
        }
        writeln!(output)?;
    }
    if hits.is_empty() {
        writeln!(output, "No matches.")?;
    }
    if envelope.meta.partial {
        if let Some(cursor) = envelope.data["next_cursor"].as_str() {
            writeln!(
                output,
                "More results: repeat this search with the same options and --cursor '{cursor}'"
            )?;
        } else if envelope.data["omitted_candidates"].as_u64().unwrap_or(0) > 0 {
            writeln!(
                output,
                "Search reached its candidate limit; increase --candidates or narrow the query. No continuation cursor is available."
            )?;
        } else {
            writeln!(
                output,
                "Some matching evidence may be unavailable. See the warnings for details."
            )?;
        }
    }
    Ok(())
}

fn present_freshness(envelope: &Envelope, output: &mut impl Write) -> io::Result<()> {
    match envelope.meta.freshness.as_deref() {
        Some("verified_snapshot") => writeln!(output, "Results checked against the current notes."),
        Some("indexed_evidence") => writeln!(
            output,
            "Displayed evidence checked; discovery uses the saved index."
        ),
        Some("index_snapshot") => writeln!(
            output,
            "Results from the saved index; note freshness has not been checked."
        ),
        Some(other) => writeln!(output, "Freshness: {}", terminal_text(other, false)),
        None => Ok(()),
    }
}

fn command_prefix(envelope: &Envelope) -> String {
    envelope
        .meta
        .selection
        .as_ref()
        .and_then(|selection| selection["root"].as_str())
        .map(|root| format!("lwiki --wiki {}", shell_quote(root)))
        .unwrap_or_else(|| "lwiki".into())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", terminal_text(value, false).replace('\'', "'\\''"))
}

fn present_locator(
    hit: &Value,
    envelope: &Envelope,
    output: &mut impl Write,
    indent: &str,
    verbose: bool,
) -> io::Result<()> {
    let state = hit["eligibility"].as_str().unwrap_or("unknown");
    let authored = hit["authored_status"].as_str();
    let state = if let Some(authored) = authored {
        format!("{authored}; {state}")
    } else {
        state.to_owned()
    };
    let state = terminal_text(&state, false);
    if let (Some(source), Some(revision)) =
        (hit["source_id"].as_str(), hit["owner_revision"].as_str())
    {
        writeln!(output, "{indent}Captured source ({state})")?;
        let revision_option = if hit["eligibility"] == "current" {
            String::new()
        } else {
            format!(" --revision {}", shell_quote(revision))
        };
        writeln!(
            output,
            "{indent}Read: {} source read {}{revision_option}",
            command_prefix(envelope),
            shell_quote(source)
        )?;
        if verbose {
            writeln!(
                output,
                "{indent}Stored text: {}",
                terminal_text(hit["path"].as_str().unwrap_or_default(), false)
            )?;
        }
    } else {
        let kind = match hit["kind"].as_str() {
            Some("page") => "Page",
            Some("entity") => "Named item",
            Some("source") => "Source note",
            Some("revision") => "Saved-copy metadata",
            Some("vault") => "Wiki home",
            _ => "Note",
        };
        let path = hit["path"].as_str().unwrap_or_default();
        writeln!(
            output,
            "{indent}{kind}: {} ({state})",
            terminal_text(path, false)
        )?;
        writeln!(
            output,
            "{indent}Read: {} read --path {}",
            command_prefix(envelope),
            shell_quote(path)
        )?;
    }
    Ok(())
}

fn excerpts(hit: &Value) -> impl Iterator<Item = &Value> {
    std::iter::once(&hit["excerpt"])
        .chain(hit["secondary_excerpts"].as_array().into_iter().flatten())
}

/// Presentation grouping leaves the original hits, ranks, cursor and citations
/// intact. A declaration alone never proves that summary text covers its source.
fn grouping(hits: &[Value]) -> Vec<Option<usize>> {
    hits.iter()
        .map(|source| {
            if source["source_id"].as_str().is_none() || source["owner_revision"].as_str().is_none()
            {
                return None;
            }
            hits.iter().position(|page| {
                page["kind"] == "page"
                    && page["eligibility"] == source["eligibility"]
                    && excerpts(source).all(|excerpt| displayed_and_declared(page, source, excerpt))
            })
        })
        .collect()
}

fn displayed_and_declared(page: &Value, source: &Value, excerpt: &Value) -> bool {
    let Some(text) = excerpt["text"].as_str().filter(|text| !text.is_empty()) else {
        return false;
    };
    let (Some(start), Some(end)) = (
        excerpt["span"]["start"].as_u64(),
        excerpt["span"]["end"].as_u64(),
    ) else {
        return false;
    };
    if end.checked_sub(start) != Some(text.len() as u64) {
        return false;
    }
    let expected = json!({"kind":"source","reference":{
        "source_id":source["source_id"],"source_revision":source["owner_revision"],
        "span":{"start":start,"end":end},"quote_hash":Blake3Hash::digest(text.as_bytes())
    }});
    // An uncited cached excerpt may be related as display text, but no citation
    // is fabricated for it and the envelope's unverified freshness stays visible.
    if !excerpt["citation"].is_null() && excerpt["citation"] != expected {
        return false;
    }
    page["declared_source_refs"]
        .as_array()
        .is_some_and(|refs| refs.contains(&expected))
        && excerpts(page).any(|page_excerpt| {
            page_excerpt["text"]
                .as_str()
                .is_some_and(|page_text| page_text.contains(text))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn excerpt(text: &str, start: u64) -> Value {
        json!({"text":text,"span":{"start":start,"end":start+text.len() as u64},"citation":null})
    }

    fn source(text: &str) -> Value {
        json!({"title":"Harbour decision","kind":null,"source_id":"Source.Harbour",
            "owner_revision":"Revision.Current","eligibility":"current",
            "path":"sources/opaque/revisions/opaque/content.md","excerpt":excerpt(text,0),
            "secondary_excerpts":[],"declared_source_refs":[]})
    }

    fn reference(source: &Value, excerpt: &Value) -> Value {
        json!({"kind":"source","reference":{"source_id":source["source_id"],
            "source_revision":source["owner_revision"],"span":excerpt["span"],
            "quote_hash":Blake3Hash::digest(excerpt["text"].as_str().unwrap())}})
    }

    fn page(text: &str, refs: Vec<Value>) -> Value {
        json!({"title":"Harbour visit","kind":"page","authored_status":"draft",
            "eligibility":"current","path":"pages/harbour-visit.md",
            "excerpt":excerpt(text,150),"secondary_excerpts":[],"declared_source_refs":refs})
    }

    fn render(hits: Vec<Value>, freshness: &str) -> (String, Value) {
        let mut envelope = Envelope::success("search", json!({"hits":hits}));
        envelope.meta.freshness = Some(freshness.into());
        envelope.meta.selection = Some(json!({"root":"/Jo's notes"}));
        let before = serde_json::to_value(&envelope).unwrap();
        let mut output = Vec::new();
        present(&envelope, &mut output, false).unwrap();
        assert_eq!(before, serde_json::to_value(&envelope).unwrap());
        (String::from_utf8(output).unwrap(), before)
    }

    #[test]
    fn an_exact_duplicate_groups_without_changing_uncited_snapshot_authority() {
        let source = source("The trial starts on 18 November in the east annex.");
        let page = page(
            source["excerpt"]["text"].as_str().unwrap(),
            vec![reference(&source, &source["excerpt"])],
        );
        let (output, envelope) = render(vec![source, page], "index_snapshot");
        assert!(
            output.contains("Results from the saved index; note freshness has not been checked.")
        );
        assert!(output.contains("Harbour visit\nPage: pages/harbour-visit.md (draft; current)"));
        assert!(
            output.contains("The same displayed text appears in captured source: Harbour decision")
        );
        assert!(output.contains("source read 'Source.Harbour'"));
        assert!(!output.contains("sources/opaque"));
        assert!(!output.contains("Exact citation") && !output.contains("Results checked"));
        assert_eq!(envelope["data"]["hits"].as_array().unwrap().len(), 2);
        assert!(envelope["data"]["hits"][0]["excerpt"]["citation"].is_null());
    }

    #[test]
    fn citations_do_not_hide_conditions_or_independent_notes() {
        let source = source(
            "The trial starts on 18 November in the east annex. Postpone if battery inspection fails before 08:45.",
        );
        let short_page = page(
            "The trial starts on 18 November in the east annex.",
            vec![reference(&source, &source["excerpt"])],
        );
        assert_eq!(grouping(&[source.clone(), short_page]), vec![None, None]);
        let mut source_with_more = source.clone();
        let dependency = excerpt(
            "Jo may collect the key only after Mara records the inspection.",
            400,
        );
        source_with_more["secondary_excerpts"] = json!([dependency]);
        let same_page = page(
            source["excerpt"]["text"].as_str().unwrap(),
            vec![reference(&source, &source["excerpt"])],
        );
        assert_eq!(
            grouping(&[source_with_more.clone(), same_page.clone()]),
            vec![None, None]
        );
        let (output, _) = render(vec![source_with_more, same_page], "verified_snapshot");
        assert!(output.contains("Postpone if battery inspection fails before 08:45"));
        assert!(output.contains("Jo may collect the key only after Mara records the inspection."));
        let mut independent = source.clone();
        independent["source_id"] = json!("Source.Independent");
        assert_eq!(
            grouping(&[
                independent,
                page(
                    source["excerpt"]["text"].as_str().unwrap(),
                    vec![reference(&source, &source["excerpt"])]
                )
            ]),
            vec![None, None]
        );
    }

    #[test]
    fn mismatched_refs_ranges_hashes_and_statuses_remain_separate() {
        let source = source("A complete exact displayed passage.");
        let good_ref = reference(&source, &source["excerpt"]);
        for field in ["source_id", "source_revision", "quote_hash"] {
            let mut wrong = good_ref.clone();
            wrong["reference"][field] = json!("wrong");
            let page = page(source["excerpt"]["text"].as_str().unwrap(), vec![wrong]);
            assert_eq!(grouping(&[source.clone(), page]), vec![None, None]);
        }
        let mut wrong_span = good_ref.clone();
        wrong_span["reference"]["span"]["end"] = json!(999);
        assert_eq!(
            grouping(&[
                source.clone(),
                page(
                    source["excerpt"]["text"].as_str().unwrap(),
                    vec![wrong_span]
                )
            ]),
            vec![None, None]
        );
        let mut historical = page(source["excerpt"]["text"].as_str().unwrap(), vec![good_ref]);
        historical["eligibility"] = json!("historical");
        assert_eq!(grouping(&[source, historical]), vec![None, None]);
    }

    #[test]
    fn diagnostics_are_optional_but_coverage_failures_remain_visible() {
        let complete = "semantic coverage 3/3 eligible units; 0 missing, 0 corrupt, 0 pending; exact linear scan. Discovery read 6 compact descriptors / 2772 logical field bytes and rendered 0 units / 0 UTF-8 bytes; catalog work is separate from selected proof.";
        let partial = "semantic coverage 1/3 eligible units; 1 missing, 1 corrupt, 0 pending; exact linear scan. Discovery read 6 compact descriptors / 2772 logical field bytes and rendered 0 units / 0 UTF-8 bytes; catalog work is separate from selected proof.";
        assert_eq!(human_warning(complete, false), None);
        assert_eq!(human_warning(complete, true), Some(complete.into()));
        let message = human_warning(partial, false).unwrap();
        assert!(message.contains("1/3 passages: 1 missing, 1 unreadable"));
        assert!(!message.contains("logical field") && !message.contains("exact linear"));
        assert_eq!(
            human_warning(
                "Vector read work: 6 reads / 384 bytes, 709 SQL VM steps",
                false
            ),
            None
        );
        for warning in [
            "PROVIDER_AUTH: helper unavailable; local results retained",
            "unknown accounting holds remain",
            "semantic coverage unknown; inspect failure",
            "invalid_note_hit: page.md",
        ] {
            assert_eq!(human_warning(warning, false), Some(warning.into()));
        }
        assert!(
            human_warning(crate::retrieval::selected_search::SCOPE_WARNING, false)
                .unwrap()
                .contains("New or edited notes elsewhere may be missing")
        );
    }

    #[test]
    fn terminal_controls_are_escaped_and_partial_coverage_is_not_a_candidate_claim() {
        let mut hit = source("Inspect \u{001b}[31mred\u{001b}[0m bytes.");
        hit["title"] = json!("Harbour\u{202e}title");
        let mut envelope = Envelope::success(
            "search",
            json!({"hits":[hit],"omitted_candidates":0,"next_cursor":null}),
        );
        envelope.meta.partial = true;
        let mut output = Vec::new();
        present(&envelope, &mut output, false).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains('\u{001b}') && !output.contains('\u{202e}'));
        assert!(output.contains("Some matching evidence may be unavailable."));
        assert!(!output.contains("candidate limit"));
    }
}
