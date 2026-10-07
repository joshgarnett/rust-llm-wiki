//! Explicit page authoring with per-file author hashes and one recoverable plan.
use super::{MAX_INPUT_BYTES, MutationOutcome, OfflineApp};
use crate::{changes::*, domain::*};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageUpdate {
    pub path: VaultRelativePath,
    pub markdown: String,
    #[serde(default)]
    pub if_match: Option<Blake3Hash>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageBatchRequest {
    pub title: String,
    pub pages: Vec<PageUpdate>,
    #[serde(default)]
    pub read_preconditions: Vec<ReadDependency>,
}
impl OfflineApp {
    /// Validate every proposal before staging; apply rechecks every author hash.
    pub fn page_batch(&self, request: PageBatchRequest) -> Result<MutationOutcome> {
        if request.pages.is_empty()
            || request.pages.len() > 16
            || request.title.trim().is_empty()
            || request.title.len() > 4096
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "page batch requires a title and 1–16 pages",
            ));
        }
        let total = request
            .pages
            .iter()
            .try_fold(0usize, |n, page| n.checked_add(page.markdown.len()))
            .ok_or_else(|| {
                WikiError::new(ErrorCode::BudgetExceeded, "page batch byte bound exceeded")
            })?;
        if total > MAX_INPUT_BYTES || request.read_preconditions.len() > 128 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "page batch exceeds 16 MiB or 128 dependencies",
            ));
        }
        let mut paths = BTreeSet::new();
        let mut identities = BTreeSet::new();
        let mut operations = Vec::new();
        for page in request.pages {
            if !paths.insert(page.path.clone()) {
                return Err(WikiError::invalid("page batch has duplicate targets"));
            }
            let operation =
                self.plan_page_write(page.path, page.markdown.into_bytes(), page.if_match)?;
            let note =
                crate::records::parse_note(operation.proposed.as_ref().expect("page proposal"));
            let id = note.canonical.expect("validated page").id().clone();
            if !identities.insert(id) {
                return Err(WikiError::invalid(
                    "page batch has duplicate page identities",
                ));
            }
            operations.push(operation);
        }
        self.execute_page_draft(ChangeDraft {
            title: request.title,
            origin: None,
            inverse_of: None,
            allocated_ids: BTreeMap::new(),
            read_preconditions: request.read_preconditions,
            operations,
        })
    }
    pub fn page_initialize(
        &self,
        path: Option<VaultRelativePath>,
        id: Option<RecordId>,
        title: String,
        body: String,
    ) -> Result<MutationOutcome> {
        let (path, id, markdown) = page_initial_proposal(path, id, title, body)?;
        let mut outcome = self.page_put(path.clone(), markdown, None)?;
        outcome.allocated_ids.insert(path.to_string(), id);
        Ok(outcome)
    }
}

/// Shared envelope assembly fixes the allocated destination before rendering citations.
pub(crate) fn page_initial_proposal(
    path: Option<VaultRelativePath>,
    id: Option<RecordId>,
    title: String,
    body: String,
) -> Result<(VaultRelativePath, RecordId, Vec<u8>)> {
    if title.trim().is_empty() || title.len() > 4096 {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "page title must contain 1–4096 bytes",
        ));
    }
    let id = id
        .map(Ok)
        .unwrap_or_else(|| RecordId::generate(RecordKind::Page))?;
    let record = CanonicalRecord::from_value(
        serde_json::json!({"wiki_schema":"1","wiki_id":id,"wiki_kind":"page","title":title,"wiki_status":"draft"}),
    )?;
    let mut markdown = String::from("---\n");
    for (key, value) in record.fields() {
        markdown.push_str(key);
        markdown.push_str(": ");
        markdown.push_str(
            &serde_json::to_string(value)
                .map_err(|_| WikiError::invalid("page envelope encoding"))?,
        );
        markdown.push('\n');
    }
    markdown.push_str("---\n");
    markdown.push_str(&body);
    let path = path.unwrap_or(VaultRelativePath::new(format!("pages/{id}.md"))?);
    Ok((path, id, markdown.into_bytes()))
}
