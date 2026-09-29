//! Bounded Brave discovery. A search lead is never a verified source passage.
use super::{types::*, wire, wire_json};
use crate::{config::providers::TrustedService, domain::*, graph::packet::canonical_json, jobs::*};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchLead {
    pub url: String,
    pub title: String,
    pub snippet: String,
    pub rank: u32,
}
pub(super) struct SearchContract {
    pub count: u8,
    pub page: u8,
}
pub fn validate(query: &str, count: u8, page: u8) -> Result<()> {
    if query.trim().is_empty()
        || query.chars().count() > 600
        || query.split_whitespace().count() > 75
        || query.chars().any(char::is_control)
        || !(1..=20).contains(&count)
        || page > 9
    {
        return Err(WikiError::invalid(
            "search query, count, or page exceeds contract",
        ));
    }
    Ok(())
}
pub(super) fn request_url(service: &TrustedService, input: &RemoteInput) -> Result<String> {
    let RemoteOperation::Search { query, count, page } = &input.operation else {
        return Err(WikiError::invalid("search input required"));
    };
    validate(query, *count, *page)?;
    let mut url = url::Url::parse(&service.service().url)
        .map_err(|_| WikiError::invalid("search endpoint invalid"))?;
    if url
        .query_pairs()
        .any(|(key, _)| matches!(key.as_ref(), "q" | "count" | "offset"))
    {
        return Err(WikiError::invalid(
            "search endpoint shadows adapter parameters",
        ));
    }
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("count", &count.to_string())
        .append_pair("offset", &page.to_string());
    Ok(url.into())
}
pub(super) fn prepare(
    service: &TrustedService,
    task: &TaskSpec,
    input: &RemoteInput,
    purpose: DispatchPurpose,
) -> Result<PreparedWire> {
    wire::verify_task(service, task, input)?;
    let RemoteOperation::Search { count, page, .. } = input.operation else {
        return Err(WikiError::invalid("search input required"));
    };
    let capability = match purpose {
        DispatchPurpose::Task => Capability::Search,
        DispatchPurpose::Probe {
            role: ServiceRole::Search,
        } => Capability::Probe,
        _ => return Err(WikiError::invalid("search purpose differs")),
    };
    if input.version != 1
        || task.capability != Some(capability)
        || service.summary().capability != Capability::Search
    {
        return Err(WikiError::invalid("search service or task differs"));
    }
    let url = request_url(service, input)?;
    let mut headers = service.service().headers.clone();
    headers.insert("Accept".into(), "application/json".into());
    let summary = service.summary();
    let wire_hash = Blake3Hash::digest(
        serde_json::to_vec(&(
            "lwiki.wire.v1",
            "GET",
            &url,
            &headers,
            Vec::<u8>::new(),
            &summary.profile_fingerprint,
        ))
        .map_err(|_| WikiError::invalid("search wire hash"))?,
    );
    let mut bound = AttemptBound {
        capability,
        profile_id: summary.profile_id,
        endpoint_fingerprint: summary.endpoint_fingerprint,
        config_fingerprint: summary.config_fingerprint,
        input_hash: task.input_hash.clone(),
        wire_hash: wire_hash.clone(),
        requested_model: None,
        requested_model_revision: None,
        request_bytes: 0,
        response_bytes: 1024 * 1024,
        timeout_ms: u64::from(service.service().timeout_seconds.unwrap_or(30)) * 1000,
        applicable_classes: BTreeSet::from([BillableClass::SearchResult]),
        billable_bounds: BTreeMap::from([(
            BillableClass::SearchResult,
            TokenBound::ProvenUpper {
                count: u64::from(count),
                method: "brave-web-v1-count".into(),
                fingerprint: Blake3Hash::digest(canonical_json(&(
                    "brave-web-v1-count",
                    count,
                    &wire_hash,
                ))?),
            },
        )]),
        rate_card: service.service().rate_card.clone(),
        quoted_allowance: None,
        bounds_fingerprint: Blake3Hash::digest([]),
    };
    if let Some(card) = &bound.rate_card
        && let Some(rate) = card.rates.get(&BillableClass::SearchResult)
    {
        bound.quoted_allowance = Some(
            Money::new(card.currency.clone(), card.request_fee_nanounits).checked_add(
                &Money::new(
                    card.currency.clone(),
                    rate.allowance_nanounits(u64::from(count))?,
                ),
            )?,
        );
    }
    bound.bounds_fingerprint = crate::jobs::budgets::bound_fingerprint(&bound)?;
    Ok(PreparedWire {
        role: ServiceRole::Search,
        purpose,
        method: "GET".into(),
        url,
        headers,
        body: vec![],
        bound,
        input: input.clone(),
        contract: WireContract::Search(SearchContract { count, page }),
    })
}
fn results(body: &[u8], contract: &SearchContract) -> Result<Vec<SearchLead>> {
    let value = wire_json::parse(body, 1024 * 1024, 8192, 24)?;
    let entries = value
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| WikiError::invalid("search web results missing"))?;
    if entries.len() > usize::from(contract.count) {
        return Err(WikiError::invalid(
            "search result count exceeds sealed request",
        ));
    }
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let field = |key: &str, limit: usize| -> Result<String> {
                entry
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .filter(|v| {
                        v.len() <= limit && !v.chars().any(|c| c.is_control() && !c.is_whitespace())
                    })
                    .map(str::to_owned)
                    .ok_or_else(|| WikiError::invalid("search lead field invalid"))
            };
            let url = field("url", 8192)?;
            let parsed =
                url::Url::parse(&url).map_err(|_| WikiError::invalid("search lead URL invalid"))?;
            if !matches!(parsed.scheme(), "http" | "https")
                || !parsed.username().is_empty()
                || parsed.password().is_some()
            {
                return Err(WikiError::invalid("search lead URL unsafe"));
            }
            Ok(SearchLead {
                url,
                title: field("title", 4096)?,
                snippet: field("description", 16384)?,
                rank: u32::from(contract.page) * u32::from(contract.count) + index as u32 + 1,
            })
        })
        .collect()
}
pub(super) fn observe(prepared: &PreparedWire, reply: &TransportReply) -> ObservedUsage {
    let WireContract::Search(contract) = &prepared.contract else {
        unreachable!()
    };
    // Billing count does not depend on title/URL/schema validity. Malformed paid
    // output remains chargeable; only a measured count breach breaks its bound.
    let count = wire_json::parse(&reply.body, 1024 * 1024, 8192, 24)
        .ok()
        .and_then(|v| {
            v.get("web")
                .and_then(|w| w.get("results"))
                .and_then(serde_json::Value::as_array)
                .map(|v| v.len() as u64)
        });
    let violation = count.is_some_and(|n| n > u64::from(contract.count));
    let units = count.map_or(KnownOrUnknown::Unknown, KnownOrUnknown::Known);
    let computed_cost = match (&prepared.bound.rate_card, &units) {
        (Some(card), KnownOrUnknown::Known(count)) if !violation => card
            .rates
            .get(&BillableClass::SearchResult)
            .and_then(|r| r.allowance_nanounits(*count).ok())
            .and_then(|n| n.checked_add(card.request_fee_nanounits))
            .map(|n| KnownOrUnknown::Known(Money::new(card.currency.clone(), n)))
            .unwrap_or(KnownOrUnknown::Unknown),
        _ => KnownOrUnknown::Unknown,
    };
    ObservedUsage {
        usage: KnownOrUnknown::Known(Usage {
            request_bytes: 0,
            response_bytes: reply.observed_body_bytes,
            billable_units: BTreeMap::from([(BillableClass::SearchResult, units)]),
        }),
        computed_cost,
        provider_request_id: None,
        returned_model: None,
        contract_violation: violation.then_some(WireContractViolation::ObservedTotalBoundExceeded),
    }
}
pub(super) fn decode(prepared: &PreparedWire, reply: &TransportReply) -> Result<ValidatedOutput> {
    let WireContract::Search(contract) = &prepared.contract else {
        return Err(WikiError::invalid("search contract required"));
    };
    let leads = results(&reply.body, contract)?;
    Ok(match prepared.purpose {
        DispatchPurpose::Probe { .. } => ValidatedOutput::Probe {
            role: ServiceRole::Search,
        },
        _ => ValidatedOutput::Search { leads },
    })
}
