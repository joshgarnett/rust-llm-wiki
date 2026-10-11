use super::super::admission::DiscoveryMaintenanceLimits;
use super::*;
use crate::vault::VaultRoot;
fn fixture() -> (tempfile::TempDir, VaultFs, WriterPermit) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("../../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    (temp, fs, writer)
}
fn admission() -> DiscoveryAdmission {
    DiscoveryAdmission::derive(
        Blake3Hash::digest("plan"),
        1,
        2,
        DiscoveryMaintenanceLimits::default(),
        StorageGeometry {
            allocation_unit_bytes: 4096,
        },
    )
    .unwrap()
}
#[test]
fn lost_credit_and_repeated_debits_are_permanently_consumed() {
    let (_temp, fs, writer) = fixture();
    let a = admission();
    let id = reserve(&fs, &writer, &a).unwrap();
    drop(debit(&fs, &writer, &id, &a, a.reserved_artifact_bytes - 1).unwrap());
    verify(&fs, &writer, &id, &a).unwrap();
    assert!(debit(&fs, &writer, &id, &a, 2).is_err());
    drop(debit(&fs, &writer, &id, &a, 1).unwrap());
    assert!(debit(&fs, &writer, &id, &a, 1).is_err());
    let mut zero = debit(&fs, &writer, &id, &a, 0).unwrap();
    assert!(zero.consume_object(&fs, 0).is_err());
}
#[test]
fn credits_bind_vault_geometry_and_cannot_be_reused() {
    let (_temp, fs, writer) = fixture();
    let a = admission();
    let id = reserve(&fs, &writer, &a).unwrap();
    let charge = a.geometry.object_charge(1).unwrap();
    let mut credit = debit(&fs, &writer, &id, &a, charge).unwrap();
    let (_other, other, _permit) = fixture();
    assert!(credit.consume_object(&other, 1).is_err());
    credit.consume_object(&fs, 1).unwrap();
    assert!(credit.consume_object(&fs, 1).is_err());
}
#[test]
fn aggregate_holds_survive_cache_reset_and_bound_new_attempts() {
    let (temp, fs, writer) = fixture();
    let original = admission();
    let mut limits = original.limits.clone();
    limits.max_graph_cache_charge_bytes =
        ACCOUNTING_ALLOWANCE + 2 * original.reserved_artifact_bytes;
    let a =
        DiscoveryAdmission::derive(original.plan_hash, 1, 2, limits, original.geometry).unwrap();
    let first = reserve(&fs, &writer, &a).unwrap();
    let second = reserve(&fs, &writer, &a).unwrap();
    assert_ne!(first, second);
    std::fs::create_dir_all(temp.path().join(".wiki/cache/semantic")).unwrap();
    std::fs::remove_dir_all(temp.path().join(".wiki/cache")).unwrap();
    verify(&fs, &writer, &first, &a).unwrap();
    assert!(reserve(&fs, &writer, &a).is_err());
}
#[test]
fn wrong_plan_or_admission_cannot_claim_credit() {
    let (_temp, fs, writer) = fixture();
    let a = admission();
    let id = reserve(&fs, &writer, &a).unwrap();
    let other = DiscoveryAdmission::derive(
        Blake3Hash::digest("other"),
        1,
        2,
        a.limits.clone(),
        a.geometry.clone(),
    )
    .unwrap();
    assert!(verify(&fs, &writer, &id, &other).is_err());
    assert!(debit(&fs, &writer, &id, &other, 0).is_err());
    let changed = DiscoveryAdmission::derive(
        a.plan_hash.clone(),
        2,
        2,
        a.limits.clone(),
        a.geometry.clone(),
    )
    .unwrap();
    assert!(verify(&fs, &writer, &id, &changed).is_err());
}
#[test]
fn missing_authority_beside_even_empty_cache_refuses_adoption() {
    let (temp, fs, writer) = fixture();
    std::fs::create_dir_all(temp.path().join(".wiki/cache/semantic")).unwrap();
    assert!(reserve(&fs, &writer, &admission()).is_err());
    assert!(!temp.path().join(PATH).exists());
}
#[test]
fn malformed_schema_and_inconsistent_aggregate_are_not_reinitialized() {
    let (temp, fs, writer) = fixture();
    let a = admission();
    let id = reserve(&fs, &writer, &a).unwrap();
    let c = Connection::open(temp.path().join(PATH)).unwrap();
    c.execute(
        "UPDATE resource_accounting SET aggregate_reserved=aggregate_reserved+1",
        [],
    )
    .unwrap();
    drop(c);
    assert!(verify(&fs, &writer, &id, &a).is_err());
    assert!(reserve(&fs, &writer, &a).is_err());
    let (temp, fs, writer) = fixture();
    std::fs::create_dir_all(temp.path().join(".wiki")).unwrap();
    let c = Connection::open(temp.path().join(PATH)).unwrap();
    c.execute("CREATE TABLE resource_accounting(id INTEGER)", [])
        .unwrap();
    drop(c);
    assert!(reserve(&fs, &writer, &a).is_err());
}
#[test]
fn database_policy_is_read_back_on_every_open() {
    let (_temp, fs, writer) = fixture();
    let a = admission();
    reserve(&fs, &writer, &a).unwrap();
    let c = open(&fs, &writer, &a, false).unwrap();
    let pages: i64 = c
        .query_row("PRAGMA max_page_count", [], |r| r.get(0))
        .unwrap();
    let mode: String = c
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pages, 4096);
    assert_eq!(mode, "delete");
}
