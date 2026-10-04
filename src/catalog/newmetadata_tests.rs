//! Fixed user-level bootstrap facts and failure propagation, without publication.
use super::*;
use crate::{
    catalog::{
        scan,
        types::{
            DocumentRow, GraphRow, IdentityClaimRow, LinkRow, NormalizedValidationProjection,
            RetrievalSink,
        },
    },
    domain::Blake3Hash,
    vault::{VaultFs, VaultRoot},
};
use std::{fs, path::Path};

const VAULT: &str = "vault_00000000-0000-7000-8000-00000000001b";
const SOURCE: &str = "source_00000000-0000-7000-8000-000000000005";
const FORWARD: &str = "assertion_00000000-0000-7000-8000-00000000000a";
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let source = entry.unwrap().path();
        let target = to.join(source.file_name().unwrap());
        if source.is_dir() {
            copy(&source, &target)
        } else {
            fs::copy(source, target).unwrap();
        }
    }
}
struct Discard;
impl RetrievalSink for Discard {
    fn identity_claim(&mut self, _: IdentityClaimRow) -> Result<()> {
        Ok(())
    }
    fn document(&mut self, _: DocumentRow) -> Result<()> {
        Ok(())
    }
    fn graph(&mut self, _: GraphRow) -> Result<()> {
        Ok(())
    }
    fn link(&mut self, _: LinkRow) -> Result<()> {
        Ok(())
    }
}
fn fixture() -> (tempfile::TempDir, NormalizedValidationProjection) {
    let temp = tempfile::tempdir().unwrap();
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bootstrap/vault"),
        temp.path(),
    );
    let forward = temp.path().join("knowledge/assertions/forward.md");
    let bytes = fs::read_to_string(&forward).unwrap().replace("wiki_status: \"accepted\"\n",
        "wiki_status: \"accepted\"\nwiki_evidence: ['[[revision]]', '[[revision]]', '[[absent.md]]']\n");
    fs::write(forward, bytes).unwrap();
    let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let input = scan::scan_input(&handle, &id(VAULT)).unwrap();
    let projection =
        scan::project_normalized_with_sink(&handle, &input, false, &mut Discard).unwrap();
    (temp, projection)
}
#[derive(Default)]
struct Collector {
    rows: Vec<(String, String)>,
    revisions: Vec<(String, String, usize)>,
    evidence: Vec<String>,
    navigation: Vec<String>,
    progress: usize,
    fail_progress: Option<usize>,
    fail_row: bool,
}
impl MetadataSink for Collector {
    fn progress(&mut self) -> Result<()> {
        self.progress += 1;
        if self.fail_progress == Some(self.progress) {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "fixed metadata progress refusal",
            ));
        }
        Ok(())
    }
    fn row(&mut self, row: MetadataRow<'_>) -> Result<()> {
        if self.fail_row {
            return Err(WikiError::new(
                ErrorCode::Internal,
                "fixed metadata callback refusal",
            ));
        }
        let (kind, owner) = match row {
            MetadataRow::AssertionNavigation { assertion, key } => {
                if assertion.as_str() == FORWARD {
                    self.navigation.push(format!(
                        "{}:{}",
                        crate::catalog::normalized_fact_delta::key_name(key.kind),
                        key.value
                    ));
                }
                ("navigation", assertion.as_str())
            }
            MetadataRow::Opposition { assertion, .. } => ("opposition", assertion.as_str()),
            MetadataRow::Baseline { id, .. } => ("baseline", id.as_str()),
            MetadataRow::DirectPath { owner, .. } => ("direct", owner.as_str()),
            MetadataRow::SemanticEdge(edge) => ("edge", edge.owner_id.as_str()),
            MetadataRow::SourceRevision {
                source,
                revision,
                ordinal,
                ..
            } => {
                self.revisions
                    .push((source.to_string(), revision.to_string(), ordinal));
                ("revision", source.as_str())
            }
            MetadataRow::SourceEvidence {
                source, evidence, ..
            } => {
                self.evidence.push(evidence.to_string());
                ("evidence", source)
            }
        };
        self.rows.push((kind.into(), owner.into()));
        Ok(())
    }
}

