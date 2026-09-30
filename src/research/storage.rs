use super::{codec::*, types::*};
use crate::{
    changes::*,
    domain::*,
    records::parse_note,
    sources::revision::{common, record_bytes, timestamp},
    vault::{ExpectedState, VaultFs},
};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;

const FENCE: &str = "lwiki-agent-research-v1";
pub(crate) fn head_path(run: &RecordId) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("runs/{run}/research.md"))
}
pub(crate) fn read<T: DeserializeOwned>(
    fs: &VaultFs,
    path: &VaultRelativePath,
    expected: Option<&Blake3Hash>,
    run: &RecordId,
    kind: RecordKind,
) -> Result<(T, Blake3Hash)> {
    let bytes = prepare::read_bounded(fs, path, MAX_ARTIFACT_BYTES + 65536)?
        .ok_or_else(|| WikiError::new(ErrorCode::RecordNotFound, "research artifact is missing"))?;
    let hash = Blake3Hash::digest(&bytes);
    if expected.is_some_and(|e| e != &hash) {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "research artifact changed",
        ));
    }
    let note = parse_note(&bytes);
    let record = note
        .canonical
        .as_ref()
        .filter(|r| r.kind() == kind)
        .ok_or_else(|| invalid("invalid research record envelope"))?;
    if (kind == RecordKind::Run && record.id() != run)
        || (kind == RecordKind::RunEvent && record.string("wiki_run_id") != Some(run.as_str()))
    {
        return Err(invalid("research artifact ownership differs"));
    }
    let json = crate::graph::packet::fenced_json(&note, FENCE, MAX_ARTIFACT_BYTES)?;
    Ok((decode(json, MAX_ARTIFACT_BYTES)?, hash))
}
pub(crate) fn load_head(
    fs: &VaultFs,
    vault: &RecordId,
    run: &RecordId,
) -> Result<(ResearchHead, Blake3Hash)> {
    let (head, hash): (ResearchHead, _) = read(fs, &head_path(run)?, None, run, RecordKind::Run)?;
    scope(&head.scope)?;
    if head.schema != "lwiki.research-head.v1"
        || &head.vault_id != vault
        || &head.run_id != run
        || Blake3Hash::digest(encode(&head.scope)?) != head.scope_hash
        || head.generation > 64
        || head.round == 0
        || head.round > head.scope.max_rounds
        || head.receipts.len() > 16
        || head
            .receipts
            .iter()
            .map(|r| &r.packet)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != head.receipts.len()
        || head.captured_sources > head.scope.max_sources
        || head.captured_bytes > head.scope.max_source_bytes
        || head.gaps.len() > 64
        || (head.packet.is_none() && head.report.is_none())
    {
        return Err(invalid("invalid research head binding or counters"));
    }
    for artifact in head
        .packet
        .iter()
        .chain(&head.report)
        .chain(head.receipts.iter().map(|r| &r.artifact))
    {
        if !artifact
            .path
            .as_str()
            .starts_with(&format!("runs/{run}/outputs/"))
        {
            return Err(invalid("research artifact path is outside its run"));
        }
    }
    Ok((head, hash))
}
pub(crate) fn load_packet(fs: &VaultFs, head: &ResearchHead) -> Result<ResearchPacket> {
    let reference = head
        .packet
        .as_ref()
        .ok_or_else(|| invalid("research has no outstanding packet"))?;
    let (packet, _): (ResearchPacket, _) = read(
        fs,
        &reference.path,
        Some(&reference.hash),
        &head.run_id,
        RecordKind::RunEvent,
    )?;
    if packet.schema != "lwiki.research-packet.v1"
        || packet.vault_id != head.vault_id
        || packet.run_id != head.run_id
        || packet.scope_hash != head.scope_hash
        || packet.source_ranges_retired != head.source_ranges_retired
        || packet.generation != head.generation
        || packet.round != head.round
        || packet.remaining_sources != head.scope.max_sources - head.captured_sources
        || packet.remaining_source_bytes != head.scope.max_source_bytes - head.captured_bytes
        || packet.scope_hash != Blake3Hash::digest(encode(&packet.scope)?)
        || packet.packet_fingerprint != fingerprint(&packet)?
        || packet.passages.len() > MAX_PASSAGES
    {
        return Err(invalid("research packet binding or fingerprint differs"));
    }
    Ok(packet)
}
fn note<T: Serialize>(
    id: &RecordId,
    run: &RecordId,
    kind: RecordKind,
    sequence: u32,
    value: &T,
    status: &str,
    readable: Option<&str>,
) -> Result<Vec<u8>> {
    let mut fields = common(id, kind, "Agent research");
    if kind == RecordKind::Run {
        fields.insert("wiki_status".into(), status.into());
        fields.insert("wiki_created_at".into(), timestamp()?.into());
    } else {
        fields.insert("wiki_run_id".into(), run.as_str().into());
        fields.insert("wiki_sequence".into(), u64::from(sequence).into());
        fields.insert("wiki_event_type".into(), "agent_research".into());
        fields.insert("wiki_occurred_at".into(), timestamp()?.into());
    }
    let json = String::from_utf8(encode(value)?).map_err(|_| invalid("research JSON UTF-8"))?;
    // JSON escapes embedded newlines, so submitted backticks cannot close this fence.
    let heading = readable.unwrap_or("# Agent research\n");
    record_bytes(
        CanonicalRecord::new(fields)?,
        format!("{heading}\n```{FENCE}\n{json}\n```\n").as_bytes(),
    )
}
pub(crate) fn artifact<T: Serialize>(
    head: &ResearchHead,
    label: &str,
    value: &T,
) -> Result<(ArtifactRef, ExpectedWrite)> {
    let digest = Blake3Hash::digest(encode(&(
        head.run_id.clone(),
        head.generation,
        label,
        value,
    ))?);
    let id = RecordId::new(format!(
        "run_event_research_{}",
        digest.as_str().trim_start_matches("blake3:")
    ))?;
    let path = VaultRelativePath::new(format!("runs/{}/outputs/{label}_{id}.md", head.run_id))?;
    let bytes = note(
        &id,
        &head.run_id,
        RecordKind::RunEvent,
        head.generation,
        value,
        "paused",
        None,
    )?;
    Ok((
        ArtifactRef {
            path: path.clone(),
            hash: Blake3Hash::digest(&bytes),
        },
        ExpectedWrite {
            target: path,
            expected: ExpectedState::Absent,
            proposed: Some(bytes),
            apply_after: vec![],
        },
    ))
}
/// A bounded prose projection accompanies, but never replaces, the immutable JSON fence.
pub(crate) fn report_artifact(
    fs: &VaultFs,
    head: &ResearchHead,
    report: &ResearchReport,
) -> Result<(ArtifactRef, ExpectedWrite)> {
    let (reference, mut operation) = artifact(head, "report", report)?;
    let prefix = format!("runs/{}/outputs/report_", head.run_id);
    let id = RecordId::new(
        reference
            .path
            .as_str()
            .strip_prefix(&prefix)
            .and_then(|name| name.strip_suffix(".md"))
            .ok_or_else(|| invalid("research report artifact ID missing"))?,
    )?;
    let readable = readable_report(fs, report)?;
    let bytes = note(
        &id,
        &head.run_id,
        RecordKind::RunEvent,
        head.generation,
        report,
        "paused",
        Some(&readable),
    )?;
    if bytes.len() > MAX_ARTIFACT_BYTES + 65536 {
        return Err(invalid(
            "readable research report exceeds retained note bound",
        ));
    }
    operation.proposed = Some(bytes.clone());
    Ok((
        ArtifactRef {
            path: reference.path,
            hash: Blake3Hash::digest(&bytes),
        },
        operation,
    ))
}

