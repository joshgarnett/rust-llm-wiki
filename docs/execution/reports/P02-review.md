# P02 independent gate review

Date: 2026-09-28. Reviewer: assigned independent storage/parser reviewer. Base accepted P00: `77ef9b7`; P02 reviewed at worker report fingerprints. **Disposition: two corrections required before acceptance.** Root owns finding disposition.

## R1 — P1: interrupted directory creation is not made durable on retry

Location: `src/vault/fs.rs:230–245`, `VaultFs::ensure_directory`.

Inject an error immediately after the production `create_directory` succeeds, then retry `ensure_directory` for that same path. The retry takes `AlreadyExists` and returns `DirectorySync::Supported` without syncing the directory or its parent. Actual focused test observed zero `sync_directory` calls. A process stopping between mkdir and sync creates the same recovery condition. Later syncing files or deeper directories does not establish durability of the ancestor's entry in its parent. This undermines the P03 prerequisite that prepared payload directories are durable before journal transitions.

Correction: on successful creation **or** accepted existing-directory state, sync each relevant directory and parent before successful return, propagating unsupported/error results. Regression: post-mkdir failure followed by a successful retry must exercise those syncs; also retry a directory-sync failure.

## R2 — P2: parent scans silently combine nested vaults

Location: `src/vault/paths.rs:180–181`, recursive `scan_dir`.

Create outer `WIKI.md`, `nested/WIKI.md`, and `nested/page.md`. `VaultRoot::explicit(outer).scan_markdown()` returns all three. Expected: child vaults remain outside the parent's canonical scan or cause an explicit boundary diagnostic. This violates the CLI contract “Never combine nested vaults silently” and imports child identities/content into parent discovery. Regression should prove exact-case regular child `WIKI.md` establishes the boundary while lowercase markers and symlink markers follow the existing exact discovery policy.

## Evidence and limits

`cargo test --locked --offline --test p01_p02_review_repro -- --nocapture` completed with three expectation-of-bug tests passing, reproducing both findings and P01 numeric behavior. Temporary test retained for conversion/removal by root. Existing eight P02 tests exercise real native operations with before/after injected errors and real subprocess lock acquisition/kill; static review verified root-bound permits/stages, expected-state guards, scan payload/control exclusions, Unicode folding, and narrow operational append/truncate methods.

Stage-path symlink substitution could publish a symlink because verification follows it; this is optional defense under the explicitly excluded hostile concurrent symlink threat, not an additional gate blocker. No P03 recovery engine, power-loss durability, or native Linux/Windows qualification was demonstrated. Production source/test files were not modified by this reviewer.

## Final bounded re-review disposition — 2026-09-28

**R1 and R2 resolved; no remaining actionable finding in the reviewed P02 gate.** `ensure_directory` now syncs each traversed directory and its parent after creation or accepted existing state; errors and unsupported-directory-sync results propagate. The permanent retry regression injects post-mkdir and before/after-sync failures, verifies all four sync calls for a two-prefix retry, and repeats the check when both directories already exist.

Recursive scanning now excludes a directory containing exact-case regular `WIKI.md`. Managed path resolution likewise refuses crossing that child boundary. Permanent regressions cover parent scan/read/new-target isolation, explicit/discovered child access, and lowercase/symlink markers. Optional stage defense now rejects symlink/nonregular staging entries before reading or renaming; its same-byte symlink regression preserves target absence and payload bytes. Root's missing-root correction maps explicit/discovery `NotFound` to `VaultNotFound`; the permanent test checks exit 3. No new issue was found in these changed invariants. Hostile concurrent substitution and native power-loss/other-OS qualification remain outside this evidence.

Re-review was static; no additional Cargo run was needed. Root reports the final 30-test all-target suite, all-target Clippy with warnings denied, formatting, debug build and diff checks passing. The temporary expectation-of-bug test is absent. Reviewed SHA-256: `src/vault/fs.rs` `5999224fd95f93ab9282a97c90af2f381a031050d9b8945d232ec4e5ded801c3`; `src/vault/paths.rs` `d055088d1700bf3bab4e18cfa386f131eba30f93d55c3308237337e88f4604bf`; `tests/vault_fs.rs` `cefb5e5bd4996fe6f8877d2b347d6c719d018d907496058326739342b0bf8e3c`. Initial findings/evidence above remain historical; root retains package acceptance authority.
