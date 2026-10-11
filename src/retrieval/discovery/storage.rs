//! Immutable authenticated records and bounded directory lookup. No mmap or FFI reads.
use super::*;
use super::{accounting::ArtifactCredit, admission::DiscoveryAdmission};
use crate::vault::{DirectorySync, ExpectedState, VaultFs, WriterPermit};
use std::{collections::BTreeMap, io::Write, time::Instant};

const FANOUT: usize = 64;
const MAX_HEIGHT: u8 = 8;
const MAGIC: &[u8; 8] = b"LWKIGR01";

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObjectRef {
    hash: Blake3Hash,
    first: NodeId,
    count: NodeId,
    height: u8,
    metadata_bytes: usize,
    vector_bytes: usize,
}
impl ObjectRef {
    fn bytes(&self) -> Result<usize> {
        self.metadata_bytes
            .checked_add(self.vector_bytes)
            .ok_or_else(|| corrupt("graph object length overflow"))
    }
    fn end(&self) -> Result<NodeId> {
        self.first
            .checked_add(self.count)
            .ok_or_else(|| corrupt("graph object range overflow"))
    }
    fn validate(&self) -> Result<()> {
        if self.metadata_bytes == 0
            || self.metadata_bytes > MAX_NODE_METADATA
            || self.vector_bytes > MAX_NODE_VECTOR_BYTES
            || self.height > MAX_HEIGHT
            || (self.height == 0 && (self.count != 1 || self.vector_bytes == 0))
            || (self.height > 0 && self.vector_bytes != 0)
        {
            return Err(corrupt("invalid graph object bounds"));
        }
        self.end()?;
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Directory {
    format: u32,
    children: Vec<ObjectRef>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeMetadata {
    format: u32,
    id: NodeId,
    descriptor: UnitDescriptor,
    binding: UnitOwnerBinding,
    neighbors: Vec<Vec<NodeId>>,
    vector_hash: Blake3Hash,
}

fn path(hash: &Blake3Hash) -> Result<VaultRelativePath> {
    let value = hash
        .as_str()
        .strip_prefix("blake3:")
        .ok_or_else(|| corrupt("graph object hash prefix"))?;
    VaultRelativePath::new(format!(
        ".wiki/cache/semantic/objects/{}/{}",
        &value[..2],
        value
    ))
}

fn json_bounded(value: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Buffer {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            if input.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("graph metadata bound"));
            }
            self.bytes.extend_from_slice(input);
            Ok(input.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Buffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut out, value)
        .map_err(|_| exhausted("graph metadata serialization bound exceeded"))?;
    Ok(out.bytes)
}

fn read(fs: &VaultFs, reference: &ObjectRef) -> Result<Vec<u8>> {
    reference.validate()?;
    let size = reference.bytes()?;
    let bytes = crate::vault::fs::read_regular_bounded(
        fs.root(),
        &path(&reference.hash)?,
        true,
        size,
        "semantic-graph-object",
    )?
    .ok_or_else(|| corrupt("selected semantic graph object is missing"))?;
    if bytes.len() != size || Blake3Hash::digest(&bytes) != reference.hash {
        return Err(corrupt("selected semantic graph object changed"));
    }
    Ok(bytes)
}

/// One query owns its authenticated directory cache. Node/vector cache lives in search.
pub(crate) struct GraphReader<'a> {
    fs: &'a VaultFs,
    reference: GraphRef,
    directories: BTreeMap<Blake3Hash, (ObjectRef, Vec<ObjectRef>)>,
}
impl<'a> GraphReader<'a> {
    pub(crate) fn new(fs: &'a VaultFs, reference: GraphRef) -> Result<Self> {
        if reference.format != FORMAT
            || reference.dimensions == 0
            || reference.dimensions > 65_536
            || reference.max_level > MAX_LEVEL
            || reference.directory_height == 0
            || reference.directory_height > MAX_HEIGHT
            || reference.root_bytes == 0
            || reference.root_bytes > MAX_NODE_METADATA
            || (reference.node_count == 0) != reference.entry.is_none()
            || reference.entry.is_some_and(|id| id >= reference.node_count)
        {
            return Err(corrupt("semantic graph root bounds invalid"));
        }
        Ok(Self {
            fs,
            reference,
            directories: BTreeMap::new(),
        })
    }
    fn locate(&mut self, id: NodeId, meter: &mut DiscoveryMeter) -> Result<ObjectRef> {
        if id >= self.reference.node_count {
            return Err(corrupt("graph node outside root"));
        }
        let mut current = ObjectRef {
            hash: self.reference.root.clone(),
            first: 0,
            count: self.reference.node_count,
            height: self.reference.directory_height,
            metadata_bytes: self.reference.root_bytes,
            vector_bytes: 0,
        };
        while current.height > 0 {
            meter.check()?;
            if !self.directories.contains_key(&current.hash) {
                meter.read(current.metadata_bytes, 0)?;
                // Bound decoded JSON/container ownership before allocation.
                meter.retain(current.metadata_bytes.saturating_mul(8).saturating_add(256))?;
                let bytes = read(self.fs, &current)?;
                let directory: Directory = serde_json::from_slice(&bytes)
                    .map_err(|_| corrupt("invalid graph directory encoding"))?;
                if directory.format != FORMAT
                    || directory.children.is_empty()
                    || directory.children.len() > FANOUT
                {
                    return Err(corrupt("graph directory arity/version invalid"));
                }
                let mut next = current.first;
                for child in &directory.children {
                    child.validate()?;
                    if child.first != next || child.count == 0 || child.height >= current.height {
                        return Err(corrupt("graph directory range/height invalid"));
                    }
                    next = child.end()?;
                }
                if next != current.end()? {
                    return Err(corrupt("graph directory coverage differs"));
                }
                self.directories
                    .insert(current.hash.clone(), (current.clone(), directory.children));
            }
            let (validated, children) = &self.directories[&current.hash];
            if validated != &current {
                return Err(corrupt("cached directory framing differs"));
            }
            current = children
                .iter()
                .find(|item| id >= item.first && id < item.end().unwrap_or(item.first))
                .ok_or_else(|| corrupt("graph directory lacks requested node"))?
                .clone();
        }
        Ok(current)
    }
}
impl GraphAccess for GraphReader<'_> {
    fn reference(&self) -> &GraphRef {
        &self.reference
    }
    fn node(&mut self, id: NodeId, meter: &mut DiscoveryMeter) -> Result<GraphNode> {
        let object = self.locate(id, meter)?;
        if object.vector_bytes != self.reference.dimensions as usize * 4 {
            return Err(corrupt("graph node vector length differs from root"));
        }
        meter.read(object.metadata_bytes, object.vector_bytes)?;
        // Temporary decoded metadata is separately admitted from retained search caches.
        meter.retain(
            object
                .metadata_bytes
                .saturating_mul(8)
                .saturating_add(object.vector_bytes.saturating_mul(2)),
        )?;
        let bytes = read(self.fs, &object)?;
        if bytes.len() < 12 || &bytes[..8] != MAGIC {
            return Err(corrupt("graph node magic/length invalid"));
        }
        let metadata_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        if metadata_len.checked_add(12) != Some(object.metadata_bytes) {
            return Err(corrupt("graph node metadata framing differs"));
        }
        let metadata: NodeMetadata = serde_json::from_slice(&bytes[12..object.metadata_bytes])
            .map_err(|_| corrupt("invalid graph node metadata"))?;
        let raw = &bytes[object.metadata_bytes..];
        if metadata.format != FORMAT
            || metadata.id != id
            || metadata.descriptor.owner != metadata.binding.owner
            || metadata.descriptor.policy != metadata.binding.policy
            || metadata.binding.tombstone
            || metadata.binding.unit_count == 0
            || Blake3Hash::digest(raw) != metadata.vector_hash
        {
            return Err(corrupt("graph node identity/vector binding differs"));
        }
        match self.reference.kind {
            GraphKind::StagingInput if !metadata.neighbors.is_empty() => {
                return Err(corrupt("staging input unexpectedly contains adjacency"));
            }
            GraphKind::Base | GraphKind::Delta if metadata.neighbors.is_empty() => {
                return Err(corrupt("graph node lacks its base layer"));
            }
            _ => {}
        }
        let vector = super::super::vectors::decode(raw, self.reference.dimensions)?;
        Ok(GraphNode {
            id,
            descriptor: metadata.descriptor,
            binding: metadata.binding,
            vector,
            neighbors: metadata.neighbors,
        })
    }
}

