//! Ownership of generated, noncanonical saved-note displays.
use super::{
    Blake3Hash, CanonicalRecord, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath,
    WikiError,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const PRESENTATION_PREFIX: &str = "sources/views/";
pub(crate) const MAX_PRESENTATION_BYTES: usize = 64 * 1024;
pub(crate) const MAX_PUBLISHED_CAPTURES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourcePresentation {
    pub version: u32,
    pub source_id: RecordId,
    pub current: CurrentSourcePresentation,
    pub published: BTreeMap<RecordId, Option<SavedSourcePresentation>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CurrentSourcePresentation {
    pub path: VaultRelativePath,
    pub revision_id: RecordId,
    pub content_hash: Option<Blake3Hash>,
    pub generated_hash: Blake3Hash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedSourcePresentation {
    pub path: VaultRelativePath,
    pub generated_hash: Blake3Hash,
}

pub(crate) fn presentation_path(path: &VaultRelativePath, saved: bool) -> bool {
    let Some(tail) = path.as_str().strip_prefix(PRESENTATION_PREFIX) else {
        return false;
    };
    let tail = if saved {
        let Some(tail) = tail.strip_prefix("saved/") else {
            return false;
        };
        tail
    } else {
        tail
    };
    !tail.contains('/') && tail.ends_with(".md") && tail.len() > 3
}

fn identity_suffix(path: &VaultRelativePath, id: &RecordId) -> bool {
    let hash = Blake3Hash::digest(id.as_str().as_bytes());
    let hex = hash.hex();
    [12, 20, 32, 64]
        .into_iter()
        .any(|length| path.as_str().ends_with(&format!("-{}.md", &hex[..length])))
}
impl SourcePresentation {
    pub(crate) fn decode(encoded: &str) -> Result<Self> {
        if encoded.len() > MAX_PRESENTATION_BYTES {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "saved-note metadata exceeds 64 KiB",
            ));
        }
        let value: Self = crate::changes::prepare::strict_json(encoded.as_bytes())?;
        value.validate_shape()?;
        Ok(value)
    }
    pub(crate) fn encode(&self) -> Result<String> {
        self.validate_shape()?;
        let encoded = serde_json::to_string(self).map_err(|e| WikiError::invalid(e.to_string()))?;
        if encoded.len() > MAX_PRESENTATION_BYTES {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "saved-note metadata exceeds 64 KiB; no published navigation was removed",
            ));
        }
        Ok(encoded)
    }
    pub(crate) fn from_record(source: &CanonicalRecord) -> Result<Option<Self>> {
        source
            .string("wiki_presentation")
            .map(Self::decode)
            .transpose()
    }
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.version != 1
            || self.published.len() > MAX_PUBLISHED_CAPTURES
            || !presentation_path(&self.current.path, false)
        {
            return Err(WikiError::invalid(
                "invalid saved-note presentation version, size or current path",
            ));
        }
        let mut paths = BTreeSet::from([unicase::UniCase::unicode(self.current.path.as_str())]);
        for saved in self.published.values().flatten() {
            if !presentation_path(&saved.path, true)
                || !paths.insert(unicase::UniCase::unicode(saved.path.as_str()))
            {
                return Err(WikiError::invalid(
                    "saved-note presentation paths overlap or leave the reserved namespace",
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn validate_owner(&self, source: &CanonicalRecord) -> Result<()> {
        self.validate_shape()?;
        let retained: BTreeSet<_> = source
            .field("wiki_revisions")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect();
        if source.kind() != RecordKind::Source
            || source.id() != &self.source_id
            || source.string("wiki_current_revision") != Some(self.current.revision_id.as_str())
            || !retained.contains(self.current.revision_id.as_str())
            || self
                .published
                .keys()
                .any(|id| !retained.contains(id.as_str()))
            || !identity_suffix(&self.current.path, source.id())
            || self.published.iter().any(|(id, saved)| {
                saved
                    .as_ref()
                    .is_some_and(|saved| !identity_suffix(&saved.path, id))
            })
        {
            return Err(WikiError::invalid(
                "saved-note metadata differs from its owning Source or retained captures",
            ));
        }
        Ok(())
    }
}
