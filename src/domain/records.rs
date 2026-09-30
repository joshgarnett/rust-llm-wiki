//! Frontmatter contract validation, without cross-record or Markdown-body claims.
use super::types::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use time::{Date, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalRecord {
    fields: BTreeMap<String, Value>,
    id: RecordId,
    kind: RecordKind,
}

impl CanonicalRecord {
    pub fn new(fields: BTreeMap<String, Value>) -> Result<Self> {
        let schema = required_string(&fields, "wiki_schema")?;
        if schema != "1"
            && !(schema == "2" && fields.get("wiki_kind").and_then(Value::as_str) == Some("vault"))
        {
            return Err(field_error(
                "wiki_schema",
                "must equal the string \"1\" (vault markers also support storage layout \"2\")",
            ));
        }
        let id = RecordId::new(required_string(&fields, "wiki_id")?)?;
        let kind: RecordKind = required_string(&fields, "wiki_kind")?.parse()?;
        required_string(&fields, "title")?;
        let (required, optional) = kind_fields(kind);
        let mut allowed: BTreeSet<String> =
            ["wiki_schema", "wiki_id", "wiki_kind", "wiki_depends_on_ids"]
                .into_iter()
                .map(str::to_owned)
                .collect();
        for name in required.iter().chain(optional.iter()) {
            let key = format!("wiki_{name}");
            allowed.insert(key.clone());
            if required.contains(name) && !fields.contains_key(&key) {
                return Err(field_error(&key, "is required"));
            }
            if let Some(value) = fields.get(&key) {
                validate_field(name, value)?;
            }
        }
        for name in ["aliases", "tags"] {
            if let Some(value) = fields.get(name) {
                string_list(name, value)?;
            }
        }
        if let Some(value) = fields.get("description") {
            string_value("description", value)?;
        }
        if let Some(value) = fields.get("wiki_depends_on_ids") {
            id_list("wiki_depends_on_ids", value)?;
        }
        // Navigation is optional and never substitutes for the authoritative ID.
        for (link, reference) in navigation_fields(kind) {
            let key = format!("wiki_{link}");
            allowed.insert(key.clone());
            if let Some(value) = fields.get(&key) {
                if !fields.contains_key(&format!("wiki_{reference}")) {
                    return Err(field_error(&key, "requires its authoritative ID field"));
                }
                validate_link(&key, string_value(&key, value)?)?;
            }
        }
        for key in fields.keys() {
            if key.starts_with("wiki_") && !allowed.contains(key) {
                return Err(field_error(
                    key,
                    "is an unsupported semantic field for this record kind",
                ));
            }
        }
        match kind {
            RecordKind::Assertion => validate_assertion(&fields)?,
            RecordKind::Evidence => {
                let start = fields["wiki_span_start"]
                    .as_u64()
                    .expect("validated integer");
                let end = fields["wiki_span_end"].as_u64().expect("validated integer");
                if ByteSpan::new(start, end)?.is_empty() {
                    return Err(field_error(
                        "wiki_span_end",
                        "must be later than wiki_span_start for evidence",
                    ));
                }
            }
            RecordKind::Revision => {
                let complete = fields["wiki_extraction_status"] == "complete";
                let path = fields.contains_key("wiki_content_path");
                let hash = fields.contains_key("wiki_content_hash");
                if complete && !(path && hash) || !complete && (path || hash) {
                    return Err(field_error(
                        "wiki_content_path",
                        "complete revisions require both content fields; unsupported/failed revisions omit both",
                    ));
                }
            }
            RecordKind::ExtractionPacket => {
                let hash = Blake3Hash::new(required_string(&fields, "wiki_packet_fingerprint")?)?;
                if id != RecordId::packet(&hash) {
                    return Err(field_error(
                        "wiki_id",
                        "must equal packet_<packet fingerprint hex>",
                    ));
                }
            }
            RecordKind::Decision => {
                let action = required_string(&fields, "wiki_action")?;
                if matches!(action, "bind_mention" | "create_entity" | "reject_mention")
                    || action == "add_alias" && fields.contains_key("wiki_mention_ids")
                {
                    required_string(&fields, "wiki_extraction_id")?;
                    let mentions = fields.get("wiki_mention_ids").ok_or_else(|| {
                        field_error("wiki_mention_ids", "is required for mention actions")
                    })?;
                    if string_list("wiki_mention_ids", mentions)?.is_empty() {
                        return Err(field_error(
                            "wiki_mention_ids",
                            "must include an affected mention",
                        ));
                    }
                }
            }
            _ => {}
        }
        let record = Self { fields, id, kind };
        validate_status(&record)?;
        Ok(record)
    }

    pub fn from_value(value: Value) -> Result<Self> {
        let Value::Object(fields) = value else {
            return Err(WikiError::invalid("record frontmatter must be an object"));
        };
        Self::new(fields.into_iter().collect())
    }
    pub fn fields(&self) -> &BTreeMap<String, Value> {
        &self.fields
    }
    pub fn into_fields(self) -> BTreeMap<String, Value> {
        self.fields
    }
    pub fn id(&self) -> &RecordId {
        &self.id
    }
    pub const fn kind(&self) -> RecordKind {
        self.kind
    }
    pub fn title(&self) -> &str {
        self.fields["title"].as_str().expect("validated title")
    }
    pub fn field(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }
    pub fn string(&self, key: &str) -> Option<&str> {
        self.field(key).and_then(Value::as_str)
    }
}

impl Serialize for CanonicalRecord {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.fields.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for CanonicalRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::new(BTreeMap::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

fn kind_fields(kind: RecordKind) -> (&'static [&'static str], &'static [&'static str]) {
    match kind {
        RecordKind::Vault => (&[], &["profile"]),
        RecordKind::Page => (&["status"], &[]),
        RecordKind::Entity => (&["status", "entity_type"], &["superseded_by_id"]),
        RecordKind::Assertion => (
            &["status", "subject_id", "predicate"],
            &[
                "object_id",
                "literal_type",
                "literal_value",
                "property",
                "negated",
                "modality",
                "valid_from",
                "valid_until",
                "unit",
                "evidence",
            ],
        ),
        RecordKind::Evidence => (
            &[
                "status",
                "assertion_id",
                "source_id",
                "source_revision",
                "stance",
                "locator_kind",
                "span_start",
                "span_end",
                "quote_hash",
            ],
            &["supersedes_id", "extraction_id"],
        ),
        RecordKind::Source => (
            &[
                "status",
                "origin_kind",
                "origin",
                "current_revision",
                "revisions",
            ],
            &["withdrawn_at", "withdrawal_reason"],
        ),
        RecordKind::Revision => (
            &[
                "source_id",
                "captured_at",
                "original_path",
                "original_hash",
                "extractor",
                "extractor_fingerprint",
                "extraction_status",
            ],
            &["content_path", "content_hash", "media_type", "published_at"],
        ),
        RecordKind::ExtractionPacket => (
            &[
                "source_id",
                "source_revision",
                "packet_fingerprint",
                "output_schema",
                "created_at",
            ],
            &[],
        ),
        RecordKind::Extraction => (
            &[
                "status",
                "packet_id",
                "input_hash",
                "extractor_fingerprint",
                "executor",
                "source_ids",
                "source_revision_ids",
                "completed_at",
            ],
            &["model", "model_revision"],
        ),
        RecordKind::Decision => (
            &["status", "action", "input_ids", "output_ids", "created_at"],
            &["supersedes_id", "extraction_id", "mention_ids"],
        ),
        RecordKind::Change => (&["status", "created_at", "manifest_hash"], &[]),
        RecordKind::Run => (&["status", "created_at"], &["checkpoint_event_id"]),
        RecordKind::RunEvent => (
            &["run_id", "sequence", "event_type", "occurred_at"],
            &["request_id"],
        ),
    }
}

fn navigation_fields(kind: RecordKind) -> &'static [(&'static str, &'static str)] {
    match kind {
        RecordKind::Entity => &[("superseded_by", "superseded_by_id")],
        RecordKind::Assertion => &[("subject", "subject_id"), ("object", "object_id")],
        RecordKind::Evidence => &[
            ("assertion", "assertion_id"),
            ("source", "source_id"),
            ("revision", "source_revision"),
            ("supersedes", "supersedes_id"),
            ("extraction", "extraction_id"),
        ],
        RecordKind::Source => &[("revision", "current_revision")],
        RecordKind::Revision => &[("source", "source_id")],
        RecordKind::ExtractionPacket => &[("source", "source_id"), ("revision", "source_revision")],
        RecordKind::Extraction => &[("packet", "packet_id")],
        RecordKind::Decision => &[
            ("supersedes", "supersedes_id"),
            ("extraction", "extraction_id"),
        ],
        RecordKind::Run => &[("checkpoint_event", "checkpoint_event_id")],
        RecordKind::RunEvent => &[("run", "run_id"), ("request", "request_id")],
        _ => &[],
    }
}

