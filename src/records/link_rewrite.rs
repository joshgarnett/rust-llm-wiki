//! Parser-preserving navigation edits over a caller-authenticated resolver.
use crate::{
    domain::{ErrorCode, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    records::{LinkResolution, LinkSyntax, ParsedNote, extract_links},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};
fn markdown_destination(
    raw: &str,
    range: &Range<usize>,
    destination: &str,
    syntax: LinkSyntax,
) -> String {
    if syntax == LinkSyntax::Markdown
        && destination.chars().any(char::is_whitespace)
        && (range.start == 0 || raw.as_bytes()[range.start - 1] != b'<')
    {
        format!("<{destination}>")
    } else {
        destination.to_owned()
    }
}
fn splice(raw: &[u8], mut edits: Vec<(Range<usize>, Vec<u8>)>) -> Result<Vec<u8>> {
    edits.sort_by_key(|(range, _)| range.start);
    edits.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    let mut out = Vec::new();
    let mut cursor = 0;
    for (range, value) in edits {
        if range.start < cursor || range.end > raw.len() {
            return Err(WikiError::invalid(
                "overlapping or invalid link edit ranges",
            ));
        }
        out.extend_from_slice(&raw[cursor..range.start]);
        out.extend(value);
        cursor = range.end;
    }
    out.extend_from_slice(&raw[cursor..]);
    Ok(out)
}
fn destination_range(raw: &str, start: usize) -> Result<Range<usize>> {
    let bytes = raw.as_bytes();
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) == Some(&b'<') {
        let begin = i + 1;
        let end = raw[begin..]
            .find('>')
            .map(|n| begin + n)
            .ok_or_else(|| WikiError::invalid("cannot safely locate Markdown destination"))?;
        return Ok(begin..end);
    }
    let begin = i;
    let mut depth = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i = i.saturating_add(2);
                continue;
            }
            b'(' => depth += 1,
            b')' if depth == 0 => break,
            b')' => depth -= 1,
            b if b.is_ascii_whitespace() && depth == 0 => break,
            _ => {}
        }
        i += 1;
    }
    if begin == i {
        return Err(WikiError::invalid(
            "cannot safely locate Markdown destination",
        ));
    }
    Ok(begin..i)
}
/// Only parser-resolved links change; original labels, fragments and titles survive.
pub(crate) fn rewrite_links_selected(
    body: &str,
    resolve: &mut dyn FnMut(&str) -> Result<LinkResolution>,
    target: &RecordId,
    to: &VaultRelativePath,
) -> Result<Vec<u8>> {
    let mut edits = Vec::new();
    let mut reference_destinations = BTreeSet::new();
    let original_links = extract_links(body);
    let mut expected = Vec::new();
    for (index, link) in original_links.iter().enumerate() {
        let LinkResolution::Resolved { id, fragment, .. } = resolve(&link.destination)? else {
            continue;
        };
        if &id != target {
            continue;
        }
        let new_dest = fragment.map_or_else(|| to.as_str().to_owned(), |f| format!("{to}#{f}"));
        expected.push((index, new_dest.clone()));
        let raw = &body[link.range.clone()];
        let local = match link.syntax {
            LinkSyntax::Wiki => {
                let end = raw[2..raw.len() - 2]
                    .find('|')
                    .map_or(raw.len() - 2, |n| n + 2);
                Some(2..end)
            }
            LinkSyntax::Markdown => {
                if let Some(at) = raw.find("](") {
                    Some(destination_range(raw, at + 2)?)
                } else {
                    reference_destinations.insert((link.destination.clone(), new_dest.clone()));
                    None
                }
            }
        };
        if let Some(range) = local {
            if raw[range.clone()] != link.destination {
                return Err(WikiError::invalid(
                    "cannot safely preserve escaped or complex Markdown destination",
                ));
            }
            edits.push((
                link.range.start + range.start..link.range.start + range.end,
                markdown_destination(raw, &range, &new_dest, link.syntax).into_bytes(),
            ));
        }
    }
    let parser = pulldown_cmark::Parser::new(body);
    for (_, definition) in parser.reference_definitions().iter() {
        if let Some((_, new_dest)) = reference_destinations
            .iter()
            .find(|(dest, _)| dest == definition.dest.as_ref())
        {
            let raw = &body[definition.span.clone()];
            let at = raw
                .find("]:")
                .ok_or_else(|| WikiError::invalid("cannot safely locate reference destination"))?;
            let range = destination_range(raw, at + 2)?;
            if &raw[range.clone()] != definition.dest.as_ref() {
                return Err(WikiError::invalid(
                    "cannot safely preserve escaped reference destination",
                ));
            }
            edits.push((
                definition.span.start + range.start..definition.span.start + range.end,
                markdown_destination(raw, &range, new_dest, LinkSyntax::Markdown).into_bytes(),
            ));
        }
    }
    let changed = splice(body.as_bytes(), edits)?;
    let text = std::str::from_utf8(&changed)
        .map_err(|_| WikiError::invalid("link edit produced non-UTF-8"))?;
    let after = extract_links(text);
    if after.len() != original_links.len()
        || expected
            .iter()
            .any(|(index, destination)| after[*index].destination != *destination)
    {
        return Err(WikiError::invalid(
            "link edit cannot preserve parsed destinations safely",
        ));
    }
    Ok(changed)
}
pub(crate) fn rewrite_companions_selected(
    note: &ParsedNote,
    resolve: &mut dyn FnMut(&str) -> Result<LinkResolution>,
    target: &RecordId,
    to: &VaultRelativePath,
) -> Result<BTreeMap<String, serde_json::Value>> {
    let mut changes = BTreeMap::new();
    let Some(record) = &note.canonical else {
        return Ok(changes);
    };
    for (key, value) in record.fields() {
        if !key.starts_with("wiki_") {
            continue;
        }
        let Some(value) = value
            .as_str()
            .filter(|v| v.starts_with("[[") && v.ends_with("]]"))
        else {
            continue;
        };
        let id_field = if key == "wiki_revision" && record.kind() == RecordKind::Source {
            "wiki_current_revision".to_owned()
        } else if key == "wiki_revision" {
            "wiki_source_revision".to_owned()
        } else {
            format!("{key}_id")
        };
        if record.string(&id_field) != Some(target.as_str()) {
            continue;
        }
        match resolve(value)? {
            LinkResolution::Resolved { id, .. } if &id != target => {
                return Err(WikiError::invalid(
                    "incoming companion resolves to another identity",
                ));
            }
            LinkResolution::Ambiguous { .. } => {
                return Err(WikiError::new(
                    ErrorCode::ReferenceAmbiguous,
                    "incoming companion path is ambiguous",
                ));
            }
            LinkResolution::External => {
                return Err(WikiError::invalid("incoming companion is external"));
            }
            _ => {}
        }
        let interior = &value[2..value.len() - 2];
        let (destination, label) = interior
            .split_once('|')
            .map_or((interior, None), |(path, label)| (path, Some(label)));
        let fragment = destination.split_once('#').map(|(_, fragment)| fragment);
        let mut replacement = format!("[[{to}");
        if let Some(fragment) = fragment {
            replacement.push('#');
            replacement.push_str(fragment);
        }
        if let Some(label) = label {
            replacement.push('|');
            replacement.push_str(label);
        }
        replacement.push_str("]]");
        changes.insert(key.clone(), replacement.into());
    }
    Ok(changes)
}
