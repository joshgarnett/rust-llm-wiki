//! Original-byte excerpt mapping; FTS snippets are never citation spans.
use crate::domain::{ErrorCode, Result, WikiError};
use pulldown_cmark::{Event, Parser, TagEnd};
use rusqlite::{Connection, ffi};
use std::{ffi::c_void, ops::Range, ptr};

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub text: String,
    pub span: Range<usize>,
}

/// SQLite owns API pointers for the borrowed connection lifetime. Tokenizer
/// instances are independent and destroyed before that connection can close.
pub(crate) struct Tokenizer<'a> {
    instance: *mut ffi::Fts5Tokenizer,
    api: ffi::fts5_tokenizer,
    _connection: &'a Connection,
}
impl<'a> Tokenizer<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Result<Self> {
        let mut statement = ptr::null_mut();
        let mut api: *mut ffi::fts5_api = ptr::null_mut();
        // SAFETY: fixed SQL and C strings are NUL terminated; all out-pointers
        // address live local storage. The statement is finalized on every path.
        let code = unsafe {
            ffi::sqlite3_prepare_v2(
                connection.handle(),
                c"SELECT fts5(?1)".as_ptr(),
                -1,
                &mut statement,
                ptr::null_mut(),
            )
        };
        if code != ffi::SQLITE_OK {
            if !statement.is_null() {
                unsafe {
                    ffi::sqlite3_finalize(statement);
                }
            }
            return Err(tokenizer_error(code));
        }
        let result = (|| -> Result<()> {
            // SAFETY: SQLite's documented fts5_api_ptr exchange writes one API
            // pointer into api during this synchronous step; no destructor needed.
            let code = unsafe {
                ffi::sqlite3_bind_pointer(
                    statement,
                    1,
                    (&mut api as *mut *mut ffi::fts5_api).cast(),
                    c"fts5_api_ptr".as_ptr(),
                    None,
                )
            };
            if code != ffi::SQLITE_OK {
                return Err(tokenizer_error(code));
            }
            let code = unsafe { ffi::sqlite3_step(statement) };
            if code != ffi::SQLITE_ROW && code != ffi::SQLITE_DONE {
                return Err(tokenizer_error(code));
            }
            if api.is_null() {
                return Err(WikiError::new(
                    ErrorCode::CapabilityUnavailable,
                    "FTS5 tokenizer API unavailable",
                ));
            }
            if unsafe { (*api).iVersion } < 2 {
                return Err(WikiError::new(
                    ErrorCode::CapabilityUnavailable,
                    "FTS5 tokenizer API version unsupported",
                ));
            }
            Ok(())
        })();
        let finalized = unsafe { ffi::sqlite3_finalize(statement) };
        result?;
        if finalized != ffi::SQLITE_OK {
            return Err(tokenizer_error(finalized));
        }
        let mut context = ptr::null_mut();
        let mut tokenizer = ffi::fts5_tokenizer {
            xCreate: None,
            xDelete: None,
            xTokenize: None,
        };
        // SAFETY: api came from this still-live SQLite connection. The registered
        // unicode61 tokenizer and function pointers are supplied by bundled SQLite.
        let find = unsafe { (*api).xFindTokenizer }.ok_or_else(|| {
            WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "FTS5 tokenizer lookup unavailable",
            )
        })?;
        let code = unsafe { find(api, c"unicode61".as_ptr(), &mut context, &mut tokenizer) };
        if code != ffi::SQLITE_OK {
            return Err(tokenizer_error(code));
        }
        let create = tokenizer.xCreate.ok_or_else(|| {
            WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "FTS5 tokenizer creation unavailable",
            )
        })?;
        if tokenizer.xDelete.is_none() || tokenizer.xTokenize.is_none() {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "FTS5 tokenizer methods unavailable",
            ));
        }
        let mut args = [c"remove_diacritics".as_ptr(), c"2".as_ptr()];
        let mut instance = ptr::null_mut();
        let code = unsafe { create(context, args.as_mut_ptr(), 2, &mut instance) };
        if code != ffi::SQLITE_OK {
            if !instance.is_null() {
                unsafe {
                    tokenizer.xDelete.expect("checked delete")(instance);
                }
            }
            return Err(tokenizer_error(code));
        }
        if instance.is_null() {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "FTS5 tokenizer instance unavailable",
            ));
        }
        Ok(Self {
            instance,
            api: tokenizer,
            _connection: connection,
        })
    }
    pub(crate) fn tokens(&self, text: &str) -> Result<Vec<Token>> {
        let length = i32::try_from(text.len())
            .map_err(|_| WikiError::invalid("tokenizer input exceeds SQLite byte range"))?;
        let mut result: Vec<Token> = Vec::new();
        // SAFETY: SQLite calls collect synchronously; result and UTF-8 input stay
        // alive for that call. The callback copies token bytes and retains no pointers.
        let code = unsafe {
            self.api.xTokenize.expect("checked tokenizer")(
                self.instance,
                (&mut result as *mut Vec<Token>).cast(),
                ffi::FTS5_TOKENIZE_DOCUMENT,
                text.as_ptr().cast(),
                length,
                Some(collect),
            )
        };
        if code != ffi::SQLITE_OK {
            return Err(tokenizer_error(code));
        }
        if result.iter().any(|token| {
            token.span.end > text.len()
                || !text.is_char_boundary(token.span.start)
                || !text.is_char_boundary(token.span.end)
        }) {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "tokenizer returned invalid source boundaries",
            ));
        }
        Ok(result)
    }
}
impl Drop for Tokenizer<'_> {
    fn drop(&mut self) {
        unsafe {
            self.api.xDelete.expect("checked tokenizer")(self.instance);
        }
    }
}
unsafe extern "C" fn collect(
    context: *mut c_void,
    _flags: i32,
    text: *const std::ffi::c_char,
    length: i32,
    start: i32,
    end: i32,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if context.is_null() || text.is_null() || length < 0 || start < 0 || end < start {
            return ffi::SQLITE_ERROR;
        }
        // SAFETY: these are tokenizer-owned token bytes valid during the callback,
        // and context points to the exclusive Vec passed to this synchronous call.
        let bytes = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), length as usize) };
        let Ok(value) = std::str::from_utf8(bytes) else {
            return ffi::SQLITE_ERROR;
        };
        unsafe { &mut *context.cast::<Vec<Token>>() }.push(Token {
            text: value.into(),
            span: start as usize..end as usize,
        });
        ffi::SQLITE_OK
    }))
    .unwrap_or(ffi::SQLITE_ERROR)
}
fn tokenizer_error(code: i32) -> WikiError {
    WikiError::new(
        ErrorCode::CapabilityUnavailable,
        format!("bundled FTS5 tokenizer failed ({code})"),
    )
}

