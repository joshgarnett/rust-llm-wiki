//! Pure navigation extraction and registry resolution. Labels never create IDs.
use crate::domain::{RecordId, RecordKind, VaultRelativePath};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use std::{collections::BTreeSet, ops::Range};

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
