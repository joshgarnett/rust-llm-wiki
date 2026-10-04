//! Pure navigation extraction and registry resolution. Labels never create IDs.
use crate::domain::{RecordId, RecordKind, VaultRelativePath};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use std::{
    collections::{BTreeSet, HashMap},
    ops::Range,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSyntax {
    Wiki,
    Markdown,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownLink {
    pub destination: String,
    pub label: Option<String>,
    pub range: Range<usize>,
    pub syntax: LinkSyntax,
}
/// Extract body links, retaining source byte ranges. Call with ParsedNote::body,
/// since YAML companion fields are validated separately.
pub fn extract_links(markdown: &str) -> Vec<MarkdownLink> {
    let mut links = vec![];
    let mut excluded = vec![];
    let mut code_start = None;
    for (event, range) in Parser::new(markdown).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = code_start.take() {
                    excluded.push(start..range.end);
                }
            }
            Event::Code(_) => excluded.push(range),
            Event::Start(Tag::Link { dest_url, .. }) if code_start.is_none() => {
                excluded.push(range.clone());
                links.push(MarkdownLink {
                    destination: dest_url.to_string(),
                    label: None,
                    range,
                    syntax: LinkSyntax::Markdown,
                });
            }
            _ => {}
        }
    }
    if let Some(start) = code_start {
        excluded.push(start..markdown.len());
    }
    excluded.sort_by_key(|r| r.start);
    let raw = markdown.as_bytes();
    let mut i = 0;
    while i + 1 < raw.len() {
        if let Some(range) = excluded.iter().find(|r| r.contains(&i)) {
            i = range.end.max(i + 1);
            continue;
        }
        if raw[i..].starts_with(b"[[")
            && !escaped(raw, i)
            && let Some(relative_end) = markdown[i + 2..].find("]]")
        {
            let end = i + 2 + relative_end;
            let interior = &markdown[i + 2..end];
            if !interior.contains(['\n', '\r', '[', ']'])
                && !excluded.iter().any(|r| r.start < end + 2 && r.end > i)
            {
                let (destination, label) = interior
                    .split_once('|')
                    .map_or((interior, None), |(d, l)| (d, Some(l.to_owned())));
                links.push(MarkdownLink {
                    destination: destination.to_owned(),
                    label,
                    range: i..end + 2,
                    syntax: LinkSyntax::Wiki,
                });
                i = end + 2;
                continue;
            }
        }
        i += 1;
    }
    links.sort_by_key(|link| link.range.start);
    links
}
fn escaped(raw: &[u8], index: usize) -> bool {
    raw[..index]
        .iter()
        .rev()
        .take_while(|b| **b == b'\\')
        .count()
        % 2
        == 1
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryEntry {
    pub id: RecordId,
    pub kind: RecordKind,
    pub path: VaultRelativePath,
    pub aliases: Vec<String>,
}
/// Immutable lookup indexes over one owned registry. Buckets retain entry positions,
/// including duplicate IDs and paths, so resolution can preserve ambiguity.
#[derive(Debug, Clone)]
pub(crate) struct IndexedRegistry {
    entries: Vec<RegistryEntry>,
    by_id: HashMap<RecordId, Vec<usize>>,
    by_path: HashMap<String, Vec<usize>>,
    by_stem: HashMap<String, Vec<usize>>,
    by_alias: HashMap<String, Vec<usize>>,
    #[cfg(test)]
    candidate_visits: std::cell::Cell<usize>,
}

impl IndexedRegistry {
    pub(crate) fn new(entries: Vec<RegistryEntry>) -> Self {
        let mut registry = Self {
            entries,
            by_id: HashMap::new(),
            by_path: HashMap::new(),
            by_stem: HashMap::new(),
            by_alias: HashMap::new(),
            #[cfg(test)]
            candidate_visits: std::cell::Cell::new(0),
        };
        for (position, entry) in registry.entries.iter().enumerate() {
            registry
                .by_id
                .entry(entry.id.clone())
                .or_default()
                .push(position);
            registry
                .by_path
                .entry(entry.path.as_str().to_owned())
                .or_default()
                .push(position);
            let stem = registry_basename(entry.path.as_str());
            registry
                .by_stem
                .entry(stem.to_owned())
                .or_default()
                .push(position);
            for alias in &entry.aliases {
                let positions = registry.by_alias.entry(alias.clone()).or_default();
                // Each entry matches an alias predicate at most once, even if it
                // contains repeated aliases. Positions arrive in ascending order.
                if positions.last() != Some(&position) {
                    positions.push(position);
                }
            }
        }
        registry
    }

    #[cfg(test)]
    fn entries(&self) -> &[RegistryEntry] {
        &self.entries
    }

    // Logical work instrumentation counts the candidate positions supplied by
    // index lookups, rather than elapsed time or hash-table implementation details.
    fn candidates<'a>(&self, positions: Option<&'a Vec<usize>>) -> &'a [usize] {
        let positions = positions.map_or(&[][..], Vec::as_slice);
        #[cfg(test)]
        self.candidate_visits
            .set(self.candidate_visits.get() + positions.len());
        positions
    }

    fn id_positions(&self, id: &RecordId) -> &[usize] {
        self.candidates(self.by_id.get(id))
    }

    fn exact_positions(&self, path: &str) -> &[usize] {
        self.exact_key_positions(&exact_paths(path))
    }

    fn exact_key_positions(&self, keys: &ExactPaths<'_>) -> &[usize] {
        let direct = self.candidates(self.by_path.get(keys.direct));
        if !direct.is_empty() {
            return direct;
        }
        keys.fallback.as_ref().map_or(&[][..], |fallback| {
            self.candidates(self.by_path.get(fallback))
        })
    }

    fn ambiguous_positions(&self, positions: &[usize]) -> LinkResolution {
        LinkResolution::Ambiguous {
            ids: positions
                .iter()
                .map(|position| self.entries[*position].id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }

    fn unique_positions(&self, positions: &[usize], fragment: Option<String>) -> LinkResolution {
        match positions {
            [] => LinkResolution::Missing,
            [position] => {
                let entry = &self.entries[*position];
                let copies = self.id_positions(&entry.id);
                if copies.len() != 1 {
                    return self.ambiguous_positions(copies);
                }
                LinkResolution::Resolved {
                    id: entry.id.clone(),
                    path: entry.path.clone(),
                    fragment,
                    companion_stale: false,
                }
            }
            _ => self.ambiguous_positions(positions),
        }
    }

    pub(crate) fn resolve_untyped(&self, destination: &str) -> LinkResolution {
        let (exact, stem, fragment) = match untyped_lookup(destination) {
            UntypedLookup::External => return LinkResolution::External,
            UntypedLookup::Missing => return LinkResolution::Missing,
            UntypedLookup::Local {
                exact,
                basename,
                fragment,
            } => (exact, basename, fragment),
        };
        let path = exact.direct;
        let positions = self.exact_key_positions(&exact);
        if !positions.is_empty() {
            return self.unique_positions(positions, fragment);
        }
        // The original predicate is basename-stem OR alias(path) OR alias(stem).
        // A position set preserves one match per entry and registry ordering.
        let mut positions = BTreeSet::new();
        positions.extend(self.candidates(self.by_stem.get(stem)));
        positions.extend(self.candidates(self.by_alias.get(path)));
        if stem != path {
            positions.extend(self.candidates(self.by_alias.get(stem)));
        }
        self.unique_positions(&positions.into_iter().collect::<Vec<_>>(), fragment)
    }

    pub(crate) fn resolve_typed(
        &self,
        id: &RecordId,
        expected_kind: RecordKind,
        companion: Option<&str>,
    ) -> LinkResolution {
        let matches = self.id_positions(id);
        let entry = match matches {
            [] => return LinkResolution::Missing,
            [position] => &self.entries[*position],
            _ => return self.ambiguous_positions(matches),
        };
        if entry.kind != expected_kind {
            return LinkResolution::WrongKind { actual: entry.kind };
        }
        let mut fragment = None;
        let mut stale = companion.is_none();
        if let Some(companion) = companion {
            let (path, hint) = target(companion);
            fragment = hint;
            let at_path = self.exact_positions(path);
            match at_path {
                [] => stale = true,
                [position] if self.entries[*position].id != entry.id => {
                    return LinkResolution::CompanionConflict {
                        expected: id.clone(),
                        actual: self.entries[*position].id.clone(),
                    };
                }
                [_] => {}
                _ => return self.ambiguous_positions(at_path),
            }
        }
        LinkResolution::Resolved {
            id: entry.id.clone(),
            path: entry.path.clone(),
            fragment,
            companion_stale: stale,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkResolution {
    Resolved {
        id: RecordId,
        path: VaultRelativePath,
        fragment: Option<String>,
        companion_stale: bool,
    },
    Missing,
    External,
    Ambiguous {
        ids: Vec<RecordId>,
    },
    WrongKind {
        actual: RecordKind,
    },
    CompanionConflict {
        expected: RecordId,
        actual: RecordId,
    },
}
/// Exact path always wins; extension fallback is tried only without direct matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactPaths<'a> {
    pub direct: &'a str,
    pub fallback: Option<String>,
}
pub(crate) fn exact_paths(path: &str) -> ExactPaths<'_> {
    ExactPaths {
        direct: path,
        fallback: (!path.ends_with(".md")).then(|| format!("{path}.md")),
    }
}
/// The basename match is byte-exact, with only the lowercase `.md` suffix removed.
pub(crate) fn registry_basename(path: &str) -> &str {
    let basename = path.rsplit('/').next().unwrap_or_default();
    basename.strip_suffix(".md").unwrap_or(basename)
}
/// The resolver's potential lookup buckets, including currently shadowed fallbacks.
/// Labels/fragments are presentation; they do not change candidate identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UntypedLookup<'a> {
    External,
    Missing,
    Local {
        exact: ExactPaths<'a>,
        basename: &'a str,
        fragment: Option<String>,
    },
}
pub(crate) fn untyped_lookup(destination: &str) -> UntypedLookup<'_> {
    let (path, fragment) = target(destination);
    if path.starts_with("//") || path.contains(':') {
        return UntypedLookup::External;
    }
    if path.is_empty() || VaultRelativePath::new(path).is_err() {
        return UntypedLookup::Missing;
    }
    UntypedLookup::Local {
        exact: exact_paths(path),
        basename: registry_basename(path),
        fragment,
    }
}
pub(crate) fn companion_paths(destination: &str) -> ExactPaths<'_> {
    // Typed companions intentionally do not use untyped alias/basename matching
    // or its invalid/external-target gate. The typed ID determines identity.
    exact_paths(target(destination).0)
}
fn target(destination: &str) -> (&str, Option<String>) {
    let destination = destination
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .unwrap_or(destination);
    let destination = destination.split('|').next().unwrap_or_default();
    destination
        .split_once('#')
        .map_or((destination, None), |(path, fragment)| {
            (path, Some(fragment.to_owned()))
        })
}
fn exact<'a>(registry: &'a [RegistryEntry], path: &str) -> Vec<&'a RegistryEntry> {
    let direct: Vec<_> = registry
        .iter()
        .filter(|entry| entry.path.as_str() == path)
        .collect();
    if !direct.is_empty() || path.ends_with(".md") {
        return direct;
    }
    let with_extension = format!("{path}.md");
    registry
        .iter()
        .filter(|entry| entry.path.as_str() == with_extension)
        .collect()
}
fn ambiguous(entries: &[&RegistryEntry]) -> LinkResolution {
    LinkResolution::Ambiguous {
        ids: entries
            .iter()
            .map(|e| e.id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    }
}
fn unique<'a>(
    registry: &'a [RegistryEntry],
    entries: &[&'a RegistryEntry],
    fragment: Option<String>,
) -> LinkResolution {
    match entries {
        [] => LinkResolution::Missing,
        [entry] => {
            let copies: Vec<_> = registry.iter().filter(|e| e.id == entry.id).collect();
            if copies.len() != 1 {
                return ambiguous(&copies);
            }
            LinkResolution::Resolved {
                id: entry.id.clone(),
                path: entry.path.clone(),
                fragment,
                companion_stale: false,
            }
        }
        _ => ambiguous(entries),
    }
}
pub fn resolve_untyped(registry: &[RegistryEntry], destination: &str) -> LinkResolution {
    let (path, fragment) = target(destination);
    if path.starts_with("//") || path.contains(':') {
        return LinkResolution::External;
    }
    if path.is_empty() || VaultRelativePath::new(path).is_err() {
        return LinkResolution::Missing;
    }
    let entries = exact(registry, path);
    if !entries.is_empty() {
        return unique(registry, &entries, fragment);
    }
    let basename = path.rsplit('/').next().unwrap_or(path);
    let stem = basename.strip_suffix(".md").unwrap_or(basename);
    let entries: Vec<_> = registry
        .iter()
        .filter(|entry| {
            let basename = entry.path.as_str().rsplit('/').next().unwrap_or_default();
            basename.strip_suffix(".md").unwrap_or(basename) == stem
                || entry
                    .aliases
                    .iter()
                    .any(|alias| alias == path || alias == stem)
        })
        .collect();
    unique(registry, &entries, fragment)
}
pub fn resolve_typed(
    registry: &[RegistryEntry],
    id: &RecordId,
    expected_kind: RecordKind,
    companion: Option<&str>,
) -> LinkResolution {
    let matches: Vec<_> = registry.iter().filter(|entry| &entry.id == id).collect();
    let entry = match matches.as_slice() {
        [] => return LinkResolution::Missing,
        [entry] => *entry,
        _ => return ambiguous(&matches),
    };
    if entry.kind != expected_kind {
        return LinkResolution::WrongKind { actual: entry.kind };
    }
    let mut fragment = None;
    let mut stale = companion.is_none();
    if let Some(companion) = companion {
        let (path, hint) = target(companion);
        fragment = hint;
        let at_path = exact(registry, path);
        match at_path.as_slice() {
            [] => stale = true,
            [actual] if actual.id != entry.id => {
                return LinkResolution::CompanionConflict {
                    expected: id.clone(),
                    actual: actual.id.clone(),
                };
            }
            [_] => {}
            _ => return ambiguous(&at_path),
        }
    }
    LinkResolution::Resolved {
        id: entry.id.clone(),
        path: entry.path.clone(),
        fragment,
        companion_stale: stale,
    }
}

#[cfg(test)]
mod indexed_tests {
    use super::*;

    fn entry(id: &str, kind: RecordKind, path: &str, aliases: &[&str]) -> RegistryEntry {
        RegistryEntry {
            id: RecordId::new(id).unwrap(),
            kind,
            path: VaultRelativePath::new(path).unwrap(),
            aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        }
    }

    fn assert_parity(entries: Vec<RegistryEntry>) {
        let indexed = IndexedRegistry::new(entries.clone());
        assert_eq!(indexed.entries(), entries);
        for destination in [
            "one",
            "one.md",
            "a/one",
            "a/one.md#Heading",
            "[[a/one#^block|label]]",
            "a/one.md#",
            "a/one.md#first#second|label",
            "b/one.md",
            "missing/one.md",
            "alias",
            "dir/label",
            "single",
            "missing",
            "copy.md",
            "legacy.ID-2",
            "Éclair",
            "éclair",
            "E\u{301}clair",
            "İstanbul",
            "istanbul",
            "Straße",
            "STRASSE",
            "unicode/éclair.md",
            "",
            "#Heading",
            "../one.md",
            "a//one.md",
            "one.md|label",
            "[[one|label#Heading]]",
            "[[one.md]]tail",
            "//host/path",
            "https://example.invalid#Heading",
            "mailto:person@example.invalid",
        ] {
            assert_eq!(
                indexed.resolve_untyped(destination),
                resolve_untyped(&entries, destination),
                "untyped {destination:?}; registry {entries:?}"
            );
        }
        for id in ["A", "B", "C", "D", "E", "legacy.ID-2", "unknown-id"] {
            let id = RecordId::new(id).unwrap();
            for kind in [
                RecordKind::Entity,
                RecordKind::Source,
                RecordKind::Page,
                RecordKind::Revision,
            ] {
                for companion in [
                    None,
                    Some("one"),
                    Some("one.md"),
                    Some("a/one"),
                    Some("a/one.md"),
                    Some("[[a/one#^block|label]]"),
                    Some("a/one.md#"),
                    Some("a/one.md#first#second|label"),
                    Some("b/one.md"),
                    Some("copy.md"),
                    Some("other.md"),
                    Some("missing"),
                    Some("alias"),
                    Some(""),
                    Some("#fragment"),
                    Some("../one.md"),
                    Some("https://example.invalid#Heading"),
                    Some("[[one.md]]tail"),
                ] {
                    assert_eq!(
                        indexed.resolve_typed(&id, kind, companion),
                        resolve_typed(&entries, &id, kind, companion),
                        "typed {id:?}/{kind:?}/{companion:?}; registry {entries:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn exhaustive_small_catalogs_match_independent_scan_oracle() {
        let choices = [
            entry(
                "A",
                RecordKind::Entity,
                "a/one.md",
                &[
                    "one",
                    "one",
                    "alias",
                    "dir/label",
                    "Éclair",
                    "İstanbul",
                    "Straße",
                ],
            ),
            entry("B", RecordKind::Source, "a/one.md", &["alias"]),
            entry("A", RecordKind::Revision, "copy.md", &[""]),
            entry("C", RecordKind::Entity, "one", &["single"]),
            entry("D", RecordKind::Entity, "one.md", &["alias"]),
            entry("E", RecordKind::Entity, "b/one.md", &["single"]),
            entry(
                "legacy.ID-2",
                RecordKind::Page,
                "unicode/éclair.md",
                &["E\u{301}clair", "éclair"],
            ),
            entry("B", RecordKind::Entity, "other.md", &["one", "missing"]),
        ];
        // All ordered catalogs of size 0..=3, including repeated entries.
        // Order permutations expose accidental first-match shortcuts.
        assert_parity(vec![]);
        for a in &choices {
            assert_parity(vec![a.clone()]);
            for b in &choices {
                assert_parity(vec![a.clone(), b.clone()]);
                for c in &choices {
                    assert_parity(vec![a.clone(), b.clone(), c.clone()]);
                }
            }
        }
    }

    #[test]
    fn path_precedence_union_and_ambiguity_order_are_explicit() {
        let entries = vec![
            entry("C", RecordKind::Entity, "one", &["alias"]),
            entry("A", RecordKind::Entity, "one.md", &["alias"]),
            entry(
                "B",
                RecordKind::Entity,
                "b/one.md",
                &["alias", "alias", "one"],
            ),
        ];
        let indexed = IndexedRegistry::new(entries);
        assert!(matches!(
            indexed.resolve_untyped("one#Heading"),
            LinkResolution::Resolved { id, fragment: Some(fragment), .. }
                if id.as_str() == "C" && fragment == "Heading"
        ));
        let expected = LinkResolution::Ambiguous {
            ids: ["A", "B", "C"]
                .map(|id| RecordId::new(id).unwrap())
                .to_vec(),
        };
        assert_eq!(indexed.resolve_untyped("alias"), expected);
        assert_eq!(indexed.resolve_untyped("absent/one"), expected);
        assert_eq!(
            indexed.resolve_typed(
                &RecordId::new("C").unwrap(),
                RecordKind::Source,
                Some("one.md")
            ),
            LinkResolution::WrongKind {
                actual: RecordKind::Entity
            }
        );
    }

    #[test]
    fn alias_matches_are_case_sensitive_and_entry_positions_are_deduplicated() {
        let indexed = IndexedRegistry::new(vec![entry(
            "A",
            RecordKind::Entity,
            "folder/Éclair.md",
            &["Éclair", "Éclair", "İstanbul", "Straße"],
        )]);
        for destination in ["Éclair", "absent/Éclair", "İstanbul", "Straße"] {
            assert!(matches!(
                indexed.resolve_untyped(destination),
                LinkResolution::Resolved { .. }
            ));
        }
        for destination in ["éclair", "E\u{301}clair", "istanbul", "STRASSE"] {
            assert_eq!(
                indexed.resolve_untyped(destination),
                LinkResolution::Missing
            );
        }
    }

    #[test]
    fn unrelated_registry_growth_does_not_increase_query_candidate_visits() {
        let target = entry(
            "A",
            RecordKind::Entity,
            "target/note.md",
            &["alias", "alias"],
        );
        let mut large_entries = vec![target.clone()];
        for n in 0..10_000 {
            large_entries.push(entry(
                &format!("unrelated-{n}"),
                RecordKind::Page,
                &format!("unrelated/{n}.md"),
                &[&format!("unrelated-alias-{n}")],
            ));
        }
        let small = IndexedRegistry::new(vec![target]);
        let large = IndexedRegistry::new(large_entries);
        for destination in ["target/note", "alias", "elsewhere/note.md", "missing"] {
            small.candidate_visits.set(0);
            large.candidate_visits.set(0);
            assert_eq!(
                small.resolve_untyped(destination),
                large.resolve_untyped(destination)
            );
            let visits = small.candidate_visits.get();
            assert_eq!(visits, large.candidate_visits.get(), "{destination}");
            assert!(visits <= 2, "{destination}: {visits}");
        }
        for (id, kind, companion) in [
            ("A", RecordKind::Entity, None),
            ("A", RecordKind::Entity, Some("target/note#Heading")),
            ("A", RecordKind::Entity, Some("stale.md#Heading")),
            ("A", RecordKind::Source, Some("target/note")),
            ("missing-id", RecordKind::Entity, Some("target/note")),
        ] {
            let id = RecordId::new(id).unwrap();
            small.candidate_visits.set(0);
            large.candidate_visits.set(0);
            assert_eq!(
                small.resolve_typed(&id, kind, companion),
                large.resolve_typed(&id, kind, companion)
            );
            let visits = small.candidate_visits.get();
            assert_eq!(visits, large.candidate_visits.get(), "{id}/{companion:?}");
            assert!(visits <= 2, "{id}/{companion:?}: {visits}");
        }
    }
}