fn display(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let clipped = &text[..end];
    let mut out = String::with_capacity(clipped.len());
    for ch in clipped.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '`' => out.push_str("&#96;"),
            '[' => out.push_str("&#91;"),
            ']' => out.push_str("&#93;"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    if end < text.len() {
        out.push_str(" … [display shortened; complete text in JSON below]");
    }
    out
}

fn readable_report(fs: &VaultFs, report: &ResearchReport) -> Result<String> {
    let mut out = format!(
        "# Research report\n\n**Question:** {}\n\n**Completion:** {} ({})\n\n**Freshness:** retained history; check current citations before using this report.\n\n**Claim assessment:** unassessed. Citation validity does not establish truth.\n\n## Claims\n",
        display(&report.question, 4096),
        if report.partial {
            "partial"
        } else {
            "complete"
        },
        display(
            report.completion_reason.as_deref().unwrap_or("unspecified"),
            64
        ),
    );
    if report.claims.is_empty() {
        out.push_str("\nNo claims were submitted.\n");
    }
    for (index, claim) in report.claims.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. **Unassessed:** {}\n",
            index + 1,
            display(&claim.text, 512)
        ));
        for citation in claim.citations.iter().take(4) {
            let source = match citation {
                CitationRef::Source(value) => value,
                CitationRef::Assertion(value) => {
                    out.push_str(&format!(
                        "   - Assertion evidence `{}`.\n",
                        value.assertion_id
                    ));
                    let path = format!(
                        "sources/{}/revisions/{}/revision.md",
                        value.source_id, value.source_revision
                    );
                    let date = acquisition_date(fs, &path)?;
                    out.push_str(&format!("     - [[{path}|Source revision]]{}\n", date));
                    continue;
                }
            };
            let path = format!(
                "sources/{}/revisions/{}/revision.md",
                source.source_id, source.source_revision
            );
            let date = acquisition_date(fs, &path)?;
            out.push_str(&format!(
                "   - [[{path}|Source revision]] · UTF-8 bytes {}–{}{}\n",
                source.span.start(),
                source.span.end(),
                date
            ));
        }
        if claim.citations.len() > 4 {
            out.push_str(&format!(
                "   - {} more citations are in the JSON record.\n",
                claim.citations.len() - 4
            ));
        }
    }
    out.push_str("\n## Unresolved gaps\n");
    if report.gaps.is_empty() {
        out.push_str("\nNone reported.\n");
    }
    for gap in &report.gaps {
        out.push_str(&format!("\n- {}", display(gap, 128)));
    }
    out.push_str(
        "\n\nThe JSON record below is authoritative for exact claims, citations and identity.\n",
    );
    if out.len() > 48 * 1024 {
        return Err(invalid("research report readable view exceeds byte bound"));
    }
    Ok(out)
}

