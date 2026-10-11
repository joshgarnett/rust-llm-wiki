//! Finite maintenance policy. Artifact charges are conservative reservations,
//! not a guarantee about physical allocation or other processes' disk usage.
use super::*;
use crate::vault::VaultFs;

const GIB: u64 = 1024 * 1024 * 1024;
pub(crate) const ACCOUNTING_ALLOWANCE: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscoveryMaintenanceLimits {
    pub(crate) max_child_address_space_bytes: u64,
    pub(crate) max_graph_cache_charge_bytes: u64,
    pub(crate) max_invocation_ms: u64,
}
impl Default for DiscoveryMaintenanceLimits {
    fn default() -> Self {
        Self {
            max_child_address_space_bytes: 8 * GIB,
            max_graph_cache_charge_bytes: 32 * GIB,
            max_invocation_ms: 120_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StorageGeometry {
    pub(crate) allocation_unit_bytes: u64,
}
impl StorageGeometry {
    pub(crate) fn observe(fs: &VaultFs) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let path = std::ffi::CString::new(fs.root().path().as_os_str().as_bytes())
                .map_err(|_| corrupt("semantic filesystem path contains NUL"))?;
            let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
                return Err(WikiError::new(
                    ErrorCode::CapabilityUnavailable,
                    "semantic filesystem allocation geometry unavailable",
                ));
            }
            let geometry = Self {
                allocation_unit_bytes: stat.f_frsize as u64,
            };
            geometry.validate()?;
            Ok(geometry)
        }
        #[cfg(not(unix))]
        {
            let _ = fs;
            Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "semantic filesystem allocation geometry unsupported",
            ))
        }
    }
    fn validate(&self) -> Result<()> {
        if !(512..=1024 * 1024).contains(&self.allocation_unit_bytes)
            || !self.allocation_unit_bytes.is_power_of_two()
        {
            return Err(exhausted("semantic allocation unit outside finite policy"));
        }
        Ok(())
    }
    /// One inode transitions from staged to final by rename. The extra two
    /// units reserve directory-entry/metadata allowance, not a filesystem proof.
    pub(crate) fn object_charge(&self, bytes: u64) -> Result<u64> {
        self.validate()?;
        let unit = self.allocation_unit_bytes;
        bytes
            .checked_add(unit - 1)
            .map(|v| v / unit)
            .and_then(|v| v.checked_add(2))
            .and_then(|v| v.checked_mul(unit))
            .ok_or_else(|| exhausted("semantic artifact charge overflow"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscoveryAdmission {
    pub(crate) plan_hash: Blake3Hash,
    pub(crate) node_count: u32,
    pub(crate) dimensions: u32,
    pub(crate) limits: DiscoveryMaintenanceLimits,
    pub(crate) geometry: StorageGeometry,
    pub(crate) native_estimate_bytes: u64,
    pub(crate) topology_bytes: u64,
    pub(crate) total_memory_estimate_bytes: u64,
    pub(crate) reserved_artifact_bytes: u64,
    pub(crate) logical_tree_cap_bytes: u64,
}
impl DiscoveryAdmission {
    pub(crate) fn derive(
        plan_hash: Blake3Hash,
        node_count: u32,
        dimensions: u32,
        limits: DiscoveryMaintenanceLimits,
        geometry: StorageGeometry,
    ) -> Result<Self> {
        geometry.validate()?;
        let ceiling = DiscoveryMaintenanceLimits::default();
        if dimensions == 0
            || dimensions as usize * 4 > MAX_NODE_VECTOR_BYTES
            || limits.max_child_address_space_bytes == 0
            || limits.max_child_address_space_bytes > ceiling.max_child_address_space_bytes
            || limits.max_graph_cache_charge_bytes <= ACCOUNTING_ALLOWANCE
            || limits.max_graph_cache_charge_bytes > ceiling.max_graph_cache_charge_bytes
            || limits.max_invocation_ms == 0
            || limits.max_invocation_ms > ceiling.max_invocation_ms
        {
            return Err(exhausted(
                "semantic maintenance policy or geometry exceeds ceiling",
            ));
        }
        let n = u64::from(node_count);
        let vector = u64::from(dimensions) * 4;
        let native_estimate_bytes = n
            .checked_mul(
                vector + (CONNECTIVITY * 2 * 4 + MAX_LEVEL * CONNECTIVITY * 4 + 256) as u64,
            )
            .ok_or_else(|| exhausted("semantic native estimate overflow"))?;
        // Flat base adjacency, degrees, reachability bitmap and full u32 queue.
        let topology_bytes = n
            .checked_mul((CONNECTIVITY * 2 * 4 + 1 + 1 + 4) as u64)
            .ok_or_else(|| exhausted("semantic topology estimate overflow"))?;
        let total_memory_estimate_bytes = native_estimate_bytes
            .checked_add(topology_bytes)
            .and_then(|v| v.checked_add(512 * 1024 * 1024))
            .filter(|v| *v <= limits.max_child_address_space_bytes)
            .ok_or_else(|| {
                exhausted("semantic memory planning demand exceeds address-space policy")
            })?;
        // A 64-ary carry tree has <N/63 internal nodes. Finalizing the bounded
        // frontier adds at most eight directories including its final root.
        let directories = n / 63 + 8;
        let node_bytes = MAX_NODE_METADATA as u64 + vector;
        let logical_tree_cap_bytes = n
            .checked_mul(node_bytes)
            .and_then(|v| v.checked_add(directories * MAX_NODE_METADATA as u64))
            .ok_or_else(|| exhausted("semantic logical artifact estimate overflow"))?;
        let nodes_charge = n
            .checked_mul(geometry.object_charge(node_bytes)?)
            .ok_or_else(|| exhausted("semantic node allocation estimate overflow"))?;
        let directory_charge = directories
            .checked_mul(geometry.object_charge(MAX_NODE_METADATA as u64)?)
            .ok_or_else(|| exhausted("semantic directory allocation estimate overflow"))?;
        let reserved_artifact_bytes = nodes_charge
            .checked_add(directory_charge)
            .and_then(|v| v.checked_mul(2)) // both staged input and exported output
            .and_then(|v| v.checked_add(1024 * 1024)) // durable stage/catalog allowance
            .filter(|v| *v <= limits.max_graph_cache_charge_bytes - ACCOUNTING_ALLOWANCE)
            .ok_or_else(|| {
                exhausted("semantic input/output reservation exceeds graph-cache policy")
            })?;
        Ok(Self {
            plan_hash,
            node_count,
            dimensions,
            limits,
            geometry,
            native_estimate_bytes,
            topology_bytes,
            total_memory_estimate_bytes,
            reserved_artifact_bytes,
            logical_tree_cap_bytes,
        })
    }
    pub(crate) fn validate(&self) -> Result<()> {
        let expected = Self::derive(
            self.plan_hash.clone(),
            self.node_count,
            self.dimensions,
            self.limits.clone(),
            self.geometry.clone(),
        )?;
        if expected != *self {
            return Err(corrupt("semantic admission accounting differs"));
        }
        Ok(())
    }
    pub(crate) fn fingerprint(&self) -> Result<Blake3Hash> {
        self.validate()?;
        Ok(Blake3Hash::digest(crate::graph::packet::canonical_json(
            self,
        )?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn realistic_passage_geometry_is_explicit_and_finite() {
        let value = DiscoveryAdmission::derive(
            Blake3Hash::digest("plan"),
            325_246,
            1536,
            DiscoveryMaintenanceLimits::default(),
            StorageGeometry {
                allocation_unit_bytes: 4096,
            },
        )
        .unwrap();
        assert_eq!(value.native_estimate_bytes, 2_789_309_696);
        assert!(value.reserved_artifact_bytes > 14 * GIB);
        assert!(value.total_memory_estimate_bytes < 8 * GIB);
        value.validate().unwrap();
        let mut changed = value.clone();
        changed.reserved_artifact_bytes -= 1;
        assert!(changed.validate().is_err());
        changed = value;
        changed.limits.max_child_address_space_bytes += 1;
        assert!(changed.validate().is_err());
    }
    #[test]
    fn invalid_geometry_and_excessive_work_refuse_before_allocation() {
        for (n, d, block) in [
            (u32::MAX, 1536, 4096),
            (1, 0, 4096),
            (1, 65_537, 4096),
            (1, 1536, 0),
            (1, 1536, 1000),
        ] {
            assert!(
                DiscoveryAdmission::derive(
                    Blake3Hash::digest("plan"),
                    n,
                    d,
                    DiscoveryMaintenanceLimits::default(),
                    StorageGeometry {
                        allocation_unit_bytes: block
                    }
                )
                .is_err()
            );
        }
    }
}