#[test]
fn real_bootstrap_metadata_preserves_navigation_dedup_order_and_all_seven_families() {
    let (_temp, projection) = fixture();
    let mut actual = Collector::default();
    visit_eligibility_rows(&projection.validation, &projection.facts, &mut actual).unwrap();
    visit_refresh_rows(&projection.validation, &mut actual).unwrap();
    assert_eq!(
        actual.navigation,
        [
            "path:revision",
            "path:revision.md",
            "basename:revision",
            "alias:revision",
            "path:absent.md",
            "basename:absent",
            "alias:absent",
            "alias:absent.md"
        ]
    );
    assert_eq!(
        actual.revisions,
        vec![
            (
                SOURCE.into(),
                "revision_00000000-0000-7000-8000-000000000007".into(),
                0
            ),
            (
                SOURCE.into(),
                "revision_00000000-0000-7000-8000-000000000008".into(),
                1
            ),
            (
                "source_00000000-0000-7000-8000-000000000006".into(),
                "revision_00000000-0000-7000-8000-000000000009".into(),
                0
            ),
        ]
    );
    assert_eq!(actual.evidence.len(), 9);
    for family in [
        "navigation",
        "opposition",
        "baseline",
        "direct",
        "edge",
        "revision",
        "evidence",
    ] {
        assert!(
            actual.rows.iter().any(|(kind, _)| kind == family),
            "{family}"
        );
    }
    let forward: Vec<_> = actual
        .rows
        .iter()
        .filter(|(_, owner)| owner == FORWARD)
        .map(|(kind, _)| kind.as_str())
        .collect();
    assert_eq!(
        &forward[..11],
        [
            "navigation",
            "navigation",
            "navigation",
            "navigation",
            "navigation",
            "navigation",
            "navigation",
            "navigation",
            "opposition",
            "baseline",
            "direct"
        ]
    );
}

#[test]
fn refresh_lookup_policy_skips_invalid_source_but_retains_invalid_evidence() {
    let (_temp, mut projection) = fixture();
    projection
        .validation
        .records
        .get_mut(&id(SOURCE))
        .unwrap()
        .eligibility = Eligibility::Invalid;
    let evidence = id("evidence_00000000-0000-7000-8000-000000000013");
    projection
        .validation
        .records
        .get_mut(&evidence)
        .unwrap()
        .eligibility = Eligibility::Invalid;
    let mut actual = Collector::default();
    visit_refresh_rows(&projection.validation, &mut actual).unwrap();
    assert_eq!(actual.revisions.len(), 1);
    assert_eq!(
        actual.revisions[0].1,
        "revision_00000000-0000-7000-8000-000000000009"
    );
    assert_eq!(actual.evidence.len(), 9);
    assert!(actual.evidence.contains(&evidence.to_string()));
}

#[test]
fn visitor_poison_and_row_failure_stop_before_later_metadata_is_emitted() {
    let (_temp, projection) = fixture();
    let mut progress = Collector {
        fail_progress: Some(2),
        ..Collector::default()
    };
    assert_eq!(
        visit_eligibility_rows(&projection.validation, &projection.facts, &mut progress)
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
    assert!(progress.rows.is_empty());
    let mut row = Collector {
        fail_row: true,
        ..Collector::default()
    };
    assert_eq!(
        visit_refresh_rows(&projection.validation, &mut row)
            .unwrap_err()
            .code,
        ErrorCode::Internal
    );
    assert!(row.rows.is_empty());
}

#[test]
fn inconsistent_observation_and_missing_revision_refuse_instead_of_omitting_metadata() {
    let (_temp, mut projection) = fixture();
    let path = projection.facts.observed.keys().next().unwrap().clone();
    projection.facts.observed.insert(
        path,
        ExpectedState::Hash(Blake3Hash::digest(b"different observation")),
    );
    let mut sink = Collector::default();
    assert_eq!(
        visit_eligibility_rows(&projection.validation, &projection.facts, &mut sink)
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    assert!(sink.rows.is_empty());
    projection
        .validation
        .records
        .remove(&id("revision_00000000-0000-7000-8000-000000000007"));
    assert_eq!(
        visit_refresh_rows(&projection.validation, &mut sink)
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    assert!(sink.rows.is_empty());
}
