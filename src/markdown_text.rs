//! Bounded readable runs from original HTML events; no rendering or decoding.
use std::ops::Range;

const MAX_PAIR_BYTES: usize = 1024 * 1024;
const MAX_OPEN_UNITS: usize = 64;
const MAX_RETURNED_UNITS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HtmlUnitKind {
    Paragraph,
    Preformatted,
    Heading(u8),
}

#[derive(Clone, Debug)]
pub(crate) struct HtmlUnit {
    pub(crate) kind: HtmlUnitKind,
    pub(crate) range: Range<usize>,
    /// False for a balanced child inside an unresolved or crossing heading.
    pub(crate) scope_complete: bool,
}

#[derive(Default)]
pub(crate) struct HtmlScanner {
    state: State,
    raw: Option<RawTag>,
    definition: Option<Definition>,
    open_units: Vec<HtmlUnit>,
}

#[derive(Default)]
pub(crate) struct HtmlText {
    pub(crate) spans: Vec<Range<usize>>,
    pub(crate) definition_pairs: Vec<Range<usize>>,
    /// Explicitly balanced original elements, never repaired HTML or a DOM.
    pub(crate) units: Vec<HtmlUnit>,
    pub(crate) units_truncated: bool,
}

#[derive(Default)]
enum State {
    #[default]
    Text,
    Tag(Tag),
    Comment(u8),
}

#[derive(Clone, Copy)]
enum RawTag {
    Script,
    Style,
}
impl RawTag {
    fn name(self) -> &'static [u8] {
        match self {
            Self::Script => b"script",
            Self::Style => b"style",
        }
    }
}

struct Definition {
    start: usize,
    label: bool,
    value: bool,
    phase: Phase,
}
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Label,
    Between,
    Value,
}

struct Tag {
    start: usize,
    name: [u8; 16],
    len: usize,
    closing: bool,
    stage: Stage,
    quote: Option<u8>,
}
#[derive(Clone, Copy)]
enum Stage {
    Begin,
    Name,
    Attributes,
    Bang,
    Dash,
}
impl Tag {
    fn unit_kind(&self) -> Option<HtmlUnitKind> {
        if self.named(b"p") {
            Some(HtmlUnitKind::Paragraph)
        } else if self.named(b"pre") {
            Some(HtmlUnitKind::Preformatted)
        } else if self.len == 2 && self.name[0] == b'h' && (b'1'..=b'6').contains(&self.name[1]) {
            Some(HtmlUnitKind::Heading(self.name[1] - b'0'))
        } else {
            None
        }
    }
    fn new(start: usize) -> Self {
        Self {
            start,
            name: [0; 16],
            len: 0,
            closing: false,
            stage: Stage::Begin,
            quote: None,
        }
    }
    fn named(&self, name: &[u8]) -> bool {
        self.len == name.len() && self.name.get(..self.len) == Some(name)
    }
    fn push(&mut self, byte: u8) {
        if let Some(slot) = self.name.get_mut(self.len) {
            *slot = byte.to_ascii_lowercase();
        }
        // An oversized name cannot match any supported structural tag.
        self.len = self.len.saturating_add(1).min(self.name.len() + 1);
    }
}

impl HtmlScanner {
    pub(crate) fn has_open_heading(&self) -> bool {
        self.open_units
            .iter()
            .any(|unit| matches!(unit.kind, HtmlUnitKind::Heading(_)))
    }
    pub(crate) fn suppressing_text(&self) -> bool {
        // Partial ordinary markup is suppressed only in scan-fed HTML runs.
        // It cannot establish that a separate Markdown Text event is hidden.
        self.raw.is_some() || matches!(self.state, State::Comment(_))
    }

