//! Validated primitives shared by storage, records, and application boundaries.
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::{fmt, str::FromStr};

pub type Result<T> = std::result::Result<T, WikiError>;
pub type RevisionId = RecordId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Internal,
    Usage,
    ConfigInvalid,
    VaultNotFound,
    RecordNotFound,
    ContentConflict,
    LockTimeout,
    CursorStale,
    FreshnessConflict,
    RecoveryRequired,
    IndexCorrupt,
    SourceIntegrity,
    CapabilityUnavailable,
    OfflineUnavailable,
    ProfileUntrusted,
    BudgetExceeded,
    ProviderAuth,
    ProviderRateLimit,
    ProviderResponse,
    ProviderUnavailable,
    RecordInvalid,
    ReferenceAmbiguous,
    ExtractionInvalid,
    Cancelled,
}

impl ErrorCode {
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Internal => 1,
            Self::Usage | Self::ConfigInvalid => 2,
            Self::VaultNotFound | Self::RecordNotFound => 3,
            Self::ContentConflict
            | Self::LockTimeout
            | Self::CursorStale
            | Self::FreshnessConflict => 4,
            Self::RecoveryRequired | Self::IndexCorrupt | Self::SourceIntegrity => 5,
            Self::CapabilityUnavailable | Self::OfflineUnavailable | Self::ProfileUntrusted => 6,
            Self::BudgetExceeded => 7,
            Self::ProviderAuth
            | Self::ProviderRateLimit
            | Self::ProviderResponse
            | Self::ProviderUnavailable => 8,
            Self::RecordInvalid | Self::ReferenceAmbiguous | Self::ExtractionInvalid => 9,
            Self::Cancelled => 130,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The serialization spelling is the single public error-code registry.
        let value = serde_json::to_value(self).map_err(|_| fmt::Error)?;
        f.write_str(value.as_str().ok_or(fmt::Error)?)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WikiError {
    /// Local execution telemetry; untrusted serialized errors cannot supply it.
    #[serde(skip)]
    pub(crate) network_used: bool,
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub hint: Option<String>,
    pub details: Value,
}

impl WikiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            network_used: false,
            code,
            message: message.into(),
            retryable: false,
            hint: None,
            details: Value::Null,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::RecordInvalid, message)
    }

    pub const fn exit_code(&self) -> u8 {
        self.code.exit_code()
    }
}

impl fmt::Display for WikiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for WikiError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Vault,
    Page,
    Entity,
    Assertion,
    Evidence,
    Source,
    Revision,
    ExtractionPacket,
    Extraction,
    Decision,
    Change,
    Run,
    RunEvent,
}

impl RecordKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Vault => "vault",
            Self::Page => "page",
            Self::Entity => "entity",
            Self::Assertion => "assertion",
            Self::Evidence => "evidence",
            Self::Source => "source",
            Self::Revision => "revision",
            Self::ExtractionPacket => "extraction_packet",
            Self::Extraction => "extraction",
            Self::Decision => "decision",
            Self::Change => "change",
            Self::Run => "run",
            Self::RunEvent => "run_event",
        }
    }
}
impl fmt::Display for RecordKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
impl FromStr for RecordKind {
    type Err = WikiError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "vault" => Ok(Self::Vault),
            "page" => Ok(Self::Page),
            "entity" => Ok(Self::Entity),
            "assertion" => Ok(Self::Assertion),
            "evidence" => Ok(Self::Evidence),
            "source" => Ok(Self::Source),
            "revision" => Ok(Self::Revision),
            "extraction_packet" => Ok(Self::ExtractionPacket),
            "extraction" => Ok(Self::Extraction),
            "decision" => Ok(Self::Decision),
            "change" => Ok(Self::Change),
            "run" => Ok(Self::Run),
            "run_event" => Ok(Self::RunEvent),
            _ => Err(WikiError::invalid(format!("unknown record kind {s:?}"))),
        }
    }
}

