//! Direct observed facts for normalized serving. These do not constitute a
//! selected proof or permission to run the full validator over a partial graph.
use super::{structural_rules::StructuralFact, types::RecordRow};
use crate::{
    domain::{Eligibility, ErrorCode, RecordId, Result, VaultRelativePath, WikiError},
    sources::revision::canonical_path,
    vault::ExpectedState,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EligibilityBaseline {
    pub eligibility: Eligibility,
    pub reasons: Vec<String>,
    pub identity_eligibility: Option<Eligibility>,
    pub description_eligibility: Option<Eligibility>,
    pub disputed: bool,
}
impl EligibilityBaseline {
    fn from_row(row: &RecordRow) -> Self {
        let mut reasons = row.reasons.clone();
        reasons.sort();
        reasons.dedup();
        Self {
            eligibility: row.eligibility,
            reasons,
            identity_eligibility: row.identity_eligibility,
            description_eligibility: row.description_eligibility,
            disputed: row.disputed,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EligibilityFact {
    /// Policy and structural state before lifecycle/support computation. Evidence
    /// integrity is retained independently even though verified later in compute.
    pub baseline: EligibilityBaseline,
    /// Actual structural-stage effects; mandatory in the new proof layout.
    pub structural: StructuralFact,
    /// Own canonical path and noncanonical observed assets/absences only.
    /// Related notes are resolved by typed edges, not stale captured head paths.
    /// Resolve expected states from the same pinned epoch, never copied hashes.
    pub direct_paths: BTreeSet<VaultRelativePath>,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum EligibilityRole {
    /// Field names come only from eligibility::references's fixed vocabulary.
    TypedReference {
        field: String,
    },
    DeclaredSupport,
    /// Retained membership is inventory, never a recursive payload proof edge.
    SourceInventory,
    ExtractionSource,
    ExtractionRevision,
    DecisionInput,
    DecisionOutput,
    /// A support edge, not structural-invalid propagation from damaged evidence.
    AssertionEvidence,
    GenerationPacket,
    PolicySupersession,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EligibilityEdge {
    pub owner_id: RecordId,
    pub target_id: RecordId,
    pub role: EligibilityRole,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NormalizedEligibilityFacts {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<super::policy_facts::NormalizedPolicyFacts>,
    pub records: BTreeMap<RecordId, EligibilityFact>,
    pub observed: BTreeMap<VaultRelativePath, ExpectedState>,
    pub edges: BTreeSet<EligibilityEdge>,
}
impl NormalizedEligibilityFacts {
    pub(super) fn new() -> Self {
        Self {
            version: 2,
            policy: None,
            records: BTreeMap::new(),
            observed: BTreeMap::new(),
            edges: BTreeSet::new(),
        }
    }
    pub(super) fn edge(&mut self, owner: &RecordId, target: &RecordId, role: EligibilityRole) {
        self.edges.insert(EligibilityEdge {
            owner_id: owner.clone(),
            target_id: target.clone(),
            role,
        });
    }
    pub(super) fn capture_baseline(&mut self, records: &BTreeMap<RecordId, RecordRow>) {
        self.records = records
            .iter()
            .map(|(id, row)| {
                (
                    id.clone(),
                    EligibilityFact {
                        baseline: EligibilityBaseline::from_row(row),
                        structural: StructuralFact::default(),
                        direct_paths: BTreeSet::new(),
                    },
                )
            })
            .collect();
    }
    pub(super) fn install_structural(
        &mut self,
        mut effects: BTreeMap<RecordId, StructuralFact>,
    ) -> Result<()> {
        for (id, fact) in &mut self.records {
            fact.structural = effects.remove(id).unwrap_or_default();
            if fact.structural.baseline() != fact.baseline {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    format!("structural provenance differs from computed baseline for {id}"),
                ));
            }
        }
        if !effects.is_empty() {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "structural effect owner absent",
            ));
        }
        Ok(())
    }
    pub(super) fn evidence_integrity_failure(&mut self, id: &RecordId) {
        let baseline = &mut self
            .records
            .get_mut(id)
            .expect("captured baseline")
            .baseline;
        baseline.eligibility = Eligibility::Invalid;
        baseline.reasons.push("evidence_integrity".into());
        baseline.reasons.sort();
        baseline.reasons.dedup();
    }
    pub(super) fn capture_direct(
        &mut self,
        records: &mut BTreeMap<RecordId, RecordRow>,
    ) -> Result<()> {
        // Validate all observations first. A failed fact capture leaves original
        // record dependencies intact rather than yielding a partial layout.
        for row in records.values() {
            for dependency in &row.dependencies {
                if self
                    .observed
                    .get(&dependency.path)
                    .is_some_and(|old| old != &dependency.expected)
                {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        format!("normalized observed state changed: {}", dependency.path),
                    ));
                }
                self.observed
                    .insert(dependency.path.clone(), dependency.expected.clone());
            }
        }
        for (id, row) in records {
            let fact = self.records.get_mut(id).expect("captured baseline");
            fact.direct_paths.extend(
                std::mem::take(&mut row.dependencies)
                    .into_iter()
                    .map(|dependency| dependency.path)
                    .filter(|path| path == &row.path || !canonical_path(path)),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::{eligibility, scan},
        changes::{ChangeDraft, ReadDependency},
        domain::Blake3Hash,
        sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore, SourceView},
        vault::{VaultFs, VaultRoot},
    };
    use std::fs;
    fn id(value: &str) -> RecordId {
        RecordId::new(value).unwrap()
    }
    fn path(value: &str) -> VaultRelativePath {
        VaultRelativePath::new(value).unwrap()
    }
    fn write(fs_handle: &VaultFs, name: &str, bytes: &[u8]) {
        let target = fs_handle.root().path().join(name);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    fn seed(fs_handle: &VaultFs, draft: ChangeDraft) {
        for operation in draft.operations {
            write(
                fs_handle,
                operation.target.as_str(),
                &operation.proposed.unwrap(),
            );
        }
    }
    fn request(text: &[u8]) -> CaptureRequest {
        CaptureRequest {
            title: "Fact fixture".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture.txt".into(),
            original: text.to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        }
    }
    fn fixture() -> (tempfile::TempDir, VaultFs, RecordId, RecordId, RecordId) {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            b"---\nwiki_schema: '1'\nwiki_id: vault_facts\nwiki_kind: vault\ntitle: Facts\n---\n",
        )
        .unwrap();
        let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let store = SourceStore::new(fs_handle.clone());
        let first = store
            .plan_capture(request(b"first immutable text"))
            .unwrap();
        seed(&fs_handle, first.draft.unwrap());
        let second = store
            .plan_refresh(&first.source_id, request(b"second immutable text"))
            .unwrap();
        seed(&fs_handle, second.draft.unwrap());
        write(&fs_handle, "unrelated.md", b"---\nwiki_schema: '1'\nwiki_id: page_unrelated\nwiki_kind: page\ntitle: Unrelated\nwiki_status: reviewed\n---\nUnaffected prose.\n");
        (
            temp,
            fs_handle,
            first.source_id,
            first.revision_id,
            second.revision_id,
        )
    }
    fn compute(fs_handle: &VaultFs) -> (BTreeMap<RecordId, RecordRow>, NormalizedEligibilityFacts) {
        let input = scan::scan_input(fs_handle, &id("vault_facts")).unwrap();
        let notes = scan::input_notes(&input).unwrap();
        let rows: BTreeMap<_, _> = notes
            .iter()
            .filter_map(|(path, note)| {
                note.canonical.as_ref().map(|record| {
                    (
                        record.id().clone(),
                        RecordRow {
                            record: record.clone(),
                            path: path.clone(),
                            hash: note.source_hash.clone(),
                            authored_status: record.string("wiki_status").map(str::to_owned),
                            eligibility: Eligibility::Current,
                            reasons: vec![],
                            identity_eligibility: None,
                            description_eligibility: None,
                            disputed: false,
                            dependencies: vec![ReadDependency {
                                path: path.clone(),
                                expected: ExpectedState::Hash(note.source_hash.clone()),
                            }],
                        },
                    )
                })
            })
            .collect();
        let view = SourceView::from_input(fs_handle, &input).unwrap();
        let mut legacy = rows.clone();
        let mut legacy_diagnostics = vec![];
        eligibility::compute(&view, &notes, &mut legacy, &mut legacy_diagnostics).unwrap();
        let mut normalized = rows;
        let mut diagnostics = vec![];
        let facts =
            eligibility::compute_normalized(&view, &notes, &mut normalized, &mut diagnostics)
                .unwrap();
        assert_eq!(diagnostics, legacy_diagnostics);
        for (id, row) in &normalized {
            assert!(row.dependencies.is_empty());
            assert_eq!(
                facts.records[id].structural.baseline(),
                facts.records[id].baseline
            );
            let retained = serde_json::to_vec(&facts.records[id].structural).unwrap();
            let decoded: StructuralFact = serde_json::from_slice(&retained).unwrap();
            assert_eq!(decoded, facts.records[id].structural);
            for emitted in decoded.diagnostics(row) {
                assert!(
                    diagnostics.contains(&emitted),
                    "retained structural diagnostic differs for {id}"
                );
            }
            let mut original = legacy[id].clone();
            original.dependencies.clear();
            assert_eq!(
                row, &original,
                "same eligibility and authored state for {id}"
            );
        }
        (normalized, facts)
    }

    #[test]
    fn historical_revision_keeps_only_own_note_and_assets_not_captured_head_paths() {
        let (_temp, fs_handle, source, first, second) = fixture();
        let (records, facts) = compute(&fs_handle);
        let row = &records[&first];
        assert_eq!(row.eligibility, Eligibility::Historical);
        assert_eq!(
            facts.records[&first].baseline.eligibility,
            Eligibility::Current
        );
        assert!(
            !facts.records[&first]
                .baseline
                .reasons
                .iter()
                .any(|reason| reason == "older_revision")
        );
        let expected: BTreeSet<_> = ["revision.md", "original.bin", "content.md"]
            .into_iter()
            .map(|name| path(&format!("sources/{source}/revisions/{first}/{name}")))
            .collect();
        assert_eq!(facts.records[&first].direct_paths, expected);
        assert!(
            !facts.records[&first]
                .direct_paths
                .contains(&records[&source].path)
        );
        assert!(
            !facts.records[&first]
                .direct_paths
                .contains(&records[&second].path)
        );
        assert!(facts.observed.contains_key(&records[&source].path));
        assert!(facts.observed.contains_key(&records[&second].path));
        assert!(facts.edges.contains(&EligibilityEdge {
            owner_id: source.clone(),
            target_id: first,
            role: EligibilityRole::SourceInventory
        }));
        assert!(facts.edges.contains(&EligibilityEdge {
            owner_id: source,
            target_id: second,
            role: EligibilityRole::TypedReference {
                field: "wiki_current_revision".into()
            }
        }));
    }

    #[test]
    fn unrelated_decisions_and_retained_history_do_not_expand_direct_record_proof() {
        let (_temp, fs_handle, source, first, _) = fixture();
        let (_, before) = compute(&fs_handle);
        write(&fs_handle, "entity.md", b"---\nwiki_schema: '1'\nwiki_id: entity_fixture\nwiki_kind: entity\ntitle: Fixture\nwiki_status: active\nwiki_entity_type: component\n---\n");
        write(&fs_handle, "claim.md", b"---\nwiki_schema: '1'\nwiki_id: assertion_fixture\nwiki_kind: assertion\ntitle: Fixture claim\nwiki_status: accepted\nwiki_subject_id: entity_fixture\nwiki_object_id: entity_fixture\nwiki_predicate: uses\n---\n");
        for n in 0..24 {
            write(&fs_handle, &format!("decisions/{n}.md"), format!("---\nwiki_schema: '1'\nwiki_id: decision_{n}\nwiki_kind: decision\ntitle: Decision {n}\nwiki_status: active\nwiki_action: accept\nwiki_created_at: '2026-09-28T00:00:00Z'\nwiki_input_ids: [assertion_fixture]\nwiki_output_ids: [assertion_fixture]\n---\n").as_bytes());
        }
        let store = SourceStore::new(fs_handle.clone());
        for n in 0..4 {
            let next = store
                .plan_refresh(&source, request(format!("later revision {n}").as_bytes()))
                .unwrap();
            seed(&fs_handle, next.draft.unwrap());
        }
        let (_, after) = compute(&fs_handle);
        assert_eq!(
            before.records[&id("page_unrelated")],
            after.records[&id("page_unrelated")]
        );
        assert_eq!(before.records[&first], after.records[&first]);
        assert!(
            after
                .edges
                .iter()
                .all(|edge| edge.owner_id != id("page_unrelated"))
        );
        assert_eq!(
            after.records[&id("page_unrelated")].direct_paths,
            BTreeSet::from([path("unrelated.md")])
        );
        assert!(after.observed.len() > before.observed.len());
    }

    #[test]
    fn missing_assets_remain_direct_absence_facts_and_structural_baseline_is_invalid() {
        let (_temp, fs_handle, source, first, _) = fixture();
        let missing = path(&format!("sources/{source}/revisions/{first}/original.bin"));
        fs::remove_file(fs_handle.root().path().join(missing.as_str())).unwrap();
        let (rows, facts) = compute(&fs_handle);
        assert_eq!(rows[&first].eligibility, Eligibility::Invalid);
        assert_eq!(
            facts.records[&first].baseline.eligibility,
            Eligibility::Invalid
        );
        assert!(
            facts.records[&first]
                .baseline
                .reasons
                .iter()
                .any(|reason| reason == "revision_integrity")
        );
        assert!(facts.records[&first].direct_paths.contains(&missing));
        assert_eq!(facts.observed[&missing], ExpectedState::Absent);
    }

    #[test]
    fn conflicting_observed_state_refuses_before_clearing_dependencies() {
        let (_temp, fs_handle, _, _, _) = fixture();
        let (mut rows, _) = compute(&fs_handle);
        let mut facts = NormalizedEligibilityFacts::new();
        facts.capture_baseline(&rows);
        for (n, row) in rows.values_mut().take(2).enumerate() {
            row.dependencies = vec![ReadDependency {
                path: path("asset.bin"),
                expected: ExpectedState::Hash(Blake3Hash::digest(format!("{n}"))),
            }];
        }
        let original = rows.clone();
        assert_eq!(
            facts.capture_direct(&mut rows).unwrap_err().code,
            ErrorCode::ContentConflict
        );
        assert_eq!(rows, original);
    }
    #[test]
    fn evidence_integrity_is_static_but_lifecycle_and_support_are_not() {
        let (_temp, fs_handle, source, first, current) = fixture();
        write(&fs_handle, "entity.md", b"---\nwiki_schema: '1'\nwiki_id: entity_fixture\nwiki_kind: entity\ntitle: Fixture\nwiki_status: active\nwiki_entity_type: component\n---\n");
        write(&fs_handle, "claim.md", b"---\nwiki_schema: '1'\nwiki_id: assertion_fixture\nwiki_kind: assertion\ntitle: Fixture claim\nwiki_status: accepted\nwiki_subject_id: entity_fixture\nwiki_object_id: entity_fixture\nwiki_predicate: uses\n---\n");
        for (name, revision, quote) in [
            ("evidence_old", &first, b"first immutable text".as_slice()),
            (
                "evidence_current",
                &current,
                b"second immutable text".as_slice(),
            ),
            ("evidence_bad", &current, b"wrong quote".as_slice()),
        ] {
            let mut bytes = format!("---\nwiki_schema: '1'\nwiki_id: {name}\nwiki_kind: evidence\ntitle: {name}\nwiki_status: active\nwiki_assertion_id: assertion_fixture\nwiki_source_id: '{source}'\nwiki_source_revision: '{revision}'\nwiki_stance: supports\nwiki_locator_kind: utf8-bytes\nwiki_span_start: 0\nwiki_span_end: {}\nwiki_quote_hash: {}\n---\n", quote.len(), Blake3Hash::digest(quote)).into_bytes();
            bytes.extend(
                crate::sources::evidence::exact_quote_body(quote, "\n", "Fixture").unwrap(),
            );
            write(&fs_handle, &format!("{name}.md"), &bytes);
        }
        let (rows, facts) = compute(&fs_handle);
        assert_eq!(
            rows[&id("evidence_old")].eligibility,
            Eligibility::Historical
        );
        assert_eq!(
            facts.records[&id("evidence_old")].baseline.eligibility,
            Eligibility::Current
        );
        assert!(
            facts.records[&id("evidence_old")]
                .baseline
                .reasons
                .is_empty()
        );
        assert_eq!(
            facts.records[&id("evidence_bad")].baseline.eligibility,
            Eligibility::Invalid
        );
        assert!(
            facts.records[&id("evidence_bad")]
                .baseline
                .reasons
                .iter()
                .any(|reason| reason == "evidence_integrity")
        );
        assert_eq!(
            rows[&id("assertion_fixture")].eligibility,
            Eligibility::Current
        );
        assert!(
            rows[&id("assertion_fixture")]
                .reasons
                .iter()
                .any(|reason| reason == "current_support")
        );
        assert!(
            !facts.records[&id("assertion_fixture")]
                .baseline
                .reasons
                .iter()
                .any(|reason| reason == "current_support")
        );
        assert!(facts.edges.contains(&EligibilityEdge {
            owner_id: id("assertion_fixture"),
            target_id: id("evidence_bad"),
            role: EligibilityRole::AssertionEvidence
        }));
        assert!(facts.edges.contains(&EligibilityEdge {
            owner_id: id("evidence_old"),
            target_id: source,
            role: EligibilityRole::TypedReference {
                field: "wiki_source_id".into()
            }
        }));
    }
}
