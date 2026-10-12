//! Explicit roots always win; discovery never combines nested vaults.
use super::VaultRoot;
use crate::{config::DefaultWiki, domain::*, records::parse_note, vault::VaultFs};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionOrigin {
    Explicit,
    Ancestor,
    Default,
}

#[derive(Debug, Clone)]
pub struct SelectedWiki {
    pub root: VaultRoot,
    pub wiki_id: RecordId,
    pub origin: SelectionOrigin,
    pub requested_path: PathBuf,
}

/// Bind a selected location before any engine or application can mutate it.
pub fn identify(root: &VaultRoot) -> Result<RecordId> {
    let marker = crate::changes::prepare::read_bounded(
        &VaultFs::new(root.clone()),
        &VaultRelativePath::new("WIKI.md")?,
        crate::app::MAX_INPUT_BYTES,
    )?
    .ok_or_else(|| WikiError::new(ErrorCode::VaultNotFound, "selected wiki has no WIKI.md"))?;
    let parsed = parse_note(&marker);
    parsed
        .canonical
        .as_ref()
        .filter(|record| record.kind() == RecordKind::Vault)
        .map(|record| record.id().clone())
        .ok_or_else(|| {
            WikiError::invalid(format!(
                "selected wiki marker is invalid: {}",
                root.path().join("WIKI.md").display()
            ))
        })
}

fn selected(path: &Path, origin: SelectionOrigin) -> Result<SelectedWiki> {
    let root = VaultRoot::explicit(path)?;
    let wiki_id = identify(&root)?;
    Ok(SelectedWiki {
        root,
        wiki_id,
        origin,
        requested_path: path.to_path_buf(),
    })
}

/// Explicit and nearest-marker failures are terminal; only absence uses fallback.
pub fn select(
    explicit: Option<&Path>,
    start: &Path,
    default: Option<&DefaultWiki>,
) -> Result<SelectedWiki> {
    if let Some(path) = explicit {
        return selected(path, SelectionOrigin::Explicit);
    }
    if start.to_str().is_none() {
        return Err(WikiError::invalid(
            "non-UTF-8 discovery start is unsupported",
        ));
    }
    let mut path = fs::canonicalize(start).map_err(|error| {
        WikiError::new(
            ErrorCode::VaultNotFound,
            format!("resolve discovery start {}: {error}", start.display()),
        )
    })?;
    if !path.is_dir() {
        path.pop();
    }
    loop {
        match fs::symlink_metadata(path.join("WIKI.md")) {
            // Any nearer marker occupies the selection boundary, even if invalid.
            Ok(_) => return selected(&path, SelectionOrigin::Ancestor),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(WikiError::new(
                    ErrorCode::VaultNotFound,
                    format!(
                        "inspect nearer marker {}: {error}",
                        path.join("WIKI.md").display()
                    ),
                ));
            }
        }
        if !path.pop() {
            break;
        }
    }
    if let Some(default) = default {
        let result = selected(&default.path, SelectionOrigin::Default).map_err(|mut error| {
            error.message = format!("configured default {}: {}", default.path.display(), error.message);
            error.hint = Some("Select an existing wiki with --wiki PATH, or set a new default with wiki default set PATH".into());
            error
        })?;
        if result.wiki_id != default.wiki_id {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                format!(
                    "configured default {} expected wiki ID {}, found {}",
                    default.path.display(),
                    default.wiki_id,
                    result.wiki_id
                ),
            ));
        }
        return Ok(result);
    }
    Err(WikiError::new(
        ErrorCode::VaultNotFound,
        "no ancestor WIKI.md and no configured default wiki",
    ))
}

pub fn default_from_path(path: &Path) -> Result<DefaultWiki> {
    let selected = selected(path, SelectionOrigin::Explicit)?;
    Ok(DefaultWiki {
        path: selected.root.path().to_path_buf(),
        wiki_id: selected.wiki_id,
    })
}

pub fn expect_wiki_id(selected: &SelectedWiki, expected: Option<&RecordId>) -> Result<()> {
    if expected.is_some_and(|expected| expected != &selected.wiki_id) {
        let mut error = WikiError::new(
            ErrorCode::ContentConflict,
            format!(
                "selected wiki {} has ID {}; expected {}",
                selected.root.path().display(),
                selected.wiki_id,
                expected.expect("checked expected")
            ),
        );
        error.hint = Some("Check the intended folder with lwiki --wiki PATH wiki show, then retry using that --wiki PATH.".into());
        error.details = serde_json::json!({
            "wiki_id_mismatch": true,
            "expected_wiki_id": expected,
            "actual_wiki_id": selected.wiki_id,
        });
        return Err(error);
    }
    Ok(())
}

