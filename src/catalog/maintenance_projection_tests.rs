//! Small real-input differential oracles; no publication or provider access.
use super::*;
use crate::{
    catalog::{maintenance_input::MaintenanceInput, maintenance_types::MaintenanceLimits},
    sources::{SourceInputReads, SourceNotes},
    vault::VaultRoot,
};
use std::{
    cell::{Cell, RefCell},
    fs,
    path::Path,
    time::Duration,
};
const VAULT: &str = "vault_00000000-0000-7000-8000-00000000001b";
const SOURCE: &str = "source_00000000-0000-7000-8000-000000000005";
const REVISION: &str = "revision_00000000-0000-7000-8000-000000000008";
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn payload(name: &str) -> String {
    format!("sources/{SOURCE}/revisions/{REVISION}/{name}")
}
fn limits() -> MaintenanceLimits {
    MaintenanceLimits {
        max_files: 10000,
        max_path_steps: 1000000,
        max_manifest_bytes: 64 * 1024 * 1024,
        max_file_bytes: 64 * 1024 * 1024,
        max_retained_note_bytes: 64 * 1024 * 1024,
        max_io_bytes: 1024 * 1024 * 1024,
        max_elapsed: Duration::from_secs(60),
    }
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
fn fixture() -> (tempfile::TempDir, VaultFs) {
    let temp = tempfile::tempdir().unwrap();
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bootstrap/vault"),
        temp.path(),
    );
    let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    (temp, handle)
}
#[derive(Default, Debug, PartialEq)]
struct Sink {
    events: Vec<String>,
    claims: Vec<IdentityClaimRow>,
    documents: Vec<DocumentRow>,
    graph: Vec<GraphRow>,
    links: Vec<LinkRow>,
    facts: Vec<super::super::link_facts::OwnedLinkFact>,
    keys: Vec<(
        RecordId,
        RecordKind,
        VaultRelativePath,
        Vec<String>,
        Vec<super::super::link_facts::MatchKey>,
    )>,
}
impl RetrievalSink for Sink {
    fn identity_claim(&mut self, row: IdentityClaimRow) -> Result<()> {
        self.events.push(format!("claim:{}:{}", row.id, row.path));
        self.claims.push(row);
        Ok(())
    }
    fn document(&mut self, row: DocumentRow) -> Result<()> {
        self.events.push(format!("document:{}", row.path));
        self.documents.push(row);
        Ok(())
    }
    fn graph(&mut self, row: GraphRow) -> Result<()> {
        self.events.push(format!("graph:{}", row.target_id));
        self.graph.push(row);
        Ok(())
    }
    fn link(&mut self, row: LinkRow) -> Result<()> {
        self.events
            .push(format!("link:{}:{}", row.from_path, row.byte_start));
        self.links.push(row);
        Ok(())
    }
    fn link_fact(&mut self, row: super::super::link_facts::OwnedLinkFact) -> Result<()> {
        self.events
            .push(format!("link_fact:{}:{}", row.from_path, row.byte_start));
        self.facts.push(row);
        Ok(())
    }
    fn registry_keys(
        &mut self,
        entry: &RegistryEntry,
        keys: &[super::super::link_facts::MatchKey],
    ) -> Result<()> {
        self.events.push(format!("registry:{}", entry.id));
        self.keys.push((
            entry.id.clone(),
            entry.kind,
            entry.path.clone(),
            entry.aliases.clone(),
            keys.to_vec(),
        ));
        Ok(())
    }
}
#[test]
fn maintenance_complete_projection_and_sink_match_real_normalized_oracle() {
    for variant in [
        "baseline",
        "mixed_claims",
        "missing_original",
        "missing_content",
        "binary_original",
        "changed_content",
        "nonutf8_content",
        "withdrawn",
    ] {
        let (temp, handle) = fixture();
        match variant {
            "mixed_claims" => {
                fs::write(
                    temp.path().join("plain.md"),
                    b"# Plain\n[[North Lab]] [[missing-path]]\n",
                )
                .unwrap();
                fs::write(temp.path().join("malformed.md"),b"---\nwiki_id: broken_maintenance\nwiki_kind: page\nwiki_status: [\n---\nMalformed\n").unwrap();
                fs::write(temp.path().join("invalid_utf8.md"), [0xff, 0xfe]).unwrap();
                fs::copy(
                    temp.path().join("knowledge/entities/north_lab.md"),
                    temp.path().join("duplicate.md"),
                )
                .unwrap();
                fs::write(temp.path().join("broken_ref.md"),b"---\nwiki_schema: '1'\nwiki_id: page_maintenance_missing\nwiki_kind: page\ntitle: Missing support\nwiki_status: reviewed\nwiki_depends_on_ids: [assertion_missing_maintenance]\n---\nBroken declared support\n").unwrap();
            }
            "missing_original" => {
                fs::remove_file(temp.path().join(payload("original.md"))).unwrap()
            }
            "missing_content" => fs::remove_file(temp.path().join(payload("content.md"))).unwrap(),
            "binary_original" => {
                fs::rename(
                    temp.path().join(payload("original.md")),
                    temp.path().join(payload("original.bin")),
                )
                .unwrap();
                let revision = temp.path().join(payload("revision.md"));
                let bytes = fs::read_to_string(&revision).unwrap().replace(
                    "wiki_original_path: \"original.md\"",
                    "wiki_original_path: \"original.bin\"",
                );
                fs::write(revision, bytes).unwrap();
            }
            "changed_content" => fs::write(
                temp.path().join(payload("content.md")),
                b"# Changed capture\nStable preexisting wrong hash.\n",
            )
            .unwrap(),
            "nonutf8_content" => {
                fs::write(temp.path().join(payload("content.md")), [0xff]).unwrap()
            }
            "withdrawn" => {
                let source = temp.path().join(format!("sources/{SOURCE}/source.md"));
                let bytes = fs::read_to_string(&source)
                    .unwrap()
                    .replace("wiki_status: \"active\"", "wiki_status: \"withdrawn\"");
                fs::write(source, bytes).unwrap();
            }
            _ => {}
        }
        let legacy = scan_input(&handle, &id(VAULT)).unwrap();
        let mut expected_rows = Sink::default();
        let expected =
            project_normalized_with_sink(&handle, &legacy, false, &mut expected_rows).unwrap();
        let input = MaintenanceInput::capture(&handle, &id(VAULT), limits()).unwrap();
        assert!(!input.notes().contains_key(&path(&payload("content.md"))));
        assert!(!input.notes().contains_key(&path(&payload("original.md"))));
        let mut actual_rows = Sink::default();
        let actual = project_maintenance_with_sink(&input, &mut actual_rows).unwrap();
        assert_eq!(actual, expected, "{variant}: complete validation/facts");
        assert_eq!(
            actual_rows, expected_rows,
            "{variant}: complete rows/emission order"
        );
        input.require_clean().unwrap();
        input.final_recheck().unwrap();
        if variant == "baseline" {
            let selected = actual_rows
                .documents
                .iter()
                .find(|row| row.path.as_str() == payload("content.md"))
                .unwrap();
            assert_eq!(
                selected.raw_text,
                fs::read_to_string(temp.path().join(payload("content.md"))).unwrap()
            );
            assert!(actual.validation.records.values().any(|row| row.disputed));
            assert!(
                actual
                    .validation
                    .records
                    .values()
                    .any(|row| row.eligibility == Eligibility::Historical)
            );
        }
        if matches!(
            variant,
            "missing_original" | "missing_content" | "changed_content" | "nonutf8_content"
        ) {
            assert_eq!(
                actual.validation.records[&id(REVISION)].eligibility,
                Eligibility::Invalid,
                "{variant}"
            );
        }
        if variant == "mixed_claims" {
            assert!(
                actual
                    .validation
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == ErrorCode::ReferenceAmbiguous)
            );
            assert!(
                actual
                    .validation
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.path.as_str() == "invalid_utf8.md")
            );
        }
    }
}
/// Simulates a latched observation failure independently of semantic validity.
/// Later successful reads cannot clear the first fatal error.
struct Observed<'a> {
    fs: &'a VaultFs,
    reads: Cell<usize>,
    fail_at: usize,
    error: WikiError,
    poison: RefCell<Option<WikiError>>,
}
impl SourceInputReads for Observed<'_> {
    fn read_observed(&self, path: &VaultRelativePath, max_bytes: usize) -> Result<Vec<u8>> {
        let next = self.reads.get() + 1;
        self.reads.set(next);
        if next == self.fail_at {
            *self.poison.borrow_mut() = Some(self.error.clone());
            return Err(self.error.clone());
        }
        let before = self
            .fs
            .read_before(path)?
            .ok_or_else(|| WikiError::new(ErrorCode::SourceIntegrity, "stable missing asset"))?;
        if before.bytes.len() > max_bytes {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "test observed read bound",
            ));
        }
        Ok(before.bytes)
    }
    fn state_observed(&self, path: &VaultRelativePath) -> Result<ExpectedState> {
        Ok(self
            .fs
            .read_before(path)?
            .map_or(ExpectedState::Absent, |before| {
                ExpectedState::Hash(before.hash)
            }))
    }
    fn require_clean(&self) -> Result<()> {
        self.poison.borrow().clone().map_or(Ok(()), Err)
    }
}
fn observed(handle: &VaultFs, fail_at: usize, code: ErrorCode) -> Observed<'_> {
    Observed {
        fs: handle,
        reads: Cell::new(0),
        fail_at,
        error: WikiError::new(code, "injected sticky observation failure"),
        poison: RefCell::new(None),
    }
}
#[test]
fn shared_projector_refuses_swallowed_compute_and_emission_observation_failures() {
    let (_temp, handle) = fixture();
    let input = scan_input(&handle, &id(VAULT)).unwrap();
    let notes: SourceNotes = input_notes(&input).unwrap().into();
    let clean = observed(&handle, usize::MAX, ErrorCode::ContentConflict);
    let view = SourceView::from_shared_maintenance(&handle, notes.shared(), &clean);
    let mut rows = Sink::default();
    let mut facts = super::super::eligibility_facts::NormalizedEligibilityFacts::new();
    project_notes(
        &handle,
        &id(VAULT),
        &notes,
        &view,
        Some(&mut rows),
        Some(&mut facts),
    )
    .unwrap();
    let final_read = clean.reads.get();
    assert!(final_read > 1);
    for (fail_at, code) in [
        (1, ErrorCode::ContentConflict),
        (final_read, ErrorCode::ContentConflict),
        (final_read, ErrorCode::BudgetExceeded),
    ] {
        let observer = observed(&handle, fail_at, code);
        let view = SourceView::from_shared_maintenance(&handle, notes.shared(), &observer);
        let mut rows = Sink::default();
        let mut facts = super::super::eligibility_facts::NormalizedEligibilityFacts::new();
        let error = project_notes(
            &handle,
            &id(VAULT),
            &notes,
            &view,
            Some(&mut rows),
            Some(&mut facts),
        )
        .unwrap_err();
        assert_eq!(error, observer.error, "failure at observed read{fail_at}");
        // The named bytes can be read successfully again, as after restoration.
        let mut dependencies = BTreeMap::new();
        view.read(&path(&payload("content.md")), &mut dependencies)
            .unwrap();
        assert_eq!(view.require_consistent().unwrap_err(), observer.error);
    }
}
#[test]
fn observed_source_reads_precede_notes_and_overlay_fallback() {
    let (_temp, handle) = fixture();
    let input = scan_input(&handle, &id(VAULT)).unwrap();
    let notes: SourceNotes = input_notes(&input).unwrap().into();
    let observer = observed(&handle, 1, ErrorCode::ContentConflict);
    let mut view = SourceView::from_shared_maintenance(&handle, notes, &observer);
    let target = path("WIKI.md");
    view.overlay
        .insert(target.clone(), Some(b"unobserved overlay".to_vec()));
    let mut dependencies = BTreeMap::new();
    assert_eq!(
        view.read(&target, &mut dependencies).unwrap_err(),
        observer.error
    );
    assert_ne!(
        view.expected_state(&target).unwrap(),
        ExpectedState::Hash(Blake3Hash::digest(b"unobserved overlay"))
    );
    assert!(view.require_consistent().is_err());
}
