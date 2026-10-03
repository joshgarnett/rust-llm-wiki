//! Immutable canonical notes with lookup indexes built once per source view.
use crate::{
    domain::{RecordId, RecordKind, VaultRelativePath},
    records::{LinkResolution, ParsedNote, RegistryEntry, links::IndexedRegistry},
};
use std::{collections::BTreeMap, ops::Deref};

/// Notes and indexes move together. No mutable dereference is exposed: callers
/// construct a new view after applying an overlay, so indexes cannot go stale.
pub(crate) struct SourceNotes {
    notes: BTreeMap<VaultRelativePath, ParsedNote>,
    claims: BTreeMap<RecordId, usize>,
    registry: IndexedRegistry,
}

impl SourceNotes {
    pub(super) fn ambiguous(&self, id: &RecordId) -> bool {
        self.claims.get(id).is_some_and(|count| *count > 1)
    }

    pub(super) fn resolve_typed(
        &self,
        id: &RecordId,
        kind: RecordKind,
        companion: Option<&str>,
    ) -> LinkResolution {
        self.registry.resolve_typed(id, kind, companion)
    }
}

impl From<BTreeMap<VaultRelativePath, ParsedNote>> for SourceNotes {
    fn from(notes: BTreeMap<VaultRelativePath, ParsedNote>) -> Self {
        let mut claims = BTreeMap::new();
        let mut entries = Vec::new();
        for (path, note) in &notes {
            // readable_ids deduplicates declarations within one note. Identity
            // ambiguity counts distinct claiming paths, including invalid notes.
            for id in super::identity::readable_ids(note) {
                *claims.entry(id).or_insert(0) += 1;
            }
            if let Some(record) = &note.canonical {
                entries.push(RegistryEntry {
                    id: record.id().clone(),
                    kind: record.kind(),
                    path: path.clone(),
                    aliases: vec![],
                });
            }
        }
        Self {
            notes,
            claims,
            registry: IndexedRegistry::new(entries),
        }
    }
}

impl Deref for SourceNotes {
    type Target = BTreeMap<VaultRelativePath, ParsedNote>;

    fn deref(&self) -> &Self::Target {
        &self.notes
    }
}

impl<'a> IntoIterator for &'a SourceNotes {
    type Item = (&'a VaultRelativePath, &'a ParsedNote);
    type IntoIter = std::collections::btree_map::Iter<'a, VaultRelativePath, ParsedNote>;

    fn into_iter(self) -> Self::IntoIter {
        self.notes.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        changes::{ProposedTarget, ScanDocument, ValidationInput},
        domain::{Blake3Hash, ErrorCode, RecordKind},
        records::parse_note,
        sources::SourceView,
        vault::{VaultFs, VaultRoot},
    };

    fn entity(id: &str) -> Vec<u8> {
        format!("---\nwiki_schema: '1'\nwiki_id: {id}\nwiki_kind: entity\ntitle: Indexed entity\nwiki_status: active\nwiki_entity_type: concept\n---\n").into_bytes()
    }

    #[test]
    fn malformed_claims_count_paths_including_repeated_declarations() {
        let id = RecordId::new("entity_lookup").unwrap();
        let repeated =
            b"---\nwiki_id: entity_lookup\nwiki_id: entity_lookup\nwiki_kind: invalid\n---\n";
        let one = BTreeMap::from([(
            VaultRelativePath::new("broken.md").unwrap(),
            parse_note(repeated),
        )]);
        let notes = SourceNotes::from(one.clone());
        assert_eq!(notes.claims.get(&id), Some(&1));
        let mut two = one;
        two.insert(
            VaultRelativePath::new("entity.md").unwrap(),
            parse_note(&entity(id.as_str())),
        );
        assert_eq!(SourceNotes::from(two).claims.get(&id), Some(&2));
    }

    #[test]
    fn each_overlay_rebuilds_claim_and_path_indexes() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("WIKI.md"),
            b"---\nwiki_schema: '1'\nwiki_id: vault_lookup\nwiki_kind: vault\ntitle: Lookup\n---\n",
        )
        .unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let original = entity("entity_lookup");
        let original_path = VaultRelativePath::new("entity.md").unwrap();
        let copy_path = VaultRelativePath::new("copy.md").unwrap();
        let id = RecordId::new("entity_lookup").unwrap();
        let mut input = ValidationInput {
            vault_id: RecordId::new("vault_lookup").unwrap(),
            documents: vec![ScanDocument {
                path: original_path.clone(),
                hash: Blake3Hash::digest(&original),
                bytes: original,
            }],
            overlay: vec![ProposedTarget {
                path: copy_path.clone(),
                bytes: Some(b"---\nwiki_id: entity_lookup\nwiki_id: entity_other\nwiki_id: entity_lookup\nwiki_kind: invalid\n---\n".to_vec()),
            }],
        };
        let duplicate = SourceView::from_closed_input(&fs, &input).unwrap();
        let other = RecordId::new("entity_other").unwrap();
        assert_eq!(duplicate.notes.claims.get(&id), Some(&2));
        assert_eq!(duplicate.notes.claims.get(&other), Some(&1));
        assert_eq!(
            duplicate
                .resolve(&other, RecordKind::Entity, None)
                .unwrap_err()
                .code,
            ErrorCode::RecordNotFound
        );
        assert_eq!(
            duplicate
                .resolve(&id, RecordKind::Entity, None)
                .unwrap_err()
                .code,
            ErrorCode::ReferenceAmbiguous
        );
        input.overlay = vec![ProposedTarget {
            path: copy_path.clone(),
            bytes: None,
        }];
        let repaired = SourceView::from_closed_input(&fs, &input).unwrap();
        assert_eq!(
            repaired.resolve(&id, RecordKind::Entity, None).unwrap().0,
            &original_path
        );
        assert!(!repaired.notes.claims.contains_key(&other));
        input.overlay = vec![
            ProposedTarget {
                path: original_path,
                bytes: None,
            },
            ProposedTarget {
                path: copy_path.clone(),
                bytes: Some(entity(id.as_str())),
            },
        ];
        let renamed = SourceView::from_closed_input(&fs, &input).unwrap();
        assert_eq!(
            renamed
                .resolve(&id, RecordKind::Entity, Some("[[copy#Heading|label]]"))
                .unwrap()
                .0,
            &copy_path
        );
        assert_eq!(
            renamed
                .resolve(&id, RecordKind::Source, None)
                .unwrap_err()
                .code,
            ErrorCode::SourceIntegrity
        );
        // The previously constructed view remains bound to its own unchanged notes.
        assert_eq!(
            duplicate
                .resolve(&id, RecordKind::Entity, None)
                .unwrap_err()
                .code,
            ErrorCode::ReferenceAmbiguous
        );
        input.overlay = vec![ProposedTarget {
            path: VaultRelativePath::new("entity.md").unwrap(),
            bytes: None,
        }];
        let deleted = SourceView::from_closed_input(&fs, &input).unwrap();
        assert_eq!(
            deleted
                .resolve(&id, RecordKind::Entity, None)
                .unwrap_err()
                .code,
            ErrorCode::RecordNotFound
        );
    }
}
