//! Narrow isolated native builder transport. Parent owns every durable write.
use super::admission::DiscoveryAdmission;
use super::audit::export_digest as advance_digest;
use super::{builder, corrupt, exhausted, storage::GraphReader, *};
use crate::vault::{VaultFs, VaultRoot};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

const REQUEST_BYTES: usize = 4096;
const FRAME_BYTES: usize = 2 * 1024 * 1024;
const MAX_CHILD_MS: u64 = 120_000;
pub(crate) const CHILD_ARGUMENT: &str = "__semantic-builder-child-v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    root: PathBuf,
    input: GraphRef,
    input_fingerprint: Blake3Hash,
    remaining_ms: u64,
    admission: DiscoveryAdmission,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BuildResult {
    pub(crate) entry: Option<NodeId>,
    pub(crate) max_level: usize,
    pub(crate) node_count: NodeId,
    pub(crate) native_memory_lower_bound: usize,
    pub(crate) input_fingerprint: Blake3Hash,
    pub(crate) output_digest: Blake3Hash,
    pub(crate) resource_limit_kind: String,
    pub(crate) as_bytes: u64,
    pub(crate) admission_fingerprint: Blake3Hash,
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "frame",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Frame {
    Node(GraphNode),
    Complete(BuildResult),
    Failed { code: ErrorCode, message: String },
}

fn validate_admission(
    input: &GraphRef,
    fingerprint: &Blake3Hash,
    admission: &DiscoveryAdmission,
) -> Result<()> {
    admission.validate()?;
    if admission.node_count != input.node_count
        || admission.dimensions != input.dimensions
        || &admission.plan_hash != fingerprint
        || admission.limits.max_invocation_ms == 0
        || admission.limits.max_invocation_ms > MAX_CHILD_MS
    {
        return Err(corrupt(
            "semantic builder request differs from frozen admission",
        ));
    }
    Ok(())
}

fn io_error(_: impl std::fmt::Display) -> WikiError {
    corrupt("semantic builder transport failed")
}
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| exhausted("semantic builder process deadline exceeded"))
}
fn bounded_json(value: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    struct Output {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("frame exceeds bound"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Output {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut out, value).map_err(io_error)?;
    Ok(out.bytes)
}
fn write_frame(writer: &mut impl Write, value: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    let bytes = bounded_json(value, limit)?;
    writer
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .map_err(io_error)?;
    writer.write_all(&bytes).map_err(io_error)?;
    writer.flush().map_err(io_error)?;
    Ok(bytes)
}
fn read_frame(reader: &mut impl Read, limit: usize) -> Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    match reader.read(&mut length[..1]).map_err(io_error)? {
        0 => return Ok(None),
        _ => {}
    }
    reader.read_exact(&mut length[1..]).map_err(io_error)?;
    let size = u32::from_le_bytes(length) as usize;
    if size == 0 || size > limit {
        return Err(corrupt("semantic builder frame length exceeds bound"));
    }
    let mut bytes = vec![0u8; size];
    reader.read_exact(&mut bytes).map_err(io_error)?;
    Ok(Some(bytes))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn install_limit_using(
    as_bytes: u64,
    set: impl FnOnce(&libc::rlimit) -> i32,
    get: impl FnOnce(&mut libc::rlimit) -> i32,
) -> Result<()> {
    let limit = libc::rlimit {
        rlim_cur: as_bytes as libc::rlim_t,
        rlim_max: as_bytes as libc::rlim_t,
    };
    let mut observed: libc::rlimit = unsafe { std::mem::zeroed() };
    if set(&limit) != 0
        || get(&mut observed) != 0
        || observed.rlim_cur != limit.rlim_cur
        || observed.rlim_max != limit.rlim_max
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "semantic builder address-space limit installation/readback failed",
        ));
    }
    Ok(())
}
fn verify_limit(as_bytes: u64) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        install_limit_using(
            as_bytes,
            |limit| unsafe { libc::setrlimit(libc::RLIMIT_AS, limit) },
            |observed| unsafe { libc::getrlimit(libc::RLIMIT_AS, observed) },
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "semantic builder resource platform unsupported",
        ))
    }
}