    pub(crate) fn scan(&mut self, raw: &str, range: Range<usize>) -> HtmlText {
        let mut output = HtmlText::default();
        if range.start > range.end || raw.get(range.clone()).is_none() {
            return output;
        }
        let bytes = raw.as_bytes();
        let mut cursor = range.start;
        while cursor < range.end {
            if self
                .definition
                .as_ref()
                .is_some_and(|pair| cursor.saturating_sub(pair.start) >= MAX_PAIR_BYTES)
            {
                self.definition = None;
            }
            let byte = bytes[cursor];
            match std::mem::take(&mut self.state) {
                State::Text => {
                    if byte == b'<' {
                        self.state = State::Tag(Tag::new(cursor));
                        cursor += 1;
                    } else {
                        let end = bytes[cursor..range.end]
                            .iter()
                            .position(|byte| *byte == b'<')
                            .map_or(range.end, |relative| cursor + relative);
                        if self.raw.is_none() {
                            output.spans.push(cursor..end);
                            if !raw[cursor..end].trim().is_empty()
                                && let Some(pair) = &mut self.definition
                            {
                                match pair.phase {
                                    Phase::Label => pair.label = true,
                                    Phase::Value => pair.value = true,
                                    Phase::Between => {}
                                }
                            }
                        }
                        cursor = end;
                    }
                }
                State::Comment(dashes) => {
                    if byte == b'>' && dashes >= 2 {
                        self.state = State::Text;
                    } else {
                        self.state = State::Comment(if byte == b'-' {
                            dashes.saturating_add(1).min(2)
                        } else {
                            0
                        });
                    }
                    cursor += 1;
                }
                State::Tag(mut tag) => {
                    // Within raw text, only an actual matching end tag matters.
                    // A comparison '<' must not consume a later '</script>'.
                    if self.raw.is_some()
                        && matches!(tag.stage, Stage::Begin)
                        && byte != b'/'
                        && !tag.closing
                    {
                        continue;
                    }
                    let mut done = false;
                    let mut invalid = false;
                    match tag.stage {
                        Stage::Begin => match byte {
                            b'/' if !tag.closing => tag.closing = true,
                            b'!' if !tag.closing => tag.stage = Stage::Bang,
                            b'?' if !tag.closing => tag.stage = Stage::Attributes,
                            byte if byte.is_ascii_alphabetic() => {
                                tag.push(byte);
                                tag.stage = Stage::Name;
                            }
                            _ => invalid = true,
                        },
                        Stage::Bang if byte == b'-' => tag.stage = Stage::Dash,
                        Stage::Dash if byte == b'-' => {
                            self.state = State::Comment(0);
                            cursor += 1;
                            continue;
                        }
                        Stage::Bang | Stage::Dash => {
                            tag.stage = Stage::Attributes;
                            // Reprocess the declaration byte, including '>'.
                            self.state = State::Tag(tag);
                            continue;
                        }
                        Stage::Name => {
                            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b':') {
                                tag.push(byte);
                            } else if byte == b'>' || byte == b'/' || byte.is_ascii_whitespace() {
                                if self.raw.is_some_and(|kind| !tag.named(kind.name())) {
                                    // Revisit this byte as raw text; do not swallow a close tag.
                                    continue;
                                }
                                tag.stage = Stage::Attributes;
                                self.state = State::Tag(tag);
                                continue;
                            } else {
                                invalid = true;
                            }
                        }
                        Stage::Attributes => {
                            if let Some(quote) = tag.quote {
                                if byte == quote {
                                    tag.quote = None;
                                }
                            } else if matches!(byte, b'\'' | b'"') {
                                tag.quote = Some(byte);
                            } else if byte == b'>' {
                                done = true;
                            }
                        }
                    }
                    if invalid {
                        // Preserve a malformed '<' prefix only within this input.
                        if self.raw.is_none() && tag.start >= range.start {
                            output.spans.push(tag.start..cursor);
                        }
                        continue;
                    }
                    cursor += 1;
                    if done {
                        self.finish_tag(&tag, cursor, &mut output);
                    } else {
                        self.state = State::Tag(tag);
                    }
                }
            }
        }
        output
    }

    fn finish_tag(&mut self, tag: &Tag, end: usize, output: &mut HtmlText) {
        if let Some(kind) = self.raw {
            if tag.closing && tag.named(kind.name()) {
                self.raw = None;
            }
            return;
        }
        if !tag.closing && (tag.named(b"script") || tag.named(b"style")) {
            self.raw = Some(if tag.named(b"script") {
                RawTag::Script
            } else {
                RawTag::Style
            });
            return;
        }
        if let Some(kind) = tag.unit_kind() {
            if tag.closing {
                if let Some(index) = self.open_units.iter().rposition(|unit| unit.kind == kind) {
                    // Crossing or implicit closures are unknown structure. Do
                    // not turn a surviving prefix into a supposedly full unit.
                    if index + 1 == self.open_units.len() {
                        let mut unit = self.open_units.pop().expect("matching open unit");
                        if end.saturating_sub(unit.range.start) <= MAX_PAIR_BYTES
                            && output.units.len() < MAX_RETURNED_UNITS
                        {
                            unit.range.end = end;
                            unit.scope_complete &= !self.has_open_heading();
                            output.units.push(unit);
                        } else {
                            output.units_truncated = true;
                        }
                    } else {
                        output.units_truncated = true;
                        self.open_units.truncate(index);
                    }
                }
            } else {
                for ancestor in &mut self.open_units {
                    if matches!(ancestor.kind, HtmlUnitKind::Heading(_)) {
                        ancestor.scope_complete = false;
                    }
                }
                // HTML permits optional p closures, but we require explicit
                // original evidence. An incomplete prior unit is discarded.
                if let Some(index) = self.open_units.iter().rposition(|unit| unit.kind == kind) {
                    output.units_truncated = true;
                    self.open_units.truncate(index);
                }
                if self.open_units.len() < MAX_OPEN_UNITS {
                    self.open_units.push(HtmlUnit {
                        kind,
                        range: tag.start..end,
                        scope_complete: true,
                    });
                }
            }
        }
        if tag.named(b"dt") {
            if !tag.closing {
                self.definition = Some(Definition {
                    start: tag.start,
                    label: false,
                    value: false,
                    phase: Phase::Label,
                });
            } else if let Some(pair) = &mut self.definition {
                pair.phase = Phase::Between;
            }
        } else if tag.named(b"dd") {
            if tag.closing {
                if let Some(pair) = self.definition.take()
                    && pair.label
                    && pair.value
                    && pair.phase == Phase::Value
                    && end.saturating_sub(pair.start) <= MAX_PAIR_BYTES
                {
                    output.definition_pairs.push(pair.start..end);
                }
            } else if let Some(pair) = &mut self.definition
                && pair.label
                && pair.phase == Phase::Between
            {
                pair.phase = Phase::Value;
            }
        } else if tag.named(b"dl") {
            self.definition = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readable(raw: &str) -> (String, HtmlText) {
        let output = HtmlScanner::default().scan(raw, 0..raw.len());
        let text = output
            .spans
            .iter()
            .map(|span| &raw[span.clone()])
            .collect::<Vec<_>>()
            .join("|");
        (text, output)
    }

    #[test]
    fn unicode_runs_are_exact_and_attributes_do_not_supply_text() {
        let raw =
            "前<div title='fake > hidden' data-x=\"also > hidden\">安全<b>éclair</b>後</div>末";
        let (text, output) = readable(raw);
        assert_eq!(text, "前|安全|éclair|後|末");
        for span in output.spans {
            assert!(raw.is_char_boundary(span.start) && raw.is_char_boundary(span.end));
            assert!(!span.is_empty());
        }
    }

    #[test]
    fn comments_and_raw_regions_never_supply_visible_text() {
        let raw = "<p>before</p><!-- fake <dt>label</dt>\n--><ScRiPt>if (a < b) x = '<dd>value</dd>'; </SCRIPT><STYLE>.x { color: red; }</style><p>after</p>";
        assert_eq!(readable(raw).0, "before|after");
        assert!(readable(raw).1.definition_pairs.is_empty());
    }

    #[test]
    fn split_quotes_comments_and_script_end_tags_keep_state() {
        let raw = "<p title='a > b'>good</p><!-- bad -->safe<script>bad</script>done";
        let mut scanner = HtmlScanner::default();
        let mut spans = Vec::new();
        for cursor in 0..raw.len() {
            spans.extend(scanner.scan(raw, cursor..cursor + 1).spans);
        }
        assert_eq!(
            spans
                .iter()
                .map(|span| &raw[span.clone()])
                .collect::<String>(),
            "goodsafedone"
        );
        assert!(!scanner.suppressing_text());
    }

    #[test]
    fn definition_pair_contains_readable_label_and_value_across_events() {
        let raw = "<dl><dt>Profile <b>release</b></dt><dd>Uses 安全 settings.</dd><dt>empty</dt><dd><a id=x></a></dd></dl>";
        let split = raw.find("<dd>").unwrap();
        let mut scanner = HtmlScanner::default();
        assert!(scanner.scan(raw, 0..split).definition_pairs.is_empty());
        let result = scanner.scan(raw, split..raw.len());
        assert_eq!(result.definition_pairs.len(), 1);
        assert_eq!(
            &raw[result.definition_pairs[0].clone()],
            "<dt>Profile <b>release</b></dt><dd>Uses 安全 settings.</dd>"
        );
        assert!(
            readable("<dt> </dt><dd>value</dd>")
                .1
                .definition_pairs
                .is_empty()
        );
        assert!(
            readable("<dd>orphan value</dd>")
                .1
                .definition_pairs
                .is_empty()
        );
    }

    #[test]
    fn tags_entities_and_clipped_fragments_are_conservative() {
        assert_eq!(readable("<a id=foo></a><!-- text --><hr>").0, "");
        assert_eq!(readable("<p>A &amp; B &#233;</p>").0, "A &amp; B &#233;");
        assert_eq!(readable("one < two").0, "one |<| two");
        for raw in ["<!-- hidden", "<script>hidden", "<style>hidden"] {
            let mut scanner = HtmlScanner::default();
            assert!(scanner.scan(raw, 0..raw.len()).spans.is_empty());
            assert!(scanner.suppressing_text());
        }
        let raw = "<p title='hidden >";
        let mut scanner = HtmlScanner::default();
        assert!(scanner.scan(raw, 0..raw.len()).spans.is_empty());
        assert!(
            !scanner.suppressing_text(),
            "unfinished markup cannot hide separate Markdown text"
        );
        let mut scanner = HtmlScanner::default();
        assert!(scanner.scan("安全", 1..3).spans.is_empty());
        assert!(!scanner.suppressing_text());
    }

    #[test]
    fn only_supplied_original_html_ranges_are_scanned() {
        let raw =
            "Markdown 前\n<div>readable 安全</div>\nUnrelated paragraph\n<em>éclair</em> tail";
        let first = raw.find("<div>").unwrap();
        let first_end = raw.find("</div>").unwrap() + "</div>".len();
        let second = raw.find("<em>").unwrap();
        let second_end = raw.find("</em>").unwrap() + "</em>".len();
        let mut scanner = HtmlScanner::default();
        let mut spans = scanner.scan(raw, first..first_end).spans;
        spans.extend(scanner.scan(raw, second..second_end).spans);
        assert_eq!(
            spans
                .iter()
                .map(|span| &raw[span.clone()])
                .collect::<Vec<_>>(),
            ["readable 安全", "éclair"]
        );
        assert!(!scanner.suppressing_text());
    }

    #[test]
    fn definition_pending_state_is_bounded_without_hiding_text() {
        let raw = format!("<dt>label</dt><dd>{}</dd>", "v".repeat(MAX_PAIR_BYTES));
        let (text, output) = readable(&raw);
        assert!(text.contains("label"));
        assert!(text.len() > MAX_PAIR_BYTES);
        assert!(output.definition_pairs.is_empty());
    }

    #[test]
    fn complete_html_units_preserve_original_unicode_ranges_across_chunks() {
        let raw = "<h2 title='a > b'>安全</h2><p>Use the <b>violet</b> profile.</p><pre><code>tool --release</code></pre><p>Only after approval.</p>";
        let split = raw.find("b'>").unwrap() + 1;
        let mut scanner = HtmlScanner::default();
        assert!(scanner.scan(raw, 0..split).units.is_empty());
        let output = scanner.scan(raw, split..raw.len());
        assert_eq!(output.units.len(), 4);
        assert_eq!(output.units[0].kind, HtmlUnitKind::Heading(2));
        assert_eq!(output.units[2].kind, HtmlUnitKind::Preformatted);
        assert_eq!(
            output
                .units
                .iter()
                .map(|unit| &raw[unit.range.clone()])
                .collect::<Vec<_>>(),
            [
                "<h2 title='a > b'>安全</h2>",
                "<p>Use the <b>violet</b> profile.</p>",
                "<pre><code>tool --release</code></pre>",
                "<p>Only after approval.</p>"
            ]
        );
    }

    #[test]
    fn incomplete_html_scopes_and_hidden_markup_do_not_become_complete_units() {
        for raw in [
            "<p>Unclosed",
            "<h2>scope<p>crossing</h2></p>",
            "<!-- <h2>false scope</h2> -->",
            "<script><p>false unit</p></script>",
        ] {
            assert!(
                HtmlScanner::default()
                    .scan(raw, 0..raw.len())
                    .units
                    .is_empty(),
                "{raw}"
            );
        }
        let raw = "<p>implicitly closed<p>explicitly closed</p>";
        let output = HtmlScanner::default().scan(raw, 0..raw.len());
        assert_eq!(output.units.len(), 1);
        assert_eq!(
            &raw[output.units[0].range.clone()],
            "<p>explicitly closed</p>"
        );
        let raw = format!("<pre>{}</pre>", "v".repeat(MAX_PAIR_BYTES));
        assert!(
            HtmlScanner::default()
                .scan(&raw, 0..raw.len())
                .units
                .is_empty()
        );
    }
}
