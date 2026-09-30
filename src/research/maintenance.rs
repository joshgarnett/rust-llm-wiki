//! Read-only host repair queue. It proposes work, never validates entailment or publishes pages.
use super::{engine, storage};
use crate::{app::OfflineApp, catalog::Catalog, domain::*};
use serde::Serialize;
use std::collections::BTreeSet;

const MAX_TASKS: usize = 128;

#[derive(Serialize)]
struct RepairTask {
    kind: &'static str,
    record_id: Option<RecordId>,
    title: String,
    path: Option<VaultRelativePath>,
    reason: &'static str,
    action: &'static str,
}

pub fn maintenance(app: &OfflineApp, run: &RecordId) -> Result<serde_json::Value> {
    let (head, _) = storage::load_head(app.fs(), app.vault_id(), run)?;
    let report = head
        .report
        .as_ref()
        .map(|_| engine::report(app, run))
        .transpose()?;
    let report_view = head
        .report
        .as_ref()
        .map(|_| engine::report_view(app, run))
        .transpose()?;
    let packet = head
        .packet
        .as_ref()
        .map(|_| storage::load_packet(app.fs(), &head))
        .transpose()?;
    let mut source_ids: BTreeSet<RecordId> = head.scope.source_ids.iter().cloned().collect();
    let mut captured = 0usize;
    for reference in &head.receipts {
        let (receipt, _): (super::types::ImportReceipt, _) = storage::read(
            app.fs(),
            &reference.artifact.path,
            Some(&reference.artifact.hash),
            &head.run_id,
            RecordKind::RunEvent,
        )?;
        if receipt.run_id != head.run_id
            || receipt.packet_fingerprint != reference.packet
            || receipt.submission_hash != reference.submission
            || Blake3Hash::digest(super::codec::encode(&receipt.submission)?)
                != reference.submission
        {
            return Err(super::codec::invalid("research receipt binding differs"));
        }
        captured += receipt.captured_sources.len();
        source_ids.extend(
            receipt
                .captured_sources
                .into_iter()
                .map(|source| source.source_id),
        );
    }
    if captured != head.captured_sources as usize {
        return Err(super::codec::invalid(
            "research capture counters differ from retained receipts",
        ));
    }
    for citation in report
        .iter()
        .flat_map(|r| &r.claims)
        .flat_map(|c| &c.citations)
        .chain(packet.iter().flat_map(|p| &p.passages).map(|p| &p.citation))
    {
        match citation {
            CitationRef::Source(value) => {
                source_ids.insert(value.source_id.clone());
            }
            CitationRef::Assertion(value) => {
                source_ids.insert(value.source_id.clone());
            }
        }
    }
    let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
    let snapshot = catalog.canonical_snapshot()?;
    let projection = snapshot.projection();
    let mut assertion_ids = BTreeSet::new();
    let mut evidence_ids = BTreeSet::new();
    for row in projection.records.values() {
        if row.record.kind() == RecordKind::Evidence
            && row
                .record
                .string("wiki_source_id")
                .and_then(|id| RecordId::new(id).ok())
                .is_some_and(|id| source_ids.contains(&id))
            && let Some(id) = row.record.string("wiki_assertion_id")
        {
            assertion_ids.insert(RecordId::new(id)?);
            evidence_ids.insert(row.record.id().clone());
        }
    }
    let mut tasks = Vec::new();
    if let Some(view) = &report_view {
        if view["citation_freshness"]["state"] != "current" {
            tasks.push(RepairTask {
                kind: "revalidate_report_citations", record_id: Some(run.clone()),
                title: report.as_ref().map(|r| r.question.clone()).unwrap_or_default(),
                path: head.report.as_ref().map(|r| r.path.clone()),
                reason: "retained_report_has_noncurrent_citations",
                action: "Inspect changed sources, start a new research run, and review a new cited answer before updating pages.",
            });
        }
        if report.as_ref().is_some_and(|r| !r.claims.is_empty()) {
            tasks.push(RepairTask {
                kind: "review_report_for_synthesis", record_id: Some(run.clone()),
                title: report.as_ref().map(|r| r.question.clone()).unwrap_or_default(),
                path: head.report.as_ref().map(|r| r.path.clone()),
                reason: "unassessed_claims_require_explicit_synthesis_review",
                action: "Assess each cited claim, then prepare a guarded page or graph update with explicit support and author review.",
            });
        }
    }
    if let Some(packet) = &packet
        && super::inspection::verify(app.fs(), packet).is_err()
    {
        tasks.push(RepairTask {
            kind: "refresh_research_packet",
            record_id: Some(run.clone()),
            title: head.scope.question.clone(),
            path: head.packet.as_ref().map(|p| p.path.clone()),
            reason: "outstanding_packet_stale",
            action: "Run research resume RUN --refresh, then use the new packet fingerprint.",
        });
    }
    for row in projection.records.values() {
        if row.record.kind() == RecordKind::Assertion
            && assertion_ids.contains(row.record.id())
            && row.disputed
        {
            tasks.push(RepairTask {
                kind: "review_contradiction", record_id: Some(row.record.id().clone()),
                title: row.record.title().into(), path: Some(row.path.clone()),
                reason: "current_assertion_disputed",
                action: "Inspect opposing assertions and evidence; submit an explicit review decision or leave the dispute visible.",
            });
        }
        if row.record.kind() == RecordKind::Assertion
            && assertion_ids.contains(row.record.id())
            && projection.diagnostics.iter().any(|d| {
                d.record_id.as_ref() == Some(row.record.id())
                    && d.details["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.starts_with("evidence_link_"))
            })
        {
            tasks.push(RepairTask {
                kind: "repair_evidence_navigation", record_id: Some(row.record.id().clone()),
                title: row.record.title().into(), path: Some(row.path.clone()),
                reason: "wiki_evidence_link_invalid",
                action: "Repair the assertion's wiki_evidence link; validate authoritative evidence fields separately before accepting support.",
            });
        }
        if row.record.kind() != RecordKind::Page
            || row.authored_status.as_deref() != Some("reviewed")
        {
            continue;
        }
        let dependencies: BTreeSet<_> = row
            .record
            .field("wiki_depends_on_ids")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .filter_map(|id| RecordId::new(id).ok())
            .collect();
        let relevant = dependencies.iter().any(|id| assertion_ids.contains(id));
        if relevant && row.eligibility != Eligibility::Current {
            tasks.push(RepairTask {
                kind: "repair_page_support", record_id: Some(row.record.id().clone()),
                title: row.record.title().into(), path: Some(row.path.clone()),
                reason: "reviewed_page_has_noncurrent_authoritative_dependency",
                action: "Inspect the affected assertions and current source evidence; prepare a guarded successor page with corrected dependencies.",
            });
        }
        if relevant
            || projection.links.iter().any(|link| {
                link.from_path == row.path
                    && link.target_id.as_ref().is_some_and(|id| {
                        source_ids.contains(id)
                            || assertion_ids.contains(id)
                            || evidence_ids.contains(id)
                    })
            })
        {
            for link in projection
                .links
                .iter()
                .filter(|link| link.from_path == row.path)
            {
                if link.resolution == "Missing"
                    || link.resolution.starts_with("Ambiguous")
                    || link.resolution.starts_with("WrongKind")
                    || link.resolution.starts_with("CompanionConflict")
                {
                    tasks.push(RepairTask {
                        kind: "repair_navigation", record_id: Some(row.record.id().clone()),
                        title: row.record.title().into(), path: Some(row.path.clone()),
                        reason: "reviewed_page_has_broken_navigation",
                        action: "Repair the link target or label in a guarded page update; navigation links are not evidence authority.",
                    });
                    break;
                }
            }
        }
    }
    let omitted = tasks.len().saturating_sub(MAX_TASKS);
    tasks.truncate(MAX_TASKS);
    Ok(serde_json::json!({
        "run_id":run,
        "freshness":"read_time_current_catalog",
        "network_used":false,
        "tasks":tasks,
        "omitted_tasks":omitted,
        "publication":"host_review_required",
    }))
}
