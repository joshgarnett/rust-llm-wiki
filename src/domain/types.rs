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
            Self::ContentConflict | Self::CursorStale | Self::FreshnessConflict => 4,
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
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub hint: Option<String>,
    pub details: Value,
}

impl WikiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadSnapshot {
    pub generation: u64,
    pub parser_fingerprint: Blake3Hash,
    pub control_manifest: Blake3Hash,
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