fn field_error(field: &str, message: &str) -> WikiError {
    let mut error = WikiError::invalid(format!("{field} {message}"));
    error.details = serde_json::json!({ "field": field });
    error
}
fn string_value<'a>(field: &str, value: &'a Value) -> Result<&'a str> {
    value
        .as_str()
        .ok_or_else(|| field_error(field, "must be a string"))
}
fn required_string<'a>(fields: &'a BTreeMap<String, Value>, field: &str) -> Result<&'a str> {
    string_value(
        field,
        fields
            .get(field)
            .ok_or_else(|| field_error(field, "is required"))?,
    )
}
fn string_list<'a>(field: &str, value: &'a Value) -> Result<Vec<&'a str>> {
    let array = value
        .as_array()
        .ok_or_else(|| field_error(field, "must be a string list"))?;
    array.iter().map(|v| string_value(field, v)).collect()
}
fn id_list(field: &str, value: &Value) -> Result<()> {
    for id in string_list(field, value)? {
        RecordId::new(id)?;
    }
    Ok(())
}
fn validate_field(name: &str, value: &Value) -> Result<()> {
    let key = format!("wiki_{name}");
    match name {
        "negated" => {
            if !value.is_boolean() {
                return Err(field_error(&key, "must be a boolean"));
            }
        }
        "span_start" | "span_end" | "sequence" => {
            if value.as_u64().is_none() {
                return Err(field_error(&key, "must be a nonnegative integer"));
            }
        }
        "mention_ids" => {
            string_list(&key, value)?;
        }
        "revisions" | "source_ids" | "source_revision_ids" | "input_ids" | "output_ids" => {
            id_list(&key, value)?;
            if matches!(name, "source_ids" | "source_revision_ids") {
                let ids = string_list(&key, value)?;
                if ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
                    return Err(field_error(&key, "must be a set without duplicates"));
                }
            }
        }
        "evidence" => {
            for link in string_list(&key, value)? {
                validate_link(&key, link)?;
            }
        }
        "valid_from" | "valid_until" => {
            parse_date(&key, string_value(&key, value)?)?;
        }
        "captured_at" | "published_at" | "created_at" | "completed_at" | "occurred_at"
        | "withdrawn_at" => {
            let timestamp = string_value(&key, value)?;
            let parsed = OffsetDateTime::parse(timestamp, &Rfc3339)
                .map_err(|_| field_error(&key, "must be an RFC3339 UTC timestamp"))?;
            if parsed.offset() != UtcOffset::UTC
                || !(timestamp.ends_with(['Z', 'z']) || timestamp.ends_with("+00:00"))
            {
                return Err(field_error(&key, "must use UTC"));
            }
        }
        "original_path" | "content_path" => {
            VaultRelativePath::new(string_value(&key, value)?)?;
        }
        "quote_hash"
        | "original_hash"
        | "content_hash"
        | "input_hash"
        | "manifest_hash"
        | "packet_fingerprint"
        | "extractor_fingerprint" => {
            Blake3Hash::new(string_value(&key, value)?)?;
        }
        "subject_id"
        | "object_id"
        | "superseded_by_id"
        | "assertion_id"
        | "source_id"
        | "source_revision"
        | "supersedes_id"
        | "extraction_id"
        | "current_revision"
        | "packet_id"
        | "checkpoint_event_id"
        | "run_id"
        | "request_id" => {
            RecordId::new(string_value(&key, value)?)?;
        }
        _ => {
            string_value(&key, value)?;
        }
    }
    let alternatives: &[&str] = match name {
        "entity_type" => &[
            "person",
            "organization",
            "project",
            "component",
            "concept",
            "place",
            "event",
            "other",
        ],
        "stance" => &["supports", "contradicts"],
        "locator_kind" => &["utf8-bytes"],
        "origin_kind" => &["local-file", "url", "agent-report"],
        "extraction_status" => &["complete", "unsupported", "failed"],
        "executor" => &["agent", "api"],
        "action" => &[
            "merge",
            "split",
            "reject",
            "accept",
            "correct",
            "bind_mention",
            "create_entity",
            "reject_mention",
            "add_alias",
        ],
        "modality" => &["asserted", "possible", "planned"],
        "literal_type" => &["string", "decimal", "boolean", "date"],
        _ => &[],
    };
    if !alternatives.is_empty()
        && !alternatives.contains(&value.as_str().expect("validated string"))
    {
        return Err(field_error(&key, "has an unsupported value"));
    }
    Ok(())
}

