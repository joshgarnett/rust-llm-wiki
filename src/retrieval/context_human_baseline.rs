//! Frozen d6bbb8a canonical renderer for differential fixtures.
use super::*;

pub(super) fn render_before(
    reader: &dyn QueryCatalog,
    scope: ContextScope,
    passages: &[ContextPassage],
    bundles: &[EvidenceBundle],
    navigation: &[NavigationEdge],
) -> Result<(String, usize)> {
    #[cfg(test)]
    CONTEXT_RENDER_COUNTS.with(|counter| -> Result<()> {
        if let Some(mut state) = counter.get() {
            if state.0.calls == state.1 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "experimental context render cap",
                ));
            }
            state.0.calls += 1;
            counter.set(Some(state));
        }
        Ok(())
    })?;
    let mut text = match scope {
        ContextScope::Snapshot => "[context index_snapshot; unverified]\n\n".to_owned(),
        ContextScope::IndexedEvidence => format!(
            "[context indexed_evidence; discovery generation {}; captured sources only; selected canonical bytes verified; global membership, identity uniqueness and completeness not verified]\n\n",
            reader.snapshot().generation
        ),
        ContextScope::IndexedDocuments => format!(
            "[context indexed_documents; discovery generation {}; selected document dependencies verified; global membership, identity uniqueness and completeness not verified]\n\n",
            reader.snapshot().generation
        ),
        _ => String::new(),
    };
    let mut graph_bytes = 0;
    for (i, p) in passages.iter().enumerate() {
        let start = text.len();
        text.push_str(&format!(
            "[passage {i}; {:?}; {:?}; path {}; bytes {}..{}]\n",
            p.label,
            p.eligibility,
            p.locator.path,
            p.span.start(),
            p.span.end()
        ));
        if let Some(r) = &p.locator.record {
            text.push_str(&format!("Record: {} ({})\n", r.record_id, r.expected_kind))
        }
        for c in &p.citations {
            let state = citation_state(reader, c)?;
            text.push_str(&format!(
                "Citation ({state:?}): {}\n",
                serde_json::to_string(c).map_err(|e| WikiError::invalid(e.to_string()))?
            ))
        }
        for link in &p.source_links {
            text.push_str(&format!(
                "Source: {} ({})\n",
                link.markdown,
                link.status.label()
            ));
        }
        for c in &p.contributors {
            text.push_str(&format!(
                "Contributor: {}\n",
                serde_json::to_string(c).map_err(|e| WikiError::invalid(e.to_string()))?
            ))
        }
        text.push_str(&p.text);
        text.push_str("\n\n");
        if !p.contributors.is_empty() {
            graph_bytes += text.len() - start
        }
    }
    for b in bundles {
        let start = text.len();
        let path_labels = b
            .path
            .iter()
            .map(|step| -> Result<_> {
                let row = reader
                    .record(&step.assertion.record_id)?
                    .ok_or_else(|| WikiError::invalid("assertion path record missing"))?;
                Ok(format!(
                    "{}: {:?}, status {:?}",
                    step.assertion.record_id, row.eligibility, row.authored_status
                ))
            })
            .collect::<Result<Vec<_>>>()?
            .join("; ");
        text.push_str(&format!("[assertion {}; {:?}; status {:?}; disputed {}]\n{} {} {}\nQualifiers: {}\nPath: {}\nPath states: {}\nPassages: {:?}; omitted support {}; omitted contradiction {}\n\n",b.assertion.record_id,b.eligibility,b.authored_status,b.disputed,b.subject.record_id,b.predicate,serde_json::to_string(&b.object).map_err(|e|WikiError::invalid(e.to_string()))?,serde_json::to_string(&b.qualifiers).map_err(|e|WikiError::invalid(e.to_string()))?,serde_json::to_string(&b.path).map_err(|e|WikiError::invalid(e.to_string()))?,path_labels,b.passage_indices,b.omitted_support,b.omitted_contradictions));
        graph_bytes += text.len() - start
    }
    for edge in navigation {
        let start = text.len();
        text.push_str(&format!(
            "[navigation; {:?}; no asserted relationship]\nFrom: {} ({})\nTo: {} ({})\n\n",
            edge.reason,
            edge.from.path,
            edge.from
                .record
                .as_ref()
                .expect("validated navigation")
                .record_id,
            edge.to.path,
            edge.to
                .record
                .as_ref()
                .expect("validated navigation")
                .record_id,
        ));
        graph_bytes += text.len() - start;
    }
    #[cfg(test)]
    CONTEXT_RENDER_COUNTS.with(|counter| {
        if let Some(mut state) = counter.get() {
            state.0.bytes += text.len() as u64;
            counter.set(Some(state));
        }
    });
    Ok((text, graph_bytes))
}