fn acquisition_date(fs: &VaultFs, path: &str) -> Result<String> {
    let path = VaultRelativePath::new(path)?;
    let Some(bytes) = prepare::read_bounded(fs, &path, 64 * 1024)? else {
        return Ok(String::new());
    };
    let parsed = parse_note(&bytes);
    let Some(record) = parsed.canonical else {
        return Ok(String::new());
    };
    if record.kind() != RecordKind::Revision {
        return Ok(String::new());
    }
    if let Some(claimed) = record.string("origin_retrieved_at") {
        return Ok(format!(
            " · Host-claimed retrieval {}",
            display(claimed, 64)
        ));
    }
    Ok(record
        .string("wiki_captured_at")
        .map(|captured| format!(" · Captured {}", display(captured, 64)))
        .unwrap_or_default())
}
pub(crate) fn head_write(
    head: &ResearchHead,
    expected: ExpectedState,
    after: Vec<VaultRelativePath>,
) -> Result<ExpectedWrite> {
    Ok(ExpectedWrite {
        target: head_path(&head.run_id)?,
        expected,
        proposed: Some(note(
            &head.run_id,
            &head.run_id,
            RecordKind::Run,
            head.generation,
            head,
            if head.packet.is_some() {
                "paused"
            } else {
                "completed"
            },
            None,
        )?),
        apply_after: after,
    })
}
pub(crate) fn draft(
    operations: Vec<ExpectedWrite>,
    dependencies: Vec<ReadDependency>,
) -> ChangeDraft {
    ChangeDraft {
        title: "Agent research handoff".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: dependencies,
        operations,
    }
}
