//! One private permission for automatic embedding text, independent of role grants.
use super::*;
use crate::config::DefaultWiki;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticEmbeddingBindingChange {
    Vault,
    Profile,
    Endpoint,
    Model,
    Revision,
    Dimensions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticEmbeddingConsent {
    Absent,
    Matching,
    BindingChanged(AutomaticEmbeddingBindingChange),
}

/// A caller must display the final destination, model and automatic text scope
/// before choosing Reviewed. Preserve never creates consent from role grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderAutomaticEmbeddingDecision {
    Preserve,
    Reviewed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum AutomaticEmbeddingScope {
    EligibleWikiTextAndSearchQueries,
}

/// Private persisted consent. Auth, price and generation settings are excluded.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::config) struct AutomaticEmbeddingGrant {
    version: u32,
    profile: String,
    endpoint_fingerprint: Blake3Hash,
    model: Option<String>,
    revision: Option<String>,
    dimensions: Option<Dimensions>,
    scope: AutomaticEmbeddingScope,
}

impl AutomaticEmbeddingGrant {
    pub(super) fn validate(&self, profiles: &BTreeMap<String, ProfileConfig>) -> Result<()> {
        if self.version != 1
            || profiles
                .get(&self.profile)
                .is_none_or(|p| p.embedding.is_none())
            || self
                .dimensions
                .as_ref()
                .is_some_and(|dimensions| match dimensions {
                    Dimensions::Fixed(value) => !(1..=65_536).contains(value),
                    Dimensions::Auto(value) => value != "auto",
                })
        {
            return Err(config_error("invalid automatic embedding permission"));
        }
        Ok(())
    }

    fn mismatch(&self, candidate: &Self) -> Option<AutomaticEmbeddingBindingChange> {
        use AutomaticEmbeddingBindingChange::*;
        if self.endpoint_fingerprint != candidate.endpoint_fingerprint {
            Some(Endpoint)
        } else if self.model != candidate.model {
            Some(Model)
        } else if self.revision != candidate.revision {
            Some(Revision)
        } else if self.dimensions != candidate.dimensions {
            Some(Dimensions)
        } else {
            None
        }
    }

    /// Profile IDs may change during copy-on-write credential edits. This
    /// comparison permits transfer only after original profile/root/ID consent
    /// has independently matched, and includes the fixed text scope/version.
    pub(in crate::config) fn same_text_identity(&self, candidate: &Self) -> bool {
        self.version == candidate.version
            && self.scope == candidate.scope
            && self.mismatch(candidate).is_none()
    }
}

impl ProviderConfig {
    /// Inspect permission without reading keys, resolving environment values or
    /// executing a helper. The current private bytes, wiki and CA remain guarded.
    pub fn automatic_embedding_consent(
        &self,
        fs: &VaultFs,
        vault_id: &RecordId,
        profile: &str,
    ) -> Result<AutomaticEmbeddingConsent> {
        if Blake3Hash::digest(checked_file(&self.path, MAX_PROVIDER_CONFIG_BYTES, true)?)
            != self.content_guard
        {
            return Err(untrusted());
        }
        marker(fs, vault_id)?;
        self.consent_at(fs.root().path(), vault_id, profile, true)
    }

    /// This authority always constructs an embedding service. An automatic
    /// binding with no allowed_profiles cannot authorize generation.
    pub fn authorize_automatic_embeddings(
        &self,
        fs: &VaultFs,
        vault_id: &RecordId,
        profile: &str,
    ) -> Result<TrustedService> {
        if self.automatic_embedding_consent(fs, vault_id, profile)?
            != AutomaticEmbeddingConsent::Matching
        {
            return Err(untrusted());
        }
        self.trusted_service(
            fs,
            vault_id,
            profile,
            Capability::Embed,
            marker(fs, vault_id)?,
        )
    }

    fn consent_at(
        &self,
        root: &Path,
        wiki_id: &RecordId,
        profile: &str,
        canonicalize: bool,
    ) -> Result<AutomaticEmbeddingConsent> {
        use AutomaticEmbeddingBindingChange::*;
        use AutomaticEmbeddingConsent::*;
        let mut changed = None;
        for binding in self.bindings.values() {
            let Some(grant) = &binding.automatic_embedding else {
                continue;
            };
            let root_matches = if canonicalize {
                std::fs::canonicalize(&binding.root).ok().as_deref() == Some(root)
            } else {
                binding.root == root
            };
            if binding.wiki_id != *wiki_id || !root_matches {
                // A grant for another wiki cannot imply permission here.
                if grant.profile == profile {
                    changed.get_or_insert(Vault);
                }
                continue;
            }
            if grant.profile != profile {
                changed = Some(Profile);
                continue;
            }
            let candidate = self.automatic_grant_for_profile(profile)?;
            match grant.mismatch(&candidate) {
                None => return Ok(Matching),
                Some(reason) => changed = Some(reason),
            }
        }
        Ok(changed.map(BindingChanged).unwrap_or(Absent))
    }

    pub(in crate::config) fn from_planned_bytes(bytes: &[u8]) -> Result<Self> {
        Self::from_bytes(PathBuf::new(), bytes)
    }

    /// Planned Matching describes only the proposed private descriptor. Fresh
    /// setup may plan before its wiki exists; runtime must validate its marker,
    /// canonical root, unchanged config bytes and CA separately before admission.
    pub(in crate::config) fn planned_automatic_embedding_consent(
        &self,
        wiki: &DefaultWiki,
        profile: &str,
    ) -> Result<AutomaticEmbeddingConsent> {
        self.consent_at(&wiki.path, &wiki.wiki_id, profile, false)
    }

    pub(in crate::config) fn automatic_grant_for_profile(
        &self,
        profile: &str,
    ) -> Result<AutomaticEmbeddingGrant> {
        let id = self
            .profiles
            .get(profile)
            .and_then(|p| p.embedding.as_ref())
            .ok_or_else(|| {
                config_error("automatic embedding permission requires an embedding service")
            })?;
        let service = self.services.get(id).ok_or_else(untrusted)?;
        check_ca(service)?;
        Ok(AutomaticEmbeddingGrant {
            version: 1,
            profile: profile.into(),
            endpoint_fingerprint: service.endpoint_fingerprint.clone(),
            model: service.model.clone(),
            revision: service.revision.clone(),
            dimensions: service.dimensions.clone(),
            scope: AutomaticEmbeddingScope::EligibleWikiTextAndSearchQueries,
        })
    }
}

#[cfg(test)]
#[path = "providers_automatic_tests.rs"]
mod tests;
