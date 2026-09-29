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
    let bytes = prepare::read_bounded(fs, path, MAX_ARTIFACT_BYTES + 16384)?
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
    record_bytes(
        CanonicalRecord::new(fields)?,
        format!("# Agent research\n\n```{FENCE}\n{json}\n```\n").as_bytes(),
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