/// Isolated maintenance only. Callback output remains private until success.
/// RLIMIT_AS is an address-space cap, not a qualified resident-memory guarantee.
pub(crate) fn build(
    fs: &VaultFs,
    input: &GraphRef,
    input_fingerprint: &Blake3Hash,
    admission: &DiscoveryAdmission,
    deadline: Instant,
    mut emit: impl FnMut(GraphNode) -> Result<()>,
) -> Result<BuildResult> {
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (fs, input, input_fingerprint, admission, deadline, &mut emit);
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "semantic builder resource platform unsupported",
        ));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if input.kind != GraphKind::StagingInput {
            return Err(corrupt("semantic builder requires staging input"));
        }
        validate_admission(input, input_fingerprint, admission)?;
        let deadline = deadline
            .min(Instant::now() + Duration::from_millis(admission.limits.max_invocation_ms));
        GraphReader::new(fs, input.clone())?;
        let executable = std::env::current_exe().map_err(io_error)?;
        let request = Request {
            root: fs.root().path().to_path_buf(),
            input: input.clone(),
            input_fingerprint: input_fingerprint.clone(),
            remaining_ms: remaining(deadline)?.as_millis().min(MAX_CHILD_MS as u128) as u64,
            admission: admission.clone(),
        };
        let request_bytes = bounded_json(&request, REQUEST_BYTES)?;
        let mut command = Command::new(executable);
        command
            .arg(CHILD_ARGUMENT)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| {
            WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "semantic isolated builder could not start with resource cap",
            )
        })?;
        let result = transport(
            &mut child,
            request_bytes,
            input,
            input_fingerprint,
            admission,
            deadline,
            &mut emit,
        );
        result
    }
}

