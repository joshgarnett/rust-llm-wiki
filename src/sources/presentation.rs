//! Pure, guarded presentation plans. Immutable captures remain the only evidence.
use crate::{
    changes::{ExpectedWrite, ReadDependency},
    domain::{
        Blake3Hash, CanonicalRecord, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath,
        WikiError,
        source_presentation::{
            CurrentSourcePresentation, MAX_PUBLISHED_CAPTURES, SavedSourcePresentation,
            SourcePresentation,
        },
    },
    vault::ExpectedState,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct AuthenticatedCapture<'a> {
    pub revision: &'a CanonicalRecord,
    pub content: Option<&'a [u8]>,
}
pub(crate) struct SourcePresentationInput<'a> {
    pub source: &'a CanonicalRecord,
    pub source_body: &'a [u8],
    pub previous: Option<&'a SourcePresentation>,
    pub captures: &'a BTreeMap<RecordId, AuthenticatedCapture<'a>>,
    pub requested: &'a BTreeSet<RecordId>,
    pub allocated_current: &'a VaultRelativePath,
    pub allocated_saved: &'a BTreeMap<RecordId, VaultRelativePath>,
    pub observed_views: &'a BTreeMap<VaultRelativePath, Option<Vec<u8>>>,
}
pub(crate) struct SourcePresentationPlan {
    pub metadata: SourcePresentation,
    pub writes: Vec<ExpectedWrite>,
    pub dependencies: Vec<ReadDependency>,
    pub navigation: BTreeMap<RecordId, SourceNavigation>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceNavigation {
    pub current_path: VaultRelativePath,
    pub capture_anchor: String,
    pub saved_path: Option<VaultRelativePath>,
}

pub(crate) fn capture_anchor(revision: &RecordId) -> String {
    format!(
        "capture-{}",
        Blake3Hash::digest(revision.as_str().as_bytes()).hex()
    )
}

fn integrity(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::SourceIntegrity, message)
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}

/// Allocation is independent of title identity and portable across supported hosts.
/// The bounded caller supplies one namespace inventory shared by its entire batch.
pub(crate) fn allocate_path(
    title: &str,
    identity: &RecordId,
    date: Option<&str>,
    reserved: &BTreeSet<String>,
) -> Result<VaultRelativePath> {
    let mut title_slug = String::new();
    for ch in title.chars() {
        if title_slug.len() + ch.len_utf8() > 96 {
            break;
        }
        if ch.is_alphanumeric() || matches!(ch, '-' | '_') {
            title_slug.push(ch);
        } else if !title_slug.ends_with(' ') && !title_slug.is_empty() {
            title_slug.push(' ');
        }
    }
    let title_slug = title_slug.trim();
    let title_slug = if title_slug.is_empty() {
        "Saved note"
    } else {
        title_slug
    };
    let hash = Blake3Hash::digest(identity.as_str().as_bytes());
    let hex = hash.hex();
    for length in [12, 20, 32, 64] {
        let suffix = &hex[..length];
        let path = match date {
            Some(date)
                if date.len() == 10 && date.bytes().all(|b| b.is_ascii_digit() || b == b'-') =>
            {
                format!("sources/views/saved/{title_slug}-{date}-{suffix}.md")
            }
            Some(_) => return Err(WikiError::invalid("saved-note date is invalid")),
            None => format!("sources/views/{title_slug}-{suffix}.md"),
        };
        let folded = unicase::UniCase::unicode(path.as_str());
        if !reserved
            .iter()
            .any(|old| unicase::UniCase::unicode(old.as_str()) == folded)
        {
            return VaultRelativePath::new(path);
        }
    }
    Err(WikiError::new(
        ErrorCode::ContentConflict,
        "saved-note filename collides with an existing file; existing bytes were preserved",
    ))
}

fn text_label(value: &str) -> String {
    let mut text = String::new();
    for ch in value.chars() {
        if ch.is_control() {
            text.push(' ');
        } else {
            if matches!(
                ch,
                '\\' | '[' | ']' | '(' | ')' | '<' | '>' | '*' | '_' | '`' | '#' | '!' | '|'
            ) {
                text.push('\\');
            }
            text.push(ch);
        }
    }
    text
}

fn capture<'a, 'b>(
    input: &'a SourcePresentationInput<'b>,
    id: &RecordId,
    require_content: bool,
) -> Result<&'a AuthenticatedCapture<'b>> {
    let capture = input
        .captures
        .get(id)
        .ok_or_else(|| integrity(format!("saved-note capture {id} was not authenticated")))?;
    if capture.revision.kind() != RecordKind::Revision
        || capture.revision.id() != id
        || capture.revision.string("wiki_source_id") != Some(input.source.id().as_str())
    {
        return Err(integrity("saved-note capture ownership differs"));
    }
    if !require_content {
        return Ok(capture);
    }
    match (
        capture.revision.string("wiki_content_hash"),
        capture.content,
    ) {
        (Some(expected), Some(bytes)) if Blake3Hash::digest(bytes).as_str() == expected => {
            std::str::from_utf8(bytes).map_err(|_| integrity("saved-note capture is not UTF-8"))?;
        }
        (None, None) if capture.revision.string("wiki_extraction_status") != Some("complete") => {}
        _ => {
            return Err(integrity(
                "saved-note content hash or extraction state differs",
            ));
        }
    }
    Ok(capture)
}