fn parse_date(field: &str, value: &str) -> Result<Date> {
    if value.len() != 10
        || !value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 4 | 7) {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return Err(field_error(field, "must be a YYYY-MM-DD date string"));
    }
    let format = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")
        .expect("constant date format");
    Date::parse(value, &format).map_err(|_| field_error(field, "must be a valid calendar date"))
}

fn validate_link(field: &str, value: &str) -> Result<()> {
    let target = value
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .ok_or_else(|| field_error(field, "must be a path wikilink"))?;
    if target.contains(['[', ']', '\n', '\r']) {
        return Err(field_error(field, "must contain one path wikilink"));
    }
    let path = target
        .split('|')
        .next()
        .unwrap_or_default()
        .split('#')
        .next()
        .unwrap_or_default();
    VaultRelativePath::new(path)?;
    Ok(())
}

fn validate_assertion(fields: &BTreeMap<String, Value>) -> Result<()> {
    let predicate = required_string(fields, "wiki_predicate")?;
    let object = fields.contains_key("wiki_object_id");
    let literal_type = fields.contains_key("wiki_literal_type");
    let literal_value = fields.contains_key("wiki_literal_value");
    if object == (literal_type || literal_value) || literal_type != literal_value {
        return Err(field_error(
            "wiki_object_id",
            "requires exactly an entity object or a complete typed literal",
        ));
    }
    if predicate == "has_property" {
        if object {
            return Err(field_error(
                "wiki_predicate",
                "has_property requires a typed literal",
            ));
        }
        required_string(fields, "wiki_property")?;
    } else {
        if ![
            "is_a",
            "part_of",
            "depends_on",
            "maintains",
            "works_for",
            "uses",
            "plans_to_use",
            "located_in",
        ]
        .contains(&predicate)
        {
            return Err(field_error(
                "wiki_predicate",
                "is not in the directed predicate registry",
            ));
        }
        if !object {
            return Err(field_error("wiki_predicate", "requires an entity object"));
        }
        if fields.contains_key("wiki_property") {
            return Err(field_error(
                "wiki_property",
                "is only supported for has_property",
            ));
        }
    }
    if literal_type {
        let kind = required_string(fields, "wiki_literal_type")?;
        let literal = required_string(fields, "wiki_literal_value")?;
        match kind {
            "decimal" if !is_decimal(literal) => {
                return Err(field_error(
                    "wiki_literal_value",
                    "does not match the exact decimal grammar",
                ));
            }
            "boolean" if !matches!(literal, "true" | "false") => {
                return Err(field_error(
                    "wiki_literal_value",
                    "must be the string true or false",
                ));
            }
            "date" => {
                parse_date("wiki_literal_value", literal)?;
            }
            _ => {}
        }
    }
    if fields.contains_key("wiki_unit")
        && fields.get("wiki_literal_type").and_then(Value::as_str) != Some("decimal")
    {
        return Err(field_error(
            "wiki_unit",
            "is supported only for decimal literals",
        ));
    }
    if let (Some(start), Some(end)) = (
        fields.get("wiki_valid_from"),
        fields.get("wiki_valid_until"),
    ) {
        let start = parse_date("wiki_valid_from", start.as_str().expect("validated date"))?;
        let end = parse_date("wiki_valid_until", end.as_str().expect("validated date"))?;
        if end <= start {
            return Err(field_error(
                "wiki_valid_until",
                "must be later than wiki_valid_from",
            ));
        }
    }
    Ok(())
}