macro_rules! string_type {
    ($name:ident) => {
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl FromStr for $name {
            type Err = WikiError;
            fn from_str(s: &str) -> Result<Self> {
                Self::new(s)
            }
        }
        impl TryFrom<String> for $name {
            type Error = WikiError;
            fn try_from(s: String) -> Result<Self> {
                Self::new(s)
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
                Self::new(String::deserialize(d)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RecordId(String);
impl RecordId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value.as_bytes()[0].is_ascii_alphanumeric()
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(WikiError::invalid(
                "record ID must match [A-Za-z0-9][A-Za-z0-9._-]{0,127}",
            ));
        }
        Ok(Self(value))
    }

    pub fn generate(kind: RecordKind) -> Result<Self> {
        if kind == RecordKind::ExtractionPacket {
            return Err(WikiError::invalid(
                "extraction packet IDs require a deterministic fingerprint",
            ));
        }
        // Decimal preserves every UUID bit and gives Source directory names a
        // singleton Unicode fold class. Kind remains an explicit record field;
        // existing tagged identities continue to be accepted unchanged.
        if kind == RecordKind::Source {
            return Self::new(format!("{:039}", uuid::Uuid::now_v7().as_u128()));
        }
        Self::new(format!("{}_{}", kind.as_str(), uuid::Uuid::now_v7()))
    }

    pub fn packet(fingerprint: &Blake3Hash) -> Self {
        Self(format!("packet_{}", fingerprint.hex()))
    }
}
string_type!(RecordId);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Blake3Hash(String);
impl Blake3Hash {
    pub(crate) fn owned_capacity(&self) -> usize {
        self.0.capacity()
    }
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let valid = value.strip_prefix("blake3:").is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        if !valid {
            return Err(WikiError::invalid(
                "hash must be blake3:<64 lowercase hex digits>",
            ));
        }
        Ok(Self(value))
    }
    pub fn digest(bytes: impl AsRef<[u8]>) -> Self {
        Self(format!("blake3:{}", blake3::hash(bytes.as_ref()).to_hex()))
    }
    pub fn hex(&self) -> &str {
        &self.0[7..]
    }
}
string_type!(Blake3Hash);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct VaultRelativePath(String);
impl VaultRelativePath {
    /// Owned path bytes included in joined-maintenance descriptor reservations.
    pub(crate) fn owned_capacity(&self) -> usize {
        self.0.capacity()
    }
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.starts_with('/') || value.contains('\\') {
            return Err(WikiError::invalid(
                "path must be a nonempty relative slash-separated path",
            ));
        }
        for part in value.split('/') {
            if part.is_empty()
                || part == "."
                || part == ".."
                || part.ends_with([' ', '.'])
                || part
                    .chars()
                    .any(|c| c.is_control() || "<>:\"|?*".contains(c))
            {
                return Err(WikiError::invalid(
                    "path contains an unsafe or reserved component",
                ));
            }
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || stem
                    .strip_prefix("COM")
                    .or_else(|| stem.strip_prefix("LPT"))
                    .is_some_and(|n| {
                        matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                    })
                || matches!(
                    stem.as_str(),
                    "COM¹" | "COM²" | "COM³" | "LPT¹" | "LPT²" | "LPT³"
                );
            if reserved {
                return Err(WikiError::invalid("path contains a platform-reserved name"));
            }
        }
        Ok(Self(value))
    }
}
string_type!(VaultRelativePath);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct ByteSpan {
    start: u64,
    end: u64,
}
impl ByteSpan {
    pub fn new(start: u64, end: u64) -> Result<Self> {
        if end < start {
            return Err(WikiError::invalid("byte span end precedes start"));
        }
        Ok(Self { start, end })
    }
    pub const fn start(self) -> u64 {
        self.start
    }
    pub const fn end(self) -> u64 {
        self.end
    }
    pub const fn len(self) -> u64 {
        self.end - self.start
    }
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
    pub fn slice(self, text: &str) -> Result<&str> {
        let start = usize::try_from(self.start)
            .map_err(|_| WikiError::invalid("byte span exceeds host size"))?;
        let end = usize::try_from(self.end)
            .map_err(|_| WikiError::invalid("byte span exceeds host size"))?;
        text.get(start..end)
            .ok_or_else(|| WikiError::invalid("byte span exceeds text or splits a UTF-8 character"))
    }
}
impl<'de> Deserialize<'de> for ByteSpan {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            start: u64,
            end: u64,
        }
        let value = Fields::deserialize(d)?;
        Self::new(value.start, value.end).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordRef {
    pub vault_id: RecordId,
    pub record_id: RecordId,
    pub expected_kind: RecordKind,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentLocator {
    pub record: Option<RecordRef>,
    pub path: VaultRelativePath,
    pub observed_hash: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpanRef {
    pub source_id: RecordId,
    pub source_revision: RevisionId,
    pub span: ByteSpan,
    pub quote_hash: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub evidence_id: RecordId,
    pub assertion_id: RecordId,
    pub source_id: RecordId,
    pub source_revision: RevisionId,
    pub span: ByteSpan,
    pub quote_hash: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "reference", rename_all = "snake_case")]
pub enum CitationRef {
    Source(SourceSpanRef),
    Assertion(EvidenceRef),
}
/// Identity of a read view. Canonical manifests and published epochs provide
/// different guarantees and must never be compared as interchangeable hashes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadSnapshot {
    pub generation: u64,
    pub parser_fingerprint: Blake3Hash,
    #[serde(flatten)]
    pub binding: SnapshotBinding,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum SnapshotBinding {
    CanonicalManifest { control_manifest: Blake3Hash },
    PublishedEpoch { publication: PublishedEpochBinding },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedEpochBinding {
    pub version: u32,
    pub file_id: String,
    pub publication_hash: Blake3Hash,
}
impl PublishedEpochBinding {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.file_id.len() != 32
            || !self
                .file_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(WikiError::invalid("invalid published snapshot binding"));
        }
        Ok(())
    }
}
impl ReadSnapshot {
    /// Preserve the original serialized snapshot shape for retained receipts.
    pub fn canonical(
        generation: u64,
        parser_fingerprint: Blake3Hash,
        control_manifest: Blake3Hash,
    ) -> Self {
        Self {
            generation,
            parser_fingerprint,
            binding: SnapshotBinding::CanonicalManifest { control_manifest },
        }
    }
    pub fn published(
        generation: u64,
        parser_fingerprint: Blake3Hash,
        file_id: String,
        publication_hash: Blake3Hash,
    ) -> Result<Self> {
        let publication = PublishedEpochBinding {
            version: 1,
            file_id,
            publication_hash,
        };
        publication.validate()?;
        if generation == 0 || generation > i64::MAX as u64 {
            return Err(WikiError::invalid("invalid published snapshot epoch"));
        }
        Ok(Self {
            generation,
            parser_fingerprint,
            binding: SnapshotBinding::PublishedEpoch { publication },
        })
    }
    pub fn canonical_manifest(&self) -> Option<&Blake3Hash> {
        match &self.binding {
            SnapshotBinding::CanonicalManifest { control_manifest } => Some(control_manifest),
            SnapshotBinding::PublishedEpoch { .. } => None,
        }
    }
    pub fn require_canonical_manifest(&self) -> Result<&Blake3Hash> {
        self.canonical_manifest().ok_or_else(|| WikiError::new(
            ErrorCode::OfflineUnavailable,
            "this operation requires a canonical manifest; a published epoch is not a full-vault audit",
        ))
    }
    pub fn publication(&self) -> Option<&PublishedEpochBinding> {
        match &self.binding {
            SnapshotBinding::CanonicalManifest { .. } => None,
            SnapshotBinding::PublishedEpoch { publication } => Some(publication),
        }
    }
}
fn present_snapshot_binding<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl<'de> Deserialize<'de> for ReadSnapshot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            generation: u64,
            parser_fingerprint: Blake3Hash,
            #[serde(default, deserialize_with = "present_snapshot_binding")]
            control_manifest: Option<Blake3Hash>,
            #[serde(default, deserialize_with = "present_snapshot_binding")]
            publication: Option<PublishedEpochBinding>,
        }
        let wire = Wire::deserialize(deserializer)?;
        match (wire.control_manifest, wire.publication) {
            (Some(manifest), None) => Ok(Self::canonical(
                wire.generation,
                wire.parser_fingerprint,
                manifest,
            )),
            (None, Some(publication)) => {
                publication.validate().map_err(serde::de::Error::custom)?;
                Self::published(
                    wire.generation,
                    wire.parser_fingerprint,
                    publication.file_id,
                    publication.publication_hash,
                )
                .map_err(serde::de::Error::custom)
            }
            _ => Err(serde::de::Error::custom(
                "snapshot must have exactly one canonical or published binding",
            )),
        }
    }
}

