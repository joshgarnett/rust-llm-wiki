//! One readable-text stream over original Markdown/HTML byte ranges.
//! HTML support is deliberately lexical: no rendering, decoding, or repair.
//! HTML-event runs keep entity spellings; parser Text/Code keeps Markdown transforms.
use pulldown_cmark::{CowStr, Event, Parser, Tag, TagEnd};
use std::ops::Range;

pub(crate) enum TextEvent<'a> {
    Text {
        value: CowStr<'a>,
        original: Range<usize>,
        exact: bool,
    },
    Break,
    HeadingStart,
    HeadingEnd,
}

pub(crate) fn visit<'a>(raw: &'a str, mut emit: impl FnMut(TextEvent<'a>)) {
    let mut html = HtmlScanner::default();
    for (event, range) in Parser::new(raw).into_offset_iter() {
        match event {
            Event::Html(_) | Event::InlineHtml(_) => {
                emit(TextEvent::Break);
                html.scan(raw, range, &mut emit);
                emit(TextEvent::Break);
            }
            Event::Text(value) | Event::Code(value) if !html.suppressing() => {
                // Keep the accepted SourceMap mapping for Markdown transforms.
                let original = if raw.get(range.clone()) == Some(value.as_ref()) {
                    range
                } else if let Some(relative) = raw
                    .get(range.clone())
                    .and_then(|slice| slice.find(value.as_ref()))
                {
                    range.start + relative..range.start + relative + value.len()
                } else {
                    range
                };
                let exact = raw.get(original.clone()) == Some(value.as_ref());
                emit(TextEvent::Text {
                    value,
                    original,
                    exact,
                });
            }
            Event::Start(Tag::Heading { .. }) if !html.suppressing() => {
                emit(TextEvent::HeadingStart);
            }
            Event::End(TagEnd::Heading(_)) => {
                // Always close a Markdown heading opened before suppression.
                emit(TextEvent::HeadingEnd);
                emit(TextEvent::Break);
            }
            Event::SoftBreak
            | Event::HardBreak
            | Event::End(TagEnd::Paragraph | TagEnd::CodeBlock | TagEnd::Item) => {
                emit(TextEvent::Break);
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum RawKind {
    Script,
    Style,
}

#[derive(Default)]
enum State {
    #[default]
    Data,
    Tag(HtmlTag),
    Comment(u8),
    Raw(RawKind),
}

#[derive(Default)]
struct HtmlScanner {
    state: State,
}

#[derive(Default)]
enum Phase {
    #[default]
    Start,
    Bang(u8),
    Name,
    Attributes,
}

#[derive(Default)]
struct HtmlTag {
    phase: Phase,
    name: [u8; 8],
    name_len: usize,
    closing: bool,
    quote: Option<u8>,
    resume_raw: Option<RawKind>,
}

enum TagStep {
    More,
    Comment,
    End,
}

impl HtmlTag {
    fn push(&mut self, byte: u8) -> TagStep {
        match self.phase {
            Phase::Start => match byte {
                b'/' => {
                    self.closing = true;
                    self.phase = Phase::Name;
                    return TagStep::More;
                }
                b'!' => {
                    self.phase = Phase::Bang(0);
                    return TagStep::More;
                }
                b'?' => self.phase = Phase::Attributes,
                _ => self.phase = Phase::Name,
            },
            Phase::Bang(n) if byte == b'-' => {
                if n == 1 {
                    return TagStep::Comment;
                }
                self.phase = Phase::Bang(1);
                return TagStep::More;
            }
            Phase::Bang(_) => self.phase = Phase::Attributes,
            _ => {}
        }
        if matches!(self.phase, Phase::Name) {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':') {
                if self.name_len < self.name.len() {
                    self.name[self.name_len] = byte.to_ascii_lowercase();
                }
                // Saturate at one beyond capacity: long names cannot be script/style.
                self.name_len = (self.name_len + 1).min(self.name.len() + 1);
                return TagStep::More;
            }
            self.phase = Phase::Attributes;
        }
        if let Some(quote) = self.quote {
            if byte == quote {
                self.quote = None;
            }
        } else if matches!(byte, b'\'' | b'"') {
            self.quote = Some(byte);
        } else if byte == b'>' {
            return TagStep::End;
        }
        TagStep::More
    }

    fn raw_kind(&self) -> Option<RawKind> {
        match self.name.get(..self.name_len) {
            Some(b"script") => Some(RawKind::Script),
            Some(b"style") => Some(RawKind::Style),
            _ => None,
        }
    }

    fn next_state(&self) -> State {
        match self.resume_raw {
            Some(kind) if self.closing && self.raw_kind() == Some(kind) => State::Data,
            Some(kind) => State::Raw(kind),
            None if !self.closing => self.raw_kind().map_or(State::Data, State::Raw),
            None => State::Data,
        }
    }
}

impl HtmlScanner {
    fn suppressing(&self) -> bool {
        !matches!(self.state, State::Data)
    }

    fn scan<'a>(
        &mut self,
        raw: &'a str,
        range: Range<usize>,
        emit: &mut impl FnMut(TextEvent<'a>),
    ) {
        let bytes = raw.as_bytes();
        let mut at = range.start;
        while at < range.end {
            match &mut self.state {
                State::Data => {
                    let start = at;
                    while at < range.end {
                        let next = bytes.get(at + 1).filter(|_| at + 1 < range.end);
                        if bytes[at] == b'<'
                            && next.is_none_or(|&b| {
                                b.is_ascii_alphabetic() || matches!(b, b'/' | b'!' | b'?')
                            })
                        {
                            break;
                        }
                        at += 1;
                    }
                    if start < at {
                        emit(TextEvent::Text {
                            value: CowStr::Borrowed(&raw[start..at]),
                            original: start..at,
                            exact: true,
                        });
                    }
                    if at < range.end {
                        self.state = State::Tag(HtmlTag::default());
                        emit(TextEvent::Break);
                        at += 1;
                    }
                }
                State::Tag(tag) => {
                    match tag.push(bytes[at]) {
                        TagStep::More => {}
                        TagStep::Comment => self.state = State::Comment(0),
                        TagStep::End => {
                            self.state = tag.next_state();
                            emit(TextEvent::Break);
                        }
                    }
                    at += 1;
                }
                State::Comment(matched) => {
                    if *matched == 2 && bytes[at] == b'>' {
                        self.state = State::Data;
                        emit(TextEvent::Break);
                    } else {
                        *matched = if bytes[at] == b'-' {
                            (*matched + 1).min(2)
                        } else {
                            0
                        };
                    }
                    at += 1;
                }
                State::Raw(kind) => {
                    // Raw script/style bodies have no nested HTML structure.
                    // Only a possible closing tag can end their suppression.
                    if bytes[at] == b'<' && (at + 1 == range.end || bytes[at + 1] == b'/') {
                        self.state = State::Tag(HtmlTag {
                            resume_raw: Some(*kind),
                            ..HtmlTag::default()
                        });
                    }
                    at += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projected(raw: &str) -> (String, Vec<(Range<usize>, String, bool)>, usize) {
        let mut text = String::new();
        let mut spans = Vec::new();
        let mut headings = 0;
        visit(raw, |event| match event {
            TextEvent::Text {
                value,
                original,
                exact,
            } => {
                text.push_str(&value);
                spans.push((original, value.into_string(), exact));
            }
            TextEvent::Break => text.push('\n'),
            TextEvent::HeadingStart => headings += 1,
            TextEvent::HeadingEnd => {}
        });
        (text, spans, headings)
    }

    #[test]
    fn markdown_transforms_code_and_heading_behavior_are_preserved() {
        let raw = "## Café\n\n`a<b>` \\<script> &#233;\n";
        let (text, spans, headings) = projected(raw);
        assert_eq!(text, "Café\na<b> <script> é\n");
        assert_eq!(headings, 1);
        assert!(
            spans.iter().any(|(span, value, exact)| value == "a<b>"
                && *exact
                && &raw[span.clone()] == value)
        );
        assert!(spans.iter().any(|(span, value, exact)| value.contains("é")
            && !exact
            && raw[span.clone()].contains("&#233;")));
    }

    #[test]
    fn balanced_and_inline_html_emit_original_unicode_runs() {
        let raw = "<div><h2>Guide</h2><p>café λ🙂</p></div>\n\nA <b>naïve</b> result.\n";
        let (text, spans, headings) = projected(raw);
        assert!(text.contains("café λ🙂"));
        assert!(text.contains("naïve"));
        assert_eq!(headings, 0); // HTML headings are body text, not new Markdown ancestry.
        for (span, value, exact) in spans {
            assert!(exact);
            assert!(raw.is_char_boundary(span.start) && raw.is_char_boundary(span.end));
            assert_eq!(&raw[span], value);
        }
    }

    #[test]
    fn quoted_greater_than_does_not_expose_attribute_text() {
        let raw = "<div data-note=\"greater > attributeonly\"><p>readable</p></div>\n";
        let (text, _, _) = projected(raw);
        assert!(text.contains("readable"));
        assert!(!text.contains("attributeonly"));
        assert!(!text.contains("greater"));
    }

    #[test]
    fn markup_comments_script_and_style_are_not_readable_evidence() {
        let raw = "<div>shown<!--commentonly--><script>scriptOnly</script><STYLE>styleOnly</STYLE><img alt='attributeOnly'>kept</div>\n";
        let (text, _, _) = projected(raw);
        assert!(text.contains("shown") && text.contains("kept"));
        for hidden in ["commentonly", "scriptOnly", "styleOnly", "attributeOnly"] {
            assert!(!text.contains(hidden), "{hidden}");
        }
    }

    #[test]
    fn suppression_survives_fragments_and_intervening_markdown_events() {
        let raw = "before <script>secret\n\n## secretHeading\n\n`secretCode`\n\n</script> after\n\n<!-- secretComment\n\nsecretTail -->\n\nvisible\n";
        let (text, _, _) = projected(raw);
        assert!(text.contains("before") && text.contains("after") && text.contains("visible"));
        assert!(!text.contains("secret"));
        // A delimiter split between admitted fragments must not leak its tail.
        let raw = "<!-- hidden --><p>visible</p>";
        let mut scanner = HtmlScanner::default();
        let mut text = String::new();
        let mut emit = |event| {
            if let TextEvent::Text { value, .. } = event {
                text.push_str(&value)
            }
        };
        scanner.scan(raw, 0..3, &mut emit);
        scanner.scan(raw, 3..14, &mut emit);
        scanner.scan(raw, 14..raw.len(), &mut emit);
        assert_eq!(text, "visible");
    }

    #[test]
    fn unclosed_admitted_regions_omit_without_guessing() {
        for raw in [
            "<div title=\"unfinished >\nattributeTail\n",
            "<div>readable<script>scriptTail",
            "<div>readable<!--commentTail",
        ] {
            let (text, _, _) = projected(raw);
            assert!(!text.contains("Tail"));
        }
    }

    #[test]
    fn removed_boundaries_do_not_join_words_and_entities_stay_raw() {
        let raw = "<p>ab<b>cd</b>ef &amp; &#233; &lt;</p>\n";
        let (text, spans, _) = projected(raw);
        assert!(!text.contains("abcdef") && !text.contains("abcd"));
        assert!(text.contains("&amp; &#233; &lt;"));
        assert!(
            spans
                .iter()
                .all(|(span, value, exact)| *exact && &raw[span.clone()] == value)
        );
        // Inline tag neighbors do not reclassify parser Text as raw HTML.
        let inline = "A <span>&amp;</span> &#233; B";
        let (text, spans, _) = projected(inline);
        assert_eq!(
            text.split_whitespace().collect::<Vec<_>>().join(" "),
            "A & é B"
        );
        assert!(spans.iter().any(|(span, value, exact)| {
            value.contains('é') && !exact && inline[span.clone()].contains("&#233;")
        }));
    }
}
