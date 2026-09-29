//! Pure, sealed float embedding adapter.
use super::{types::*, wire, wire_json};
use crate::{
    config::providers::{Dimensions, TrustedService},
    domain::*,
    jobs::TaskSpec,
};
use serde_json::{Value, json};

pub(super) fn prepare(
    service: &TrustedService,
    task: &TaskSpec,
    input: &RemoteInput,
    purpose: DispatchPurpose,
) -> Result<PreparedWire> {
    wire::verify_task(service, task, input)?;
    let RemoteOperation::Embed {
        inputs,
        expected_dimensions,
        representation_fingerprint,
    } = &input.operation
    else {
        return Err(WikiError::invalid("embedding operation required"));
    };
    let s = service.service();
    if input.version != 1
        || inputs.is_empty()
        || inputs.len() > usize::from(s.max_batch_items.unwrap_or(32)).min(4096)
    {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "embedding batch count ceiling",
        ));
    }
    let model = s
        .model
        .as_deref()
        .ok_or_else(|| WikiError::invalid("embedding model required"))?;
    let basis = if s.ca_bytes.is_none() && s.url == "https://api.openai.com/v1/embeddings" {
        match (model, s.revision.as_ref().filter(|r| !r.is_empty())) {
            ("text-embedding-3-small", Some(r)) => BoundBasis::OpenAiEmbedding3SmallV1 {
                representation_revision: r.clone(),
            },
            ("text-embedding-3-large", Some(r)) => BoundBasis::OpenAiEmbedding3LargeV1 {
                representation_revision: r.clone(),
            },
            _ => BoundBasis::UnknownCompatible,
        }
    } else {
        BoundBasis::UnknownCompatible
    };
    let maximum_dimensions = match basis {
        BoundBasis::OpenAiEmbedding3SmallV1 { .. } => 1536,
        BoundBasis::OpenAiEmbedding3LargeV1 { .. } => 3072,
        _ => 65536,
    };
    let requested = match s.dimensions {
        Some(Dimensions::Fixed(n)) => Some(n),
        _ => None,
    };
    if requested
        .zip(*expected_dimensions)
        .is_some_and(|(a, b)| a != b)
        || requested
            .into_iter()
            .chain(*expected_dimensions)
            .any(|n| n == 0 || n > maximum_dimensions)
    {
        return Err(WikiError::invalid(
            "embedding dimensions differ or exceed contract",
        ));
    }
    let items = inputs
        .iter()
        .enumerate()
        .map(|(position, item)| {
            if item.utf8.is_empty() || Blake3Hash::digest(item.utf8.as_bytes()) != item.input_hash {
                return Err(WikiError::invalid("embedding input bytes or hash invalid"));
            }
            Ok(EmbeddingItemContract {
                position: position as u32,
                input_hash: item.input_hash.clone(),
                utf8_bytes: item.utf8.len() as u64,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut body = json!({"model":model,"input":inputs.iter().map(|i|i.utf8.as_str()).collect::<Vec<_>>(),"encoding_format":"float"});
    if let Some(n) = requested {
        body["dimensions"] = n.into();
    }
    wire::seal(
        service,
        task,
        input,
        purpose,
        ServiceRole::Embed,
        wire::json_body(service, &body)?,
        WireContract::Embedding(EmbeddingContract {
            basis,
            items,
            representation_fingerprint: representation_fingerprint.clone(),
            requested_dimensions: requested,
            expected_dimensions: requested.or(*expected_dimensions),
            maximum_dimensions,
            maximum_coordinates: 1_048_576,
        }),
    )
}
pub(super) fn observe(p: &PreparedWire, r: &TransportReply) -> ObservedUsage {
    wire_json::observe(p, r, false)
}
pub(super) fn decode(p: &PreparedWire, r: &TransportReply) -> Result<ValidatedOutput> {
    if !(200..300).contains(&r.status) {
        return Err(WikiError::invalid("unsuccessful embedding response"));
    }
    let WireContract::Embedding(c) = &p.contract else {
        return Err(WikiError::invalid("embedding contract required"));
    };
    let RemoteOperation::Embed {
        inputs,
        representation_fingerprint,
        ..
    } = &p.input.operation
    else {
        return Err(WikiError::invalid("retained embedding input required"));
    };
    if &c.representation_fingerprint != representation_fingerprint
        || c.items.len() != inputs.len()
        || c.requested_dimensions
            .is_some_and(|n| c.expected_dimensions != Some(n))
        || c.items
            .iter()
            .zip(inputs)
            .enumerate()
            .any(|(position, (item, input))| {
                item.position as usize != position
                    || item.input_hash != input.input_hash
                    || item.utf8_bytes != input.utf8.len() as u64
            })
    {
        return Err(WikiError::invalid("retained embedding membership differs"));
    }
    let v = wire_json::reply_json(p, r)?;
    let model = wire_json::returned_model(p, &v)?;
    if !wire_json::usage_valid(&v, false)
        || !wire_json::supported_usage(p, &v, false)
        || !wire_json::totals_within_contract(p, &v)
    {
        return Err(WikiError::invalid("embedding usage malformed"));
    }
    if v.get("object").is_some_and(|o| o.as_str() != Some("list")) {
        return Err(WikiError::invalid("embedding object invalid"));
    }
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| WikiError::invalid("embedding data missing"))?;
    if data.len() != c.items.len() {
        return Err(WikiError::invalid("embedding batch incomplete"));
    }
    let mut vectors = vec![None; c.items.len()];
    let mut dimension = c.expected_dimensions;
    let mut coordinates = 0u64;
    for entry in data {
        if entry
            .get("object")
            .is_some_and(|o| o.as_str() != Some("embedding"))
            || entry.get("model").is_some()
        {
            return Err(WikiError::invalid("embedding entry metadata invalid"));
        }
        let index = entry
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n < vectors.len())
            .ok_or_else(|| WikiError::invalid("embedding index invalid"))?;
        if vectors[index].is_some() {
            return Err(WikiError::invalid("duplicate embedding index"));
        }
        let array = entry
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| WikiError::invalid("embedding vector missing"))?;
        if array.is_empty()
            || array.len() > c.maximum_dimensions as usize
            || dimension.is_some_and(|n| n as usize != array.len())
        {
            return Err(WikiError::invalid("embedding vector dimensions invalid"));
        }
        dimension = Some(array.len() as u32);
        coordinates = coordinates
            .checked_add(array.len() as u64)
            .filter(|n| *n <= c.maximum_coordinates)
            .ok_or_else(|| {
                WikiError::new(ErrorCode::BudgetExceeded, "embedding coordinate ceiling")
            })?;
        let mut vector = Vec::with_capacity(array.len());
        let mut norm = 0.0f64;
        for coordinate in array {
            let n = coordinate
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or_else(|| WikiError::invalid("embedding coordinate invalid"))?;
            let f = n as f32;
            if !f.is_finite() {
                return Err(WikiError::invalid("embedding coordinate exceeds float32"));
            }
            norm += f64::from(f) * f64::from(f);
            vector.push(f);
        }
        if !norm.is_finite() || norm == 0.0 {
            return Err(WikiError::invalid("embedding norm invalid"));
        }
        vectors[index] = Some(vector);
    }
    let vectors = vectors
        .into_iter()
        .map(|v| v.ok_or_else(|| WikiError::invalid("embedding batch incomplete")))
        .collect::<Result<Vec<_>>>()?;
    if matches!(p.purpose, DispatchPurpose::Probe { .. }) {
        Ok(ValidatedOutput::Probe {
            role: ServiceRole::Embed,
        })
    } else {
        Ok(ValidatedOutput::Embeddings {
            vectors,
            returned_model: Some(model),
        })
    }
}