fn transport(
    child: &mut Child,
    request_bytes: Vec<u8>,
    input: &GraphRef,
    fingerprint: &Blake3Hash,
    admission: &DiscoveryAdmission,
    deadline: Instant,
    emit: &mut impl FnMut(GraphNode) -> Result<()>,
) -> Result<BuildResult> {
    if let Err(error) = validate_admission(input, fingerprint, admission) {
        cleanup(child, Instant::now() + Duration::from_millis(500))?;
        return Err(error);
    }
    let admission_fingerprint = admission.fingerprint()?;
    let deadline =
        deadline.min(Instant::now() + Duration::from_millis(admission.limits.max_invocation_ms));
    let cancellation = Arc::new((Mutex::new(false), Condvar::new()));
    let signal = cancellation.clone();
    let pid = child.id();
    // Child remains owned/unreaped until this watchdog is cancelled; no PID reuse.
    let watchdog = thread::spawn(move || {
        let (lock, wake) = &*signal;
        let mut cancelled = lock.lock().unwrap();
        while !*cancelled {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let (next, _) = wake.wait_timeout(cancelled, left).unwrap();
            cancelled = next;
        }
        if !*cancelled {
            #[cfg(unix)]
            unsafe {
                if libc::kill(pid as libc::pid_t, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
        Ok::<(), std::io::Error>(())
    });
    let result = (|| {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| corrupt("semantic builder stdin unavailable"))?;
        nonblocking(&stdin)?;
        let mut wire = Vec::with_capacity(request_bytes.len() + 4);
        wire.extend_from_slice(&(request_bytes.len() as u32).to_le_bytes());
        wire.extend_from_slice(&request_bytes);
        write_deadline(&mut stdin, &wire, deadline)?;
        drop(stdin);
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| corrupt("semantic builder stdout unavailable"))?;
        nonblocking(&stdout)?;
        let mut next_id = 0;
        let mut digest = Blake3Hash::digest("lwiki-semantic-export-v1");
        let mut complete = None;
        loop {
            remaining(deadline)?;
            let bytes = read_deadline_frame(&mut stdout, FRAME_BYTES, deadline)?;
            let Some(bytes) = bytes else {
                break;
            };
            if complete.is_some() {
                return Err(corrupt("semantic builder output follows completion"));
            }
            match serde_json::from_slice::<Frame>(&bytes).map_err(io_error)? {
                Frame::Failed { code, message } => {
                    if message.len() > 4096 {
                        return Err(corrupt("builder failure frame exceeds bound"));
                    }
                    return Err(WikiError::new(code, message));
                }
                Frame::Node(node) => {
                    if node.id != next_id
                        || next_id >= input.node_count
                        || node.vector.len() != input.dimensions as usize
                        || node.neighbors.is_empty()
                        || node.neighbors.len() > MAX_LEVEL + 1
                    {
                        return Err(corrupt("semantic builder output node geometry invalid"));
                    }
                    if node.descriptor.owner != node.binding.owner
                        || node.descriptor.policy != node.binding.policy
                        || node.binding.tombstone
                        || node.binding.unit_count == 0
                    {
                        return Err(corrupt("semantic builder output identity differs"));
                    }
                    bounded_json(
                        &(&node.descriptor, &node.binding, &node.neighbors),
                        MAX_NODE_METADATA,
                    )?;
                    for (level, neighbors) in node.neighbors.iter().enumerate() {
                        let cap = CONNECTIVITY * if level == 0 { 2 } else { 1 };
                        if neighbors.len() > cap
                            || neighbors.iter().enumerate().any(|(i, n)| {
                                *n >= input.node_count
                                    || *n == node.id
                                    || neighbors[..i].contains(n)
                            })
                        {
                            return Err(corrupt("semantic builder output adjacency invalid"));
                        }
                    }
                    let norm = node
                        .vector
                        .iter()
                        .map(|v| f64::from(*v).powi(2))
                        .sum::<f64>();
                    if node.vector.iter().any(|v| !v.is_finite())
                        || !norm.is_finite()
                        || (norm - 1.).abs() > 1e-5
                    {
                        return Err(corrupt("semantic builder output vector invalid"));
                    }
                    digest = advance_digest(&digest, &node)?;
                    remaining(deadline)?;
                    emit(node)?;
                    remaining(deadline)?;
                    next_id += 1;
                }
                Frame::Complete(result) => {
                    if result.node_count != input.node_count
                        || next_id != input.node_count
                        || &result.input_fingerprint != fingerprint
                        || result.max_level > MAX_LEVEL
                        || result.entry.is_some_and(|n| n >= input.node_count)
                        || (input.node_count == 0) != result.entry.is_none()
                        || result.as_bytes != admission.limits.max_child_address_space_bytes
                        || result.admission_fingerprint != admission_fingerprint
                        || result.resource_limit_kind != "rlimit_as"
                        || result.output_digest != digest
                    {
                        return Err(corrupt("semantic builder completion binding differs"));
                    }
                    complete = Some(result);
                }
            }
        }
        let complete =
            complete.ok_or_else(|| corrupt("semantic builder exited without complete topology"))?;
        loop {
            remaining(deadline)?;
            {
                let (lock, wake) = &*cancellation;
                *lock.lock().unwrap() = true;
                wake.notify_one();
            }
            if let Some(status) = child.try_wait().map_err(io_error)? {
                if !status.success() {
                    return Err(corrupt("semantic builder exited unsuccessfully"));
                }
                break;
            }
            thread::sleep(remaining(deadline)?.min(Duration::from_millis(10)));
        }
        Ok(complete)
    })();
    // Wake/cancel before reaping to prevent watchdog PID reuse.
    {
        let (lock, wake) = &*cancellation;
        *lock.lock().unwrap() = true;
        wake.notify_one();
    }
    let cleanup_deadline = Instant::now() + Duration::from_millis(500);
    while !watchdog.is_finished() && Instant::now() < cleanup_deadline {
        thread::sleep(Duration::from_millis(2));
    }
    if !watchdog.is_finished() {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "semantic builder watchdog termination unavailable",
        ));
    }
    let watchdog_result = watchdog.join();
    if result.is_err() {
        cleanup(child, cleanup_deadline)?;
    }
    if !matches!(watchdog_result, Ok(Ok(()))) {
        cleanup(child, cleanup_deadline)?;
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "semantic builder watchdog signal failed; child cleanup completed",
        ));
    }
    result
}

