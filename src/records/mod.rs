//! Byte-preserving Markdown records and explicit reference resolution.
pub mod edit;
pub(crate) mod link_rewrite;
pub mod links;
pub mod parse;

pub use edit::edit_note;
pub use links::{
    LinkResolution, LinkSyntax, MarkdownLink, RegistryEntry, extract_links, resolve_typed,
    resolve_untyped,
};
pub use parse::{
    ParseLimits, ParseStatus, ParsedNote, parse_note, parse_note_with_limits, parser_fingerprint,
};
