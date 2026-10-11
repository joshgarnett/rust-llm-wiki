//! User- or project-scoped installation of one immutable package with selected discovery links.
//! One bounded current manifest and one pending operation own exact files and link
//! identities. Publication uses retained directories and atomic exchanges.
use crate::domain::{ErrorCode, Result, WikiError};
use serde_json::Value;
use std::path::Path;

pub(crate) fn operate(
    action: &str,
    target: &str,
    root: &Path,
    scope: &str,
    dry_run: bool,
) -> Result<Value> {
    if !["user", "project"].contains(&scope) {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "scope must be user or project",
        ));
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        native::run_scoped(
            action,
            target,
            root,
            scope,
            dry_run,
            &mut native::Hooks::default(),
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (action, target, root, dry_run);
        Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "guarded skill installation is available on macOS and Linux; use skill export on this platform",
        ))
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod native {
    use super::*;
    use crate::cli::skill_export::{
        self, Events, Location,
        filesystem::{self, Directory, FileGuard, LinkGuard},
    };
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
    };

    #[cfg(test)]
    const ROOT: &str = ".local/share/lwiki/skills";
    fn root_relative(scope: &str) -> &'static str {
        if scope == "project" {
            ".agents/skills/.lwiki"
        } else {
            ".local/share/lwiki/skills"
        }
    }
    fn scope_for(root: &Directory) -> &'static str {
        if root.path().ends_with(".agents/skills/.lwiki") {
            "project"
        } else {
            "user"
        }
    }
    const OWNER: &[u8] = b"lwiki.skill-install-root.v2\n";
    const MAX_RECORD: usize = 8 * 1024 * 1024;
    const MAX_FILE: usize = 1024 * 1024;
    const LAYOUTS: [&str; 2] = [".agents/skills/llm-wiki", ".claude/skills/llm-wiki"];
    const MEMBERS: [&str; 7] = [
        "SKILL.md",
        "agents/openai.yaml",
        "references/workflows.md",
        "references/cited-page.md",
        "references/examples.json",
        "references/commands.md",
        "manifest.json",
    ];

    fn conflict(message: impl Into<String>) -> WikiError {
        WikiError::new(ErrorCode::ContentConflict, message)
    }
    fn layout(target: &str) -> Result<&'static str> {
        match target {
            "codex" | "cursor" => Ok(LAYOUTS[0]),
            "claude-code" => Ok(LAYOUTS[1]),
            _ => Err(WikiError::new(
                ErrorCode::Usage,
                "target must be codex, cursor or claude-code",
            )),
        }
    }
    fn bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
        serde_json::to_vec(value).map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))
    }
    fn checksum(value: &[u8]) -> String {
        blake3::hash(value).to_hex().to_string()
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Member {
        size: usize,
        checksum: String,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Package {
        key: String,
        version: String,
        files: BTreeMap<String, Member>,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LinkStamp {
        target: PathBuf,
        identity: (u64, u64),
    }
    impl LinkStamp {
        fn of(link: &LinkGuard) -> Self {
            Self {
                target: link.target().into(),
                identity: link.identity(),
            }
        }
        fn matches(&self, link: &LinkGuard) -> bool {
            self.target == link.target() && self.identity == link.identity()
        }
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FileStamp {
        identity: (u64, u64),
        member: Member,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct State {
        scope: String,
        root: PathBuf,
        root_identity: (u64, u64),
        installation_id: String,
        generation: u64,
        operation_id: Option<String>,
        active: Option<Package>,
        hosts: BTreeSet<String>,
        links: BTreeMap<String, LinkStamp>,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Envelope<T> {
        schema: String,
        checksum: String,
        value: T,
    }
    fn encoded<T: Serialize>(schema: &str, value: &T) -> Result<Vec<u8>> {
        bytes(&Envelope {
            schema: schema.into(),
            checksum: checksum(&bytes(value)?),
            value,
        })
    }
    fn decoded<T: for<'a> Deserialize<'a> + Serialize>(schema: &str, value: &[u8]) -> Result<T> {
        let envelope: Envelope<T> = serde_json::from_slice(value)
            .map_err(|_| conflict("skill ownership record is invalid"))?;
        if envelope.schema != schema || envelope.checksum != checksum(&bytes(&envelope.value)?) {
            return Err(conflict("skill ownership record was changed"));
        }
        if value != encoded(schema, &envelope.value)? {
            return Err(conflict("skill ownership record bytes were changed"));
        }
        Ok(envelope.value)
    }
    const STATE_SCHEMA: &str = "lwiki.skill-install-state.v2";
    const INTENT_SCHEMA: &str = "lwiki.skill-install-pending.v2";
    const LINK_SCHEMA: &str = "lwiki.skill-install-link.v2";
    const FILE_SCHEMA: &str = "lwiki.skill-install-file.v2";
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Logical {
        active: Option<Package>,
        hosts: BTreeSet<String>,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LinkChange {
        path: String,
        before: Option<LinkStamp>,
        after: Option<PathBuf>,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Intent {
        id: String,
        action: String,
        target: String,
        before: State,
        before_manifest: FileStamp,
        after: Logical,
        payload: Option<BTreeMap<String, Vec<u8>>>,
        links: Vec<LinkChange>,
    }
    struct Pending {
        directory: Directory,
        intent: Intent,
        prepared: BTreeMap<usize, LinkStamp>,
        manifest: Option<FileStamp>,
    }
    #[derive(Default)]
    struct Guards {
        directories: Vec<Directory>,
        files: Vec<FileGuard>,
        links: Vec<LinkGuard>,
    }
    impl Guards {
        fn verify(&self) -> Result<()> {
            for d in &self.directories {
                d.verify_binding()?
            }
            for f in &self.files {
                f.verify_binding()?
            }
            for l in &self.links {
                l.verify_binding()?
            }
            Ok(())
        }
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum Phase {
        BootstrapStage,
        Bootstrap,
        Intent,
        Payload,
        LinkCreate,
        LinkStamp,
        Link,
        LinkPublished,
        Manifest,
        Complete,
        Cleanup,
        CleanupFile,
        CleanupRecord,
        CleanupState,
        Return,
    }
    #[cfg(test)]
    type Callback = Box<dyn FnMut(Phase, &Path) -> Result<()>>;
    #[derive(Default)]
    pub(super) struct Hooks {
        #[cfg(test)]
        callback: Option<Callback>,
        #[cfg(test)]
        package: Option<BTreeMap<String, Vec<u8>>>,
    }
    impl Hooks {
        fn fire(&mut self, phase: Phase, path: &Path) -> Result<()> {
            #[cfg(test)]
            if let Some(callback) = &mut self.callback {
                return callback(phase, path);
            }
            #[cfg(not(test))]
            let _ = (phase, path);
            Ok(())
        }
    }
    fn read(
        dir: &Directory,
        name: &str,
        limit: usize,
        guards: &mut Guards,
    ) -> Result<Option<Vec<u8>>> {
        let Some(size) = dir.file_size(name, limit)? else {
            return Ok(None);
        };
        let (data, guard) = dir.read_file(name, size)?;
        guards.files.push(guard);
        Ok(Some(data))
    }
    fn record<T: for<'a> Deserialize<'a> + Serialize>(
        dir: &Directory,
        name: &str,
        schema: &str,
        guards: &mut Guards,
    ) -> Result<Option<T>> {
        read(dir, name, MAX_RECORD, guards)?
            .map(|data| decoded(schema, &data))
            .transpose()
    }
    fn member(data: &[u8]) -> Member {
        Member {
            size: data.len(),
            checksum: checksum(data),
        }
    }
    fn package(files: &BTreeMap<String, Vec<u8>>) -> Result<Package> {
        if files.len() != MEMBERS.len()
            || MEMBERS.iter().any(|n| !files.contains_key(*n))
            || files.values().any(|v| v.len() > MAX_FILE)
        {
            return Err(conflict(
                "skill package has unexpected files or exceeds its size limit",
            ));
        }
        let members: BTreeMap<_, _> = files.iter().map(|(n, b)| (n.clone(), member(b))).collect();
        let manifest: Value = serde_json::from_slice(&files["manifest.json"])
            .map_err(|_| conflict("skill package manifest is invalid"))?;
        let version = manifest["version"]
            .as_str()
            .ok_or_else(|| conflict("skill package has no release version"))?
            .to_owned();
        if version.len() > 128 {
            return Err(conflict("skill version is too long"));
        }
        Ok(Package {
            key: checksum(&bytes(&members)?),
            version,
            files: members,
        })
    }
    fn validate_package(package: &Package) -> Result<()> {
        if package.version.len() > 128
            || package.files.len() != MEMBERS.len()
            || MEMBERS.iter().any(|n| !package.files.contains_key(*n))
            || package.files.values().any(|m| {
                m.size > MAX_FILE
                    || m.checksum.len() != 64
                    || !m.checksum.bytes().all(|b| b.is_ascii_hexdigit())
            })
            || package.key != checksum(&bytes(&package.files)?)
        {
            return Err(conflict(
                "skill package ownership record has unexpected members",
            ));
        }
        Ok(())
    }
    fn targets(logical: &Logical, root: &Path) -> Result<BTreeMap<String, PathBuf>> {
        let mut result = BTreeMap::new();
        for host in &logical.hosts {
            let package = logical
                .active
                .as_ref()
                .ok_or_else(|| conflict("installed skill has no recorded package"))?;
            result.insert(
                layout(host)?.into(),
                root.join("versions").join(&package.key),
            );
        }
        Ok(result)
    }
    fn logical(state: &State) -> Logical {
        Logical {
            active: state.active.clone(),
            hosts: state.hosts.clone(),
        }
    }
    fn default_state_at(root: &Directory, final_path: &Path, scope: &str) -> Result<State> {
        let root_identity = root.identity();
        Ok(State {
            scope: scope.into(),
            root: final_path.into(),
            root_identity,
            installation_id: checksum(&bytes(&(final_path, root_identity))?),
            generation: 0,
            operation_id: None,
            active: None,
            hosts: BTreeSet::new(),
            links: BTreeMap::new(),
        })
    }
    fn bootstrap_root(
        home: &Location,
        scope: &str,
        created: &mut Vec<String>,
        hooks: &mut Hooks,
    ) -> Result<(Directory, filesystem::DirectoryLock)> {
        let (relative_parent, name) = root_relative(scope)
            .rsplit_once('/')
            .expect("fixed managed root");
        let parent = materialize(home.append(relative_parent)?, created)?;
        if parent.child(name)?.is_some() {
            return Err(conflict(
                "an installation folder appeared before bootstrap; it was preserved",
            ));
        }
        let temporary = format!(".lwiki-bootstrap-{}", uuid::Uuid::now_v7());
        let stage = parent.create_directory(&temporary, created, &mut Events::default())?;
        parent.sync()?;
        let lock = stage.lock()?;
        let final_path = parent.path().join(name);
        let initial = default_state_at(&stage, &final_path, scope)?;
        stage.write_new("owner", OWNER, created, &mut Events::default())?;
        stage.write_new(
            "current.json",
            &encoded(STATE_SCHEMA, &initial)?,
            created,
            &mut Events::default(),
        )?;
        stage.sync()?;
        hooks.fire(Phase::BootstrapStage, stage.path())?;
        stage.verify_binding()?;
        let identity = stage.identity();
        parent.move_new(&temporary, &parent, name)?;
        created.push(final_path.display().to_string());
        let published = parent
            .child(name)?
            .ok_or_else(|| conflict("published installation folder disappeared"))?;
        if published.identity() != identity {
            return Err(conflict(
                "installation folder binding changed during bootstrap",
            ));
        }
        Ok((published, lock))
    }
    fn validate_state(state: &State, root: &Directory) -> Result<()> {
        if state.scope != scope_for(root)
            || state.root != root.path()
            || state.root_identity != root.identity()
            || state.installation_id != checksum(&bytes(&(root.path(), root.identity()))?)
        {
            return Err(conflict(
                "skill installation moved or was copied; restore its original folder, remove the skill there, then install it in this folder",
            ));
        }
        if state.hosts.is_empty() != state.active.is_none()
            || state
                .operation_id
                .as_ref()
                .is_some_and(|id| id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()))
            || (state.generation == 0) != state.operation_id.is_none()
        {
            return Err(conflict("skill ownership state is inconsistent"));
        }
        if let Some(package) = &state.active {
            validate_package(package)?
        }
        let expected = targets(&logical(state), root.path())?;
        if state.links.len() != expected.len()
            || state
                .links
                .iter()
                .any(|(p, s)| expected.get(p) != Some(&s.target))
        {
            return Err(conflict(
                "skill ownership record contains unexpected discovery paths",
            ));
        }
        Ok(())
    }
    fn changes(before: &State, after: &Logical) -> Result<Vec<LinkChange>> {
        let wanted = targets(after, &before.root)?;
        Ok(LAYOUTS
            .into_iter()
            .filter_map(|p| {
                let old = before.links.get(p).cloned();
                let new = wanted.get(p).cloned();
                (old.as_ref().map(|s| &s.target) != new.as_ref()).then(|| LinkChange {
                    path: p.into(),
                    before: old,
                    after: new,
                })
            })
            .collect())
    }
    fn make_intent(
        action: &str,
        target: &str,
        before: &State,
        before_manifest: FileStamp,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Option<Intent>> {
        let available = package(files)?;
        let mut after = logical(before);
        match action {
            "install" => {
                if !before.hosts.is_empty()
                    && before
                        .active
                        .as_ref()
                        .is_some_and(|p| p.key != available.key)
                {
                    return Err(conflict(
                        "an earlier skill package is installed; run skill update before installing another agent",
                    ));
                }
                after.hosts.insert(target.into());
                after.active = Some(available.clone());
            }
            "update" => {
                if !before.hosts.contains(target) {
                    return Err(WikiError::new(
                        ErrorCode::Usage,
                        "install the skill for this agent before updating it",
                    ));
                }
                after.active = Some(available.clone());
            }
            "remove" => {
                after.hosts.remove(target);
                if after.hosts.is_empty() {
                    after.active = None
                }
            }
            _ => {
                return Err(WikiError::new(
                    ErrorCode::Usage,
                    "unknown skill installation operation",
                ));
            }
        }
        if after == logical(before) {
            return Ok(None);
        }
        let payload = if after.active != before.active && after.active.is_some() {
            Some(files.clone())
        } else {
            None
        };
        let links = changes(before, &after)?;
        let id = checksum(&bytes(&(action, target, before, &after))?);
        Ok(Some(Intent {
            id,
            action: action.into(),
            target: target.into(),
            before: before.clone(),
            before_manifest,
            after,
            payload,
            links,
        }))
    }
    fn validate_intent(intent: &Intent, root: &Directory) -> Result<()> {
        validate_state(&intent.before, root)?;
        if intent.before_manifest.member != member(&encoded(STATE_SCHEMA, &intent.before)?) {
            return Err(conflict(
                "pending operation has an unexpected current manifest checksum",
            ));
        }
        layout(&intent.target)?;
        if intent.id
            != checksum(&bytes(&(
                &intent.action,
                &intent.target,
                &intent.before,
                &intent.after,
            ))?)
            || changes(&intent.before, &intent.after)? != intent.links
        {
            return Err(conflict("pending skill operation was changed"));
        }
        let mut expected = logical(&intent.before);
        match intent.action.as_str() {
            "install" => {
                expected.hosts.insert(intent.target.clone());
            }
            "update" if expected.hosts.contains(&intent.target) => {}
            "remove" => {
                expected.hosts.remove(&intent.target);
                if expected.hosts.is_empty() {
                    expected.active = None
                }
            }
            _ => return Err(conflict("pending skill operation is invalid")),
        }
        if let Some(payload) = &intent.payload {
            if intent.action == "remove" {
                return Err(conflict("skill removal cannot replace the package"));
            }
            expected.active = Some(package(payload)?);
        }
        if expected != intent.after {
            return Err(conflict(
                "pending skill operation contains an unowned change",
            ));
        }
        if let Some(package) = &intent.after.active {
            validate_package(package)?
        }
        Ok(())
    }
    fn after_state(pending: &Pending) -> Result<State> {
        let mut after = pending.intent.before.clone();
        after.generation = after
            .generation
            .checked_add(1)
            .ok_or_else(|| conflict("skill state counter is exhausted"))?;
        after.operation_id = Some(pending.intent.id.clone());
        after.active = pending.intent.after.active.clone();
        after.hosts = pending.intent.after.hosts.clone();
        for (index, change) in pending.intent.links.iter().enumerate() {
            if change.after.is_none() {
                after.links.remove(&change.path);
            } else {
                after.links.insert(
                    change.path.clone(),
                    pending
                        .prepared
                        .get(&index)
                        .ok_or_else(|| {
                            conflict("a new discovery link has not been durably prepared")
                        })?
                        .clone(),
                );
            }
        }
        Ok(after)
    }
    fn materialize(location: Location, created: &mut Vec<String>) -> Result<Directory> {
        let mut directory = location.held;
        for name in location.missing {
            let child = directory.create_directory(&name, created, &mut Events::default())?;
            directory.sync()?;
            directory = child;
        }
        Ok(directory)
    }
    fn directory(parent: &Directory, name: &str, created: &mut Vec<String>) -> Result<Directory> {
        if let Some(child) = parent.child(name)? {
            return Ok(child);
        }
        let result = parent.create_directory(name, created, &mut Events::default())?;
        parent.sync()?;
        Ok(result)
    }
    fn link_parent(home: &Location, path: &str) -> Result<Location> {
        home.append(path.rsplit_once('/').expect("fixed layout").0)
    }
    fn observed_link(home: &Location, path: &str) -> Result<Option<LinkGuard>> {
        let parent = link_parent(home, path)?;
        if parent.missing.is_empty() {
            parent.held.read_link("llm-wiki")
        } else {
            Ok(None)
        }
    }
    fn publish_file(
        parent: &Directory,
        name: &str,
        expected: &[u8],
        scratch: &Directory,
        temporary: &str,
        created: &mut Vec<String>,
    ) -> Result<FileGuard> {
        if let Some(size) = parent.file_size(name, MAX_RECORD)? {
            let (data, guard) = parent.read_file(name, size)?;
            if data != expected {
                return Err(conflict(format!(
                    "owned skill file was changed: {}",
                    parent.path().join(name).display()
                )));
            }
            return Ok(guard);
        }
        if let Some(size) = scratch.file_size(temporary, MAX_RECORD)? {
            let (data, guard) = scratch.read_file(temporary, size)?;
            if !expected.starts_with(&data) {
                return Err(conflict(format!(
                    "owned skill scratch file was changed: {}",
                    scratch.path().join(temporary).display()
                )));
            }
            scratch.remove_file(temporary, &guard)?;
        }
        scratch.write_new(temporary, expected, created, &mut Events::default())?;
        scratch.sync()?;
        let (_, guard) = scratch.read_file(temporary, expected.len())?;
        guard.verify_binding()?;
        let identity = guard.identity()?;
        scratch.move_new(temporary, parent, name)?;
        created.push(parent.path().join(name).display().to_string());
        let (data, published) = parent.read_file(name, expected.len())?;
        if data != expected || published.identity()? != identity {
            return Err(conflict(
                "skill publication binding changed; preserve the reported paths",
            ));
        }
        Ok(published)
    }
    fn package_parent(root: &Directory, relative: &str) -> Result<Option<(Directory, String)>> {
        let (parent, name) = relative.rsplit_once('/').unwrap_or(("", relative));
        if parent.is_empty() {
            return Ok(Some((root.clone(), name.into())));
        }
        Ok(root.child(parent)?.map(|dir| (dir, name.into())))
    }
    fn verify_package(
        root: &Directory,
        package: &Package,
        incomplete: bool,
        guards: &mut Guards,
    ) -> Result<()> {
        let top: BTreeSet<_> = package
            .files
            .keys()
            .map(|p| p.split('/').next().unwrap())
            .collect();
        for name in root.entries(MEMBERS.len() + 1)? {
            if !top.contains(name.as_str()) {
                return Err(conflict(format!(
                    "unknown file in owned skill package was preserved: {}",
                    root.path().join(&name).display()
                )));
            }
            if name == "agents" || name == "references" {
                let dir = root
                    .child(&name)?
                    .ok_or_else(|| conflict("skill package directory disappeared"))?;
                guards.directories.push(dir.clone());
                for child in dir.entries(MEMBERS.len() + 1)? {
                    if !package.files.contains_key(&format!("{name}/{child}")) {
                        return Err(conflict(format!(
                            "unknown file in owned skill package was preserved: {}",
                            dir.path().join(&child).display()
                        )));
                    }
                }
            }
        }
        for (relative, expected) in &package.files {
            let Some((parent, name)) = package_parent(root, relative)? else {
                if incomplete {
                    continue;
                } else {
                    return Err(conflict("owned skill directory is missing"));
                }
            };
            guards.directories.push(parent.clone());
            let Some(data) = read(&parent, &name, MAX_FILE, guards)? else {
                if incomplete {
                    continue;
                } else {
                    return Err(conflict("owned skill file is missing"));
                }
            };
            if member(&data) != *expected {
                return Err(conflict(format!(
                    "owned skill file was changed: {}",
                    parent.path().join(name).display()
                )));
            }
        }
        Ok(())
    }
    fn prepare_payload(
        root: &Directory,
        pending: &Pending,
        created: &mut Vec<String>,
    ) -> Result<()> {
        let Some(files) = &pending.intent.payload else {
            return Ok(());
        };
        let package = pending
            .intent
            .after
            .active
            .as_ref()
            .expect("validated package");
        let versions = directory(root, "versions", created)?;
        if let Some(published) = versions.child(&package.key)? {
            let mut guards = Guards::default();
            verify_package(&published, package, false, &mut guards)?;
            return guards.verify();
        }
        let payload = directory(&pending.directory, "payload", created)?;
        for (index, (relative, expected)) in files.iter().enumerate() {
            let (subdir, name) = relative.rsplit_once('/').unwrap_or(("", relative));
            let parent = if subdir.is_empty() {
                payload.clone()
            } else {
                directory(&payload, subdir, created)?
            };
            publish_file(
                &parent,
                name,
                expected,
                &pending.directory,
                &format!("file-{index}"),
                created,
            )?;
        }
        let mut guards = Guards::default();
        verify_package(&payload, package, false, &mut guards)?;
        guards.verify()?;
        let identity = payload.identity();
        pending
            .directory
            .move_new("payload", &versions, &package.key)?;
        let published = versions
            .child(&package.key)?
            .ok_or_else(|| conflict("published package disappeared"))?;
        if published.identity() != identity {
            return Err(conflict("published package directory was replaced"));
        }
        Ok(())
    }
    fn pending_names(intent: &Intent) -> BTreeSet<String> {
        let mut names: BTreeSet<String> = [
            "intent.json",
            "write-intent",
            "payload",
            "state-next",
            "state-stamp.json",
            "write-state",
            "write-state-stamp",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if let Some(files) = &intent.payload {
            for i in 0..files.len() {
                names.insert(format!("file-{i}"));
            }
        }
        for i in 0..intent.links.len() {
            names.extend([
                format!("link-{i}"),
                format!("stamp-{i}.json"),
                format!("write-stamp-{i}"),
            ]);
        }
        names
    }
    fn load_pending(
        dir: &Directory,
        intent: Intent,
        current: &State,
        guards: &mut Guards,
    ) -> Result<Pending> {
        let names = pending_names(&intent);
        for name in dir.entries(24)? {
            if !names.contains(&name) {
                return Err(conflict(format!(
                    "unknown file in pending skill installation: {}",
                    dir.path().join(name).display()
                )));
            }
        }
        let mut prepared = BTreeMap::new();
        for (index, change) in intent.links.iter().enumerate() {
            if let Some(stamp) =
                record::<LinkStamp>(dir, &format!("stamp-{index}.json"), LINK_SCHEMA, guards)?
            {
                if change.after.as_ref() != Some(&stamp.target) {
                    return Err(conflict("staged discovery stamp has an unexpected target"));
                }
                prepared.insert(index, stamp);
            } else if current.operation_id.as_deref() == Some(&intent.id)
                && let Some(stamp) = current.links.get(&change.path)
                && change.after.as_ref() == Some(&stamp.target)
            {
                // The committed manifest supplies preparation evidence already removed by cleanup.
                prepared.insert(index, stamp.clone());
            }
        }
        let manifest = record(dir, "state-stamp.json", FILE_SCHEMA, guards)?;
        let pending = Pending {
            directory: dir.clone(),
            intent,
            prepared,
            manifest,
        };
        validate_scratch(&pending, current)?;
        Ok(pending)
    }
    fn scratch_prefix(dir: &Directory, name: &str, expected: &[u8]) -> Result<()> {
        if let Some(size) = dir.file_size(name, MAX_RECORD)? {
            let (data, guard) = dir.read_file(name, size)?;
            if !expected.starts_with(&data) {
                return Err(conflict(format!(
                    "owned scratch bytes changed: {}",
                    dir.path().join(name).display()
                )));
            }
            guard.verify_binding()?;
        }
        Ok(())
    }
    fn validate_scratch(pending: &Pending, current: &State) -> Result<()> {
        let dir = &pending.directory;
        scratch_prefix(
            dir,
            "write-intent",
            &encoded(INTENT_SCHEMA, &pending.intent)?,
        )?;
        if let Some(files) = &pending.intent.payload {
            for (i, data) in files.values().enumerate() {
                scratch_prefix(dir, &format!("file-{i}"), data)?
            }
        }
        for (i, change) in pending.intent.links.iter().enumerate() {
            let temporary = format!("write-stamp-{i}");
            if dir.file_size(&temporary, MAX_RECORD)?.is_some() {
                let stamp = if let Some(stamp) = pending.prepared.get(&i) {
                    stamp.clone()
                } else {
                    let link = dir
                        .read_link(&format!("link-{i}"))?
                        .ok_or_else(|| conflict("scratch stamp has no staged link"))?;
                    if change.after.as_ref() != Some(&link.target().to_path_buf()) {
                        return Err(conflict("scratch stamp has an unexpected staged target"));
                    }
                    LinkStamp::of(&link)
                };
                scratch_prefix(dir, &temporary, &encoded(LINK_SCHEMA, &stamp)?)?;
            }
        }
        if let Ok(after) = after_state(pending) {
            let data = encoded(STATE_SCHEMA, &after)?;
            scratch_prefix(dir, "write-state", &data)?;
            let committed = current.operation_id.as_deref() == Some(&pending.intent.id);
            let size = dir.file_size("state-next", MAX_RECORD)?;
            if let Some(size) = size {
                let (actual, guard) = dir.read_file("state-next", size)?;
                let expected = if committed {
                    encoded(STATE_SCHEMA, &pending.intent.before)?
                } else {
                    data.clone()
                };
                if actual != expected {
                    return Err(conflict("staged or displaced skill manifest changed"));
                }
                if let Some(stamp) = &pending.manifest {
                    let identity = if committed {
                        pending.intent.before_manifest.identity
                    } else {
                        stamp.identity
                    };
                    if guard.identity()? != identity || stamp.member != member(&data) {
                        return Err(conflict("staged skill manifest identity changed"));
                    }
                }
                guard.verify_binding()?;
                let stamp = pending.manifest.clone().unwrap_or(FileStamp {
                    identity: guard.identity()?,
                    member: member(&data),
                });
                scratch_prefix(dir, "write-state-stamp", &encoded(FILE_SCHEMA, &stamp)?)?;
            } else if pending.manifest.is_some() && !committed {
                return Err(conflict("prepared skill manifest is missing"));
            }
        } else if dir.file_size("state-next", MAX_RECORD)?.is_some()
            || dir.file_size("write-state", MAX_RECORD)?.is_some()
            || dir.file_size("state-stamp.json", MAX_RECORD)?.is_some()
            || dir.file_size("write-state-stamp", MAX_RECORD)?.is_some()
        {
            return Err(conflict(
                "state publication began before discovery links were prepared",
            ));
        }
        Ok(())
    }
    fn validate_links(
        home: &Location,
        state: &State,
        pending: Option<&Pending>,
        committed: bool,
        guards: &mut Guards,
    ) -> Result<()> {
        for path in LAYOUTS {
            let current = observed_link(home, path)?;
            let change = pending.and_then(|p| {
                p.intent
                    .links
                    .iter()
                    .enumerate()
                    .find(|(_, c)| c.path == path)
            });
            if let Some((index, change)) = change {
                let staged = pending
                    .expect("pending")
                    .directory
                    .read_link(&format!("link-{index}"))?;
                let prepared = pending.expect("pending").prepared.get(&index);
                let before_public = match (&change.before, &current) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a.matches(b),
                    _ => false,
                };
                let after_public = if change.after.is_some() {
                    match (prepared, &current) {
                        (Some(a), Some(b)) => a.matches(b),
                        _ => false,
                    }
                } else {
                    current.is_none()
                        && staged
                            .as_ref()
                            .zip(change.before.as_ref())
                            .is_some_and(|(s, b)| b.matches(s))
                };
                if committed {
                    // Displaced objects may already have been removed during bounded cleanup.
                    let expected = state.links.get(path);
                    if !match (expected, &current) {
                        (None, None) => true,
                        (Some(a), Some(b)) => a.matches(b),
                        _ => false,
                    } {
                        return Err(conflict(format!(
                            "committed discovery link was replaced: {path}"
                        )));
                    }
                } else if !before_public && !after_public {
                    return Err(conflict(format!(
                        "owned discovery link was replaced or removed: {path}"
                    )));
                }
                if let Some(stage) = &staged {
                    let valid = prepared.is_some_and(|p| p.matches(stage))
                        || change.before.as_ref().is_some_and(|p| p.matches(stage));
                    if !valid
                        && (prepared.is_some()
                            || change.after.as_ref() != Some(&stage.target().to_path_buf()))
                    {
                        return Err(conflict("owned staged discovery link was replaced"));
                    }
                }
                if let Some(link) = current
                    && (committed || (!before_public && after_public))
                {
                    guards.links.push(link)
                }
            } else {
                let expected = state.links.get(path);
                if !match (expected, &current) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a.matches(b),
                    _ => false,
                } {
                    return Err(conflict(format!(
                        "unknown or changed skill discovery link: {}",
                        home.held.path().join(path).display()
                    )));
                }
                if let Some(link) = current {
                    guards.links.push(link)
                }
            }
        }
        Ok(())
    }
    fn validate_versions(
        root: &Directory,
        state: &State,
        pending: Option<&Pending>,
        committed: bool,
        guards: &mut Guards,
    ) -> Result<()> {
        let old = pending.and_then(|p| p.intent.before.active.as_ref());
        let new = pending.and_then(|p| p.intent.after.active.as_ref());
        if let Some(versions) = root.child("versions")? {
            guards.directories.push(versions.clone());
            let allowed: BTreeSet<_> = state
                .active
                .iter()
                .chain(old)
                .chain(new)
                .map(|p| p.key.as_str())
                .collect();
            for key in versions.entries(3)? {
                if !allowed.contains(key.as_str()) {
                    return Err(conflict(format!(
                        "unknown skill package was preserved: {}",
                        versions.path().join(&key).display()
                    )));
                }
            }
            let packages: BTreeMap<_, _> = state
                .active
                .iter()
                .chain(old)
                .chain(new)
                .map(|p| (p.key.clone(), p))
                .collect();
            for (key, package) in packages {
                if let Some(dir) = versions.child(&key)? {
                    guards.directories.push(dir.clone());
                    let obsolete = committed && state.active.as_ref().is_none_or(|p| p.key != key);
                    verify_package(&dir, package, obsolete, guards)?;
                } else if state.active.as_ref().is_some_and(|p| p.key == key)
                    || (!committed && old.is_some_and(|p| p.key == key))
                {
                    return Err(conflict("an owned skill package is missing"));
                }
            }
        } else if state.active.is_some() {
            return Err(conflict("an owned skill package is missing"));
        }
        if let Some(pending) = pending
            && let Some(payload) = pending.directory.child("payload")?
        {
            let package = pending
                .intent
                .after
                .active
                .as_ref()
                .ok_or_else(|| conflict("removal has unexpected staged package"))?;
            guards.directories.push(payload.clone());
            verify_package(&payload, package, true, guards)?;
        }
        Ok(())
    }
    fn bootstrap_observation(
        root: &Directory,
        guards: &mut Guards,
    ) -> Result<Option<(State, FileStamp)>> {
        let allowed = ["owner", "current.json", "versions", "pending"];
        for name in root.entries(5)? {
            if !allowed.contains(&name.as_str()) {
                return Err(conflict(format!(
                    "unknown content in skill installation folder: {}",
                    root.path().join(name).display()
                )));
            }
        }
        let marker = read(root, "owner", OWNER.len(), guards)?.ok_or_else(|| conflict("the existing skill installation folder is not owned by lwiki; preserve or move it before installing"))?;
        if marker != OWNER {
            return Err(conflict("skill installation ownership marker was changed"));
        }
        let data = read(root, "current.json", MAX_RECORD, guards)?.ok_or_else(|| conflict("owned skill state is missing; preserve this folder and restore its saved state before retrying"))?;
        let state: State = decoded(STATE_SCHEMA, &data)?;
        validate_state(&state, root)?;
        let guard = guards.files.last().expect("current file guard");
        Ok(Some((
            state,
            FileStamp {
                identity: guard.identity()?,
                member: member(&data),
            },
        )))
    }
    fn prepare_links(
        pending: &mut Pending,
        home: &Location,
        created: &mut Vec<String>,
        hooks: &mut Hooks,
    ) -> Result<()> {
        for (index, change) in pending.intent.links.iter().enumerate() {
            let Some(target) = &change.after else {
                continue;
            };
            if pending.prepared.contains_key(&index) {
                continue;
            }
            let name = format!("link-{index}");
            let staged = match pending.directory.read_link(&name)? {
                Some(link) if link.target() == target => link,
                Some(_) => {
                    return Err(conflict(
                        "intent-owned scratch link has an unexpected target",
                    ));
                }
                None => pending.directory.symlink_new(&name, target, created)?,
            };
            hooks.fire(Phase::LinkCreate, &pending.directory.path().join(&name))?;
            staged.verify_binding()?;
            let stamp = LinkStamp::of(&staged);
            publish_file(
                &pending.directory,
                &format!("stamp-{index}.json"),
                &encoded(LINK_SCHEMA, &stamp)?,
                &pending.directory,
                &format!("write-stamp-{index}"),
                created,
            )?;
            pending.prepared.insert(index, stamp);
            hooks.fire(Phase::LinkStamp, &pending.directory.path().join(&name))?;
        }
        let mut guards = Guards::default();
        validate_links(
            home,
            &pending.intent.before,
            Some(pending),
            false,
            &mut guards,
        )?;
        guards.verify()
    }
    fn apply_links(
        pending: &Pending,
        root: &Directory,
        home: &Location,
        created: &mut Vec<String>,
        published: &mut Vec<String>,
        hooks: &mut Hooks,
    ) -> Result<()> {
        for (index, change) in pending.intent.links.iter().enumerate() {
            let parent = materialize(link_parent(home, &change.path)?, created)?;
            let name = format!("link-{index}");
            let current = parent.read_link("llm-wiki")?;
            let staged = pending.directory.read_link(&name)?;
            if let Some(prepared) = pending.prepared.get(&index)
                && current.as_ref().is_some_and(|l| prepared.matches(l))
            {
                if let Some(before) = &change.before
                    && !staged.as_ref().is_some_and(|l| before.matches(l))
                {
                    return Err(conflict("displaced discovery link is missing or changed"));
                }
                continue;
            }
            if change.after.is_none() && current.is_none() {
                if !staged
                    .as_ref()
                    .zip(change.before.as_ref())
                    .is_some_and(|(l, s)| s.matches(l))
                {
                    return Err(conflict(
                        "removed discovery link has no retained ownership proof",
                    ));
                }
                continue;
            }
            if !match (&change.before, &current) {
                (None, None) => true,
                (Some(a), Some(b)) => a.matches(b),
                _ => false,
            } {
                return Err(conflict("owned discovery link changed before publication"));
            }
            hooks.fire(Phase::Link, &parent.path().join("llm-wiki"))?;
            let mut package_guards = Guards::default();
            validate_versions(
                root,
                &pending.intent.before,
                Some(pending),
                false,
                &mut package_guards,
            )?;
            package_guards.verify()?;
            if let Some(link) = &current {
                link.verify_binding()?
            }
            match (&change.before, &change.after) {
                (None, Some(_)) => {
                    let staged =
                        staged.ok_or_else(|| conflict("prepared discovery link is missing"))?;
                    let expected = pending.prepared.get(&index).expect("prepared link");
                    if !expected.matches(&staged) {
                        return Err(conflict("prepared discovery link was replaced"));
                    }
                    staged.verify_binding()?;
                    pending.directory.move_new(&name, &parent, "llm-wiki")?;
                }
                (Some(before), Some(_)) => {
                    let staged =
                        staged.ok_or_else(|| conflict("prepared discovery link is missing"))?;
                    let expected = pending.prepared.get(&index).expect("prepared link");
                    if !expected.matches(&staged) {
                        return Err(conflict("prepared discovery link was replaced"));
                    }
                    staged.verify_binding()?;
                    pending.directory.exchange(&name, &parent, "llm-wiki")?;
                    let displaced = pending
                        .directory
                        .read_link(&name)?
                        .ok_or_else(|| conflict("displaced link disappeared"))?;
                    if !before.matches(&displaced) {
                        return Err(conflict(format!(
                            "discovery binding changed; displaced object is preserved at {}",
                            pending.directory.path().join(name).display()
                        )));
                    }
                }
                (Some(before), None) => {
                    if staged.is_some() {
                        return Err(conflict("displaced removal name is occupied"));
                    }
                    parent.move_new("llm-wiki", &pending.directory, &name)?;
                    let displaced = pending
                        .directory
                        .read_link(&name)?
                        .ok_or_else(|| conflict("removed link disappeared"))?;
                    if !before.matches(&displaced) {
                        return Err(conflict(
                            "removed discovery binding changed; displaced object was preserved",
                        ));
                    }
                }
                (None, None) => return Err(conflict("empty discovery operation")),
            }
            published.push(parent.path().join("llm-wiki").display().to_string());
            hooks.fire(Phase::LinkPublished, &parent.path().join("llm-wiki"))?;
            let mut guards = Guards::default();
            validate_links(
                home,
                &pending.intent.before,
                Some(pending),
                false,
                &mut guards,
            )?;
            guards.verify()?;
        }
        Ok(())
    }
    fn prepare_manifest(pending: &mut Pending, created: &mut Vec<String>) -> Result<State> {
        let after = after_state(pending)?;
        let data = encoded(STATE_SCHEMA, &after)?;
        if pending.manifest.is_none() {
            let guard = publish_file(
                &pending.directory,
                "state-next",
                &data,
                &pending.directory,
                "write-state",
                created,
            )?;
            let stamp = FileStamp {
                identity: guard.identity()?,
                member: member(&data),
            };
            publish_file(
                &pending.directory,
                "state-stamp.json",
                &encoded(FILE_SCHEMA, &stamp)?,
                &pending.directory,
                "write-state-stamp",
                created,
            )?;
            pending.manifest = Some(stamp);
        }
        Ok(after)
    }
    fn commit(root: &Directory, pending: &Pending, after: &State, hooks: &mut Hooks) -> Result<()> {
        let expected = pending.manifest.as_ref().expect("prepared manifest");
        let data = encoded(STATE_SCHEMA, after)?;
        let size = root
            .file_size("current.json", MAX_RECORD)?
            .ok_or_else(|| conflict("current skill state disappeared"))?;
        let (current, guard) = root.read_file("current.json", size)?;
        if member(&current) == expected.member && guard.identity()? == expected.identity {
            return Ok(());
        }
        if member(&current) != pending.intent.before_manifest.member
            || guard.identity()? != pending.intent.before_manifest.identity
        {
            return Err(conflict("current skill state was replaced"));
        }
        let (staged, stage_guard) = pending.directory.read_file("state-next", data.len())?;
        if staged != data || stage_guard.identity()? != expected.identity {
            return Err(conflict("prepared skill state was replaced"));
        }
        hooks.fire(Phase::Manifest, root.path())?;
        guard.verify_binding()?;
        stage_guard.verify_binding()?;
        pending
            .directory
            .exchange("state-next", root, "current.json")?;
        let (published, published_guard) = root.read_file("current.json", data.len())?;
        let (displaced, displaced_guard) =
            pending.directory.read_file("state-next", current.len())?;
        if published != data
            || published_guard.identity()? != expected.identity
            || displaced != current
            || displaced_guard.identity()? != guard.identity()?
        {
            return Err(conflict(
                "skill state binding changed during commit; displaced bytes were preserved",
            ));
        }
        Ok(())
    }
    fn remove_exact_file(
        dir: &Directory,
        name: &str,
        expected: &[u8],
        incomplete: bool,
    ) -> Result<()> {
        if let Some(size) = dir.file_size(name, MAX_RECORD)? {
            let (data, guard) = dir.read_file(name, size)?;
            if if incomplete {
                !expected.starts_with(&data)
            } else {
                data != expected
            } {
                return Err(conflict(format!(
                    "cleanup preserved a changed skill file: {}",
                    dir.path().join(name).display()
                )));
            }
            dir.remove_file(name, &guard)?;
        }
        Ok(())
    }
    fn remove_package(
        parent: &Directory,
        name: &str,
        package: &Package,
        hooks: &mut Hooks,
    ) -> Result<()> {
        let Some(root) = parent.child(name)? else {
            return Ok(());
        };
        let mut guards = Guards::default();
        verify_package(&root, package, true, &mut guards)?;
        guards.verify()?;
        for relative in package.files.keys() {
            if let Some((dir, name)) = package_parent(&root, relative)?
                && let Some(size) = dir.file_size(&name, MAX_FILE)?
            {
                let (data, guard) = dir.read_file(&name, size)?;
                if member(&data) != package.files[relative] {
                    return Err(conflict("cleanup preserved a changed obsolete skill file"));
                }
                dir.remove_file(&name, &guard)?;
                hooks.fire(Phase::CleanupFile, &dir.path().join(name))?;
            }
        }
        for child in ["agents", "references"] {
            if let Some(dir) = root.child(child)? {
                root.remove_directory(child, &dir)?
            }
        }
        parent.remove_directory(name, &root)
    }
    fn cleanup(
        root: &Directory,
        pending: &Pending,
        after: &State,
        hooks: &mut Hooks,
    ) -> Result<()> {
        hooks.fire(Phase::Cleanup, pending.directory.path())?;
        if let Some(old) = &pending.intent.before.active
            && after.active.as_ref().is_none_or(|p| p.key != old.key)
            && let Some(versions) = root.child("versions")?
        {
            remove_package(&versions, &old.key, old, hooks)?
        }
        if let Some(staged) = pending.directory.child("payload")? {
            let package = pending
                .intent
                .after
                .active
                .as_ref()
                .ok_or_else(|| conflict("unexpected removal payload"))?;
            let _ = staged;
            remove_package(&pending.directory, "payload", package, hooks)?;
        }
        if let Some(files) = &pending.intent.payload {
            for (i, data) in files.values().enumerate() {
                remove_exact_file(&pending.directory, &format!("file-{i}"), data, true)?
            }
        }
        for (i, change) in pending.intent.links.iter().enumerate() {
            let name = format!("link-{i}");
            if let Some(link) = pending.directory.read_link(&name)? {
                if !change.before.as_ref().is_some_and(|s| s.matches(&link)) {
                    return Err(conflict(
                        "cleanup preserved an unexpected displaced discovery link",
                    ));
                }
                pending.directory.remove_link(&name, &link)?;
            }
            if let Some(stamp) = pending.prepared.get(&i) {
                let data = encoded(LINK_SCHEMA, stamp)?;
                remove_exact_file(&pending.directory, &format!("write-stamp-{i}"), &data, true)?;
                remove_exact_file(&pending.directory, &format!("stamp-{i}.json"), &data, false)?;
                hooks.fire(Phase::CleanupRecord, pending.directory.path())?;
            }
        }
        let before_data = encoded(STATE_SCHEMA, &pending.intent.before)?;
        if let Some(size) = pending.directory.file_size("state-next", MAX_RECORD)? {
            let (data, guard) = pending.directory.read_file("state-next", size)?;
            if data != before_data || guard.identity()? != pending.intent.before_manifest.identity {
                return Err(conflict(
                    "cleanup preserved a changed displaced skill state",
                ));
            }
            pending.directory.remove_file("state-next", &guard)?;
        }
        let after_data = encoded(STATE_SCHEMA, after)?;
        remove_exact_file(&pending.directory, "write-state", &after_data, true)?;
        if let Some(stamp) = &pending.manifest {
            let data = encoded(FILE_SCHEMA, stamp)?;
            remove_exact_file(&pending.directory, "write-state-stamp", &data, true)?;
            remove_exact_file(&pending.directory, "state-stamp.json", &data, false)?;
        }
        hooks.fire(Phase::CleanupState, pending.directory.path())?;
        let intent_data = encoded(INTENT_SCHEMA, &pending.intent)?;
        remove_exact_file(&pending.directory, "write-intent", &intent_data, true)?;
        // Intent is removed last, after the committed operation id establishes cleanup authority.
        remove_exact_file(&pending.directory, "intent.json", &intent_data, false)?;
        root.remove_directory("pending", &pending.directory)
    }
    fn paths(home: &Path, intent: &Intent) -> Vec<Value> {
        let root = intent.before.root.clone();
        let pending = root.join("pending");
        let mut result = vec![
            json!({"path":root,"operation":"ensure_owned_directory"}),
            json!({"path":root.join("owner"),"operation":"verify_or_create_ownership","temporary_path":root.join("write-owner")}),
            json!({"path":root.join("versions"),"operation":"ensure_package_directory"}),
            json!({"path":pending,"operation":"ensure_pending_directory"}),
            json!({"path":root.join("current.json"),"operation":"commit_owned_state"}),
            json!({"path":pending.join("intent.json"),"operation":"record_pending_change","temporary_path":pending.join("write-intent")}),
            json!({"path":pending.join("state-next"),"operation":"stage_state","temporary_path":pending.join("write-state")}),
            json!({"path":pending.join("state-stamp.json"),"operation":"record_state_identity","temporary_path":pending.join("write-state-stamp")}),
        ];
        if let Some(files) = &intent.payload {
            for (i, name) in files.keys().enumerate() {
                result.push(json!({"path":pending.join("payload").join(name),"operation":"stage_package_file","temporary_path":pending.join(format!("file-{i}"))}));
            }
            if let Some(p) = &intent.after.active {
                result.push(json!({"path":root.join("versions").join(&p.key),"operation":"publish_package"}));
            }
        }
        for (i, link) in intent.links.iter().enumerate() {
            result.push(json!({"path":home.join(&link.path),"operation":if link.after.is_none(){"remove_owned_link"}else if link.before.is_some(){"replace_owned_link"}else{"create_discovery_link"},"before":link.before.as_ref().map(|s|&s.target),"after":link.after,"staged_path":pending.join(format!("link-{i}")),"identity_record":pending.join(format!("stamp-{i}.json"))}));
        }
        if let Some(old) = &intent.before.active
            && intent
                .after
                .active
                .as_ref()
                .is_none_or(|p| p.key != old.key)
        {
            for file in old.files.keys() {
                result.push(json!({"path":root.join("versions").join(&old.key).join(file),"operation":"remove_unchanged_obsolete_file"}));
            }
        }
        result
    }
    fn retry_details(intent: &Intent, root: &Path, scope: &str) -> Value {
        let argv = vec![
            "lwiki".to_owned(),
            "skill".to_owned(),
            intent.action.clone(),
            "--target".to_owned(),
            intent.target.clone(),
            "--scope".to_owned(),
            scope.to_owned(),
            "--directory".to_owned(),
            root.to_string_lossy().into_owned(),
        ];
        let quoted = format!("'{}'", root.to_string_lossy().replace('\'', "'\"'\"'"));
        let command = format!(
            "lwiki skill {} --target {} --scope {} --directory {}",
            intent.action, intent.target, scope, quoted
        );
        json!({"action":intent.action,"target":intent.target,"scope":scope,"directory":root,
            "retry_argv":argv,"retry_command":command})
    }
    pub(super) fn run_scoped(
        action: &str,
        target: &str,
        home: &Path,
        scope: &str,
        dry_run: bool,
        hooks: &mut Hooks,
    ) -> Result<Value> {
        let selected = layout(target)?;
        let home = filesystem::acquire(home)?;
        if !home.missing.is_empty() {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "the selected user or project directory must already exist",
            ));
        }
        let absolute = home.held.path().to_path_buf();
        let destination = absolute.join(selected);
        let managed_path = absolute.join(root_relative(scope));
        let managed = home.append(root_relative(scope))?;
        let exists = managed.missing.is_empty();
        let mut guards = Guards::default();
        guards.directories.push(home.held.clone());
        let _lock = if exists {
            Some(managed.held.lock()?)
        } else {
            None
        };
        let observed = if exists {
            bootstrap_observation(&managed.held, &mut guards)?
        } else {
            None
        };
        let (state, current_stamp) = if let Some(pair) = observed {
            pair
        } else {
            // Virtual state is used only for conflict checks and previews before bootstrap.
            let root_path = managed_path.clone();
            (
                State {
                    scope: scope.into(),
                    root: root_path,
                    root_identity: (0, 0),
                    installation_id: String::new(),
                    generation: 0,
                    operation_id: None,
                    active: None,
                    hosts: BTreeSet::new(),
                    links: BTreeMap::new(),
                },
                FileStamp {
                    identity: (0, 0),
                    member: member(&[]),
                },
            )
        };
        let mut pending = None;
        let pending_dir = if exists {
            managed.held.child("pending")?
        } else {
            None
        };
        if let Some(dir) = &pending_dir {
            guards.directories.push(dir.clone());
            if let Some(intent) = record::<Intent>(dir, "intent.json", INTENT_SCHEMA, &mut guards)?
            {
                validate_intent(&intent, &managed.held)?;
                pending = Some(load_pending(dir, intent, &state, &mut guards)?);
            } else {
                for name in dir.entries(2)? {
                    if name != "write-intent" {
                        return Err(conflict(
                            "unknown content in an incomplete pending installation",
                        ));
                    }
                }
            }
        }
        let committed = if let Some(p) = &pending {
            if state.operation_id.as_deref() == Some(&p.intent.id) {
                let after = after_state(p)?;
                if state != after || p.manifest.as_ref().is_some_and(|s| s != &current_stamp) {
                    return Err(conflict(
                        "committed skill state does not match its pending operation",
                    ));
                }
                true
            } else {
                if state != p.intent.before || current_stamp != p.intent.before_manifest {
                    return Err(conflict(
                        "pending skill operation belongs to a replaced current state",
                    ));
                }
                false
            }
        } else {
            false
        };
        let observation = (|| -> Result<()> {
            validate_links(&home, &state, pending.as_ref(), committed, &mut guards)?;
            if exists && observed_is_current(&state) {
                validate_versions(
                    &managed.held,
                    &state,
                    pending.as_ref(),
                    committed,
                    &mut guards,
                )?;
            }
            guards.verify()
        })();
        if let Err(mut error) = observation {
            if let Some(pending) = &pending {
                error.details = json!({"installation":{"destination":destination,"scope":scope,"applied":committed,"durable":committed,"cleanup_pending":committed,"pending_path":pending.directory.path(),"operation":pending.intent.action,"target":pending.intent.target,"operation_id":pending.intent.id,"preserved":true},"cause":error.details});
                if committed {
                    error
                        .message
                        .push_str("; the change is committed, and cleanup remains pending")
                }
            }
            return Err(error);
        }
        let installed = state.hosts.contains(target);
        let retry = pending
            .as_ref()
            .map(|p| retry_details(&p.intent, &absolute, scope));
        if action == "status" {
            let message = if let Some(retry) = &retry {
                format!(
                    "An interrupted change remains. Retry: {}",
                    retry["retry_command"].as_str().unwrap()
                )
            } else if pending_dir.is_some() {
                "An incomplete operation record remains. Retry the command that was interrupted, using the same scope and directory.".to_owned()
            } else if installed {
                "The skill is installed. Restart your agent to load it.".to_owned()
            } else {
                "The skill is not installed for this agent.".to_owned()
            };
            hooks.fire(Phase::Return, &destination)?;
            guards.verify()?;
            return Ok(
                json!({"target":target,"destination":destination,"scope":scope,"installed":installed,"pending":pending_dir.is_some(),"cleanup_pending":committed,"version":state.active.as_ref().map(|p|&p.version),"available_version":env!("CARGO_PKG_VERSION"),"package_checksum":state.active.as_ref().map(|p|&p.key),"changed":false,"created":[],"applied":installed,"durable":observed_is_current(&state) && (!pending_dir.is_some() || committed),"dry_run":dry_run,"failure":null,"operations":[],"message":message,"pending_operation":retry}),
            );
        }
        if let Some(p) = &pending
            && (p.intent.action != action || p.intent.target != target)
        {
            let retry = retry_details(&p.intent, &absolute, scope);
            let mut error = conflict(format!(
                "finish the interrupted command first: {}",
                retry["retry_command"].as_str().unwrap()
            ));
            error.details = json!({"pending_operation":retry});
            return Err(error);
        }
        #[cfg(test)]
        let files = hooks
            .package
            .clone()
            .map_or_else(|| skill_export::package("shared"), Ok)?;
        #[cfg(not(test))]
        let files = skill_export::package("shared")?;
        let proposal = if pending.is_none() {
            make_intent(action, target, &state, current_stamp.clone(), &files)?
        } else {
            None
        };
        if pending.is_none() && proposal.is_none() {
            if pending_dir
                .as_ref()
                .is_some_and(|d| d.entries(2).is_ok_and(|e| !e.is_empty()))
            {
                return Err(conflict(
                    "an incomplete pending record remains; retry its original command",
                ));
            }
            hooks.fire(Phase::Return, &destination)?;
            guards.verify()?;
            let cleared = pending_dir.is_some() && !dry_run;
            if let Some(directory) = &pending_dir
                && !dry_run
            {
                managed.held.remove_directory("pending", directory)?;
            }
            return Ok(
                json!({"target":target,"destination":destination,"scope":scope,"installed":installed,"changed":cleared,"created":[],"applied":true,"durable":exists,"dry_run":dry_run,"failure":null,"operations":[],"message":if action=="remove"{"The skill is already removed for this agent."}else{"The skill is already up to date. Restart your agent to load it."}}),
            );
        }
        let planned = pending
            .as_ref()
            .map(|p| &p.intent)
            .or(proposal.as_ref())
            .expect("operation");
        if encoded(INTENT_SCHEMA, planned)?.len() > MAX_RECORD - 4096 {
            return Err(conflict("skill pending record exceeds its size limit"));
        }
        let operations = paths(&absolute, planned);
        let affected_hosts: Vec<_> = planned
            .before
            .hosts
            .union(&planned.after.hosts)
            .cloned()
            .collect();
        if let Some(dir) = &pending_dir
            && pending.is_none()
            && let Some(size) = dir.file_size("write-intent", MAX_RECORD)?
        {
            let (data, g) = dir.read_file("write-intent", size)?;
            if !encoded(INTENT_SCHEMA, planned)?.starts_with(&data) {
                return Err(conflict(
                    "incomplete pending intent belongs to another command",
                ));
            }
            g.verify_binding()?;
        }
        if dry_run {
            hooks.fire(Phase::Return, &destination)?;
            guards.verify()?;
            return Ok(
                json!({"target":target,"destination":destination,"scope":scope,"installed":installed,"changed":true,"created":[],"applied":false,"durable":false,"dry_run":true,"failure":null,"operations":operations,"affected_hosts":affected_hosts,"message":"Preview only. No files or links were changed."}),
            );
        }
        let mut created = Vec::new();
        let mut published = Vec::new();
        let mut applied = committed;
        let mut durable = committed;
        let mut cleanup_pending = false;
        let result = (|| -> Result<()> {
            guards.verify()?;
            let (root, _bootstrap_lock) = if exists {
                (managed.held.clone(), None)
            } else {
                let (root, held_lock) = bootstrap_root(&home, scope, &mut created, hooks)?;
                (root, Some(held_lock))
            };
            let mut boot_guards = Guards::default();
            let (actual_state, actual_stamp) = bootstrap_observation(&root, &mut boot_guards)?
                .ok_or_else(|| conflict("current skill state disappeared"))?;
            boot_guards.verify()?;
            hooks.fire(Phase::Bootstrap, root.path())?;
            if pending.is_none() {
                let intent = make_intent(action, target, &actual_state, actual_stamp, &files)?
                    .ok_or_else(|| conflict("installation state changed after preview"))?;
                let data = encoded(INTENT_SCHEMA, &intent)?;
                if data.len() > MAX_RECORD {
                    return Err(conflict("skill pending record exceeds its size limit"));
                }
                let dir = directory(&root, "pending", &mut created)?;
                publish_file(
                    &dir,
                    "intent.json",
                    &data,
                    &dir,
                    "write-intent",
                    &mut created,
                )?;
                pending = Some(Pending {
                    directory: dir,
                    intent,
                    prepared: BTreeMap::new(),
                    manifest: None,
                });
            }
            let p = pending.as_mut().expect("pending");
            hooks.fire(Phase::Intent, p.directory.path())?;
            if !committed {
                prepare_payload(&root, p, &mut created)?;
                hooks.fire(Phase::Payload, root.path())?;
                let mut payload_guards = Guards::default();
                validate_versions(&root, &p.intent.before, Some(p), false, &mut payload_guards)?;
                payload_guards.verify()?;
                prepare_links(p, &home, &mut created, hooks)?;
                apply_links(p, &root, &home, &mut created, &mut published, hooks)?;
                applied = true;
                let after = prepare_manifest(p, &mut created)?;
                let mut verify = Guards::default();
                validate_links(&home, &after, Some(p), true, &mut verify)?;
                validate_versions(&root, &after, Some(p), true, &mut verify)?;
                verify.verify()?;
                commit(&root, p, &after, hooks)?;
                durable = true;
                hooks.fire(Phase::Complete, p.directory.path())?;
            }
            let after = after_state(p)?;
            cleanup_pending = true;
            cleanup(&root, p, &after, hooks)?;
            cleanup_pending = false;
            hooks.fire(Phase::Return, &destination)?;
            let mut final_guards = Guards::default();
            let (final_state, _) = bootstrap_observation(&root, &mut final_guards)?
                .ok_or_else(|| conflict("current skill state disappeared"))?;
            validate_links(&home, &final_state, None, false, &mut final_guards)?;
            validate_versions(&root, &final_state, None, false, &mut final_guards)?;
            final_guards.verify()
        })();
        let failure = result.err();
        if failure.is_some() && durable {
            cleanup_pending = pending
                .as_ref()
                .is_some_and(|p| p.directory.verify_binding().is_ok());
        }
        let after = pending
            .as_ref()
            .map(|p| &p.intent.after)
            .or(proposal.as_ref().map(|p| &p.after))
            .expect("operation");
        let shared = action == "remove" && targets(after, &managed_path)?.contains_key(selected);
        let retry = pending
            .as_ref()
            .filter(|p| failure.is_some() && p.directory.verify_binding().is_ok())
            .map(|p| retry_details(&p.intent, &absolute, scope));
        Ok(
            json!({"target":target,"destination":destination,"scope":scope,"installed":if applied{after.hosts.contains(target)}else{installed},"version":after.active.as_ref().map(|p|&p.version),"available_version":env!("CARGO_PKG_VERSION"),"package_checksum":after.active.as_ref().map(|p|&p.key),"changed":!created.is_empty()||!published.is_empty()||applied,"created":created,"published_paths":published,"applied":applied,"durable":durable,"cleanup_pending":cleanup_pending,"pending_operation":retry,"dry_run":false,"failure":failure,"operations":operations,"affected_hosts":affected_hosts,"shared_link_retained":shared,"payloads_retained":false,"message":if failure.is_some()&&cleanup_pending{"The change was saved, but cleanup did not finish. Changed files were preserved; retry the same command after resolving the reported conflict."}else if failure.is_some()&&durable{"The change was saved, but its final check failed. Run skill status and inspect the reported conflict."}else if failure.is_some(){"The change did not finish. Your files and pending records were preserved; retry the same command after resolving the reported conflict."}else if action=="remove"&&shared{"This agent was removed from the installation. The shared link remains for the other selected agent."}else if action=="remove"{"The selected skill was removed. Your wiki and agent settings were preserved."}else if action=="update"{"The shared skill was updated for the installed agents. Restart them to load it."}else{"The skill is installed. Restart your agent to load it."}}),
        )
    }
    fn observed_is_current(state: &State) -> bool {
        !state.installation_id.is_empty()
    }
    #[cfg(test)]
    fn run(
        action: &str,
        target: &str,
        root: &Path,
        dry_run: bool,
        hooks: &mut Hooks,
    ) -> Result<Value> {
        run_scoped(action, target, root, "user", dry_run, hooks)
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{fs, os::unix::fs::symlink, time::SystemTime};
        fn fixture() -> tempfile::TempDir {
            tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
        }
        fn checked(value: Value) -> Value {
            assert!(value["failure"].is_null(), "{value:#}");
            assert_eq!(value["applied"], true, "{value:#}");
            assert_eq!(value["durable"], true, "{value:#}");
            value
        }
        fn call(action: &str, target: &str, root: &Path) -> Value {
            checked(run(action, target, root, false, &mut Hooks::default()).unwrap())
        }
        fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, Option<PathBuf>, SystemTime)> {
            fn walk(
                base: &Path,
                path: &Path,
                out: &mut BTreeMap<PathBuf, (Vec<u8>, Option<PathBuf>, SystemTime)>,
            ) {
                let m = fs::symlink_metadata(path).unwrap();
                out.insert(
                    path.strip_prefix(base).unwrap().into(),
                    (
                        if m.is_file() {
                            fs::read(path).unwrap()
                        } else {
                            vec![]
                        },
                        if m.file_type().is_symlink() {
                            Some(fs::read_link(path).unwrap())
                        } else {
                            None
                        },
                        m.modified().unwrap(),
                    ),
                );
                if m.is_dir() {
                    for entry in fs::read_dir(path).unwrap() {
                        walk(base, &entry.unwrap().path(), out)
                    }
                }
            }
            let mut out = BTreeMap::new();
            walk(root, root, &mut out);
            out
        }
        fn interrupt(phase: Phase) -> Hooks {
            Hooks {
                callback: Some(Box::new(move |at, _| {
                    if at == phase {
                        Err(WikiError::new(ErrorCode::Internal, "test interruption"))
                    } else {
                        Ok(())
                    }
                })),
                ..Hooks::default()
            }
        }
        fn next_package() -> BTreeMap<String, Vec<u8>> {
            let mut files = skill_export::package("shared").unwrap();
            files
                .get_mut("SKILL.md")
                .unwrap()
                .extend_from_slice(b"\nTest package update.\n");
            let mut manifest: Value = serde_json::from_slice(&files["manifest.json"]).unwrap();
            manifest["version"] = json!("9.0.0-test");
            manifest["files"]["SKILL.md"] = json!(checksum(&files["SKILL.md"]));
            files.insert(
                "manifest.json".into(),
                serde_json::to_vec(&manifest).unwrap(),
            );
            files
        }
        fn pending(root: &Path) -> PathBuf {
            root.join(ROOT).join("pending")
        }
        fn state(root: &Path) -> State {
            decoded(
                STATE_SCHEMA,
                &fs::read(root.join(ROOT).join("current.json")).unwrap(),
            )
            .unwrap()
        }
        fn assert_conflict(action: &str, target: &str, root: &Path) {
            let before = tree(root);
            assert_eq!(
                run(action, target, root, false, &mut Hooks::default())
                    .unwrap_err()
                    .code,
                ErrorCode::ContentConflict
            );
            assert_eq!(tree(root), before)
        }

        #[test]
        fn interrupted_status_and_other_actions_keep_the_original_scope_and_directory() {
            for scope in ["user", "project"] {
                let temp = fixture();
                let root = temp.path().join("my 'notes");
                fs::create_dir(&root).unwrap();
                let partial = run_scoped(
                    "install",
                    "codex",
                    &root,
                    scope,
                    false,
                    &mut interrupt(Phase::Intent),
                )
                .unwrap();
                assert!(!partial["failure"].is_null());
                let before = tree(&root);
                let status = run_scoped(
                    "status",
                    "cursor",
                    &root,
                    scope,
                    false,
                    &mut Hooks::default(),
                )
                .unwrap();
                let retry = &status["pending_operation"];
                assert_eq!(
                    retry["retry_argv"],
                    json!([
                        "lwiki",
                        "skill",
                        "install",
                        "--target",
                        "codex",
                        "--scope",
                        scope,
                        "--directory",
                        root
                    ])
                );
                let command = retry["retry_command"].as_str().unwrap();
                assert!(command.contains("'\"'\"'"));
                assert!(status["message"].as_str().unwrap().contains(command));
                let conflict = run_scoped(
                    "remove",
                    "cursor",
                    &root,
                    scope,
                    false,
                    &mut Hooks::default(),
                )
                .unwrap_err();
                assert_eq!(conflict.details["pending_operation"], *retry);
                assert!(conflict.message.contains(command));
                assert_eq!(tree(&root), before);
                checked(
                    run_scoped(
                        "install",
                        "codex",
                        &root,
                        scope,
                        false,
                        &mut Hooks::default(),
                    )
                    .unwrap(),
                );
                let status = run_scoped(
                    "status",
                    "codex",
                    &root,
                    scope,
                    false,
                    &mut Hooks::default(),
                )
                .unwrap();
                assert_eq!(status["installed"], true);
                assert_eq!(status["pending"], false);
            }
        }
        #[test]
        fn three_hosts_share_one_payload_and_removal_preserves_other_agents() {
            let home = fixture();
            for target in ["codex", "cursor", "claude-code"] {
                call("install", target, home.path());
            }
            let agents = home.path().join(LAYOUTS[0]);
            let claude = home.path().join(LAYOUTS[1]);
            assert_eq!(
                agents.canonicalize().unwrap(),
                claude.canonicalize().unwrap()
            );
            assert_eq!(
                fs::read_dir(home.path().join(ROOT).join("versions"))
                    .unwrap()
                    .count(),
                1
            );
            let manifest: Value =
                serde_json::from_slice(&fs::read(agents.join("manifest.json")).unwrap()).unwrap();
            assert_eq!(manifest["target"], "shared");
            for target in ["codex", "cursor", "claude-code"] {
                assert_eq!(call("status", target, home.path())["installed"], true)
            }
            assert_eq!(
                call("remove", "codex", home.path())["shared_link_retained"],
                true
            );
            assert!(agents.exists());
            assert_eq!(
                run("status", "codex", home.path(), false, &mut Hooks::default()).unwrap()["installed"],
                false
            );
            call("remove", "claude-code", home.path());
            assert!(!claude.exists());
            assert!(agents.exists());
            call("remove", "cursor", home.path());
            assert!(!agents.exists());
            assert_eq!(
                fs::read_dir(home.path().join(ROOT).join("versions"))
                    .unwrap()
                    .count(),
                0
            );
            let before = tree(home.path());
            assert_eq!(call("remove", "cursor", home.path())["changed"], false);
            assert_eq!(tree(home.path()), before);
            call("install", "cursor", home.path());
            assert!(agents.exists());
            assert!(!pending(home.path()).exists());
        }
        #[test]
        fn preview_and_repeated_operations_write_nothing() {
            let home = fixture();
            let before = tree(home.path());
            let preview =
                run("install", "codex", home.path(), true, &mut Hooks::default()).unwrap();
            assert_eq!(preview["applied"], false);
            assert_eq!(preview["durable"], false);
            assert!(!preview["operations"].as_array().unwrap().is_empty());
            assert_eq!(tree(home.path()), before);
            call("install", "codex", home.path());
            let before = tree(home.path());
            for action in ["install", "update", "status"] {
                assert_eq!(call(action, "codex", home.path())["changed"], false)
            }
            run("remove", "codex", home.path(), true, &mut Hooks::default()).unwrap();
            assert_eq!(tree(home.path()), before);
            assert!(!home.path().join("AGENTS.md").exists());
            assert!(!home.path().join("CLAUDE.md").exists());
        }
        #[test]
        fn complete_bootstrap_publication_retries_and_unowned_prefixes_conflict() {
            let home = fixture();
            let partial = run(
                "install",
                "codex",
                home.path(),
                false,
                &mut interrupt(Phase::BootstrapStage),
            )
            .unwrap();
            assert!(!partial["failure"].is_null());
            assert!(!home.path().join(ROOT).exists());
            let parent = home.path().join(".local/share/lwiki");
            let orphan = fs::read_dir(&parent)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let retained = tree(&orphan);
            call("install", "codex", home.path());
            assert_eq!(tree(&orphan), retained);
            for prefix in [None, Some(b"lwiki.skill-install-root.v2".as_slice())] {
                let home = fixture();
                fs::create_dir_all(home.path().join(ROOT)).unwrap();
                if let Some(prefix) = prefix {
                    fs::write(home.path().join(ROOT).join("write-owner"), prefix).unwrap();
                }
                let before = tree(home.path());
                for dry in [true, false] {
                    assert_eq!(
                        run("install", "codex", home.path(), dry, &mut Hooks::default())
                            .unwrap_err()
                            .code,
                        ErrorCode::ContentConflict
                    );
                    assert_eq!(tree(home.path()), before);
                }
            }
        }
        #[test]
        fn unknown_content_conflicts_without_creation_or_adoption() {
            for relative in [
                ".agents/skills/llm-wiki/user.md",
                ".local/share/lwiki/skills/user.md",
            ] {
                let home = fixture();
                let file = home.path().join(relative);
                fs::create_dir_all(file.parent().unwrap()).unwrap();
                fs::write(&file, b"Keep my work").unwrap();
                let before = tree(home.path());
                for dry in [false, true] {
                    assert_eq!(
                        run("install", "codex", home.path(), dry, &mut Hooks::default())
                            .unwrap_err()
                            .code,
                        ErrorCode::ContentConflict
                    );
                    assert_eq!(tree(home.path()), before)
                }
            }
        }
        #[test]
        fn modified_owned_files_and_manifests_are_preserved() {
            for altered in ["package", "current", "intent", "extra"] {
                let home = fixture();
                call("install", "codex", home.path());
                let package = home.path().join(LAYOUTS[0]).canonicalize().unwrap();
                let path = match altered {
                    "package" => package.join("SKILL.md"),
                    "current" => home.path().join(ROOT).join("current.json"),
                    "intent" => {
                        run(
                            "install",
                            "claude-code",
                            home.path(),
                            false,
                            &mut interrupt(Phase::Intent),
                        )
                        .unwrap();
                        pending(home.path()).join("intent.json")
                    }
                    _ => package.join("user.md"),
                };
                fs::write(path, b"Keep my edited bytes").unwrap();
                for action in ["install", "update", "remove", "status"] {
                    assert_conflict(action, "codex", home.path())
                }
            }
        }
        #[test]
        fn owned_link_substitution_is_rejected_even_with_the_same_target() {
            let home = fixture();
            call("install", "codex", home.path());
            let link = home.path().join(LAYOUTS[0]);
            let target = fs::read_link(&link).unwrap();
            fs::rename(&link, home.path().join("original-link")).unwrap();
            symlink(target, &link).unwrap();
            for action in ["status", "update", "remove"] {
                assert_conflict(action, "codex", home.path())
            }
        }
        #[test]
        fn interruption_at_publication_boundaries_retries_without_history() {
            for phase in [
                Phase::Bootstrap,
                Phase::Intent,
                Phase::Payload,
                Phase::LinkCreate,
                Phase::LinkStamp,
                Phase::LinkPublished,
                Phase::Manifest,
                Phase::Complete,
                Phase::Cleanup,
                Phase::CleanupRecord,
                Phase::CleanupState,
                Phase::Return,
            ] {
                let home = fixture();
                let partial = run(
                    "install",
                    "codex",
                    home.path(),
                    false,
                    &mut interrupt(phase),
                )
                .unwrap();
                assert!(!partial["failure"].is_null(), "phase {}", phase as usize);
                let committed = matches!(
                    phase,
                    Phase::Complete
                        | Phase::Cleanup
                        | Phase::CleanupRecord
                        | Phase::CleanupState
                        | Phase::Return
                );
                assert_eq!(partial["durable"], committed, "{partial:#}");
                call("install", "codex", home.path());
                call("status", "codex", home.path());
                assert_eq!(
                    fs::read_dir(home.path().join(ROOT).join("versions"))
                        .unwrap()
                        .count(),
                    1
                );
                assert!(!pending(home.path()).exists());
            }
        }
        #[test]
        fn repeated_creation_interruptions_do_not_fill_an_attempt_quota() {
            let home = fixture();
            for _ in 0..8 {
                let result = run(
                    "install",
                    "codex",
                    home.path(),
                    false,
                    &mut interrupt(Phase::LinkCreate),
                )
                .unwrap();
                assert!(!result["failure"].is_null());
                assert_eq!(fs::read_dir(pending(home.path())).unwrap().count(), 2)
            }
            call("install", "codex", home.path());
            assert!(!pending(home.path()).exists());
        }
        #[test]
        fn partial_owned_file_stage_retries_and_unknown_stage_bytes_are_preserved() {
            for foreign in [false, true] {
                let home = fixture();
                run(
                    "install",
                    "codex",
                    home.path(),
                    false,
                    &mut interrupt(Phase::Intent),
                )
                .unwrap();
                let intent: Intent = decoded(
                    INTENT_SCHEMA,
                    &fs::read(pending(home.path()).join("intent.json")).unwrap(),
                )
                .unwrap();
                let file = intent.payload.as_ref().unwrap().values().next().unwrap();
                let stage = pending(home.path()).join("file-0");
                fs::write(
                    &stage,
                    if foreign {
                        b"Foreign bytes".as_slice()
                    } else {
                        &file[..file.len() / 2]
                    },
                )
                .unwrap();
                let before = tree(home.path());
                if foreign {
                    assert_eq!(
                        run("install", "codex", home.path(), true, &mut Hooks::default())
                            .unwrap_err()
                            .code,
                        ErrorCode::ContentConflict
                    );
                    assert_eq!(tree(home.path()), before);
                    assert_conflict("install", "codex", home.path())
                } else {
                    call("install", "codex", home.path());
                    assert!(!pending(home.path()).exists());
                    assert_eq!(
                        fs::read(home.path().join(LAYOUTS[0]).join("SKILL.md")).unwrap(),
                        intent.payload.unwrap()["SKILL.md"]
                    )
                }
            }
        }
        #[test]
        fn prepared_and_published_same_target_replacements_are_never_adopted() {
            for phase in [Phase::LinkStamp, Phase::LinkPublished] {
                let home = fixture();
                run(
                    "install",
                    "codex",
                    home.path(),
                    false,
                    &mut interrupt(phase),
                )
                .unwrap();
                let link = if phase == Phase::LinkStamp {
                    pending(home.path()).join("link-0")
                } else {
                    home.path().join(LAYOUTS[0])
                };
                let target = fs::read_link(&link).unwrap();
                fs::rename(&link, home.path().join("saved-link")).unwrap();
                symlink(target, &link).unwrap();
                assert_conflict("install", "codex", home.path());
            }
        }
        #[test]
        fn resumed_multi_link_update_reports_publication_before_second_link_failure() {
            for scope in ["user", "project"] {
                let root = fixture();
                for host in ["codex", "cursor", "claude-code"] {
                    checked(
                        run_scoped(
                            "install",
                            host,
                            root.path(),
                            scope,
                            false,
                            &mut Hooks::default(),
                        )
                        .unwrap(),
                    );
                }
                let managed = root.path().join(root_relative(scope));
                let before_manifest = fs::read(managed.join("current.json")).unwrap();
                let before_links: Vec<_> = LAYOUTS
                    .iter()
                    .map(|path| fs::read_link(root.path().join(path)).unwrap())
                    .collect();
                let files = next_package();
                let mut hooks = interrupt(Phase::Link);
                hooks.package = Some(files.clone());
                let staged =
                    run_scoped("update", "codex", root.path(), scope, false, &mut hooks).unwrap();
                assert!(!staged["failure"].is_null());
                assert_eq!(staged["published_paths"], json!([]));
                for (index, path) in LAYOUTS.iter().enumerate() {
                    assert_eq!(
                        fs::read_link(root.path().join(path)).unwrap(),
                        before_links[index]
                    );
                    assert!(managed.join(format!("pending/stamp-{index}.json")).exists());
                }

                let second_link = root.path().join(LAYOUTS[1]);
                let mut hooks = Hooks {
                    package: Some(files.clone()),
                    callback: Some(Box::new(move |phase, path| {
                        if phase == Phase::Link && path == second_link.as_path() {
                            Err(WikiError::new(
                                ErrorCode::Internal,
                                "test second-link interruption",
                            ))
                        } else {
                            Ok(())
                        }
                    })),
                };
                let partial =
                    run_scoped("update", "codex", root.path(), scope, false, &mut hooks).unwrap();
                assert!(!partial["failure"].is_null());
                assert_eq!(partial["created"], json!([]));
                assert_eq!(
                    partial["published_paths"],
                    json!([root.path().join(LAYOUTS[0])])
                );
                assert_eq!(partial["changed"], true);
                assert_eq!(partial["applied"], false);
                assert_eq!(partial["durable"], false);
                assert_eq!(
                    fs::read(managed.join("current.json")).unwrap(),
                    before_manifest
                );
                assert_ne!(
                    fs::read_link(root.path().join(LAYOUTS[0])).unwrap(),
                    before_links[0]
                );
                assert_eq!(
                    fs::read_link(root.path().join(LAYOUTS[1])).unwrap(),
                    before_links[1]
                );
                assert_eq!(
                    partial["pending_operation"]["retry_argv"],
                    json!([
                        "lwiki",
                        "skill",
                        "update",
                        "--target",
                        "codex",
                        "--scope",
                        scope,
                        "--directory",
                        root.path()
                    ])
                );

                let mut hooks = Hooks {
                    package: Some(files.clone()),
                    ..Hooks::default()
                };
                checked(
                    run_scoped("update", "codex", root.path(), scope, false, &mut hooks).unwrap(),
                );
                assert_eq!(
                    fs::read_link(root.path().join(LAYOUTS[0])).unwrap(),
                    fs::read_link(root.path().join(LAYOUTS[1])).unwrap()
                );
                assert_eq!(
                    fs::read(root.path().join(LAYOUTS[1]).join("SKILL.md")).unwrap(),
                    files["SKILL.md"]
                );
                assert!(!managed.join("pending").exists());
            }
        }
        #[test]
        fn shared_update_and_partial_cleanup_retry_keep_current_state_bounded() {
            for phase in [
                Phase::LinkPublished,
                Phase::Complete,
                Phase::CleanupFile,
                Phase::CleanupRecord,
                Phase::CleanupState,
            ] {
                let home = fixture();
                for host in ["codex", "cursor", "claude-code"] {
                    call("install", host, home.path());
                }
                let old = fs::read_link(home.path().join(LAYOUTS[0])).unwrap();
                let files = next_package();
                let mut hooks = interrupt(phase);
                hooks.package = Some(files.clone());
                let result = run("update", "codex", home.path(), false, &mut hooks).unwrap();
                assert!(!result["failure"].is_null());
                let mut hooks = Hooks {
                    package: Some(files.clone()),
                    ..Hooks::default()
                };
                let result =
                    checked(run("update", "codex", home.path(), false, &mut hooks).unwrap());
                assert_eq!(result["affected_hosts"].as_array().unwrap().len(), 3);
                assert_ne!(fs::read_link(home.path().join(LAYOUTS[0])).unwrap(), old);
                assert_eq!(
                    fs::read_link(home.path().join(LAYOUTS[0])).unwrap(),
                    fs::read_link(home.path().join(LAYOUTS[1])).unwrap()
                );
                assert_eq!(
                    fs::read(home.path().join(LAYOUTS[0]).join("SKILL.md")).unwrap(),
                    files["SKILL.md"]
                );
                assert!(!old.exists());
                assert_eq!(
                    fs::read_dir(home.path().join(ROOT).join("versions"))
                        .unwrap()
                        .count(),
                    1
                );
                assert!(!pending(home.path()).exists());
            }
        }
        #[test]
        fn committed_cleanup_conflict_reports_saved_state_and_preserves_foreign_files() {
            let home = fixture();
            call("install", "codex", home.path());
            let old = fs::read_link(home.path().join(LAYOUTS[0])).unwrap();
            let old_for_hook = old.clone();
            let files = next_package();
            let mut hooks = Hooks {
                package: Some(files.clone()),
                callback: Some(Box::new(move |phase, _| {
                    if phase == Phase::Cleanup {
                        fs::write(old_for_hook.join("user.md"), b"Preserve this file").unwrap();
                    }
                    Ok(())
                })),
            };
            let value = run("update", "codex", home.path(), false, &mut hooks).unwrap();
            assert_eq!(value["applied"], true);
            assert_eq!(value["durable"], true);
            assert_eq!(value["cleanup_pending"], true);
            assert_eq!(value["failure"]["code"], "CONTENT_CONFLICT");
            assert_eq!(
                fs::read(old.join("user.md")).unwrap(),
                b"Preserve this file"
            );
            let before = tree(home.path());
            let error = run(
                "update",
                "codex",
                home.path(),
                false,
                &mut Hooks {
                    package: Some(files.clone()),
                    ..Hooks::default()
                },
            )
            .unwrap_err();
            assert_eq!(error.details["installation"]["durable"], true);
            assert_eq!(tree(home.path()), before);
            fs::rename(old.join("user.md"), home.path().join("saved-user.md")).unwrap();
            checked(
                run(
                    "update",
                    "codex",
                    home.path(),
                    false,
                    &mut Hooks {
                        package: Some(files),
                        ..Hooks::default()
                    },
                )
                .unwrap(),
            );
            assert_eq!(
                fs::read(home.path().join("saved-user.md")).unwrap(),
                b"Preserve this file"
            );
            assert!(!old.exists());
        }
        #[test]
        fn one_hundred_changes_have_a_fixed_filesystem_bound() {
            let home = fixture();
            for host in ["codex", "cursor", "claude-code"] {
                call("install", host, home.path());
            }
            let a = skill_export::package("shared").unwrap();
            let b = next_package();
            let mut largest = 0;
            for index in 0..100 {
                let mut hooks = Hooks {
                    package: Some(if index % 2 == 0 { b.clone() } else { a.clone() }),
                    ..Hooks::default()
                };
                checked(run("update", "codex", home.path(), false, &mut hooks).unwrap());
                assert!(!pending(home.path()).exists());
                assert_eq!(
                    fs::read_dir(home.path().join(ROOT).join("versions"))
                        .unwrap()
                        .count(),
                    1
                );
                let control = home.path().join(ROOT);
                let count = tree(&control).len();
                largest = largest.max(count);
                assert!(count <= 15, "{count}");
                assert!(fs::metadata(control.join("current.json")).unwrap().len() < 16 * 1024);
            }
            assert!(largest <= 15);
            assert_eq!(state(home.path()).generation, 103);
        }
        #[test]
        fn substitution_between_plan_and_mutation_preserves_user_link() {
            let home = fixture();
            call("install", "codex", home.path());
            let link = home.path().join(LAYOUTS[0]);
            let sentinel = home.path().join("sentinel");
            fs::write(&sentinel, b"Keep outside bytes").unwrap();
            let for_hook = sentinel.clone();
            let mut hooks = Hooks {
                callback: Some(Box::new(move |phase, path| {
                    if phase == Phase::Link {
                        fs::rename(path, path.with_file_name("original-link")).unwrap();
                        symlink(&for_hook, path).unwrap();
                    }
                    Ok(())
                })),
                ..Hooks::default()
            };
            let partial = run("remove", "codex", home.path(), false, &mut hooks).unwrap();
            assert_eq!(partial["failure"]["code"], "CONTENT_CONFLICT");
            assert_eq!(partial["applied"], false);
            assert_eq!(fs::read_link(&link).unwrap(), sentinel);
            assert_eq!(fs::read(&sentinel).unwrap(), b"Keep outside bytes");
            assert!(link.with_file_name("original-link").exists());
        }
        #[test]
        fn removal_retry_requires_the_displaced_old_identity() {
            let home = fixture();
            call("install", "codex", home.path());
            run(
                "remove",
                "codex",
                home.path(),
                false,
                &mut interrupt(Phase::LinkPublished),
            )
            .unwrap();
            fs::remove_file(pending(home.path()).join("link-0")).unwrap();
            assert_conflict("remove", "codex", home.path());
        }
        #[test]
        fn unsafe_ancestors_and_concurrent_installers_are_refused() {
            let home = fixture();
            let outside = fixture();
            fs::write(outside.path().join("sentinel"), b"Unchanged").unwrap();
            symlink(outside.path(), home.path().join(".agents")).unwrap();
            let before = tree(outside.path());
            assert_eq!(
                run(
                    "install",
                    "codex",
                    home.path(),
                    false,
                    &mut Hooks::default()
                )
                .unwrap_err()
                .code,
                ErrorCode::ContentConflict
            );
            assert_eq!(tree(outside.path()), before);
            let home = fixture();
            call("install", "codex", home.path());
            let root = filesystem::acquire(&home.path().join(ROOT)).unwrap().held;
            let _lock = root.lock().unwrap();
            let before = tree(home.path());
            assert_eq!(
                run(
                    "install",
                    "claude-code",
                    home.path(),
                    false,
                    &mut Hooks::default()
                )
                .unwrap_err()
                .code,
                ErrorCode::LockTimeout
            );
            assert_eq!(tree(home.path()), before);
        }
        #[test]
        fn project_layout_stays_inside_agents_discovery_and_preserves_instructions() {
            let project = fixture();
            fs::write(project.path().join("AGENTS.md"), b"My agent instructions").unwrap();
            fs::write(project.path().join("CLAUDE.md"), b"My Claude instructions").unwrap();
            let before = tree(project.path());
            let preview = run_scoped(
                "install",
                "codex",
                project.path(),
                "project",
                true,
                &mut Hooks::default(),
            )
            .unwrap();
            assert_eq!(preview["scope"], "project");
            assert_eq!(tree(project.path()), before);
            for target in ["codex", "cursor", "claude-code"] {
                checked(
                    run_scoped(
                        "install",
                        target,
                        project.path(),
                        "project",
                        false,
                        &mut Hooks::default(),
                    )
                    .unwrap(),
                );
            }
            let agents = project.path().join(LAYOUTS[0]);
            let claude = project.path().join(LAYOUTS[1]);
            let canonical = agents.canonicalize().unwrap();
            assert!(canonical.starts_with(project.path().join(".agents/skills")));
            assert_eq!(canonical, claude.canonicalize().unwrap());
            assert!(canonical.starts_with(project.path().join(".agents/skills/.lwiki/versions")));
            assert_eq!(
                fs::read(project.path().join("AGENTS.md")).unwrap(),
                b"My agent instructions"
            );
            assert_eq!(
                fs::read(project.path().join("CLAUDE.md")).unwrap(),
                b"My Claude instructions"
            );
            // This checks the realpath containment used by the pinned Cursor source.
            // It does not claim an authenticated host invocation or hidden-name filtering.
            assert!(
                canonical.starts_with(
                    project
                        .path()
                        .join(".agents/skills")
                        .canonicalize()
                        .unwrap()
                )
            );
            assert!(
                !canonical.starts_with(
                    project
                        .path()
                        .join(".claude/skills")
                        .canonicalize()
                        .unwrap()
                )
            );
        }
        #[test]
        fn moved_or_copied_scope_manifest_is_not_rebound() {
            let project = fixture();
            checked(
                run_scoped(
                    "install",
                    "codex",
                    project.path(),
                    "project",
                    false,
                    &mut Hooks::default(),
                )
                .unwrap(),
            );
            let moved = project.path().join("moved");
            fs::create_dir(&moved).unwrap();
            fs::rename(project.path().join(".agents"), moved.join(".agents")).unwrap();
            let before = tree(&moved);
            assert_eq!(
                run_scoped(
                    "status",
                    "codex",
                    &moved,
                    "project",
                    false,
                    &mut Hooks::default()
                )
                .unwrap_err()
                .code,
                ErrorCode::ContentConflict
            );
            assert_eq!(tree(&moved), before);
        }
    }
}
