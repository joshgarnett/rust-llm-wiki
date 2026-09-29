//! Deterministic, bounded extraction. Original bytes are never rewritten.
use crate::{domain::*, providers::public_fetch::FetchCapture};
use serde::{Deserialize, Serialize};
use std::io::Read;

pub const NORMALIZER: &str = "web-text-v1";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebGap {
    UnsupportedMedia,
    UnsupportedEncoding,
    UnsupportedCharset,
    InvalidText,
    DecompressionLimit,
    AuthWall,
    Robots,
    Unavailable,
    RedirectLimit,
    RedirectRejected,
}
pub struct NormalizedWeb {
    pub content: Option<Vec<u8>>,
    pub gap: Option<WebGap>,
    pub fingerprint: Blake3Hash,
}
fn unsupported(gap: WebGap) -> NormalizedWeb {
    NormalizedWeb {
        content: None,
        gap: Some(gap),
        fingerprint: Blake3Hash::digest(NORMALIZER),
    }
}
pub fn normalize(capture: &FetchCapture) -> NormalizedWeb {
    if capture.original_hash != Blake3Hash::digest(&capture.original) {
        return unsupported(WebGap::InvalidText);
    }
    if matches!(capture.status, 401 | 407) || capture.header("www-authenticate").is_some() {
        return unsupported(WebGap::AuthWall);
    }
    if capture.status == 403 {
        return unsupported(WebGap::AuthWall);
    }
    if !(200..300).contains(&capture.status) {
        return unsupported(WebGap::Unavailable);
    }
    if capture.header("x-robots-tag").is_some_and(robots_denied) {
        return unsupported(WebGap::Robots);
    }
    let content_type = capture
        .header("content-type")
        .unwrap_or("application/octet-stream");
    let mut parts = content_type.split(';');
    let media = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    if !matches!(
        media.as_str(),
        "text/html" | "application/xhtml+xml" | "text/plain" | "text/markdown"
    ) {
        return unsupported(WebGap::UnsupportedMedia);
    }
    let mut ascii = false;
    for param in parts {
        if let Some((key, value)) = param.trim().split_once('=')
            && key.trim().eq_ignore_ascii_case("charset")
        {
            let charset = value.trim().trim_matches(['\'', '"']);
            if charset.eq_ignore_ascii_case("us-ascii") {
                ascii = true;
            } else if !charset.eq_ignore_ascii_case("utf-8")
                && !charset.eq_ignore_ascii_case("utf8")
            {
                return unsupported(WebGap::UnsupportedCharset);
            }
        }
    }
    let expanded = match expand(
        &capture.original,
        capture.header("content-encoding").unwrap_or("identity"),
        capture.limits.expanded_bytes,
    ) {
        Ok(bytes) => bytes,
        Err(gap) => return unsupported(gap),
    };
    if ascii && !expanded.is_ascii() {
        return unsupported(WebGap::InvalidText);
    }
    let text = match std::str::from_utf8(&expanded) {
        Ok(text) => text,
        Err(_) => return unsupported(WebGap::InvalidText),
    };
    let content = if matches!(media.as_str(), "text/html" | "application/xhtml+xml") {
        match html_text(text, capture.limits.expanded_bytes as usize) {
            Ok(text) => text.into_bytes(),
            Err(gap) => return unsupported(gap),
        }
    } else {
        expanded
    };
    NormalizedWeb {
        content: Some(content),
        gap: None,
        fingerprint: Blake3Hash::digest(NORMALIZER),
    }
}
fn robots_denied(value: &str) -> bool {
    value
        .to_ascii_lowercase()
        .split([',', ';', ' '])
        .any(|p| matches!(p, "noindex" | "none" | "nosnippet" | "noai" | "noimageai"))
}
fn expand(raw: &[u8], encoding: &str, cap: u64) -> std::result::Result<Vec<u8>, WebGap> {
    if cap == 0 || cap > 8 * 1024 * 1024 {
        return Err(WebGap::DecompressionLimit);
    }
    let encoding = encoding.trim().to_ascii_lowercase();
    if encoding.is_empty() || encoding == "identity" {
        return if raw.len() as u64 <= cap {
            Ok(raw.to_vec())
        } else {
            Err(WebGap::DecompressionLimit)
        };
    }
    let reader: Box<dyn Read + '_> = match encoding.as_str() {
        "gzip" => Box::new(flate2::read::MultiGzDecoder::new(raw)),
        "deflate" => Box::new(flate2::read::ZlibDecoder::new(raw)),
        _ => return Err(WebGap::UnsupportedEncoding),
    };
    let mut bytes = Vec::new();
    reader
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| WebGap::UnsupportedEncoding)?;
    if bytes.len() as u64 > cap {
        return Err(WebGap::DecompressionLimit);
    }
    Ok(bytes)
}
/// Conservative scanner: quoted attributes, comments and opaque executable blocks
/// are skipped. Malformed/incomplete markup becomes an unsupported capture.
fn html_text(input: &str, cap: usize) -> std::result::Result<String, WebGap> {
    let mut output = String::new();
    let mut cursor = 0usize;
    let mut opaque: Option<String> = None;
    let mut opaque_depth = 0usize;
    while cursor < input.len() {
        if input.as_bytes()[cursor] == b'<' {
            if input[cursor..].starts_with("<!--") {
                let end = input[cursor + 4..].find("-->").ok_or(WebGap::InvalidText)?;
                cursor += 4 + end + 3;
                continue;
            }
            let start = cursor + 1;
            let mut end = start;
            let mut quote = None;
            while end < input.len() {
                let b = input.as_bytes()[end];
                if let Some(q) = quote {
                    if b == q {
                        quote = None;
                    }
                } else if b == b'\'' || b == b'"' {
                    quote = Some(b);
                } else if b == b'>' {
                    break;
                }
                end += 1;
            }
            if end == input.len() || end - start > 65536 {
                return Err(WebGap::InvalidText);
            }
            let tag = input[start..end].trim();
            let closing = tag.starts_with('/');
            let name = tag
                .trim_start_matches('/')
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != ':')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if let Some(block) = &opaque {
                if name == *block {
                    if closing {
                        opaque_depth -= 1;
                        if opaque_depth == 0 {
                            opaque = None;
                        }
                    } else if matches!(block.as_str(), "template" | "svg" | "math") {
                        opaque_depth = opaque_depth
                            .checked_add(1)
                            .filter(|n| *n <= 128)
                            .ok_or(WebGap::InvalidText)?;
                    }
                }
            } else if !closing
                && matches!(
                    name.as_str(),
                    "script" | "style" | "template" | "noscript" | "svg" | "math"
                )
            {
                opaque = Some(name);
                opaque_depth = 1;
            } else {
                if name == "input" && tag.to_ascii_lowercase().contains("password") {
                    return Err(WebGap::AuthWall);
                }
                if name == "meta"
                    && tag.to_ascii_lowercase().contains("robots")
                    && (tag.to_ascii_lowercase().contains("noindex")
                        || tag.to_ascii_lowercase().contains("nosnippet")
                        || tag.to_ascii_lowercase().contains("noai"))
                {
                    return Err(WebGap::Robots);
                }
                if matches!(
                    name.as_str(),
                    "p" | "div"
                        | "br"
                        | "hr"
                        | "li"
                        | "tr"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "section"
                        | "article"
                        | "header"
                        | "footer"
                        | "blockquote"
                        | "pre"
                ) && !output.ends_with('\n')
                {
                    output.push('\n');
                }
            }
            cursor = end + 1;
        } else {
            let end = input[cursor..]
                .find('<')
                .map_or(input.len(), |n| cursor + n);
            if opaque.is_none() {
                append_entities(&input[cursor..end], &mut output);
            }
            cursor = end;
        }
        if output.len() > cap {
            return Err(WebGap::DecompressionLimit);
        }
    }
    if opaque.is_some() {
        return Err(WebGap::InvalidText);
    }
    let mut normalized = String::new();
    for line in output.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !line.is_empty() {
            if !normalized.is_empty() {
                normalized.push('\n');
            }
            normalized.push_str(&line);
        }
    }
    if !normalized.is_empty() {
        normalized.push('\n');
    }
    Ok(normalized)
}
fn append_entities(text: &str, output: &mut String) {
    let mut cursor = 0usize;
    while cursor < text.len() {
        if text.as_bytes()[cursor] == b'&'
            && let Some(end) = text.as_bytes()[cursor + 1..]
                .iter()
                .take(17)
                .position(|b| *b == b';')
        {
            let name = &text[cursor + 1..cursor + 1 + end];
            let decoded = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                "ndash" => Some('–'),
                "mdash" => Some('—'),
                "hellip" => Some('…'),
                "copy" => Some('©'),
                "reg" => Some('®'),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|n| u32::from_str_radix(n, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|n| n.parse().ok()))
                    .and_then(char::from_u32),
            };
            if let Some(value) = decoded {
                output.push(if value == '\0' { '�' } else { value });
                cursor += end + 2;
                continue;
            }
        }
        let ch = text[cursor..].chars().next().expect("character boundary");
        output.push(ch);
        cursor += ch.len_utf8();
    }
}
