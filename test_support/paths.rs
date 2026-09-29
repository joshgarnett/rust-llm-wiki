//! Runtime paths shared by Cargo tests and Bazel runfiles tests.
#![allow(dead_code)]

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

pub(crate) fn binary(value: &str) -> PathBuf {
    resolve(Path::new(value), &Environment::current())
        .unwrap_or_else(|error| panic!("resolve test binary {value}: {error}"))
}

pub(crate) fn fixture(manifest_dir: &str, relative: &str) -> PathBuf {
    let path = Path::new(manifest_dir).join(relative);
    resolve(&path, &Environment::current())
        .unwrap_or_else(|error| panic!("resolve test fixture {}: {error}", path.display()))
}

// Explicit inputs keep resolver tests independent of process-global environment.
pub(crate) struct Environment {
    pub roots: Vec<PathBuf>,
    pub workspace: String,
    pub manifest: Option<PathBuf>,
    pub cwd: PathBuf,
}

impl Environment {
    fn current() -> Self {
        Self {
            roots: ["RUNFILES_DIR", "TEST_SRCDIR"]
                .into_iter()
                .filter_map(env::var_os)
                .map(PathBuf::from)
                .collect(),
            workspace: env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into()),
            manifest: env::var_os("RUNFILES_MANIFEST_FILE").map(PathBuf::from),
            cwd: env::current_dir().expect("test working directory"),
        }
    }
}

pub(crate) fn resolve(path: &Path, environment: &Environment) -> io::Result<PathBuf> {
    if path.is_absolute() {
        return fs::canonicalize(path);
    }
    // Strip a leading `.` emitted by CARGO_MANIFEST_DIR="." under Bazel.
    let relative: PathBuf = path
        .components()
        .filter(|component| *component != std::path::Component::CurDir)
        .collect();
    for root in &environment.roots {
        for candidate in [
            root.join(&environment.workspace).join(&relative),
            root.join(&relative),
        ] {
            if candidate.exists() {
                return fs::canonicalize(candidate);
            }
        }
    }
    if let Some(manifest) = &environment.manifest {
        let entries = read_manifest(manifest)?;
        let key = relative.to_string_lossy().replace('\\', "/");
        for key in [format!("{}/{key}", environment.workspace), key] {
            if let Some(actual) = manifest_path(&entries, &key) {
                return fs::canonicalize(actual);
            }
        }
    }
    fs::canonicalize(environment.cwd.join(relative))
}

fn read_manifest(path: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    fs::read_to_string(path)?
        .lines()
        .map(|line| {
            let (escaped, line) = match line.strip_prefix(' ') {
                Some(line) => (true, line),
                None => (false, line),
            };
            let (key, actual) = line.split_once(' ').unwrap_or((line, line));
            if escaped {
                Ok((unescape(key)?, PathBuf::from(unescape(actual)?)))
            } else {
                Ok((key.to_owned(), PathBuf::from(actual)))
            }
        })
        .collect()
}

fn unescape(value: &str) -> io::Result<String> {
    let mut output = String::new();
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        output.push(if character == '\\' {
            match chars.next() {
                Some('s') => ' ',
                Some('n') => '\n',
                Some('b') => '\\',
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid runfiles escape",
                    ));
                }
            }
        } else {
            character
        });
    }
    Ok(output)
}

fn manifest_path(entries: &[(String, PathBuf)], key: &str) -> Option<PathBuf> {
    if let Some((_, actual)) = entries.iter().find(|(entry, _)| entry == key) {
        return Some(actual.clone());
    }
    // A directory runfile can cover files below it.
    for ancestor in Path::new(key).ancestors().skip(1) {
        let parent = ancestor.to_string_lossy().replace('\\', "/");
        if let Some((_, actual)) = entries.iter().find(|(entry, _)| *entry == parent) {
            return Some(actual.join(Path::new(key).strip_prefix(ancestor).ok()?));
        }
    }
    // Windows manifests often list only files. Fixture trees are source inputs:
    // recover their physical directory only if all listed descendants agree.
    let prefix = format!("{key}/");
    let descendants: Vec<_> = entries
        .iter()
        .filter_map(|(entry, actual)| entry.strip_prefix(&prefix).map(|tail| (tail, actual)))
        .collect();
    let (tail, actual) = descendants.first()?;
    let directory = actual
        .ancestors()
        .nth(Path::new(tail).components().count())?;
    if directory.is_dir()
        && descendants.iter().all(|(tail, actual)| {
            fs::canonicalize(directory.join(tail)).ok() == fs::canonicalize(actual).ok()
                && actual.exists()
        })
    {
        Some(directory.to_owned())
    } else {
        None
    }
}
