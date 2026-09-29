//! Reproducible nonsecret space identity. Credentials and budgets are never identities.
use crate::{
    config::providers::{Dimensions, TrustedService},
    domain::*,
};
use serde::{Deserialize, Serialize};

pub const RENDER_VERSION: &str = "render-v1";
pub const SEGMENT_VERSION: &str = "segment-v2";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmbeddingSettings {
    pub document_prefix: String,
    pub query_prefix: String,
    pub max_input_bytes: usize,
    pub quality_target_bytes: Option<usize>,
}
impl Default for EmbeddingSettings {
    fn default() -> Self {
        Self {
            document_prefix: String::new(),
            query_prefix: String::new(),
            max_input_bytes: 12_000,
            quality_target_bytes: None,
        }
    }
}
impl EmbeddingSettings {
    pub fn validate(&self) -> Result<()> {
        if self.max_input_bytes == 0
            || self.max_input_bytes > 128 * 1024
            || self
                .quality_target_bytes
                .is_some_and(|n| n == 0 || n > self.max_input_bytes)
            || self.document_prefix.len() > 128 * 1024
            || self.query_prefix.len() > 128 * 1024
        {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "embedding byte bounds/prefixes invalid",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceSpec {
    pub version: u32,
    pub endpoint_fingerprint: Blake3Hash,
    pub profile_id: String,
    pub service_id: String,
    pub model: String,
    pub revision: Option<String>,
    /// None hashes as auto; established corpus dimension lives outside this identity.
    pub dimensions: Option<u32>,
    pub metric: String,
    pub normalization: String,
    pub render_version: String,
    pub settings: EmbeddingSettings,
}
impl SpaceSpec {
    pub fn from_service(service: &TrustedService, settings: EmbeddingSettings) -> Result<Self> {
        settings.validate()?;
        if service.summary().capability != crate::jobs::Capability::Embed {
            return Err(WikiError::new(
                ErrorCode::ProfileUntrusted,
                "embedding service required",
            ));
        }
        let summary = service.summary();
        Ok(Self {
            version: 1,
            endpoint_fingerprint: summary.endpoint_fingerprint,
            profile_id: summary.profile_id,
            service_id: summary.service_id,
            model: summary
                .model
                .ok_or_else(|| WikiError::invalid("embedding model missing"))?,
            revision: summary.revision,
            dimensions: match service.service().dimensions {
                Some(Dimensions::Fixed(n)) => Some(n),
                _ => None,
            },
            metric: "cosine".into(),
            normalization: "float64-l2-to-f32-le-v1".into(),
            render_version: RENDER_VERSION.into(),
            settings,
        })
    }
    pub fn id(&self) -> Result<Blake3Hash> {
        self.settings.validate()?;
        if self.version != 1
            || self.metric != "cosine"
            || self.normalization != "float64-l2-to-f32-le-v1"
            || self.render_version != RENDER_VERSION
            || self.model.is_empty()
            || self.dimensions.is_some_and(|n| n == 0 || n > 65536)
        {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "unreproducible embedding space",
            ));
        }
        // Segmentation policy changes only actual inputs, not compatible request roles.
        let bytes = crate::graph::packet::canonical_json(&(
            "lwiki-space-v1",
            &self.endpoint_fingerprint,
            &self.profile_id,
            &self.service_id,
            &self.model,
            &self.revision,
            self.dimensions,
            "cosine",
            &self.settings.document_prefix,
            &self.settings.query_prefix,
            &self.normalization,
            &self.render_version,
        ))?;
        Ok(Blake3Hash::digest(bytes))
    }
    pub fn require_service(&self, service: &TrustedService) -> Result<()> {
        let actual = Self::from_service(service, self.settings.clone())?;
        if actual.id()? != self.id()? {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "active space requires its retained endpoint/model/revision/dimension/profile configuration",
            ));
        }
        Ok(())
    }
    pub fn query(&self, text: &str) -> Result<crate::providers::types::EmbeddingInput> {
        if text.trim().is_empty() || text.len() > 4096 {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "query must contain 1..4096 UTF-8 bytes",
            ));
        }
        let utf8 = format!("{}{}", self.settings.query_prefix, text);
        if utf8.len() > self.settings.max_input_bytes {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "query prefix and text exceed input bound",
            ));
        }
        Ok(crate::providers::types::EmbeddingInput {
            input_hash: Blake3Hash::digest(utf8.as_bytes()),
            utf8,
        })
    }
}
