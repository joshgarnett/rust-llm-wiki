//! Portable policy tests; these do not exercise Windows access checks or handles.
#[path = "../src/vault/acl_policy.rs"]
mod acl_policy;
use acl_policy::{Protection, validate_acl};

fn sid(rid: u32) -> Vec<u8> {
    let mut sid = vec![1, 1, 0, 0, 0, 0, 0, 5];
    sid.extend(rid.to_le_bytes());
    sid
}
fn ace(kind: u8, flags: u8, rights: u32, sid: &[u8]) -> Vec<u8> {
    let mut ace = vec![kind, flags];
    ace.extend(((8 + sid.len()) as u16).to_le_bytes());
    ace.extend(rights.to_le_bytes());
    ace.extend(sid);
    ace
}
fn acl(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut acl = vec![2, 0];
    acl.extend(((8 + entries.iter().map(Vec::len).sum::<usize>()) as u16).to_le_bytes());
    acl.extend((entries.len() as u16).to_le_bytes());
    acl.extend([0, 0]);
    for entry in entries {
        acl.extend(entry);
    }
    acl
}
fn validate(
    entries: &[Vec<u8>],
    protection: Protection,
    directory: bool,
) -> Result<(), &'static str> {
    validate_acl(
        &sid(10),
        Some(&acl(entries)),
        &[sid(10), sid(18), sid(32)],
        protection,
        directory,
    )
}
fn owner() -> Vec<u8> {
    ace(0, 0, 0x001f01ff, &sid(10))
}

#[test]
fn trusted_private_grants_are_accepted() {
    for protection in [Protection::Private, Protection::IntegrityProtected] {
        for directory in [false, true] {
            assert!(
                validate(
                    &[
                        owner(),
                        ace(0, 3, 0x10000000, &sid(18)),
                        ace(0, 16, 0x001f01ff, &sid(32))
                    ],
                    protection,
                    directory
                )
                .is_ok()
            );
        }
    }
}

#[test]
fn outside_read_grants_distinguish_private_from_integrity() {
    for right in [1, 8, 0x20, 0x80, 0x20000, 0x100000, 0x80000000, 0x20000000] {
        let entries = [owner(), ace(0, 0, right, &sid(99))];
        assert!(
            validate(&entries, Protection::Private, false).is_err(),
            "{right:x}"
        );
        assert!(
            validate(&entries, Protection::IntegrityProtected, false).is_ok(),
            "{right:x}"
        );
    }
}

#[test]
fn every_outside_mutation_right_is_rejected() {
    for right in [
        2, 4, 0x10, 0x40, 0x100, 0x10000, 0x40000, 0x80000, 0x40000000, 0x10000000,
    ] {
        for protection in [Protection::Private, Protection::IntegrityProtected] {
            for directory in [false, true] {
                assert!(
                    validate(
                        &[owner(), ace(0, 0, right, &sid(99))],
                        protection,
                        directory
                    )
                    .is_err(),
                    "{right:x}"
                );
            }
        }
    }
}

#[test]
fn foreign_owner_null_and_empty_dacl_are_rejected() {
    let bytes = acl(&[owner()]);
    assert!(
        validate_acl(
            &sid(99),
            Some(&bytes),
            &[sid(10)],
            Protection::Private,
            false
        )
        .is_err()
    );
    assert!(validate_acl(&sid(10), None, &[sid(10)], Protection::Private, false).is_err());
    assert!(validate(&[], Protection::Private, false).is_err());
    assert!(validate_acl(&sid(10), Some(&bytes), &[], Protection::Private, false).is_err());
}

#[test]
fn unsupported_and_deny_aces_fail_closed() {
    for kind in 1..=u8::MAX {
        assert!(
            validate(
                &[owner(), ace(kind, 0, 1, &sid(10))],
                Protection::Private,
                false
            )
            .is_err(),
            "kind {kind}"
        );
    }
    // A deny to Everyone must never be used to erase an outside allow.
    assert!(
        validate(
            &[
                owner(),
                ace(1, 0, 0x10000000, &sid(99)),
                ace(0, 0, 1, &sid(99))
            ],
            Protection::Private,
            false
        )
        .is_err()
    );
}

#[test]
fn inherited_grants_and_inherit_only_children_are_checked() {
    assert!(
        validate(
            &[owner(), ace(0, 4, 1, &sid(10))],
            Protection::Private,
            false
        )
        .is_err()
    );
    assert!(
        validate(
            &[owner(), ace(0, 16, 1, &sid(99))],
            Protection::Private,
            false
        )
        .is_err()
    );
    let inherit_only = [owner(), ace(0, 0x0b, 0x10000000, &sid(99))];
    assert!(validate(&inherit_only, Protection::Private, false).is_ok());
    assert!(validate(&inherit_only, Protection::Private, true).is_err());
    assert!(validate(&inherit_only, Protection::IntegrityProtected, true).is_err());
    assert!(
        validate(
            &[ace(0, 0x0b, 0x10000000, &sid(10))],
            Protection::Private,
            true
        )
        .is_err()
    );
    assert!(
        validate(
            &[owner(), ace(0, 8, 1, &sid(10))],
            Protection::Private,
            false
        )
        .is_err()
    );
}

#[test]
fn malformed_lengths_flags_sids_and_masks_fail_closed() {
    let good = acl(&[owner()]);
    for length in 0..good.len() {
        assert!(
            validate_acl(
                &sid(10),
                Some(&good[..length]),
                &[sid(10)],
                Protection::Private,
                false
            )
            .is_err()
        );
    }
    for (index, value) in [
        (0, 3),
        (1, 1),
        (2, 8),
        (4, 255),
        (6, 1),
        (8, 1),
        (9, 0x20),
        (10, 0),
        (16, 2),
        (17, 16),
    ] {
        let mut bad = good.clone();
        bad[index] = value;
        assert!(
            validate_acl(&sid(10), Some(&bad), &[sid(10)], Protection::Private, false).is_err(),
            "byte {index}"
        );
    }
    for mask in [0x200, 0x01000000, 0x02000000, 0x00400000] {
        assert!(
            validate(
                &[owner(), ace(0, 0, mask, &sid(10))],
                Protection::Private,
                false
            )
            .is_err()
        );
    }
    let mut oversized_sid = sid(10);
    oversized_sid[1] = 2;
    assert!(validate(&[ace(0, 0, 1, &oversized_sid)], Protection::Private, false).is_err());
    let mut tail = good;
    tail.extend([1, 0, 0, 0]);
    let length = tail.len() as u16;
    tail[2..4].copy_from_slice(&length.to_le_bytes());
    assert!(
        validate_acl(
            &sid(10),
            Some(&tail),
            &[sid(10)],
            Protection::Private,
            false
        )
        .is_err()
    );
}