/// A small durable continuation: only partially filled directory levels are retained.
/// Completed objects are immutable and need no rewrite when preparation resumes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BuildState {
    /// Independently recomputed from the enclosing frozen build plan on resume.
    pub(crate) input_fingerprint: Blake3Hash,
    pub(crate) kind: GraphKind,
    pub(crate) dimensions: u32,
    pub(crate) expected_nodes: NodeId,
    pub(crate) next_node: NodeId,
    pub(crate) entry: Option<NodeId>,
    pub(crate) max_level: usize,
    frontier: Vec<Vec<ObjectRef>>,
    /// Logical object bytes admitted, including reuse. Not physical disk accounting.
    pub(crate) written_bytes: u64,
}
impl BuildState {
    fn admit_frontier(count: NodeId) -> Result<()> {
        // Bound every possible prefix, not only the empty initial checkpoint.
        let mut digits = Vec::new();
        let mut remaining = count;
        while remaining != 0 {
            digits.push((remaining % FANOUT as u32) as usize);
            remaining /= FANOUT as u32;
        }
        let mut prefix = 0usize;
        let mut maximum = 0usize;
        for level in (0..digits.len()).rev() {
            if digits[level] > 0 {
                maximum = maximum.max(prefix + digits[level] - 1 + level * (FANOUT - 1));
            }
            prefix += digits[level];
        }
        maximum = maximum.max(prefix);
        let widest = ObjectRef {
            hash: Blake3Hash::digest([]),
            first: u32::MAX,
            count: u32::MAX,
            height: MAX_HEIGHT,
            metadata_bytes: MAX_NODE_METADATA,
            vector_bytes: MAX_NODE_VECTOR_BYTES,
        };
        let width = json_bounded(&widest, 512)?.len() + 1;
        // 2KiB covers framing, outer identity, arrays and separators.
        if maximum.saturating_mul(width).saturating_add(2048) > 60 * 1024 {
            return Err(exhausted(
                "graph continuation cannot fit its durable stage bound",
            ));
        }
        Ok(())
    }
    pub(crate) fn new(
        input_fingerprint: Blake3Hash,
        kind: GraphKind,
        dimensions: u32,
        count: NodeId,
        entry: Option<NodeId>,
        max_level: usize,
    ) -> Result<Self> {
        Self::admit_frontier(count)?;
        if dimensions == 0
            || dimensions > 65_536
            || max_level > MAX_LEVEL
            || (count == 0) != entry.is_none()
            || entry.is_some_and(|id| id >= count)
        {
            return Err(corrupt("graph build geometry invalid"));
        }
        Ok(Self {
            input_fingerprint,
            kind,
            dimensions,
            expected_nodes: count,
            next_node: 0,
            entry,
            max_level,
            frontier: Vec::new(),
            written_bytes: 0,
        })
    }
    pub(crate) fn validate_resume(&self, expected_input: &Blake3Hash, cap: u64) -> Result<()> {
        Self::admit_frontier(self.expected_nodes)?;
        if &self.input_fingerprint != expected_input
            || self.next_node > self.expected_nodes
            || self.dimensions == 0
            || self.dimensions > 65_536
            || self.max_level > MAX_LEVEL
            || self.frontier.len() > MAX_HEIGHT as usize
            || self.written_bytes > cap
            || (self.expected_nodes == 0) != self.entry.is_none()
            || self.entry.is_some_and(|id| id >= self.expected_nodes)
        {
            return Err(corrupt("graph continuation geometry/input binding invalid"));
        }
        // Bound its serialized checkpoint before carrying a frontier or writing any object.
        json_bounded(self, 60 * 1024)?;
        let mut prefix = Vec::new();
        for (level, entries) in self.frontier.iter().enumerate() {
            if entries.len() >= FANOUT {
                return Err(corrupt("graph continuation frontier overfull"));
            }
            let count = (FANOUT as u32)
                .checked_pow(level as u32)
                .ok_or_else(|| corrupt("graph continuation level overflow"))?;
            for entry in entries {
                entry.validate()?;
                if entry.height as usize != level || entry.count != count {
                    return Err(corrupt("graph continuation frontier level/range invalid"));
                }
                prefix.push(entry);
            }
        }
        prefix.sort_by_key(|entry| entry.first);
        let mut next = 0;
        let mut minimum_bytes = 0u64;
        for entry in prefix {
            if entry.first != next {
                return Err(corrupt("graph continuation prefix has gap/overlap"));
            }
            next = entry.end()?;
            minimum_bytes = minimum_bytes
                .checked_add(entry.bytes()? as u64)
                .ok_or_else(|| corrupt("graph continuation byte overflow"))?;
        }
        if next != self.next_node || self.written_bytes < minimum_bytes {
            return Err(corrupt("graph continuation prefix/accounting differs"));
        }
        Ok(())
    }
    /// Full maximum charge for a bounded append batch, including carry directories.
    pub(crate) fn append_charge(&self, count: u32, admission: &DiscoveryAdmission) -> Result<u64> {
        admission.validate()?;
        if admission.plan_hash != self.input_fingerprint
            || admission.node_count != self.expected_nodes
            || admission.dimensions != self.dimensions
        {
            return Err(corrupt("graph credit plan differs"));
        }
        let before = u64::from(self.next_node);
        let after = before
            .checked_add(u64::from(count))
            .filter(|n| *n <= u64::from(self.expected_nodes))
            .ok_or_else(|| corrupt("graph append credit exceeds interval"))?;
        let mut divisor = FANOUT as u64;
        let mut directories = 0u64;
        while divisor <= after {
            directories += after / divisor - before / divisor;
            divisor = divisor
                .checked_mul(FANOUT as u64)
                .ok_or_else(|| exhausted("graph directory credit overflow"))?;
        }
        let leaf = admission
            .geometry
            .object_charge(MAX_NODE_METADATA as u64 + u64::from(self.dimensions) * 4)?;
        let directory = admission.geometry.object_charge(MAX_NODE_METADATA as u64)?;
        u64::from(count)
            .checked_mul(leaf)
            .and_then(|v| v.checked_add(directories * directory))
            .ok_or_else(|| exhausted("graph append credit overflow"))
    }
    pub(crate) fn finish_charge(&self, admission: &DiscoveryAdmission) -> Result<u64> {
        self.append_charge(0, admission)?;
        let mut children: usize = self.frontier.iter().map(Vec::len).sum();
        let mut directories = 1u64; // final root is always separate
        while children > FANOUT {
            children = children.div_ceil(FANOUT);
            directories += children as u64;
        }
        directories
            .checked_mul(admission.geometry.object_charge(MAX_NODE_METADATA as u64)?)
            .ok_or_else(|| exhausted("graph root credit overflow"))
    }
    // Codec fixtures exercise storage in isolation. Production has no uncharged
    // entry point; accounting and public workflows cover durable debit authority.
    #[cfg(test)]
    pub(crate) fn append(
        &mut self,
        fs: &VaultFs,
        permit: &WriterPermit,
        node: GraphNode,
        expected_input: &Blake3Hash,
        cap: u64,
        deadline: Instant,
    ) -> Result<()> {
        self.append_admitted(
            fs,
            permit,
            node,
            expected_input,
            cap,
            deadline,
            &mut ArtifactCredit::for_test(),
        )
    }
    #[cfg(test)]
    pub(crate) fn finish(
        &mut self,
        fs: &VaultFs,
        permit: &WriterPermit,
        expected_input: &Blake3Hash,
        cap: u64,
        deadline: Instant,
    ) -> Result<GraphRef> {
        self.finish_admitted(
            fs,
            permit,
            expected_input,
            cap,
            deadline,
            &mut ArtifactCredit::for_test(),
        )
    }
    fn put(
        &mut self,
        fs: &VaultFs,
        permit: &WriterPermit,
        bytes: &[u8],
        cap: u64,
        deadline: Instant,
        credit: &mut ArtifactCredit,
    ) -> Result<Blake3Hash> {
        if Instant::now() >= deadline {
            return Err(exhausted("graph export deadline exceeded"));
        }
        let next = self
            .written_bytes
            .checked_add(bytes.len() as u64)
            .filter(|n| *n <= cap)
            .ok_or_else(|| exhausted("graph export byte cap exceeded"))?;
        // Credit was durably debited before this batch; never clone or refund it.
        credit.consume_object(fs, bytes.len() as u64)?;
        let hash = Blake3Hash::digest(bytes);
        let target = path(&hash)?;
        // Never overwrite immutable content, even if a selected object is corrupt.
        if let Some(old) = crate::vault::fs::read_regular_bounded(
            fs.root(),
            &target,
            true,
            bytes.len(),
            "semantic-graph-reuse",
        )? {
            if old != bytes {
                return Err(corrupt("occupied graph object identity differs"));
            }
        } else {
            let parent = target.as_str().rsplit_once('/').unwrap().0;
            if fs.ensure_directory(&VaultRelativePath::new(parent)?, permit)?
                == DirectorySync::Unsupported
            {
                return Err(WikiError::new(
                    ErrorCode::CapabilityUnavailable,
                    "semantic graph publication requires directory durability",
                ));
            }
            let stage = fs.stage(&target, bytes, permit)?;
            if fs.replace(stage, &ExpectedState::Absent, permit)? == DirectorySync::Unsupported {
                return Err(WikiError::new(
                    ErrorCode::CapabilityUnavailable,
                    "semantic graph publication requires directory durability",
                ));
            }
        }
        self.written_bytes = next;
        if Instant::now() >= deadline {
            return Err(exhausted("graph export deadline exceeded"));
        }
        Ok(hash)
    }
    fn directory(
        &mut self,
        fs: &VaultFs,
        permit: &WriterPermit,
        children: Vec<ObjectRef>,
        cap: u64,
        deadline: Instant,
        credit: &mut ArtifactCredit,
    ) -> Result<ObjectRef> {
        if children.is_empty() || children.len() > FANOUT {
            return Err(corrupt("graph export directory arity"));
        }
        let first = children[0].first;
        let height = children.iter().map(|x| x.height).max().unwrap() + 1;
        if height > MAX_HEIGHT {
            return Err(exhausted("graph directory height exceeded"));
        }
        let count = children
            .last()
            .unwrap()
            .end()?
            .checked_sub(first)
            .ok_or_else(|| corrupt("graph export directory range"))?;
        let bytes = json_bounded(
            &Directory {
                format: FORMAT,
                children,
            },
            MAX_NODE_METADATA,
        )?;
        let hash = self.put(fs, permit, &bytes, cap, deadline, credit)?;
        Ok(ObjectRef {
            hash,
            first,
            count,
            height,
            metadata_bytes: bytes.len(),
            vector_bytes: 0,
        })
    }
    pub(crate) fn append_admitted(
        &mut self,
        fs: &VaultFs,
        permit: &WriterPermit,
        node: GraphNode,
        expected_input: &Blake3Hash,
        cap: u64,
        deadline: Instant,
        credit: &mut ArtifactCredit,
    ) -> Result<()> {
        self.validate_resume(expected_input, cap)?;
        let mut next = self.clone();
        next.append_inner(fs, permit, node, cap, deadline, credit)?;
        next.validate_resume(expected_input, cap)?;
        *self = next;
        Ok(())
    }
    fn append_inner(
        &mut self,
        fs: &VaultFs,
        permit: &WriterPermit,
        node: GraphNode,
        cap: u64,
        deadline: Instant,
        credit: &mut ArtifactCredit,
    ) -> Result<()> {
        if node.id != self.next_node
            || node.id >= self.expected_nodes
            || node.vector.len() != self.dimensions as usize
            || node.neighbors.len() > self.max_level + 1
            || node.descriptor.owner != node.binding.owner
            || node.descriptor.policy != node.binding.policy
            || node.binding.tombstone
            || node.binding.unit_count == 0
        {
            return Err(corrupt("graph append identity/geometry invalid"));
        }
        match self.kind {
            GraphKind::StagingInput if !node.neighbors.is_empty() => {
                return Err(corrupt("staging input unexpectedly contains adjacency"));
            }
            GraphKind::Base | GraphKind::Delta if node.neighbors.is_empty() => {
                return Err(corrupt("graph append lacks base adjacency layer"));
            }
            _ => {}
        }
        for (layer, neighbors) in node.neighbors.iter().enumerate() {
            if neighbors.len() > CONNECTIVITY * if layer == 0 { 2 } else { 1 }
                || neighbors.iter().enumerate().any(|(i, n)| {
                    *n >= self.expected_nodes || *n == node.id || neighbors[..i].contains(n)
                })
            {
                return Err(corrupt("graph append adjacency invalid"));
            }
        }
        let vector = node
            .vector
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>();
        super::super::vectors::decode(&vector, self.dimensions)?;
        let metadata = json_bounded(
            &NodeMetadata {
                format: FORMAT,
                id: node.id,
                descriptor: node.descriptor,
                binding: node.binding,
                neighbors: node.neighbors,
                vector_hash: Blake3Hash::digest(&vector),
            },
            MAX_NODE_METADATA - 12,
        )?;
        let mut bytes = Vec::with_capacity(12 + metadata.len() + vector.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.extend_from_slice(&vector);
        let mut object = ObjectRef {
            hash: self.put(fs, permit, &bytes, cap, deadline, credit)?,
            first: node.id,
            count: 1,
            height: 0,
            metadata_bytes: metadata.len() + 12,
            vector_bytes: vector.len(),
        };
        let mut level = 0;
        loop {
            if self.frontier.len() <= level {
                self.frontier.push(Vec::new());
            }
            self.frontier[level].push(object);
            if self.frontier[level].len() < FANOUT {
                break;
            }
            let children = std::mem::take(&mut self.frontier[level]);
            object = self.directory(fs, permit, children, cap, deadline, credit)?;
            level += 1;
        }
        self.next_node += 1;
        Ok(())
    }
    pub(crate) fn finish_admitted(
        &mut self,
        fs: &VaultFs,
        permit: &WriterPermit,
        expected_input: &Blake3Hash,
        cap: u64,
        deadline: Instant,
        credit: &mut ArtifactCredit,
    ) -> Result<GraphRef> {
        self.validate_resume(expected_input, cap)?;
        let mut next = self.clone();
        let result = next.finish_inner(fs, permit, cap, deadline, credit)?;
        next.validate_resume(expected_input, cap)?;
        *self = next;
        Ok(result)
    }
    fn finish_inner(
        &mut self,
        fs: &VaultFs,
        permit: &WriterPermit,
        cap: u64,
        deadline: Instant,
        credit: &mut ArtifactCredit,
    ) -> Result<GraphRef> {
        if self.next_node != self.expected_nodes || self.expected_nodes == 0 {
            return Err(corrupt("cannot publish incomplete/empty graph export"));
        }
        let mut children = self.frontier.iter().flatten().cloned().collect::<Vec<_>>();
        children.sort_by_key(|x| x.first);
        while children.len() > FANOUT {
            let mut next = Vec::new();
            for chunk in children.chunks(FANOUT) {
                next.push(self.directory(fs, permit, chunk.to_vec(), cap, deadline, credit)?);
            }
            children = next;
        }
        let root = self.directory(fs, permit, children, cap, deadline, credit)?;
        if root.first != 0 || root.count != self.expected_nodes {
            return Err(corrupt("graph export root does not cover all nodes"));
        }
        Ok(GraphRef {
            format: FORMAT,
            kind: self.kind,
            root: root.hash,
            root_bytes: root.metadata_bytes,
            directory_height: root.height,
            node_count: self.expected_nodes,
            dimensions: self.dimensions,
            entry: self.entry,
            max_level: self.max_level,
        })
    }
}