fn cleanup(child: &mut Child, deadline: Instant) -> Result<()> {
    if child.try_wait().map_err(io_error)?.is_some() {
        return Ok(());
    }
    if child.kill().is_err() && child.try_wait().map_err(io_error)?.is_none() {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "semantic builder termination failed; child state unresolved",
        ));
    }
    loop {
        if child.try_wait().map_err(io_error)?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "semantic builder reap deadline exceeded; child state unresolved",
            ));
        }
        thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(unix)]
fn nonblocking(pipe: &impl std::os::fd::AsRawFd) -> Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io_error("nonblocking pipe unavailable"));
    }
    Ok(())
}
#[cfg(not(unix))]
fn nonblocking<T>(_: &T) -> Result<()> {
    Err(WikiError::new(
        ErrorCode::CapabilityUnavailable,
        "semantic builder pipe platform unsupported",
    ))
}

#[cfg(unix)]
fn poll_ready(pipe: &impl std::os::fd::AsRawFd, events: i16, deadline: Instant) -> Result<()> {
    let left = remaining(deadline)?;
    let mut descriptor = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events,
        revents: 0,
    };
    let timeout = left.as_millis().min(25).max(1) as i32;
    if unsafe { libc::poll(&mut descriptor, 1, timeout) } < 0
        && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
    {
        return Err(io_error("pipe polling failed"));
    }
    remaining(deadline)?;
    Ok(())
}
#[cfg(unix)]
fn write_deadline(
    pipe: &mut (impl Write + std::os::fd::AsRawFd),
    bytes: &[u8],
    deadline: Instant,
) -> Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        remaining(deadline)?;
        match pipe.write(&bytes[offset..]) {
            Ok(0) => return Err(io_error("request pipe closed")),
            Ok(n) => offset += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                poll_ready(pipe, libc::POLLOUT, deadline)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io_error(e)),
        }
    }
    remaining(deadline)?;
    Ok(())
}
#[cfg(not(unix))]
fn write_deadline<T>(_: &mut T, _: &[u8], _: Instant) -> Result<()> {
    Err(io_error("unsupported pipe"))
}

#[cfg(unix)]
fn read_deadline_exact(
    pipe: &mut (impl Read + std::os::fd::AsRawFd),
    bytes: &mut [u8],
    deadline: Instant,
) -> Result<bool> {
    let mut offset = 0;
    while offset < bytes.len() {
        remaining(deadline)?;
        match pipe.read(&mut bytes[offset..]) {
            Ok(0) if offset == 0 => return Ok(false),
            Ok(0) => return Err(corrupt("semantic builder frame truncated")),
            Ok(n) => offset += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                poll_ready(pipe, libc::POLLIN, deadline)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io_error(e)),
        }
    }
    Ok(true)
}
#[cfg(unix)]
fn read_deadline_frame(
    pipe: &mut (impl Read + std::os::fd::AsRawFd),
    limit: usize,
    deadline: Instant,
) -> Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    if !read_deadline_exact(pipe, &mut header, deadline)? {
        return Ok(None);
    }
    let size = u32::from_le_bytes(header) as usize;
    if size == 0 || size > limit {
        return Err(corrupt("semantic builder frame length exceeds bound"));
    }
    let mut bytes = vec![0; size];
    if !read_deadline_exact(pipe, &mut bytes, deadline)? {
        return Err(corrupt("semantic builder frame body absent"));
    }
    remaining(deadline)?;
    Ok(Some(bytes))
}
#[cfg(not(unix))]
fn read_deadline_frame<T>(_: &mut T, _: usize, _: Instant) -> Result<Option<Vec<u8>>> {
    Err(io_error("unsupported pipe"))
}

