//! Synchronous DEVELOPMENT discriminator; never selectable in production.
use super::decision_types::ENTITY_DECISION_FENCE;
use crate::{domain::RecordKind, records::ParsedNote};
use pulldown_cmark::{CodeBlockKind, Event, Tag};
use std::cell::Cell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemapInputProbeMode {
    Current,
    Precise,
}
thread_local! {
    static MODE: Cell<RemapInputProbeMode> = const { Cell::new(RemapInputProbeMode::Current) };
}
pub(crate) fn mode() -> RemapInputProbeMode {
    MODE.with(Cell::get)
}
pub(crate) fn with_mode<T>(selected: RemapInputProbeMode, f: impl FnOnce() -> T) -> T {
    struct Restore(RemapInputProbeMode);
    impl Drop for Restore {
        fn drop(&mut self) {
            MODE.with(|mode| mode.set(self.0));
        }
    }
    let _restore = Restore(MODE.with(|mode| mode.replace(selected)));
    f()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemapInputWitness {
    Excluded,
    Fence,
    LegacySyntaxError,
}
pub(crate) fn classify(note: &ParsedNote) -> RemapInputWitness {
    let legacy = note.canonical.as_ref().is_some_and(|record| {
        record.kind() == RecordKind::Decision
            && matches!(
                record.string("wiki_action"),
                Some("merge" | "split" | "add_alias" | "bind_mention")
            )
    });
    let text = match std::str::from_utf8(note.body()) {
        Ok(text) => std::borrow::Cow::Borrowed(text),
        Err(_) if legacy => return RemapInputWitness::LegacySyntaxError,
        // Classification retains a raw fence; receipt decoding stays strict.
        Err(_) => String::from_utf8_lossy(note.body()),
    };
    if pulldown_cmark::Parser::new(&text).any(|event| {
        matches!(event, Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
            if info.as_ref() == ENTITY_DECISION_FENCE)
    }) {
        RemapInputWitness::Fence
    } else {
        RemapInputWitness::Excluded
    }
}

#[path = "remap_input_probe_tests.rs"]
mod tests;