fn date<'a>(capture: &'a AuthenticatedCapture<'_>) -> Result<&'a str> {
    capture
        .revision
        .string("wiki_captured_at")
        .ok_or_else(|| integrity("saved-note capture date missing"))
}

fn append_original(output: &mut String, content: &[u8]) -> Result<()> {
    let original =
        std::str::from_utf8(content).map_err(|_| integrity("saved-note content is not UTF-8"))?;
    let beginning = original.strip_prefix('\u{feff}').unwrap_or(original);
    let offset = original.len() - beginning.len();
    let mut lines = beginning.split_inclusive('\n');
    let first = lines.next().unwrap_or_default();
    let mut front_end = None;
    if first.trim_end_matches(['\r', '\n']) == "---" {
        let mut cursor = offset + first.len();
        for line in lines {
            cursor += line.len();
            if line.trim_end_matches(['\r', '\n']) == "---" {
                front_end = Some(cursor);
                break;
            }
        }
    }
    if let Some(end) = front_end {
        let front = &original[..end];
        let fence = "`".repeat(
            front
                .split(|ch| ch != '`')
                .map(str::len)
                .max()
                .unwrap_or(0)
                .max(2)
                + 1,
        );
        output.push_str("<details>\n<summary>Original note properties</summary>\n\n");
        output.push_str(&fence);
        output.push_str("text\n");
        output.push_str(front);
        if !front.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&fence);
        output.push_str("\n\n</details>\n\n");
        output.push_str(&original[end..]);
    } else {
        output.push_str(original);
    }
    Ok(())
}

fn header(
    source: &CanonicalRecord,
    capture: &AuthenticatedCapture<'_>,
    historical: bool,
) -> Result<String> {
    let mut output = format!("# {}\n\n", text_label(source.title()));
    output.push_str(&format!(
        "Saved copy from {}.\n\n",
        text_label(date(capture)?)
    ));
    output.push_str(&format!(
        "Original: {}\n\n",
        text_label(source.string("wiki_origin").unwrap_or("Unknown"))
    ));
    if historical {
        output.push_str("This is an earlier saved copy.\n\n");
    }
    if source.string("wiki_status") == Some("withdrawn") {
        output.push_str(
            "This source has been withdrawn. Its saved text is retained for earlier citations.\n\n",
        );
    }
    output.push_str("To change this text, edit the original note and refresh its saved copy. Write your own notes on a Page.\n\n");
    Ok(output)
}

fn append_text(output: &mut String, capture: &AuthenticatedCapture<'_>) -> Result<()> {
    if let Some(bytes) = capture.content {
        append_original(output, bytes)?;
    } else {
        output.push_str(
            "No readable text was available for this saved copy. The original file is retained.\n",
        );
    }
    Ok(())
}

fn checked_write(
    path: &VaultRelativePath,
    bytes: Vec<u8>,
    old_hash: Option<&Blake3Hash>,
    observed: &BTreeMap<VaultRelativePath, Option<Vec<u8>>>,
    writes: &mut Vec<ExpectedWrite>,
    dependencies: &mut Vec<ReadDependency>,
) -> Result<()> {
    if bytes.len() > crate::changes::prepare::MAX_PAYLOAD_BYTES {
        return Err(budget(
            "saved-note display exceeds the existing change payload limit",
        ));
    }
    let existing = observed
        .get(path)
        .ok_or_else(|| integrity(format!("saved-note destination {path} was not observed")))?;
    let expected = match (old_hash, existing) {
        (None, None) => ExpectedState::Absent,
        (Some(hash), Some(bytes)) if &Blake3Hash::digest(bytes) == hash => {
            ExpectedState::Hash(hash.clone())
        }
        _ => {
            let mut error = WikiError::new(
                ErrorCode::ContentConflict,
                format!("saved note {path} has unexpected bytes; your edits were preserved"),
            );
            error.hint = Some("Keep your changes in the original note or a Page. Restore this generated saved copy through an explicit reviewed write before refreshing it.".into());
            error.details = serde_json::json!({"reason":"saved_note_modified","path":path});
            return Err(error);
        }
    };
    if existing.as_ref() == Some(&bytes) {
        dependencies.push(ReadDependency {
            path: path.clone(),
            expected,
        });
    } else {
        writes.push(ExpectedWrite {
            target: path.clone(),
            expected,
            proposed: Some(bytes),
            apply_after: vec![],
        });
    }
    Ok(())
}