struct Replay<'a> {
    fs: &'a VaultFs,
    input: GraphRef,
    next: NodeId,
    deadline: Instant,
    reader: GraphReader<'a>,
    meter: DiscoveryMeter,
}
impl<'a> Replay<'a> {
    fn new(fs: &'a VaultFs, input: GraphRef, deadline: Instant) -> Result<Self> {
        let meter = DiscoveryMeter::new(VectorReadBudget::new(
            deadline.min(Instant::now() + Duration::from_secs(30)),
        )?);
        Ok(Self {
            fs,
            reader: GraphReader::new(fs, input.clone())?,
            input,
            next: 0,
            deadline,
            meter,
        })
    }
}
impl Iterator for Replay<'_> {
    type Item = Result<GraphNode>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.input.node_count {
            return None;
        }
        let id = self.next;
        self.next += 1;
        Some((|| {
            remaining(self.deadline)?;
            if id % 128 == 0 {
                self.reader = GraphReader::new(self.fs, self.input.clone())?;
                self.meter = DiscoveryMeter::new(VectorReadBudget::new(
                    self.deadline.min(Instant::now() + Duration::from_secs(30)),
                )?);
            }
            self.reader.node(id, &mut self.meter)
        })())
    }
}

/// Hidden CLI dispatch calls this before reading configuration/credentials.
pub(crate) fn child_main() -> Result<()> {
    let result = child_inner();
    if let Err(error) = &result {
        let message: String = error.message.chars().take(512).collect();
        let _ = write_frame(
            &mut std::io::stdout().lock(),
            &Frame::Failed {
                code: error.code,
                message,
            },
            4096,
        );
    }
    result
}
fn child_inner() -> Result<()> {
    let bytes = read_frame(&mut std::io::stdin().lock(), REQUEST_BYTES)?
        .ok_or_else(|| corrupt("semantic builder request absent"))?;
    let request: Request = serde_json::from_slice(&bytes).map_err(io_error)?;
    if request.remaining_ms == 0
        || request.remaining_ms > MAX_CHILD_MS
        || request.remaining_ms > request.admission.limits.max_invocation_ms
        || request.input.kind != GraphKind::StagingInput
    {
        return Err(corrupt("semantic builder request bounds invalid"));
    }
    validate_admission(
        &request.input,
        &request.input_fingerprint,
        &request.admission,
    )?;
    verify_limit(request.admission.limits.max_child_address_space_bytes)?;
    let deadline = Instant::now() + Duration::from_millis(request.remaining_ms);
    let fs = VaultFs::new(VaultRoot::explicit(&request.root)?);
    let mut out = std::io::stdout().lock();
    let mut digest = Blake3Hash::digest("lwiki-semantic-export-v1");
    let topology = builder::build(
        request.input.node_count,
        request.input.dimensions,
        &request.admission,
        deadline,
        || Replay::new(&fs, request.input.clone(), deadline),
        |node| {
            digest = advance_digest(&digest, &node)?;
            write_frame(&mut out, &Frame::Node(node), FRAME_BYTES)?;
            Ok(())
        },
    )?;
    remaining(deadline)?;
    write_frame(
        &mut out,
        &Frame::Complete(BuildResult {
            entry: topology.entry,
            max_level: topology.max_level,
            node_count: topology.node_count,
            native_memory_lower_bound: topology.native_memory_lower_bound,
            input_fingerprint: request.input_fingerprint,
            output_digest: digest,
            resource_limit_kind: "rlimit_as".into(),
            as_bytes: request.admission.limits.max_child_address_space_bytes,
            admission_fingerprint: request.admission.fingerprint()?,
        }),
        FRAME_BYTES,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
