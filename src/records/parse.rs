//! Bounded event validation around an exact-byte Markdown envelope.
use crate::domain::{Blake3Hash, CanonicalRecord, WikiError};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, ops::Range};
use yaml_rust2::{
    Yaml,
    parser::{Event, Parser},
    scanner::TScalarStyle,
};

#[derive(Debug, Clone, Copy)]
pub struct ParseLimits {
    pub max_envelope_bytes: usize,
    pub max_depth: usize,
}
impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_envelope_bytes: 256 * 1024,
            max_depth: 32,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseStatus {
    NoFrontmatter,
    Valid,
    Invalid,
    UnsupportedSchema,
}
#[derive(Debug, Clone)]
pub struct ParsedNote {
    pub raw: Vec<u8>,
    pub status: ParseStatus,
    pub fields: Option<BTreeMap<String, Value>>,
    pub canonical: Option<CanonicalRecord>,
    pub diagnostics: Vec<WikiError>,
    pub source_hash: Blake3Hash,
    pub(crate) envelope: Option<Range<usize>>,
    pub(crate) closing_start: usize,
    pub(crate) body_start: usize,
    pub(crate) newline: &'static str,
    pub(crate) field_starts: BTreeMap<String, usize>,
}
impl ParsedNote {
    pub fn body(&self) -> &[u8] {
        &self.raw[self.body_start..]
    }
    pub fn literal_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.raw).ok()
    }
    pub fn is_editable(&self) -> bool {
        self.status == ParseStatus::Valid
    }
}
/// Versions the validation/range rules independently of cache SQL.
pub fn parser_fingerprint() -> Blake3Hash {
    Blake3Hash::digest(b"lwiki-lossless-v2;yaml-rust2=0.13.0;pulldown-cmark=0.13.4;utf8-char-markers;yaml-core-exact-i64-u64-radix;iterative-depth;default-envelope=262144;default-depth=32;scalar-inline-edit-v1")
}
pub fn parse_note(raw: &[u8]) -> ParsedNote {
    parse_note_with_limits(raw, ParseLimits::default())
}
pub fn parse_note_with_limits(raw: &[u8], limits: ParseLimits) -> ParsedNote {
    let mut note = ParsedNote {
        raw: raw.to_vec(),
        status: ParseStatus::NoFrontmatter,
        fields: None,
        canonical: None,
        diagnostics: vec![],
        source_hash: Blake3Hash::digest(raw),
        envelope: None,
        closing_start: 0,
        body_start: 0,
        newline: "\n",
        field_starts: BTreeMap::new(),
    };
    let text = match std::str::from_utf8(raw) {
        Ok(s) => s,
        Err(_) => {
            invalid(&mut note, WikiError::invalid("note is not UTF-8"));
            return note;
        }
    };
    let start = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let (first, next, nl) = line(raw, start);
    if first != b"---" {
        return note;
    }
    note.newline = nl;
    let mut cursor = next;
    let mut closing = None;
    while cursor < raw.len() {
        if cursor - next > limits.max_envelope_bytes {
            invalid(
                &mut note,
                WikiError::invalid("frontmatter exceeds byte limit"),
            );
            return note;
        }
        let (content, end, _) = line(raw, cursor);
        if content == b"---" {
            closing = Some((cursor, end));
            break;
        }
        cursor = end;
    }
    let Some((close, body)) = closing else {
        invalid(&mut note, WikiError::invalid("unterminated frontmatter"));
        return note;
    };
    if close - next > limits.max_envelope_bytes {
        invalid(
            &mut note,
            WikiError::invalid("frontmatter exceeds byte limit"),
        );
        return note;
    }
    note.envelope = Some(next..close);
    note.closing_start = close;
    note.body_start = body;
    let yaml = &text[next..close];
    match validate_yaml(yaml, limits.max_depth) {
        Err(error) => invalid(&mut note, error),
        Ok((fields, starts)) => {
            note.field_starts = starts
                .into_iter()
                .map(|(key, offset)| (key, next + offset))
                .collect();
            note.fields = Some(fields.clone());
            if fields
                .get("wiki_schema")
                .and_then(Value::as_str)
                .is_some_and(|s| {
                    s != "1"
                        && !(s == "2"
                            && fields.get("wiki_kind").and_then(Value::as_str) == Some("vault"))
                })
            {
                note.status = ParseStatus::UnsupportedSchema;
                note.diagnostics.push(WikiError::invalid(
                    "unsupported wiki_schema; structured data is read-only",
                ));
            } else {
                match CanonicalRecord::new(fields) {
                    Ok(record) => {
                        note.canonical = Some(record);
                        note.status = ParseStatus::Valid;
                    }
                    Err(error) => invalid(&mut note, error),
                }
            }
        }
    }
    note
}
fn invalid(note: &mut ParsedNote, error: WikiError) {
    note.status = ParseStatus::Invalid;
    note.diagnostics.push(error);
}
pub(crate) fn line(raw: &[u8], start: usize) -> (&[u8], usize, &'static str) {
    let end = raw[start..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(raw.len(), |i| start + i + 1);
    let mut content_end = end;
    let mut newline = "\n";
    if content_end > start && raw[content_end - 1] == b'\n' {
        content_end -= 1;
        if content_end > start && raw[content_end - 1] == b'\r' {
            content_end -= 1;
            newline = "\r\n";
        }
    }
    (&raw[start..content_end], end, newline)
}
enum Frame {
    Map(Map<String, Value>, Option<String>),
    Seq(Vec<Value>),
}
type YamlFields = (BTreeMap<String, Value>, BTreeMap<String, usize>);
fn validate_yaml(yaml: &str, max_depth: usize) -> Result<YamlFields, WikiError> {
    // Pull events instead of Parser::load's recursive container traversal. The
    // explicit stack rejects depth before allocating any child container.
    let mut parser = Parser::new_from_str(yaml);
    let mut stack: Vec<Frame> = vec![];
    let mut root = None;
    let mut starts = BTreeMap::new();
    let mut documents = 0;
    let char_bytes: Vec<usize> = yaml
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(yaml.len()))
        .collect();
    loop {
        let (event, marker) = parser
            .next_token()
            .map_err(|e| WikiError::invalid(format!("invalid YAML: {e}")))?;
        let mapping_start = matches!(event, Event::MappingStart(..));
        match event {
            Event::StreamStart | Event::DocumentEnd | Event::Nothing => {}
            Event::DocumentStart => {
                documents += 1;
                if documents != 1 {
                    return Err(WikiError::invalid(
                        "multiple YAML documents are unsupported",
                    ));
                }
            }
            Event::StreamEnd => break,
            Event::Alias(_) => return Err(WikiError::invalid("YAML aliases are unsupported")),
            Event::MappingStart(anchor, tag) | Event::SequenceStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    return Err(WikiError::invalid(
                        "YAML anchors and explicit tags are unsupported",
                    ));
                }
                if stack.len() >= max_depth {
                    return Err(WikiError::invalid("frontmatter exceeds nesting limit"));
                }
                if matches!(stack.last(), Some(Frame::Map(_, None))) {
                    return Err(WikiError::invalid("mapping keys must be strings"));
                }
                if mapping_start {
                    stack.push(Frame::Map(Map::new(), None));
                } else {
                    stack.push(Frame::Seq(vec![]));
                }
            }
            Event::MappingEnd | Event::SequenceEnd => {
                let value = match stack.pop() {
                    Some(Frame::Map(map, None)) => Value::Object(map),
                    Some(Frame::Seq(seq)) => Value::Array(seq),
                    _ => return Err(WikiError::invalid("incomplete YAML mapping")),
                };
                insert(&mut stack, &mut root, value)?;
            }
            Event::Scalar(text, style, anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    return Err(WikiError::invalid(
                        "YAML anchors and explicit tags are unsupported",
                    ));
                }
                let value = scalar(&text, style)?;
                let top = stack.len() == 1;
                if let Some(Frame::Map(map, key @ None)) = stack.last_mut() {
                    let Value::String(name) = value else {
                        return Err(WikiError::invalid("mapping keys must be strings"));
                    };
                    if name == "<<" {
                        return Err(WikiError::invalid("YAML merge keys are unsupported"));
                    }
                    if map.contains_key(&name) {
                        return Err(WikiError::invalid(format!("duplicate YAML key {name:?}")));
                    }
                    if top {
                        starts.insert(
                            name.clone(),
                            *char_bytes
                                .get(marker.index())
                                .ok_or_else(|| WikiError::invalid("invalid YAML marker"))?,
                        );
                    }
                    *key = Some(name);
                } else {
                    insert(&mut stack, &mut root, value)?;
                }
            }
        }
    }
    match root {
        Some(Value::Object(fields)) if stack.is_empty() => {
            Ok((fields.into_iter().collect(), starts))
        }
        _ => Err(WikiError::invalid("frontmatter must be a mapping")),
    }
}
fn insert(stack: &mut [Frame], root: &mut Option<Value>, value: Value) -> Result<(), WikiError> {
    match stack.last_mut() {
        Some(Frame::Map(map, key)) => {
            let name = key
                .take()
                .ok_or_else(|| WikiError::invalid("mapping keys must be strings"))?;
            map.insert(name, value);
        }
        Some(Frame::Seq(seq)) => seq.push(value),
        None => {
            if root.replace(value).is_some() {
                return Err(WikiError::invalid("multiple root YAML values"));
            }
        }
    }
    Ok(())
}
fn scalar(text: &str, style: TScalarStyle) -> Result<Value, WikiError> {
    if style != TScalarStyle::Plain {
        return Ok(Value::String(text.to_owned()));
    }
    // YAML 1.2 core spells null in all three conventional capitalizations;
    // yaml-rust2's convenience resolver only recognizes the lower-case form.
    if matches!(text, "" | "~" | "null" | "Null" | "NULL") {
        return Ok(Value::Null);
    }
    // Classify integer syntax before conversion. The convenience resolver can
    // fall back to String for radix overflow, or Real for decimal overflow.
    // Neither changes the YAML core integer type or permits float rounding.
    let overflow = || WikiError::invalid("out-of-range YAML integers are unsupported");
    for (prefix, radix) in [("0x", 16), ("0o", 8)] {
        if let Some(digits) = text.strip_prefix(prefix)
            && !digits.is_empty()
            && digits.chars().all(|c| c.is_digit(radix))
        {
            return u64::from_str_radix(digits, radix)
                .map(Value::from)
                .map_err(|_| overflow());
        }
    }
    let decimal = text.strip_prefix(['-', '+']).unwrap_or(text);
    if !decimal.is_empty() && decimal.bytes().all(|b| b.is_ascii_digit()) {
        return if text.starts_with('-') {
            text.parse::<i64>().map(Value::from).map_err(|_| overflow())
        } else {
            decimal
                .parse::<u64>()
                .map(Value::from)
                .map_err(|_| overflow())
        };
    }
    match Yaml::from_str(text) {
        Yaml::String(s) => Ok(Value::String(s)),
        Yaml::Integer(i) => Ok(Value::from(i)),
        Yaml::Boolean(b) => Ok(Value::Bool(b)),
        Yaml::Null => Ok(Value::Null),
        Yaml::Real(s) => s
            .parse::<serde_json::Number>()
            .map(Value::Number)
            .map_err(|_| {
                WikiError::invalid("non-finite/out-of-range YAML numbers are unsupported")
            }),
        _ => Err(WikiError::invalid("unsupported YAML scalar")),
    }
}