pub(crate) fn plan_source_presentation(
    input: SourcePresentationInput<'_>,
) -> Result<SourcePresentationPlan> {
    if input.source.kind() != RecordKind::Source {
        return Err(integrity("saved-note owner is not a Source"));
    }
    let head = RecordId::new(
        input
            .source
            .string("wiki_current_revision")
            .ok_or_else(|| integrity("Source head missing"))?,
    )?;
    let current_capture = capture(&input, &head, true)?;
    let mut published = input
        .previous
        .map(|old| old.published.clone())
        .unwrap_or_default();
    if let Some(previous) = input.previous {
        previous.validate_shape()?;
        if &previous.source_id != input.source.id()
            || &previous.current.path != input.allocated_current
        {
            return Err(integrity("saved-note owner or stable path changed"));
        }
    }
    for id in input.requested {
        published.entry(id.clone()).or_insert(None);
    }
    if published.len() > MAX_PUBLISHED_CAPTURES {
        return Err(budget(
            "saved-note published capture limit reached; existing navigation was preserved",
        ));
    }
    let mut writes = Vec::new();
    let mut dependencies = Vec::new();
    let mut navigation = BTreeMap::new();
    for (id, saved) in &mut published {
        let evidence = capture(&input, id, id != &head && saved.is_none())?;
        if saved.is_none() && (id != &head || input.allocated_saved.contains_key(id)) {
            let path = input.allocated_saved.get(id).ok_or_else(|| {
                integrity(format!(
                    "saved-note historical path for {id} was not allocated"
                ))
            })?;
            let mut output = header(input.source, evidence, true)?;
            output.push_str(&format!(
                "[[{}|Open the latest saved copy]]\n\n",
                input.allocated_current
            ));
            append_text(&mut output, evidence)?;
            let bytes = output.into_bytes();
            checked_write(
                path,
                bytes.clone(),
                None,
                input.observed_views,
                &mut writes,
                &mut dependencies,
            )?;
            *saved = Some(SavedSourcePresentation {
                path: path.clone(),
                generated_hash: Blake3Hash::digest(&bytes),
            });
        } else if let Some(saved) = saved
            && input.requested.contains(id)
        {
            // An existing dated note is immutable presentation output. Its exact
            // owner-bound bytes are checked, but a later title/status change does
            // not rewrite an already published full capture.
            let existing = input
                .observed_views
                .get(&saved.path)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    integrity(format!("published saved note {} is missing", saved.path))
                })?;
            checked_write(
                &saved.path,
                existing.clone(),
                Some(&saved.generated_hash),
                input.observed_views,
                &mut writes,
                &mut dependencies,
            )?;
        }
        navigation.insert(
            id.clone(),
            SourceNavigation {
                current_path: input.allocated_current.clone(),
                capture_anchor: capture_anchor(id),
                saved_path: saved.as_ref().map(|saved| saved.path.clone()),
            },
        );
    }
    let mut output = header(input.source, current_capture, false)?;
    for (id, saved) in &published {
        if id == &head {
            continue;
        }
        let saved = saved
            .as_ref()
            .ok_or_else(|| integrity("historical navigation lacks a saved copy"))?;
        output.push_str(&format!(
            "A newer saved copy appears below. [[{}|Open the saved copy from {}]]. ^{}\n\n",
            saved.path,
            text_label(date(capture(&input, id, false)?)?),
            capture_anchor(id)
        ));
    }
    if !input.source_body.is_empty() {
        output.push_str("## Notes kept with this source\n\n");
        output.push_str(
            std::str::from_utf8(input.source_body)
                .map_err(|_| integrity("Source body is not UTF-8"))?,
        );
        output.push_str("\n\n");
    }
    output.push_str(&format!(
        "Saved text from {}. ^{}\n\n",
        text_label(date(current_capture)?),
        capture_anchor(&head)
    ));
    append_text(&mut output, current_capture)?;
    let bytes = output.into_bytes();
    let metadata = SourcePresentation {
        version: 1,
        source_id: input.source.id().clone(),
        current: CurrentSourcePresentation {
            path: input.allocated_current.clone(),
            revision_id: head.clone(),
            content_hash: current_capture
                .revision
                .string("wiki_content_hash")
                .map(Blake3Hash::new)
                .transpose()?,
            generated_hash: Blake3Hash::digest(&bytes),
        },
        published,
    };
    metadata.validate_owner(input.source)?;
    metadata.encode()?;
    checked_write(
        input.allocated_current,
        bytes,
        input.previous.map(|old| &old.current.generated_hash),
        input.observed_views,
        &mut writes,
        &mut dependencies,
    )?;
    navigation.entry(head.clone()).or_insert(SourceNavigation {
        current_path: input.allocated_current.clone(),
        capture_anchor: capture_anchor(&head),
        saved_path: None,
    });
    Ok(SourcePresentationPlan {
        metadata,
        writes,
        dependencies,
        navigation,
    })
}

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod tests;
