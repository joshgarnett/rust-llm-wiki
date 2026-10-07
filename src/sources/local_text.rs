//! Filename admission only; UTF-8 validity and extraction policy belong to callers.
use std::path::Path;

/// MIME metadata does not override this local filename policy.
pub(crate) fn local_text_candidate(path: &Path) -> bool {
    path.extension().is_none()
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "txt"
                        | "md"
                        | "markdown"
                        | "csv"
                        | "tsv"
                        | "json"
                        | "yaml"
                        | "yml"
                        | "rs"
                        | "toml"
                        | "log"
                        | "html"
                        | "htm"
                )
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_filename_admission_preserves_legacy_and_adds_html() {
        let cases = [
            ("manual.html", true),
            ("manual.HTM", true),
            ("manual.HtMl", true),
            ("manual.htm", true),
            ("note.txt", true),
            ("note.MD", true),
            ("note.markdown", true),
            ("data.csv", true),
            ("data.tsv", true),
            ("data.json", true),
            ("data.yaml", true),
            ("data.yml", true),
            ("module.rs", true),
            ("config.toml", true),
            ("events.log", true),
            ("README", true),
            (".hidden", true),
            ("parent.html/manual", true),
            ("manual.html.bin", false),
            ("manual.xhtml", false),
            ("manual.pdf", false),
            ("manual.", false),
        ];
        for (name, expected) in cases {
            assert_eq!(local_text_candidate(Path::new(name)), expected, "{name}");
        }
    }
}
