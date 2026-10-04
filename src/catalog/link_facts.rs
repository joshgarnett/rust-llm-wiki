//! Finite lookup dependencies for navigation, including unresolved destinations.
//! Keys discover possible affected links; they do not establish a resolution.
use crate::{
    domain::{ErrorCode, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    records::{
        LinkResolution, RegistryEntry,
        links::{UntypedLookup, companion_paths, registry_basename, untyped_lookup},
    },
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_KEYS: usize = 4096;
const MAX_TEXT_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MatchKeyKind {
    Id,
    Path,
    Basename,
    Alias,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MatchKey {
    pub kind: MatchKeyKind,
    pub value: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TypedLinkTarget {
    pub id: RecordId,
    pub expected_kind: RecordKind,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnedLinkFact {
    pub from_path: VaultRelativePath,
    pub byte_start: u64,
    pub raw_destination: String,
    pub typed: Option<TypedLinkTarget>,
    pub keys: Vec<MatchKey>,
}

pub(crate) fn untyped_fact(
    from_path: &VaultRelativePath,
    byte_start: u64,
    destination: &str,
    resolution: &LinkResolution,
) -> Result<OwnedLinkFact> {
    let mut keys = KeySet::new(from_path.as_str(), destination)?;
    match untyped_lookup(destination) {
        UntypedLookup::External => {
            if !matches!(resolution, LinkResolution::External) {
                return Err(invalid("external destination has inconsistent resolution"));
            }
        }
        UntypedLookup::Missing => {
            if !matches!(resolution, LinkResolution::Missing) {
                return Err(invalid(
                    "invalid local destination has inconsistent resolution",
                ));
            }
        }
        UntypedLookup::Local {
            exact, basename, ..
        } => {
            keys.add(MatchKeyKind::Path, exact.direct)?;
            if let Some(fallback) = exact.fallback {
                keys.add(MatchKeyKind::Path, &fallback)?;
            }
            keys.add(MatchKeyKind::Basename, basename)?;
            keys.add(MatchKeyKind::Alias, exact.direct)?;
            if basename != exact.direct {
                keys.add(MatchKeyKind::Alias, basename)?;
            }
            match resolution {
                LinkResolution::Resolved { id, .. } => keys.add(MatchKeyKind::Id, id.as_str())?,
                // Candidate membership changes are discovered by the raw
                // destination keys above. Do not copy an unbounded ambiguity
                // list into every referring link's dependency keys.
                LinkResolution::Ambiguous { .. } | LinkResolution::Missing => {}
                _ => {
                    return Err(invalid(
                        "local untyped destination has inconsistent resolution",
                    ));
                }
            }
        }
    }
    Ok(OwnedLinkFact {
        from_path: from_path.clone(),
        byte_start,
        raw_destination: destination.to_owned(),
        typed: None,
        keys: keys.finish(),
    })
}

pub(crate) fn typed_fact(
    from_path: &VaultRelativePath,
    byte_start: u64,
    destination: &str,
    id: &RecordId,
    expected_kind: RecordKind,
) -> Result<OwnedLinkFact> {
    let mut keys = KeySet::new(from_path.as_str(), destination)?;
    keys.add(MatchKeyKind::Id, id.as_str())?;
    let exact = companion_paths(destination);
    keys.add(MatchKeyKind::Path, exact.direct)?;
    if let Some(fallback) = exact.fallback {
        keys.add(MatchKeyKind::Path, &fallback)?;
    }
    Ok(OwnedLinkFact {
        from_path: from_path.clone(),
        byte_start,
        raw_destination: destination.to_owned(),
        typed: Some(TypedLinkTarget {
            id: id.clone(),
            expected_kind,
        }),
        keys: keys.finish(),
    })
}

pub(crate) fn registry_keys(entry: &RegistryEntry) -> Result<Vec<MatchKey>> {
    let mut keys = KeySet::new(entry.path.as_str(), entry.id.as_str())?;
    keys.add(MatchKeyKind::Id, entry.id.as_str())?;
    keys.add(MatchKeyKind::Path, entry.path.as_str())?;
    keys.add(
        MatchKeyKind::Basename,
        registry_basename(entry.path.as_str()),
    )?;
    for alias in &entry.aliases {
        keys.add(MatchKeyKind::Alias, alias)?;
    }
    Ok(keys.finish())
}

struct KeySet {
    keys: BTreeSet<MatchKey>,
    text_bytes: usize,
    visits: usize,
}
impl KeySet {
    fn new(owner: &str, raw: &str) -> Result<Self> {
        let text_bytes = owner
            .len()
            .checked_add(raw.len())
            .filter(|n| *n <= MAX_TEXT_BYTES)
            .ok_or_else(|| budget("link fact exceeds text byte ceiling"))?;
        Ok(Self {
            keys: BTreeSet::new(),
            text_bytes,
            visits: 0,
        })
    }
    fn add(&mut self, kind: MatchKeyKind, value: &str) -> Result<()> {
        // Charge visits and text even for duplicates, bounding work as well as output.
        self.visits = self
            .visits
            .checked_add(1)
            .filter(|n| *n <= MAX_KEYS)
            .ok_or_else(|| budget("link fact exceeds lookup key ceiling"))?;
        self.text_bytes = self
            .text_bytes
            .checked_add(value.len())
            .filter(|n| *n <= MAX_TEXT_BYTES)
            .ok_or_else(|| budget("link fact exceeds text byte ceiling"))?;
        self.keys.insert(MatchKey {
            kind,
            value: value.to_owned(),
        });
        Ok(())
    }
    fn finish(self) -> Vec<MatchKey> {
        self.keys.into_iter().collect()
    }
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn invalid(message: &str) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::{extract_links, links::IndexedRegistry};
    fn path(value: &str) -> VaultRelativePath {
        VaultRelativePath::new(value).unwrap()
    }
    fn entry(id: &str, location: &str, aliases: &[&str]) -> RegistryEntry {
        RegistryEntry {
            id: RecordId::new(id).unwrap(),
            kind: RecordKind::Revision,
            path: path(location),
            aliases: aliases.iter().map(|alias| (*alias).into()).collect(),
        }
    }
    fn key(kind: MatchKeyKind, value: &str) -> MatchKey {
        MatchKey {
            kind,
            value: value.into(),
        }
    }
    fn intersects(fact: &OwnedLinkFact, entry: &RegistryEntry) -> bool {
        registry_keys(entry)
            .unwrap()
            .iter()
            .any(|key| fact.keys.contains(key))
    }
    #[test]
    fn extracted_destination_keys_preserve_raw_bytes_and_ignore_label_fragment_for_matching() {
        let text =
            "Café [[folder/Revision#^block|Display]] and [shown](folder/Revision.md#Section).";
        let extracted = extract_links(text);
        assert_eq!(extracted.len(), 2);
        let registry = IndexedRegistry::new(vec![]);
        let fact = untyped_fact(
            &path("page.md"),
            extracted[0].range.start as u64,
            &extracted[0].destination,
            &registry.resolve_untyped(&extracted[0].destination),
        )
        .unwrap();
        assert_eq!(fact.raw_destination, "folder/Revision#^block");
        assert_eq!(fact.byte_start, 6);
        assert_eq!(
            fact.keys,
            vec![
                key(MatchKeyKind::Path, "folder/Revision"),
                key(MatchKeyKind::Path, "folder/Revision.md"),
                key(MatchKeyKind::Basename, "Revision"),
                key(MatchKeyKind::Alias, "Revision"),
                key(MatchKeyKind::Alias, "folder/Revision")
            ]
        );
        let wrapped = untyped_fact(
            &path("page.md"),
            6,
            "[[folder/Revision#^block|Display]]",
            &registry.resolve_untyped("[[folder/Revision#^block|Display]]"),
        )
        .unwrap();
        assert_eq!(fact.keys, wrapped.keys);
        assert_eq!(
            wrapped.raw_destination,
            "[[folder/Revision#^block|Display]]"
        );
        let encoded = serde_json::to_value(&fact).unwrap();
        assert_eq!(encoded["keys"][0]["kind"], "path");
        assert_eq!(
            serde_json::from_value::<OwnedLinkFact>(encoded).unwrap(),
            fact
        );
    }
    #[test]
    fn new_revision_basename_finds_unique_to_ambiguous_and_missing_to_resolved_links() {
        let old = entry("revision_old", "sources/one/revisions/old/revision.md", &[]);
        let new = entry("revision_new", "sources/one/revisions/new/revision.md", &[]);
        let before = IndexedRegistry::new(vec![old.clone()]);
        let after = IndexedRegistry::new(vec![old, new.clone()]);
        let fact = untyped_fact(
            &path("page.md"),
            0,
            "revision",
            &before.resolve_untyped("revision"),
        )
        .unwrap();
        assert!(matches!(
            before.resolve_untyped("revision"),
            LinkResolution::Resolved { .. }
        ));
        assert!(matches!(
            after.resolve_untyped("revision"),
            LinkResolution::Ambiguous { .. }
        ));
        assert!(intersects(&fact, &new));
        let empty = IndexedRegistry::new(vec![]);
        let missing = untyped_fact(
            &path("page.md"),
            0,
            "absent/revision.md",
            &empty.resolve_untyped("absent/revision.md"),
        )
        .unwrap();
        assert!(intersects(&missing, &new));
        assert!(matches!(
            IndexedRegistry::new(vec![new]).resolve_untyped("absent/revision.md"),
            LinkResolution::Resolved { .. }
        ));
    }
    #[test]
    fn shadowed_exact_fallback_alias_and_duplicate_identity_changes_are_discoverable() {
        let alias = entry("alias_target", "different.md", &["lookup"]);
        let fallback = entry("fallback_target", "lookup.md", &[]);
        let direct = entry("direct_target", "lookup", &[]);
        for (old, new) in [
            (alias.clone(), fallback.clone()),
            (fallback.clone(), direct),
        ] {
            let before = IndexedRegistry::new(vec![old.clone()]);
            let after = IndexedRegistry::new(vec![old, new.clone()]);
            let fact = untyped_fact(
                &path("page.md"),
                0,
                "lookup",
                &before.resolve_untyped("lookup"),
            )
            .unwrap();
            assert!(intersects(&fact, &new));
            assert_ne!(
                before.resolve_untyped("lookup"),
                after.resolve_untyped("lookup")
            );
        }
        let copy = entry("alias_target", "unrelated/copy.md", &[]);
        let before = IndexedRegistry::new(vec![alias.clone()]);
        let fact = untyped_fact(
            &path("page.md"),
            0,
            "lookup",
            &before.resolve_untyped("lookup"),
        )
        .unwrap();
        assert!(intersects(&fact, &copy));
        assert!(matches!(
            IndexedRegistry::new(vec![alias, copy]).resolve_untyped("lookup"),
            LinkResolution::Ambiguous { .. }
        ));
    }
    #[test]
    fn typed_companions_depend_on_id_and_exact_paths_and_never_aliases_or_basenames() {
        let target = entry("revision_target", "actual.md", &["display"]);
        let alias = entry("revision_other", "elsewhere/display.md", &["display"]);
        let registry = IndexedRegistry::new(vec![target.clone(), alias.clone()]);
        let fact = typed_fact(
            &path("source.md"),
            4,
            "[[display#Heading|Shown]]",
            &target.id,
            RecordKind::Revision,
        )
        .unwrap();
        assert_eq!(
            fact.keys,
            vec![
                key(MatchKeyKind::Id, "revision_target"),
                key(MatchKeyKind::Path, "display"),
                key(MatchKeyKind::Path, "display.md")
            ]
        );
        assert!(matches!(
            registry.resolve_typed(
                &target.id,
                RecordKind::Revision,
                Some("[[display#Heading|Shown]]")
            ),
            LinkResolution::Resolved {
                companion_stale: true,
                ..
            }
        ));
        assert!(!intersects(&fact, &alias));
        assert!(intersects(&fact, &target));
        let conflict = entry("revision_conflict", "display.md", &[]);
        assert!(intersects(&fact, &conflict));
        assert!(matches!(
            IndexedRegistry::new(vec![target.clone(), conflict]).resolve_typed(
                &target.id,
                RecordKind::Revision,
                Some("display")
            ),
            LinkResolution::CompanionConflict { .. }
        ));
    }
    #[test]
    fn exact_unicode_keys_external_invalid_destinations_and_limits_are_explicit() {
        let empty = IndexedRegistry::new(vec![]);
        for destination in [
            "https://host.invalid/note#Heading",
            "//host/path",
            "../note",
            "#Heading",
            "",
        ] {
            let fact = untyped_fact(
                &path("page.md"),
                0,
                destination,
                &empty.resolve_untyped(destination),
            )
            .unwrap();
            assert!(fact.keys.is_empty());
            assert_eq!(fact.raw_destination, destination);
        }
        let unicode = entry(
            "unicode",
            "folder/Éclair.md",
            &["Éclair", "Éclair", "E\u{301}clair"],
        );
        let keys = registry_keys(&unicode).unwrap();
        assert_eq!(
            keys.iter()
                .filter(|key| key.kind == MatchKeyKind::Alias)
                .count(),
            2
        );
        for (destination, expected) in
            [("Éclair", true), ("E\u{301}clair", true), ("éclair", false)]
        {
            let fact = untyped_fact(
                &path("page.md"),
                0,
                destination,
                &empty.resolve_untyped(destination),
            )
            .unwrap();
            assert_eq!(intersects(&fact, &unicode), expected);
        }
        let upper =
            untyped_fact(&path("page.md"), 0, "Revision.MD", &LinkResolution::Missing).unwrap();
        assert!(
            upper
                .keys
                .contains(&key(MatchKeyKind::Path, "Revision.MD.md"))
        );
        assert!(
            upper
                .keys
                .contains(&key(MatchKeyKind::Basename, "Revision.MD"))
        );
        assert!(
            !upper
                .keys
                .contains(&key(MatchKeyKind::Basename, "Revision"))
        );
        let huge = "x".repeat(MAX_TEXT_BYTES + 1);
        assert_eq!(
            untyped_fact(&path("page.md"), 0, &huge, &LinkResolution::Missing)
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        let mut many = entry("many", "many.md", &[]);
        many.aliases = vec!["same".into(); MAX_KEYS];
        assert_eq!(
            registry_keys(&many).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        assert!(
            serde_json::from_value::<MatchKey>(
                serde_json::json!({"kind":"path","value":"a","unknown":true})
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod ambiguity_scale_tests {
    use super::*;
    #[test]
    fn ambiguous_candidate_count_does_not_expand_link_keys() {
        let owner = VaultRelativePath::new("page.md").unwrap();
        let many = LinkResolution::Ambiguous {
            ids: (0..10_000)
                .map(|i| RecordId::new(format!("revision_{i}")).unwrap())
                .collect(),
        };
        let fact = untyped_fact(&owner, 0, "revision", &many).unwrap();
        let absent = untyped_fact(&owner, 0, "revision", &LinkResolution::Missing).unwrap();
        assert_eq!(fact.keys, absent.keys);
        assert_eq!(fact.keys.len(), 4);
    }
}