fn is_decimal(value: &str) -> bool {
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    let (integer, fraction) = match unsigned.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (unsigned, None),
    };
    !integer.is_empty()
        && integer.bytes().all(|b| b.is_ascii_digit())
        && (integer == "0" || !integer.starts_with('0'))
        && fraction.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

/// Validate the kind-dependent lifecycle value separately from generic field types.
pub fn validate_status(record: &CanonicalRecord) -> Result<()> {
    let statuses: &[&str] = match record.kind {
        RecordKind::Page => &["draft", "reviewed", "deprecated"],
        RecordKind::Entity | RecordKind::Decision => &["active", "superseded"],
        RecordKind::Assertion => &["proposed", "accepted", "rejected", "superseded"],
        RecordKind::Evidence => &["active", "retracted"],
        RecordKind::Source => &["active", "withdrawn"],
        RecordKind::Extraction => &["completed", "partial", "failed"],
        RecordKind::Change => &["prepared", "applying", "conflict", "committed", "aborted"],
        RecordKind::Run => &[
            "planned",
            "running",
            "paused",
            "completed",
            "failed",
            "stopped",
        ],
        _ => &[],
    };
    if !statuses.is_empty() && !statuses.contains(&record.string("wiki_status").unwrap_or_default())
    {
        return Err(field_error(
            "wiki_status",
            "has an unsupported lifecycle value for this record kind",
        ));
    }
    Ok(())
}