pub fn resolve(explicit: Option<&Path>, start: &Path, initialization: bool) -> Result<VaultRoot> {
    match explicit {
        Some(path) if initialization => VaultRoot::for_initialization(path),
        Some(path) => VaultRoot::explicit(path),
        None if initialization => VaultRoot::for_initialization(start),
        None => VaultRoot::discover(start),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        (temp, base)
    }
    fn wiki(base: &Path, name: &str, id: &str) -> DefaultWiki {
        let path = base.join(name);
        fs::create_dir(&path).unwrap();
        fs::write(
            path.join("WIKI.md"),
            format!(
                "---\nwiki_schema: \"1\"\nwiki_id: {id}\nwiki_kind: vault\ntitle: Selection\n---\n"
            ),
        )
        .unwrap();
        default_from_path(&path).unwrap()
    }

    #[test]
    fn explicit_then_nearest_ancestor_then_default_without_merging() {
        let (_temp, base) = fixture();
        let fallback = wiki(&base, "Personal Wiki 猫", "Vault.Default");
        let ancestor = wiki(&base, "ancestor", "Vault.Ancestor");
        let nested = wiki(&ancestor.path, "nested", "Vault.Nested");
        let inside = nested.path.join("inside");
        fs::create_dir(&inside).unwrap();
        let selected = select(None, &inside, Some(&fallback)).unwrap();
        assert_eq!(selected.root.path(), nested.path);
        assert_eq!(selected.origin, SelectionOrigin::Ancestor);
        let selected = select(Some(&ancestor.path), &inside, Some(&fallback)).unwrap();
        assert_eq!(selected.wiki_id, ancestor.wiki_id);
        assert_eq!(selected.origin, SelectionOrigin::Explicit);
        let selected = select(None, &base, Some(&fallback)).unwrap();
        assert_eq!(selected.wiki_id, fallback.wiki_id);
        assert_eq!(selected.origin, SelectionOrigin::Default);
    }

    #[test]
    fn unused_missing_default_does_not_block_explicit_or_ancestor() {
        let (_temp, base) = fixture();
        let existing = wiki(&base, "existing", "Vault.Existing");
        let missing = DefaultWiki {
            path: base.join("missing"),
            wiki_id: RecordId::new("Vault.Missing").unwrap(),
        };
        assert!(select(Some(&existing.path), &base, Some(&missing)).is_ok());
        assert!(select(None, &existing.path, Some(&missing)).is_ok());
        assert!(select(None, &base, Some(&missing)).is_err());
    }

    #[test]
    fn invalid_explicit_and_nearer_markers_never_redirect() {
        let (_temp, base) = fixture();
        let fallback = wiki(&base, "fallback", "Vault.Default");
        let ancestor = wiki(&base, "ancestor", "Vault.Ancestor");
        let nested = ancestor.path.join("nested");
        fs::create_dir(&nested).unwrap();
        assert!(select(Some(&nested), &base, Some(&fallback)).is_err());
        fs::write(nested.join("WIKI.md"), b"invalid marker bytes").unwrap();
        assert_eq!(
            select(None, &nested, Some(&fallback)).unwrap_err().code,
            ErrorCode::RecordInvalid
        );
        fs::remove_file(nested.join("WIKI.md")).unwrap();
        fs::create_dir(nested.join("WIKI.md")).unwrap();
        assert!(select(None, &nested, Some(&fallback)).is_err());
    }

    #[test]
    fn replacement_default_and_expected_identity_guard_fail_closed() {
        let (_temp, base) = fixture();
        let mut fallback = wiki(&base, "wiki", "Vault.First");
        let selected = select(None, &base, Some(&fallback)).unwrap();
        assert!(expect_wiki_id(&selected, Some(&fallback.wiki_id)).is_ok());
        assert_eq!(
            expect_wiki_id(&selected, Some(&RecordId::new("Other").unwrap()))
                .unwrap_err()
                .code,
            ErrorCode::ContentConflict
        );
        fallback.wiki_id = RecordId::new("Vault.Replaced").unwrap();
        assert_eq!(
            select(None, &base, Some(&fallback)).unwrap_err().code,
            ErrorCode::ContentConflict
        );
        fs::rename(&fallback.path, base.join("moved")).unwrap();
        assert!(select(None, &base, Some(&fallback)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn ancestor_alias_is_canonical_but_final_root_or_marker_alias_refuses() {
        use std::os::unix::fs::symlink;
        let (_temp, base) = fixture();
        let parent = base.join("parent");
        fs::create_dir(&parent).unwrap();
        let original = wiki(&parent, "wiki", "Vault.First");
        let alias = base.join("alias");
        symlink(&parent, &alias).unwrap();
        let selected = select(Some(&alias.join("wiki")), &base, None).unwrap();
        assert_eq!(selected.root.path(), original.path);
        assert_eq!(selected.requested_path, alias.join("wiki"));
        symlink(&original.path, base.join("final-alias")).unwrap();
        assert!(select(Some(&base.join("final-alias")), &base, Some(&original)).is_err());
        let nested = original.path.join("nested");
        fs::create_dir(&nested).unwrap();
        symlink(original.path.join("WIKI.md"), nested.join("WIKI.md")).unwrap();
        assert!(select(None, &nested, Some(&original)).is_err());
    }
}