#[derive(Debug)]
struct Segment {
    normalized: Range<usize>,
    original: Range<usize>,
    exact: bool,
}
pub(crate) struct SourceMap {
    pub text: String,
    segments: Vec<Segment>,
}
impl SourceMap {
    pub(crate) fn markdown(raw: &str, body_offset: usize) -> Self {
        let mut map = Self {
            text: String::new(),
            segments: vec![],
        };
        for (event, range) in Parser::new(&raw[body_offset..]).into_offset_iter() {
            match event {
                Event::Text(value) | Event::Code(value) => {
                    let original = range.start + body_offset..range.end + body_offset;
                    let exact = raw.get(original.clone()) == Some(value.as_ref());
                    let original = if exact {
                        original
                    } else if let Some(relative) = raw
                        .get(original.clone())
                        .and_then(|slice| slice.find(value.as_ref()))
                    {
                        original.start + relative..original.start + relative + value.len()
                    } else {
                        original
                    };
                    let exact = raw.get(original.clone()) == Some(value.as_ref());
                    let start = map.text.len();
                    map.text.push_str(&value);
                    map.segments.push(Segment {
                        normalized: start..map.text.len(),
                        original,
                        exact,
                    });
                }
                Event::SoftBreak
                | Event::HardBreak
                | Event::End(
                    TagEnd::Heading(_) | TagEnd::Paragraph | TagEnd::CodeBlock | TagEnd::Item,
                ) => map.text.push('\n'),
                _ => {}
            }
        }
        map
    }
    pub(crate) fn original_span(&self, span: Range<usize>) -> Option<Range<usize>> {
        // Segments are emitted in normalized byte order. Query-aware windows
        // map every matching token, so avoid rescanning all Markdown segments
        // for each one. Preserve the original overlap predicates exactly.
        let first = self.segments.get(
            self.segments
                .partition_point(|segment| segment.normalized.end <= span.start),
        )?;
        let last = self.segments.get(
            self.segments
                .partition_point(|segment| segment.normalized.start < span.end)
                .checked_sub(1)?,
        )?;
        if first.normalized.start >= span.end || last.normalized.end <= span.start {
            return None;
        }
        let start = if first.exact {
            first.original.start + span.start.saturating_sub(first.normalized.start)
        } else {
            first.original.start
        };
        let end = if last.exact {
            last.original.start + (span.end.min(last.normalized.end) - last.normalized.start)
        } else {
            last.original.end
        };
        Some(start..end)
    }
}
