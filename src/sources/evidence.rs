//! Exact source slices, durable assertion citations, and successor evidence.
use super::{
    capture::{create_write, draft},
    revision::{common, dependencies, integrity, record_bytes},
    types::*,
};
use crate::{
    domain::{
        Blake3Hash, ByteSpan, CanonicalRecord, CitationRef, ErrorCode, EvidenceRef, RecordId,
        RecordKind, Result, SourceSpanRef, VaultRelativePath, WikiError,
    },
    records::ParsedNote,
};
use std::collections::BTreeMap;

pub fn unique_quote_span(content: &[u8], quote: &[u8], window: ByteSpan) -> Result<ByteSpan> {
    let text = std::str::from_utf8(content).map_err(|_| integrity("content is not UTF-8"))?;
    std::str::from_utf8(quote).map_err(|_| WikiError::invalid("quote is not UTF-8"))?;
    if quote.is_empty() {
        return Err(WikiError::new(
            ErrorCode::ExtractionInvalid,
            "quote must not be empty",
        ));
    }
    let selected = window
        .slice(text)
        .map_err(|_| {
            WikiError::new(
                ErrorCode::ExtractionInvalid,
                "quote window is out of bounds or splits UTF-8",
            )
        })?
        .as_bytes();
    let mut matches = selected
        .windows(quote.len())
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == quote).then_some(index));
    let found = matches.next();
    if found.is_none() || matches.next().is_some() {
        return Err(WikiError::new(
            ErrorCode::ExtractionInvalid,
            "quotation must have one exact match in its declared window",
        ));
    }
    let start = window.start() + found.expect("single match") as u64;
    ByteSpan::new(start, start + quote.len() as u64)
}
pub fn exact_quote_body(quote: &[u8], newline: &str, explanation: &str) -> Result<Vec<u8>> {
    if !matches!(newline, "\n" | "\r\n") {
        return Err(WikiError::invalid("note newline must be LF or CRLF"));
    }
    std::str::from_utf8(quote).map_err(|_| WikiError::invalid("quote is not UTF-8"))?;
    let mut longest = 0usize;
    let mut run = 0usize;
    for &byte in quote {
        if byte == b'`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(3.max(longest + 1));
    let mut body = format!("{fence}text{newline}").into_bytes();
    body.extend_from_slice(quote);
    body.extend_from_slice(newline.as_bytes());
    body.extend_from_slice(format!("{fence}{newline}{newline}{explanation}").as_bytes());
    if quotation(&body, newline.as_bytes())? != quote {
        return Err(WikiError::invalid(
            "explanation introduces an additional quotation",
        ));
    }
    Ok(body)
}
fn line(bytes: &[u8], start: usize) -> (&[u8], usize) {
    let end = bytes[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |i| start + i + 1);
    let mut content_end = end;
    if content_end > start && bytes[content_end - 1] == b'\n' {
        content_end -= 1;
    }
    if content_end > start && bytes[content_end - 1] == b'\r' {
        content_end -= 1;
    }
    (&bytes[start..content_end], end)
}
fn quotation(body: &[u8], separator: &[u8]) -> Result<Vec<u8>> {
    // Nested blockquotes/lists can contain real fences without a raw fence prefix.
    // Use Markdown structure for cardinality, then retain raw bytes for exactness.
    let text = std::str::from_utf8(body).map_err(|_| integrity("quotation body is not UTF-8"))?;
    let blocks: Vec<_> = pulldown_cmark::Parser::new(text)
        .into_offset_iter()
        .filter_map(|(event, range)| {
            matches!(event,
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(
                    pulldown_cmark::CodeBlockKind::Fenced(info)
                )) if info.split_whitespace().next() == Some("text")
            )
            .then_some(range)
        })
        .collect();
    if blocks.len() != 1 {
        return Err(integrity(
            "evidence must contain exactly one text quotation",
        ));
    }
    // Extract from that actual block, never an HTML/comment decoy elsewhere.
    // Container prefixes stay unmodified and are conservatively refused.
    let body = &body[blocks[0].clone()];
    let mut cursor = 0;
    let mut found = None;
    while cursor < body.len() {
        let (raw, next) = line(body, cursor);
        let indent = raw.iter().take_while(|&&b| b == b' ').count();
        let raw = if indent <= 3 { &raw[indent..] } else { raw };
        let marker = raw.first().copied().unwrap_or_default();
        if !matches!(marker, b'`' | b'~') {
            cursor = next;
            continue;
        }
        let ticks = raw.iter().take_while(|&&b| b == marker).count();
        if ticks < 3 {
            cursor = next;
            continue;
        }
        let info = std::str::from_utf8(&raw[ticks..])
            .map_err(|_| integrity("quotation body is not UTF-8"))?
            .trim();
        if marker == b'`' && info.contains('`') {
            cursor = next;
            continue;
        }
        let is_text = info == "text";
        let start = next;
        cursor = next;
        let mut close = None;
        while cursor < body.len() {
            let (candidate, end) = line(body, cursor);
            let spaces = candidate.iter().take_while(|&&b| b == b' ').count();
            let candidate = if spaces <= 3 {
                &candidate[spaces..]
            } else {
                candidate
            };
            let count = candidate.iter().take_while(|&&b| b == marker).count();
            if count >= ticks && candidate[count..].iter().all(u8::is_ascii_whitespace) {
                close = Some((cursor, end));
                break;
            }
            cursor = end;
        }
        let Some((end, next)) = close else {
            return Err(integrity("unterminated quotation fence"));
        };
        if is_text {
            if found.is_some() {
                return Err(integrity(
                    "evidence must contain exactly one text quotation",
                ));
            }
            let interior = &body[start..end];
            let quote = interior
                .strip_suffix(separator)
                .ok_or_else(|| integrity("quotation lacks exactly one separator newline"))?;
            found = Some(quote.to_vec());
        }
        cursor = next;
    }
    found.ok_or_else(|| integrity("evidence must contain exactly one text quotation"))
}
pub(crate) fn note_quote(note: &ParsedNote) -> Result<Vec<u8>> {
    quotation(note.body(), note.newline.as_bytes())
}
impl SourceView<'_> {
    pub fn verify(&self, citation: &CitationRef, scope: CitationScope) -> Result<VerifiedCitation> {
        let mut deps = BTreeMap::new();
        let source_ref = match citation {
            CitationRef::Source(span) => span.clone(),
            CitationRef::Assertion(evidence) => SourceSpanRef {
                source_id: evidence.source_id.clone(),
                source_revision: evidence.source_revision.clone(),
                span: evidence.span,
                quote_hash: evidence.quote_hash.clone(),
            },
        };
        let content = self.revision_content(
            &source_ref.source_id,
            &source_ref.source_revision,
            &mut deps,
        )?;
        let text = std::str::from_utf8(&content).expect("verified text");
        if source_ref.span.is_empty() {
            return Err(integrity("empty source span"));
        }
        let quote = source_ref
            .span
            .slice(text)
            .map_err(|_| integrity("source span out of bounds or splits UTF-8"))?
            .as_bytes()
            .to_vec();
        if Blake3Hash::digest(&quote) != source_ref.quote_hash {
            return Err(integrity("selected quote hash mismatch"));
        }
        let (_, sn) = self.resolve(&source_ref.source_id, RecordKind::Source, None)?;
        let source = sn.canonical.as_ref().expect("resolved");
        let mut state = if source.string("wiki_status") == Some("withdrawn") {
            CitationState::Withdrawn
        } else if source.string("wiki_current_revision")
            == Some(source_ref.source_revision.as_str())
        {
            CitationState::Current
        } else {
            CitationState::Historical
        };
        if let CitationRef::Assertion(reference) = citation {
            let (ep, en) = self.resolve(&reference.evidence_id, RecordKind::Evidence, None)?;
            Self::note_dependency(ep, en, &mut deps);
            let evidence = en.canonical.as_ref().expect("resolved");
            let actual = evidence_reference(evidence)?;
            if &actual != reference {
                return Err(integrity("citation disagrees with durable evidence fields"));
            }
            self.resolve(
                &reference.source_id,
                RecordKind::Source,
                evidence.string("wiki_source"),
            )?;
            self.resolve(
                &reference.source_revision,
                RecordKind::Revision,
                evidence.string("wiki_revision"),
            )?;
            let (ap, an) = self.resolve(
                &reference.assertion_id,
                RecordKind::Assertion,
                evidence.string("wiki_assertion"),
            )?;
            Self::note_dependency(ap, an, &mut deps);
            if note_quote(en)? != quote {
                return Err(integrity("fenced quotation differs from source slice"));
            }
            if state == CitationState::Current
                && (evidence.string("wiki_status") != Some("active")
                    || an
                        .canonical
                        .as_ref()
                        .expect("resolved")
                        .string("wiki_status")
                        != Some("accepted"))
            {
                state = CitationState::Historical;
            }
        }
        if scope == CitationScope::Current && state != CitationState::Current {
            return Err(integrity(format!("citation is {state:?}, not current")));
        }
        Ok(VerifiedCitation {
            citation: citation.clone(),
            quote,
            state,
            dependencies: dependencies(deps),
        })
    }
}
pub(crate) fn evidence_reference(record: &CanonicalRecord) -> Result<EvidenceRef> {
    Ok(EvidenceRef {
        evidence_id: record.id().clone(),
        assertion_id: RecordId::new(record.string("wiki_assertion_id").expect("validated"))?,
        source_id: RecordId::new(record.string("wiki_source_id").expect("validated"))?,
        source_revision: RecordId::new(record.string("wiki_source_revision").expect("validated"))?,
        span: ByteSpan::new(
            record
                .field("wiki_span_start")
                .and_then(serde_json::Value::as_u64)
                .expect("validated"),
            record
                .field("wiki_span_end")
                .and_then(serde_json::Value::as_u64)
                .expect("validated"),
        )?,
        quote_hash: Blake3Hash::new(record.string("wiki_quote_hash").expect("validated"))?,
    })
}
impl SourceStore {
    pub fn plan_evidence(&self, request: EvidenceRequest) -> Result<EvidencePlan> {
        let view = self.view()?;
        build_evidence(&view, request, None, None)
    }
    pub fn plan_revalidate(
        &self,
        evidence_id: &RecordId,
        to_revision: &RecordId,
        expected_hash: &Blake3Hash,
    ) -> Result<EvidencePlan> {
        let view = self.view()?;
        let (path, note) = view.resolve(evidence_id, RecordKind::Evidence, None)?;
        if &note.source_hash != expected_hash {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "evidence hash differs from expected hash",
            ));
        }
        let record = note.canonical.as_ref().expect("resolved");
        let reference = evidence_reference(record)?;
        let verified = view.verify(
            &CitationRef::Assertion(reference.clone()),
            CitationScope::Historical,
        )?;
        let mut deps = BTreeMap::new();
        let content = view.revision_content(&reference.source_id, to_revision, &mut deps)?;
        let request = EvidenceRequest {
            assertion_id: reference.assertion_id,
            source_id: reference.source_id,
            revision_id: to_revision.clone(),
            quote: verified.quote,
            window: ByteSpan::new(0, content.len() as u64)?,
            stance: if record.string("wiki_stance") == Some("supports") {
                EvidenceStance::Supports
            } else {
                EvidenceStance::Contradicts
            },
            explanation: format!("Revalidated exact quotation from {evidence_id}."),
            title: record.title().into(),
        };
        let mut plan = build_evidence(&view, request, Some(evidence_id), Some(path))?;
        let mut all: BTreeMap<_, _> = plan
            .dependencies
            .into_iter()
            .map(|d| (d.path, d.expected))
            .collect();
        all.extend(
            verified
                .dependencies
                .into_iter()
                .map(|d| (d.path, d.expected)),
        );
        plan.dependencies = dependencies(all);
        plan.draft.read_preconditions = plan.dependencies.clone();
        Ok(plan)
    }
}
fn build_evidence(
    view: &SourceView<'_>,
    request: EvidenceRequest,
    supersedes: Option<&RecordId>,
    supersedes_path: Option<&VaultRelativePath>,
) -> Result<EvidencePlan> {
    let mut deps = BTreeMap::new();
    let content = view.revision_content(&request.source_id, &request.revision_id, &mut deps)?;
    let span = unique_quote_span(&content, &request.quote, request.window)?;
    let (ap, an) = view.resolve(&request.assertion_id, RecordKind::Assertion, None)?;
    SourceView::note_dependency(ap, an, &mut deps);
    let (sp, _) = view.resolve(&request.source_id, RecordKind::Source, None)?;
    let (rp, _) = view.resolve(&request.revision_id, RecordKind::Revision, None)?;
    let id = RecordId::generate(RecordKind::Evidence)?;
    let mut fields = common(&id, RecordKind::Evidence, &request.title);
    fields.extend(BTreeMap::from([
        ("wiki_status".into(), "active".into()),
        (
            "wiki_assertion_id".into(),
            request.assertion_id.as_str().into(),
        ),
        ("wiki_source_id".into(), request.source_id.as_str().into()),
        (
            "wiki_source_revision".into(),
            request.revision_id.as_str().into(),
        ),
        ("wiki_assertion".into(), format!("[[{ap}]]").into()),
        ("wiki_source".into(), format!("[[{sp}]]").into()),
        ("wiki_revision".into(), format!("[[{rp}]]").into()),
        ("wiki_stance".into(), request.stance.as_str().into()),
        ("wiki_locator_kind".into(), "utf8-bytes".into()),
        ("wiki_span_start".into(), span.start().into()),
        ("wiki_span_end".into(), span.end().into()),
        (
            "wiki_quote_hash".into(),
            Blake3Hash::digest(&request.quote).to_string().into(),
        ),
    ]));
    if let Some(supersedes) = supersedes {
        fields.insert("wiki_supersedes_id".into(), supersedes.as_str().into());
    }
    if let Some(path) = supersedes_path {
        fields.insert("wiki_supersedes".into(), format!("[[{path}]]").into());
    }
    let record = CanonicalRecord::new(fields)?;
    let citation = evidence_reference(&record)?;
    let body = exact_quote_body(&request.quote, "\n", &request.explanation)?;
    let bytes = record_bytes(record, &body)?;
    let read_preconditions = dependencies(deps);
    let mut change = draft(
        format!("Evidence {}", request.title),
        vec![create_write(
            VaultRelativePath::new(format!("knowledge/evidence/{id}.md"))?,
            bytes,
        )],
        BTreeMap::from([("evidence".into(), id.clone())]),
    );
    change.read_preconditions = read_preconditions.clone();
    Ok(EvidencePlan {
        draft: change,
        evidence_id: id,
        citation,
        dependencies: read_preconditions,
    })
}
