//! Compact, exact navigation state over saturated indexed candidate buckets.
//! `Many` proves ambiguity; its witnesses never purport to list every candidate.
use super::link_facts::{MatchKey, MatchKeyKind};
use crate::{
    domain::{ErrorCode, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    records::{
        LinkResolution, RegistryEntry,
        links::{ExactPaths, UntypedLookup, companion_target, untyped_lookup},
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegistryCandidate {
    pub id: RecordId,
    pub kind: RecordKind,
    pub path: VaultRelativePath,
}
impl From<&RegistryEntry> for RegistryCandidate {
    fn from(entry: &RegistryEntry) -> Self {
        Self {
            id: entry.id.clone(),
            kind: entry.kind,
            path: entry.path.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RegistryProbe {
    Zero,
    One(RegistryCandidate),
    Many([RegistryCandidate; 2]),
}
impl RegistryProbe {
    /// Append-only overlays can add a witness without enumerating a broad bucket.
    /// Entry identity is (ID,path), preserving duplicate-ID ambiguity semantics.
    pub(crate) fn insert(&mut self, candidate: RegistryCandidate) -> Result<()> {
        let existing: &[RegistryCandidate] = match self {
            Self::Zero => &[],
            Self::One(entry) => std::slice::from_ref(entry),
            Self::Many(entries) => entries,
        };
        for entry in existing {
            if entry.id == candidate.id && entry.path == candidate.path {
                if entry.kind != candidate.kind {
                    return Err(WikiError::new(
                        ErrorCode::IndexCorrupt,
                        "registry witness kind disagrees across buckets",
                    ));
                }
                return Ok(());
            }
        }
        match self {
            Self::Zero => *self = Self::One(candidate),
            Self::One(entry) => *self = Self::Many([entry.clone(), candidate]),
            Self::Many(_) => {}
        }
        Ok(())
    }
    fn merge(&mut self, other: Self) -> Result<()> {
        match other {
            Self::Zero => {}
            Self::One(entry) => self.insert(entry)?,
            Self::Many(entries) => {
                self.insert(entries[0].clone())?;
                self.insert(entries[1].clone())?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NavigationResolution {
    Resolved {
        id: RecordId,
        path: VaultRelativePath,
        fragment: Option<String>,
        companion_stale: bool,
    },
    Missing,
    External,
    Ambiguous,
    WrongKind {
        actual: RecordKind,
    },
    CompanionConflict {
        expected: RecordId,
        actual: RecordId,
    },
}
impl From<&LinkResolution> for NavigationResolution {
    fn from(resolution: &LinkResolution) -> Self {
        match resolution {
            LinkResolution::Resolved {
                id,
                path,
                fragment,
                companion_stale,
            } => Self::Resolved {
                id: id.clone(),
                path: path.clone(),
                fragment: fragment.clone(),
                companion_stale: *companion_stale,
            },
            LinkResolution::Missing => Self::Missing,
            LinkResolution::External => Self::External,
            LinkResolution::Ambiguous { .. } => Self::Ambiguous,
            LinkResolution::WrongKind { actual } => Self::WrongKind { actual: *actual },
            LinkResolution::CompanionConflict { expected, actual } => Self::CompanionConflict {
                expected: expected.clone(),
                actual: actual.clone(),
            },
        }
    }
}
fn bucket(
    kind: MatchKeyKind,
    value: &str,
    probe: &mut impl FnMut(&MatchKey) -> Result<RegistryProbe>,
) -> Result<RegistryProbe> {
    probe(&MatchKey {
        kind,
        value: value.to_owned(),
    })
}
fn exact(
    paths: ExactPaths<'_>,
    probe: &mut impl FnMut(&MatchKey) -> Result<RegistryProbe>,
) -> Result<RegistryProbe> {
    let direct = bucket(MatchKeyKind::Path, paths.direct, probe)?;
    if direct != RegistryProbe::Zero {
        return Ok(direct);
    }
    match paths.fallback {
        Some(path) => bucket(MatchKeyKind::Path, &path, probe),
        None => Ok(RegistryProbe::Zero),
    }
}
fn unique(
    candidates: RegistryProbe,
    fragment: Option<String>,
    probe: &mut impl FnMut(&MatchKey) -> Result<RegistryProbe>,
) -> Result<NavigationResolution> {
    match candidates {
        RegistryProbe::Zero => Ok(NavigationResolution::Missing),
        RegistryProbe::Many(_) => Ok(NavigationResolution::Ambiguous),
        RegistryProbe::One(entry) => match bucket(MatchKeyKind::Id, entry.id.as_str(), probe)? {
            RegistryProbe::One(copy) if copy == entry => Ok(NavigationResolution::Resolved {
                id: entry.id,
                path: entry.path,
                fragment,
                companion_stale: false,
            }),
            RegistryProbe::Many(_) => Ok(NavigationResolution::Ambiguous),
            _ => Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "registry candidate lacks consistent ID witness",
            )),
        },
    }
}
pub(crate) fn resolve_untyped(
    destination: &str,
    probe: &mut impl FnMut(&MatchKey) -> Result<RegistryProbe>,
) -> Result<NavigationResolution> {
    let (paths, basename, fragment) = match untyped_lookup(destination) {
        UntypedLookup::External => return Ok(NavigationResolution::External),
        UntypedLookup::Missing => return Ok(NavigationResolution::Missing),
        UntypedLookup::Local {
            exact,
            basename,
            fragment,
        } => (exact, basename, fragment),
    };
    let path = paths.direct;
    let direct = exact(paths, probe)?;
    if direct != RegistryProbe::Zero {
        return unique(direct, fragment, probe);
    }
    let mut candidates = bucket(MatchKeyKind::Basename, basename, probe)?;
    if matches!(candidates, RegistryProbe::Many(_)) {
        return Ok(NavigationResolution::Ambiguous);
    }
    candidates.merge(bucket(MatchKeyKind::Alias, path, probe)?)?;
    if matches!(candidates, RegistryProbe::Many(_)) {
        return Ok(NavigationResolution::Ambiguous);
    }
    if basename != path {
        candidates.merge(bucket(MatchKeyKind::Alias, basename, probe)?)?;
    }
    unique(candidates, fragment, probe)
}
pub(crate) fn resolve_typed(
    id: &RecordId,
    expected_kind: RecordKind,
    companion: Option<&str>,
    probe: &mut impl FnMut(&MatchKey) -> Result<RegistryProbe>,
) -> Result<NavigationResolution> {
    let entry = match bucket(MatchKeyKind::Id, id.as_str(), probe)? {
        RegistryProbe::Zero => return Ok(NavigationResolution::Missing),
        RegistryProbe::Many(_) => return Ok(NavigationResolution::Ambiguous),
        RegistryProbe::One(entry) => entry,
    };
    if entry.kind != expected_kind {
        return Ok(NavigationResolution::WrongKind { actual: entry.kind });
    }
    let mut fragment = None;
    let mut stale = companion.is_none();
    if let Some(companion) = companion {
        let (paths, hint) = companion_target(companion);
        fragment = hint;
        match exact(paths, probe)? {
            RegistryProbe::Zero => stale = true,
            RegistryProbe::Many(_) => return Ok(NavigationResolution::Ambiguous),
            RegistryProbe::One(at_path) if at_path.id != entry.id => {
                return Ok(NavigationResolution::CompanionConflict {
                    expected: id.clone(),
                    actual: at_path.id,
                });
            }
            RegistryProbe::One(_) => {}
        }
    }
    Ok(NavigationResolution::Resolved {
        id: entry.id,
        path: entry.path,
        fragment,
        companion_stale: stale,
    })
}
