//! Bounded traversal of recorded propositions and separately labelled navigation.
use super::{rank, types::*};
use crate::{
    catalog::{ReaderSnapshot, RecordRow, SnapshotVerification},
    domain::*,
    retrieval::{RankContribution, filters},
    sources::EvidenceStance,
};
use rusqlite::{params_from_iter, types::Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(crate) fn reference(reader: &ReaderSnapshot, row: &RecordRow) -> RecordRef {
    RecordRef {
        vault_id: reader.projection().vault_id.clone(),
        record_id: row.record.id().clone(),
        expected_kind: row.record.kind(),
    }
}
pub(crate) fn locator(reader: &ReaderSnapshot, row: &RecordRow) -> DocumentLocator {
    DocumentLocator {
        record: Some(reference(reader, row)),
        path: row.path.clone(),
        observed_hash: row.hash.clone(),
    }
}
pub(crate) fn policy(plan: &GraphPlan, kind: RecordKind) -> String {
    match kind {
        RecordKind::Entity if !plan.filters.include_historical => {
            "json_extract(r.row_json,'$.identity_eligibility')='current'".into()
        }
        RecordKind::Assertion if !plan.filters.include_historical => format!(
            "(r.eligibility='current'{})",
            if plan.filters.include_proposed {
                " OR r.authored_status='proposed'"
            } else {
                ""
            }
        ),
        RecordKind::Assertion => if plan.filters.include_proposed {
            "1"
        } else {
            "r.authored_status<>'proposed'"
        }
        .into(),
        _ => "1".into(),
    }
}
pub(crate) fn filtered_ids(
    reader: &ReaderSnapshot,
    plan: &GraphPlan,
    kind: Option<RecordKind>,
) -> Result<BTreeSet<RecordId>> {
    let mut values = vec![Value::Integer(
        i64::try_from(reader.snapshot().generation)
            .map_err(|_| WikiError::invalid("generation exceeds SQL range"))?,
    )];
    let common = filters::sql(&plan.filters, &mut values);
    let extra = kind.map_or_else(
        || "1".into(),
        |kind| format!("r.kind='{}' AND ({})", kind, policy(plan, kind)),
    );
    let sql = format!(
        "SELECT r.id FROM records r JOIN documents d ON d.gen=r.gen AND d.record_id=r.id WHERE r.gen=?1 AND ({common}) AND ({extra}) ORDER BY r.id"
    );
    let mut statement = reader.connection().prepare(&sql).map_err(sql_error)?;
    statement
        .query_map(params_from_iter(values), |r| r.get::<_, String>(0))
        .map_err(sql_error)?
        .map(|row| RecordId::new(row.map_err(sql_error)?))
        .collect()
}
pub(crate) fn sql_error(error: rusqlite::Error) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, format!("graph SQL: {error}"))
}
fn strings(row: &RecordRow, key: &str) -> Vec<String> {
    row.record
        .field(key)
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}
pub(crate) fn entity(reader: &ReaderSnapshot, row: &RecordRow, plan: &GraphPlan) -> GraphEntity {
    let graph = reader
        .projection()
        .graph
        .iter()
        .find(|g| &g.target_id == row.record.id());
    let description = if row.description_eligibility == Some(Eligibility::Current)
        || plan.filters.include_historical
    {
        row.record
            .string("description")
            .map(str::to_owned)
            .or_else(|| graph.map(|g| g.description.clone()))
            .filter(|text| !text.is_empty())
            .or_else(|| {
                if !plan.filters.include_historical {
                    return None;
                }
                reader
                    .projection()
                    .documents
                    .iter()
                    .find(|d| d.path == row.path)
                    .map(|d| {
                        String::from_utf8_lossy(
                            crate::records::parse_note(d.raw_text.as_bytes()).body(),
                        )
                        .into_owned()
                    })
                    .filter(|text| !text.is_empty())
            })
    } else {
        None
    };
    let mut description_truncated = false;
    let description = description.map(|mut text| {
        if text.len() > plan.limits.excerpt_bytes {
            let mut end = plan.limits.excerpt_bytes;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            description_truncated = true;
        }
        text
    });
    GraphEntity {
        record_ref: reference(reader, row),
        locator: locator(reader, row),
        title: row.record.title().into(),
        entity_type: row
            .record
            .string("wiki_entity_type")
            .unwrap_or_default()
            .into(),
        aliases: strings(row, "aliases"),
        eligibility: row.eligibility,
        identity_eligibility: row.identity_eligibility,
        description_eligibility: row.description_eligibility,
        description,
        description_truncated,
    }
}
pub(crate) fn proposition(
    reader: &ReaderSnapshot,
    row: &RecordRow,
) -> Option<(RecordRef, String, GraphObject, GraphQualifiers)> {
    let subject = RecordId::new(row.record.string("wiki_subject_id")?).ok()?;
    let subject = reader
        .projection()
        .records
        .get(&subject)
        .filter(|r| r.record.kind() == RecordKind::Entity)?;
    let object = if let Some(id) = row.record.string("wiki_object_id") {
        let id = RecordId::new(id).ok()?;
        GraphObject::Entity {
            record_ref: reference(
                reader,
                reader
                    .projection()
                    .records
                    .get(&id)
                    .filter(|r| r.record.kind() == RecordKind::Entity)?,
            ),
        }
    } else {
        GraphObject::Literal {
            literal_type: row.record.string("wiki_literal_type")?.into(),
            value: row.record.string("wiki_literal_value")?.into(),
        }
    };
    Some((
        reference(reader, subject),
        row.record.string("wiki_predicate")?.into(),
        object,
        GraphQualifiers {
            negated: row
                .record
                .field("wiki_negated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            modality: row
                .record
                .string("wiki_modality")
                .unwrap_or("asserted")
                .into(),
            property: row.record.string("wiki_property").map(str::to_owned),
            unit: row.record.string("wiki_unit").map(str::to_owned),
            valid_from: row.record.string("wiki_valid_from").map(str::to_owned),
            valid_until: row.record.string("wiki_valid_until").map(str::to_owned),
        },
    ))
}
fn evidence(
    reader: &ReaderSnapshot,
    assertion: &RecordRow,
    plan: &GraphPlan,
) -> Result<(Vec<GraphEvidence>, Vec<GraphEvidence>, usize, usize)> {
    let mut support = Vec::new();
    let mut contradictions = Vec::new();
    let mut support_total = 0;
    let mut contradiction_total = 0;
    for row in reader.projection().records.values().filter(|r| {
        r.record.kind() == RecordKind::Evidence
            && r.record.string("wiki_assertion_id") == Some(assertion.record.id().as_str())
    }) {
        let stance = if row.record.string("wiki_stance") == Some("supports") {
            EvidenceStance::Supports
        } else {
            EvidenceStance::Contradicts
        };
        let is_support = stance == EvidenceStance::Supports;
        if is_support {
            support_total += 1;
        } else {
            contradiction_total += 1;
        }
        // Keep current refs first; historical/ineligible refs remain counted omissions
        // unless explicitly requested. None can acquire a citation by discovery alone.
        if row.eligibility != Eligibility::Current && !plan.filters.include_historical {
            continue;
        }
        let source = SourceSpanRef {
            source_id: RecordId::new(
                row.record
                    .string("wiki_source_id")
                    .expect("canonical evidence source"),
            )?,
            source_revision: RecordId::new(
                row.record
                    .string("wiki_source_revision")
                    .expect("canonical evidence revision"),
            )?,
            span: ByteSpan::new(
                row.record
                    .field("wiki_span_start")
                    .and_then(|v| v.as_u64())
                    .expect("canonical span"),
                row.record
                    .field("wiki_span_end")
                    .and_then(|v| v.as_u64())
                    .expect("canonical span"),
            )?,
            quote_hash: Blake3Hash::new(
                row.record
                    .string("wiki_quote_hash")
                    .expect("canonical quote hash"),
            )?,
        };
        let citation = if matches!(
            reader.verification(),
            SnapshotVerification::VerifiedSnapshot { .. }
        ) && matches!(
            row.eligibility,
            Eligibility::Current | Eligibility::Historical | Eligibility::Withdrawn
        ) && row.record.string("wiki_status") == Some("active")
            && assertion.record.string("wiki_status") != Some("proposed")
            && assertion.eligibility != Eligibility::Invalid
            && (assertion.eligibility == Eligibility::Current || plan.filters.include_historical)
        {
            Some(CitationRef::Assertion(EvidenceRef {
                evidence_id: row.record.id().clone(),
                assertion_id: assertion.record.id().clone(),
                source_id: source.source_id.clone(),
                source_revision: source.source_revision.clone(),
                span: source.span,
                quote_hash: source.quote_hash.clone(),
            }))
        } else {
            None
        };
        let value = GraphEvidence {
            record_ref: reference(reader, row),
            locator: locator(reader, row),
            stance,
            eligibility: row.eligibility,
            authored_status: row.authored_status.clone(),
            source,
            citation,
        };
        if is_support {
            support.push(value);
        } else {
            contradictions.push(value);
        }
    }
    let order = |a: &GraphEvidence, b: &GraphEvidence| {
        (a.eligibility != Eligibility::Current)
            .cmp(&(b.eligibility != Eligibility::Current))
            .then(a.record_ref.record_id.cmp(&b.record_ref.record_id))
    };
    support.sort_by(order);
    contradictions.sort_by(order);
    support.truncate(plan.limits.support_per_assertion);
    contradictions.truncate(plan.limits.contradictions_per_assertion);
    let omitted_support = support_total - support.len();
    let omitted_contradictions = contradiction_total - contradictions.len();
    Ok((
        support,
        contradictions,
        omitted_support,
        omitted_contradictions,
    ))
}
#[derive(Clone)]
struct Frontier {
    node: RecordId,
    seed: RecordId,
    rank: usize,
    path: Vec<GraphPathStep>,
    depth: usize,
}
pub(crate) struct Traversed {
    pub assertions: Vec<GraphAssertion>,
    pub navigation: Vec<NavigationEdge>,
    pub coverage: GraphCoverage,
}
pub(crate) fn walk(
    reader: &ReaderSnapshot,
    seeds: &[GraphSeed],
    plan: &GraphPlan,
    mut coverage: GraphCoverage,
) -> Result<Traversed> {
    let allowed = filtered_ids(reader, plan, Some(RecordKind::Assertion))?;
    let navigation_allowed = if plan.include_navigation {
        filtered_ids(reader, plan, None)?
    } else {
        BTreeSet::new()
    };
    let mut adjacency: BTreeMap<RecordId, Vec<RecordId>> = BTreeMap::new();
    for id in &allowed {
        let row = &reader.projection().records[id];
        if let Some((subject, _, object, _)) = proposition(reader, row) {
            adjacency
                .entry(subject.record_id)
                .or_default()
                .push(id.clone());
            if let GraphObject::Entity { record_ref } = object {
                adjacency
                    .entry(record_ref.record_id)
                    .or_default()
                    .push(id.clone());
            }
        }
    }
    let direct_ranks: BTreeMap<_, _> = seeds
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == RecordKind::Assertion)
        .map(|(n, s)| (s.record_ref.record_id.clone(), n + 1))
        .collect();
    for list in adjacency.values_mut() {
        list.sort_by(|a, b| {
            direct_ranks
                .get(a)
                .unwrap_or(&usize::MAX)
                .cmp(direct_ranks.get(b).unwrap_or(&usize::MAX))
                .then(a.cmp(b))
        });
        list.dedup();
    }
    let mut edges: BTreeMap<RecordId, GraphAssertion> = BTreeMap::new();
    let mut queue = VecDeque::new();
    let mut visited = BTreeSet::new();
    let mut visited_edges = BTreeSet::new();
    // Each originating seed has one admission budget across every hop and both
    // edge categories. An already global edge still consumes a new seed's slot.
    let mut seed_admissions: BTreeMap<RecordId, usize> = BTreeMap::new();
    let mut nodes = BTreeSet::new();
    let mut omitted = BTreeSet::new();
    for (rank, seed) in seeds.iter().enumerate() {
        if seed.kind == RecordKind::Assertion {
            let row = &reader.projection().records[&seed.record_ref.record_id];
            if let Some(edge) = build(
                reader,
                row,
                plan,
                seed.record_ref.record_id.clone(),
                rank + 1,
                vec![],
                0,
                true,
                TraversalDirection::Outgoing,
                seed.rank_contributions.clone(),
            )? {
                let path = edge.path.clone();
                let subject = edge.subject.record_id.clone();
                let object = edge.object.clone();
                if edges.len() < plan.limits.assertions {
                    edges.insert(row.record.id().clone(), edge);
                    visited_edges.insert((row.record.id().clone(), row.record.id().clone()));
                    seed_admissions.insert(row.record.id().clone(), 1);
                } else {
                    omitted.insert(row.record.id().clone());
                    continue;
                }
                queue.push_back(Frontier {
                    node: subject,
                    seed: row.record.id().clone(),
                    rank: rank + 1,
                    path: path.clone(),
                    depth: 1,
                });
                if let GraphObject::Entity { record_ref } = object {
                    queue.push_back(Frontier {
                        node: record_ref.record_id,
                        seed: row.record.id().clone(),
                        rank: rank + 1,
                        path,
                        depth: 1,
                    });
                }
            }
        } else {
            queue.push_back(Frontier {
                node: seed.record_ref.record_id.clone(),
                seed: seed.record_ref.record_id.clone(),
                rank: rank + 1,
                path: vec![],
                depth: 0,
            });
        }
    }
    // Each original seed contributes one candidate per round, rotating its
    // frontier nodes. More branches earn neither extra slots nor relevance.
    let mut navigation = Vec::new();
    let mut nav_seen = BTreeSet::new();
    let mut seed_navigation = BTreeSet::new();
    let mut omitted_navigation = BTreeSet::new();
    while !queue.is_empty() {
        let width = queue.len();
        let mut rounds: Vec<VecDeque<RecordId>> = Vec::new();
        let mut frontiers = Vec::new();
        for _ in 0..width {
            let frontier = queue.pop_front().expect("known frontier");
            if !visited.insert((frontier.seed.clone(), frontier.node.clone())) {
                continue;
            }
            nodes.insert(frontier.node.clone());
            if frontier.depth >= plan.limits.depth {
                if adjacency
                    .get(&frontier.node)
                    .is_some_and(|list| list.iter().any(|id| !edges.contains_key(id)))
                {
                    coverage.depth_limited = true;
                }
                if plan.include_navigation
                    && navigation_edges(
                        reader,
                        &reader.projection().records[&frontier.node],
                        &navigation_allowed,
                    )
                    .iter()
                    .any(|nav| {
                        !nav_seen.contains(&(
                            nav.from.path.clone(),
                            nav.to.path.clone(),
                            format!("{:?}", nav.reason),
                        ))
                    })
                {
                    coverage.depth_limited = true;
                }
                continue;
            }
            let list = adjacency.get(&frontier.node).cloned().unwrap_or_default();
            rounds.push(list.into_iter().collect());
            frontiers.push(frontier);
        }
        let mut seed_rounds: BTreeMap<usize, VecDeque<usize>> = BTreeMap::new();
        for (index, frontier) in frontiers.iter().enumerate() {
            if !rounds[index].is_empty() {
                seed_rounds
                    .entry(frontier.rank)
                    .or_default()
                    .push_back(index);
            }
        }
        while seed_rounds.values().any(|nodes| !nodes.is_empty()) {
            for indices in seed_rounds.values_mut() {
                while let Some(index) = indices.pop_front() {
                    let frontier = &frontiers[index];
                    let id = rounds[index].pop_front().expect("queued nonempty frontier");
                    if !rounds[index].is_empty() {
                        indices.push_back(index);
                    }
                    let visit = (frontier.seed.clone(), id.clone());
                    if visited_edges.contains(&visit) {
                        continue;
                    }
                    if seed_admissions.get(&frontier.seed).copied().unwrap_or(0)
                        >= plan.limits.incident_per_seed
                    {
                        omitted.insert(id);
                        continue;
                    }
                    let row = &reader.projection().records[&id];
                    let Some((subject, _, object, _)) = proposition(reader, row) else {
                        continue;
                    };
                    let direction = if subject.record_id == frontier.node {
                        TraversalDirection::Outgoing
                    } else {
                        TraversalDirection::Incoming
                    };
                    if !edges.contains_key(&id)
                        && edges.len() + navigation.len() >= plan.limits.assertions
                    {
                        omitted.insert(id);
                        continue;
                    }
                    let contribution = vec![RankContribution {
                        channel: "parent_seed".into(),
                        rank: frontier.rank,
                        score: None,
                    }];
                    let Some(edge) = build(
                        reader,
                        row,
                        plan,
                        frontier.seed.clone(),
                        frontier.rank,
                        frontier.path.clone(),
                        frontier.depth + 1,
                        false,
                        direction,
                        contribution,
                    )?
                    else {
                        continue;
                    };
                    visited_edges.insert(visit);
                    *seed_admissions.entry(frontier.seed.clone()).or_default() += 1;
                    let next = match object {
                        GraphObject::Entity { record_ref }
                            if subject.record_id == frontier.node =>
                        {
                            Some(record_ref.record_id)
                        }
                        _ if subject.record_id != frontier.node => Some(subject.record_id),
                        _ => None,
                    };
                    if let Some(next) = next {
                        queue.push_back(Frontier {
                            node: next,
                            seed: frontier.seed.clone(),
                            rank: frontier.rank,
                            path: edge.path.clone(),
                            depth: frontier.depth + 1,
                        });
                    }
                    if let Some(existing) = edges.get_mut(&id) {
                        let mut seed_ids = existing.seed_ids.clone();
                        if !seed_ids.contains(&frontier.seed) {
                            seed_ids.push(frontier.seed.clone());
                            seed_ids.sort();
                        }
                        if !existing.direct_seed && rank::assertion_order(&edge, existing).is_lt() {
                            *existing = edge;
                        }
                        existing.seed_ids = seed_ids;
                    } else {
                        edges.insert(id, edge);
                    }
                    break;
                }
            }
        }
        if plan.include_navigation {
            let mut nav_rounds: Vec<VecDeque<NavigationEdge>> = frontiers
                .iter()
                .map(|frontier| {
                    navigation_edges(
                        reader,
                        &reader.projection().records[&frontier.node],
                        &navigation_allowed,
                    )
                    .into()
                })
                .collect();
            let mut seed_rounds: BTreeMap<usize, VecDeque<usize>> = BTreeMap::new();
            for (index, frontier) in frontiers.iter().enumerate() {
                if !nav_rounds[index].is_empty() {
                    seed_rounds
                        .entry(frontier.rank)
                        .or_default()
                        .push_back(index);
                }
            }
            while seed_rounds.values().any(|nodes| !nodes.is_empty()) {
                for indices in seed_rounds.values_mut() {
                    while let Some(index) = indices.pop_front() {
                        let frontier = &frontiers[index];
                        let nav = nav_rounds[index]
                            .pop_front()
                            .expect("queued nonempty navigation frontier");
                        if !nav_rounds[index].is_empty() {
                            indices.push_back(index);
                        }
                        let key = (
                            nav.from.path.clone(),
                            nav.to.path.clone(),
                            format!("{:?}", nav.reason),
                        );
                        let visit = (frontier.seed.clone(), key.clone());
                        if seed_navigation.contains(&visit) {
                            continue;
                        }
                        if seed_admissions.get(&frontier.seed).copied().unwrap_or(0)
                            >= plan.limits.incident_per_seed
                            || (!nav_seen.contains(&key)
                                && edges.len() + navigation.len() >= plan.limits.assertions)
                        {
                            omitted_navigation.insert(key);
                            continue;
                        }
                        seed_navigation.insert(visit);
                        *seed_admissions.entry(frontier.seed.clone()).or_default() += 1;
                        let target = if nav
                            .to
                            .record
                            .as_ref()
                            .is_some_and(|r| r.record_id == frontier.node)
                        {
                            nav.from.record.as_ref()
                        } else {
                            nav.to.record.as_ref()
                        }
                        .map(|r| r.record_id.clone());
                        if nav_seen.insert(key) {
                            navigation.push(nav);
                        }
                        if let Some(target) = target {
                            queue.push_back(Frontier {
                                node: target,
                                seed: frontier.seed.clone(),
                                rank: frontier.rank,
                                path: frontier.path.clone(),
                                depth: frontier.depth + 1,
                            });
                        }
                        break;
                    }
                }
            }
        }
    }
    omitted.retain(|id| !edges.contains_key(id));
    omitted_navigation.retain(|key| !nav_seen.contains(key));
    coverage.omitted_navigation = omitted_navigation.len();
    coverage.omitted_incident_assertions = omitted.len();
    coverage.omitted_assertions = omitted.len();
    coverage.visited_entities = nodes
        .iter()
        .filter(|id| {
            reader
                .projection()
                .records
                .get(*id)
                .is_some_and(|r| r.record.kind() == RecordKind::Entity)
        })
        .count();
    coverage.visited_assertions = edges.len();
    let mut assertions: Vec<_> = edges.into_values().collect();
    assertions.sort_by(rank::assertion_order);
    navigation.sort_by(|a, b| {
        (&a.from.path, &a.to.path, format!("{:?}", a.reason)).cmp(&(
            &b.from.path,
            &b.to.path,
            format!("{:?}", b.reason),
        ))
    });
    Ok(Traversed {
        assertions,
        navigation,
        coverage,
    })
}
#[allow(clippy::too_many_arguments)]
fn build(
    reader: &ReaderSnapshot,
    row: &RecordRow,
    plan: &GraphPlan,
    seed: RecordId,
    parent_rank: usize,
    mut path: Vec<GraphPathStep>,
    hop: usize,
    direct: bool,
    direction: TraversalDirection,
    contributions: Vec<RankContribution>,
) -> Result<Option<GraphAssertion>> {
    let Some((subject, predicate, object, qualifiers)) = proposition(reader, row) else {
        return Ok(None);
    };
    path.push(GraphPathStep {
        assertion: reference(reader, row),
        subject: subject.clone(),
        predicate: predicate.clone(),
        object: object.clone(),
        qualifiers: qualifiers.clone(),
        traversal: direction,
    });
    let (support, contradictions, omitted_support, omitted_contradictions) =
        evidence(reader, row, plan)?;
    Ok(Some(GraphAssertion {
        record_ref: reference(reader, row),
        locator: locator(reader, row),
        title: row.record.title().into(),
        subject,
        predicate,
        object,
        qualifiers,
        authored_status: row.authored_status.clone(),
        eligibility: row.eligibility,
        disputed: row.disputed,
        seed_ids: vec![seed],
        path,
        hop,
        direct_seed: direct,
        direct_seed_rank: direct.then_some(parent_rank),
        rrf_score: rank::rrf(&contributions),
        rank_contributions: contributions,
        support,
        contradictions,
        omitted_support,
        omitted_contradictions,
    }))
}
fn navigation_edges(
    reader: &ReaderSnapshot,
    row: &RecordRow,
    allowed: &BTreeSet<RecordId>,
) -> Vec<NavigationEdge> {
    let mut edges = Vec::new();
    let from = locator(reader, row);
    for link in reader.projection().links.iter().filter(|l| {
        l.target_id.is_some()
            && (l.from_path == row.path || l.target_id.as_ref() == Some(row.record.id()))
    }) {
        let (Some(target), Some(source)) = (
            link.target_id
                .as_ref()
                .and_then(|id| reader.projection().records.get(id)),
            reader
                .projection()
                .records
                .values()
                .find(|r| r.path == link.from_path),
        ) else {
            continue;
        };
        let other = if source.record.id() == row.record.id() {
            target
        } else {
            source
        };
        if !allowed.contains(other.record.id()) {
            continue;
        }
        edges.push(NavigationEdge {
            from: locator(reader, source),
            to: locator(reader, target),
            reason: NavigationReason::PageLink,
        });
    }
    for (key, value) in row.record.fields() {
        if key == "wiki_id" || !key.starts_with("wiki_") {
            continue;
        }
        if !(key.ends_with("_id")
            || key.ends_with("_ids")
            || matches!(
                key.as_str(),
                "wiki_current_revision" | "wiki_revisions" | "wiki_source_revision"
            ))
        {
            continue;
        }
        let values: Vec<_> = value
            .as_str()
            .into_iter()
            .chain(
                value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str()),
            )
            .collect();
        for value in values {
            let Some(target) = RecordId::new(value)
                .ok()
                .and_then(|id| reader.projection().records.get(&id))
                .filter(|r| allowed.contains(r.record.id()))
            else {
                continue;
            };
            if matches!(key.as_str(), "wiki_subject_id" | "wiki_object_id") {
                continue;
            }
            edges.push(NavigationEdge {
                from: from.clone(),
                to: locator(reader, target),
                reason: NavigationReason::Provenance,
            });
        }
    }
    edges
}
