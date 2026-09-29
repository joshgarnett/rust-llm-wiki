//! Conservative, platform-independent Windows file ACL interpretation.
//!
//! This is an upper bound on ordinary allow grants, not an AccessCheck emulator.
//! Unsupported ACEs fail closed; denying ACEs are never subtracted from allows.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Protection {
    Private,
    IntegrityProtected,
}

pub(crate) const MAX_ACL_BYTES: usize = u16::MAX as usize;
pub(crate) const MAX_SID_BYTES: usize = 68;
const ALL: u32 = 0x001f01ff;
const READ: u32 = 0x00120089;
const WRITE: u32 = 0x00120116;
const EXECUTE: u32 = 0x001200a0;
const GENERIC_READ: u32 = 0x80000000;
const GENERIC_WRITE: u32 = 0x40000000;
const GENERIC_EXECUTE: u32 = 0x20000000;
const GENERIC_ALL: u32 = 0x10000000;
const GENERICS: u32 = 0xf0000000;
// Data/append/EA/attributes, delete-child, DELETE, WRITE_DAC, WRITE_OWNER.
const MUTATION: u32 = 0x000d0156;
const INHERIT_ONLY: u8 = 0x08;
const INHERIT: u8 = 0x03;
const ALLOW_FLAGS: u8 = 0x1f;

pub(crate) fn valid_sid(sid: &[u8]) -> bool {
    sid.len() >= 8
        && sid.len() <= MAX_SID_BYTES
        && sid[0] == 1
        && sid[1] <= 15
        && sid.len() == 8 + usize::from(sid[1]) * 4
}

fn mapped_rights(mask: u32) -> Result<u32, &'static str> {
    if mask & !(ALL | GENERICS) != 0 {
        return Err("unknown file access rights");
    }
    let mut result = mask & !GENERICS;
    for (generic, concrete) in [
        (GENERIC_READ, READ),
        (GENERIC_WRITE, WRITE),
        (GENERIC_EXECUTE, EXECUTE),
        (GENERIC_ALL, ALL),
    ] {
        if mask & generic != 0 {
            result |= concrete;
        }
    }
    Ok(result)
}

/// `None` represents either an absent or null DACL; both are rejected.
/// The owner and every trusted SID must be valid binary SIDs.
pub(crate) fn validate_acl(
    owner: &[u8],
    dacl: Option<&[u8]>,
    trusted: &[Vec<u8>],
    protection: Protection,
    directory: bool,
) -> Result<(), &'static str> {
    if !valid_sid(owner) || trusted.is_empty() || trusted.iter().any(|sid| !valid_sid(sid)) {
        return Err("invalid owner or trusted SID");
    }
    if !trusted.iter().any(|sid| sid.as_slice() == owner) {
        return Err("untrusted file owner");
    }
    let acl = dacl.ok_or("missing or null DACL")?;
    if acl.len() < 8 || acl.len() > MAX_ACL_BYTES || !matches!(acl[0], 2 | 4) {
        return Err("invalid ACL header");
    }
    let size = usize::from(u16::from_le_bytes([acl[2], acl[3]]));
    let count = usize::from(u16::from_le_bytes([acl[4], acl[5]]));
    if size != acl.len() || acl[1] != 0 || acl[6..8] != [0, 0] {
        return Err("invalid ACL extent");
    }
    if count == 0 || count > (size - 8) / 16 {
        return Err("empty or excessive ACL");
    }
    let mut offset = 8;
    let mut effective_allow = false;
    for _ in 0..count {
        let header = acl.get(offset..offset + 4).ok_or("truncated ACE header")?;
        if header[0] != 0 || header[1] & !ALLOW_FLAGS != 0 {
            return Err("unsupported ACE type or flags");
        }
        let flags = header[1];
        if flags & (INHERIT_ONLY | 0x04) != 0 && flags & INHERIT == 0 {
            return Err("inheritance modifier has no inheritance target");
        }
        let ace_size = usize::from(u16::from_le_bytes([header[2], header[3]]));
        if ace_size < 16 || ace_size % 4 != 0 {
            return Err("invalid ACE extent");
        }
        let ace = acl.get(offset..offset + ace_size).ok_or("truncated ACE")?;
        let sid = &ace[8..];
        if !valid_sid(sid) {
            return Err("invalid ACE SID extent");
        }
        let rights = mapped_rights(u32::from_le_bytes(ace[4..8].try_into().unwrap()))?;
        let effective = flags & INHERIT_ONLY == 0;
        effective_allow |= effective && rights != 0;
        // An inherit-only grant can compromise children even though it grants no
        // rights on the current directory. Apply the same conservative policy.
        if (effective || directory && flags & INHERIT != 0)
            && !trusted
                .iter()
                .any(|trusted_sid| trusted_sid.as_slice() == sid)
            && match protection {
                Protection::Private => rights != 0,
                Protection::IntegrityProtected => rights & MUTATION != 0,
            }
        {
            return Err("outside principal has forbidden file rights");
        }
        offset += ace_size;
    }
    // Unused ACL capacity is valid, but cannot contain hidden nonzero records.
    if acl[offset..].iter().any(|byte| *byte != 0) || !effective_allow {
        return Err("invalid ACL tail or no effective allow grant");
    }
    Ok(())
}