#[cfg(test)]
mod snapshot_binding_tests {
    use super::*;
    fn schemas_accept(value: &Value) -> bool {
        [
            include_str!("../../schemas/run-v1.json"),
            include_str!("../../schemas/run-event-v1.json"),
        ]
        .into_iter()
        .all(|raw| {
            let schema: Value = serde_json::from_str(raw).unwrap();
            let focused = serde_json::json!({
                "$ref": "#/$defs/ReadSnapshot", "$defs": schema["$defs"]
            });
            jsonschema::validator_for(&focused).unwrap().is_valid(value)
        })
    }
    #[test]
    fn legacy_receipt_snapshot_round_trips_without_wire_change() {
        let parser = Blake3Hash::digest("parser");
        let manifest = Blake3Hash::digest("manifest");
        let old = format!(
            "{{\"generation\":7,\"parser_fingerprint\":\"{parser}\",\"control_manifest\":\"{manifest}\"}}"
        );
        let snapshot: ReadSnapshot = serde_json::from_str(&old).unwrap();
        assert_eq!(snapshot.require_canonical_manifest().unwrap(), &manifest);
        assert!(snapshot.publication().is_none());
        assert_eq!(serde_json::to_string(&snapshot).unwrap(), old);
        assert!(schemas_accept(&serde_json::to_value(snapshot).unwrap()));
    }
    #[test]
    fn published_binding_never_supplies_canonical_manifest() {
        let snapshot = ReadSnapshot::published(
            7,
            Blake3Hash::digest("parser"),
            "1234567890abcdef1234567890abcdef".into(),
            Blake3Hash::digest("publication"),
        )
        .unwrap();
        assert!(snapshot.canonical_manifest().is_none());
        assert_eq!(
            snapshot.require_canonical_manifest().unwrap_err().code,
            ErrorCode::OfflineUnavailable
        );
        let encoded = serde_json::to_vec(&snapshot).unwrap();
        assert_eq!(
            serde_json::from_slice::<ReadSnapshot>(&encoded).unwrap(),
            snapshot
        );
        assert!(schemas_accept(&serde_json::to_value(snapshot).unwrap()));
        assert!(
            !String::from_utf8(encoded)
                .unwrap()
                .contains("control_manifest")
        );
    }
    #[test]
    fn malformed_or_ambiguous_snapshot_bindings_are_rejected() {
        let snapshot = ReadSnapshot::published(
            7,
            Blake3Hash::digest("parser"),
            "1234567890abcdef1234567890abcdef".into(),
            Blake3Hash::digest("publication"),
        )
        .unwrap();
        let valid = serde_json::to_value(snapshot).unwrap();
        let mut variants = Vec::new();
        let mut value = valid.clone();
        value["control_manifest"] = Value::Null;
        variants.push(value);
        let mut value = valid.clone();
        value["publication"] = Value::Null;
        variants.push(value);
        let mut value = valid.clone();
        value["control_manifest"] = serde_json::json!(Blake3Hash::digest("mixed"));
        value["generation"] = 0.into();
        variants.push(value);

        let mut value = valid.clone();
        value["control_manifest"] = serde_json::json!(Blake3Hash::digest("false canonical"));
        variants.push(value);
        let mut value = valid.clone();
        value.as_object_mut().unwrap().remove("publication");
        variants.push(value);
        let mut value = valid.clone();
        value["publication"]["version"] = 2.into();
        variants.push(value);
        let mut value = valid.clone();
        value["publication"]["file_id"] = "../foreign".into();
        variants.push(value);
        let mut value = valid.clone();
        value["publication"]["unexpected"] = true.into();
        variants.push(value);
        let mut value = valid.clone();
        value["generation"] = 0.into();
        variants.push(value);
        let mut value = valid;
        value["generation"] = serde_json::json!(u64::MAX);
        variants.push(value);
        for value in variants {
            assert!(!schemas_accept(&value));
            assert!(serde_json::from_value::<ReadSnapshot>(value).is_err());
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Eligibility {
    Current,
    Historical,
    Stale,
    Invalid,
    Withdrawn,
    Unsupported,
}
