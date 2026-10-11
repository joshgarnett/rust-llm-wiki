# External qualification

Local disposable fixtures and deterministic providers establish mechanics. External
qualification checks actual hosts, supported native platforms, live providers,
representative corpora and final distributed bytes. Use [validation and limits](validation-status.md) to choose the required checks.
Each run needs its own finite protocol and source/executable pins.

## Actual providers

Use only an already authorized endpoint and finite request/byte/unit/deadline/cost
limits. Configure private absolute provider files outside the vault and bind the
actual canonical root/ID using [provider contracts](contracts/providers-jobs.md).
Protect credentials and private files; never include secrets/helper output in argv,
logs, wiki content or public reports. Preferences alone do not grant provider access.

Confirm deployed model/revision, output-limit field, usage classes and complete
rate card before promising a monetary ceiling. Preview offline first; it must make
no helper/network call. Then probe a single explicit role with finite limits from
the running generated command reference, for example `doctor --probe --role embed`
or `generate` and `--max-requests 1 --concurrency 1 --attempts-per-task 1` plus the
confirmed service-specific bounds. Use `--extraction-schema` when qualifying that
schema, rather than treating a generic generation probe as extraction compatibility.

Record endpoint fingerprint, returned model, actual usage and retained run/receipts.
Exercise cache reuse, malformed/refused/truncated response, offline replay, exact
citations and accounting uncertainty. Preserve unknown attempts/holds and reconcile
billing before uncertain retries. One embedding endpoint does not qualify generation,
other roles, gateways or deployments. Historical authorized OpenAI embedding calls
are evidence for that endpoint/run, not present universal provider qualification.

## Editors and hosts

Use an isolated Obsidian vault on each supported OS. Follow landing/Page/Source/Revision
links; edit unknown properties, BOM/CRLF/Unicode, body/title/headings and supported
Page moves. Verify exact author preservation, stable IDs, provenance, exclusions and
refresh/history. Source mode handles unsupported nested-property representations;
ordinary editor state remains editor-owned.

For the hosted macOS and Linux editor checks, first let a release-build run finish verifying
its archives. Then run the qualification workflow with that run ID and full source
commit:

```sh
gh workflow run qualification.yml --ref BRANCH \
  -f archive-run-id=RUN_ID -f source-commit=FULL_COMMIT
```

To repeat only a failed check, add `-f platform=macos` or `-f platform=linux`
and choose `-f checks=editor`, `-f checks=installer` or `-f checks=download`.
The download check requires macOS. The defaults run all checks on both platforms.

Each job downloads and verifies the existing archive, then opens a test wiki in a
pinned official Obsidian build on a fresh GitHub-hosted runner. It checks real editor
saves, capture and refresh, old and new citations, Page moves and stale-edit
conflicts. The report records the product and test-tool commits separately.
The same jobs check that a new shell finds the installed command and that
reinstalling or removing it preserves the wiki, settings and unrelated files.
Download each `editor-evidence-*` artifact to inspect the saved bytes, command output
and screenshots. A newer test-tool commit does not require rebuilding the product.

The tests use synthetic notes and no accounts or provider credentials. On Linux,
the editor runs as the runner user with a virtual display and an isolated network.
On macOS, the editor uses its normal network access. Both preserve the editor's
shipped sandbox settings, disable updates and limit each app run to five
minutes. All lwiki commands run offline.

The Mac job also downloads the shell installer through Chrome with its normal
download protections. It records quarantine metadata and runs that script with
the verified Actions archive. This checks the downloaded script and installation;
public release URLs and archives downloaded through a browser need separate checks.

Record any newline or byte-order-mark changes made by the editor. Captured bytes
must match the saved file exactly; never restore a fixture to hide editor changes.

Install fresh versioned exports in real Codex, Claude and Cursor sessions without
overwriting existing instructions. Check explicit invocation, natural-language
discovery, a complete capture/edit/refresh/cited-draft workflow and an unrelated-task
negative control. Successful export or a schema-valid skill does not prove host behavior.

## Native and release bytes

Run required correctness, fault/kill/restart/concurrency, minimal-PATH and ordinary
workflow gates natively against the final staged executable on every supported
target. Record compiler/profile/settings, full source commit, executable/archive
hashes and environment. A rebuild or re-sign that changes executable bytes requires
replay. Container-only changes retain correctness evidence only after identical
extracted executable bytes are proved; rerun extraction/trust checks as applicable.
Windows remains build/capability-only; power-loss remains distinct from process kill.

Build and run the native Rust test harnesses with `--config=release`, using the
same optimization level as the shipped command. The qualification report retains
Bazel's compiler arguments and verifies `opt-level=3`. Keep all test limits in
place; an unoptimized diagnostic run does not establish release performance.

Release archives are built in GitHub Actions. Build the macOS archive on the oldest
supported version, then run the same archive on each supported version. The macOS
support policy covers the current major release and the previous two, on their
latest updates. The release matrix uses macOS 15, 26 and 27; the macOS 27 runner
currently has the label `xcode-27`. Check its reported OS version rather than
inferring it from the label. This follows the recent-version approach in
[Homebrew's support policy](https://docs.brew.sh/Support-Tiers).

Archive verification reads the minimum OS or glibc version from the executable
and checks it against the release metadata. It requires `otool` or `llvm-otool`
for Mach-O files and `readelf` or `llvm-readelf` for ELF files. Native qualification
checks installation and the installed executable, and compares its hash with the
archived executable before and after the tests. All native jobs must pass before
the workflow stages release assets.

The project uses MIT and GitHub Issues for support. macOS releases are unsigned
and are not notarized. Verify dependency notices, artifact origin and hashes,
and normal macOS download approval. Do not require global security disabling.
Check the archive layout and installer PATH, conflict and rollback behavior.

Publication is a distinct authorized action after gates. After any publication,
download unauthenticated on each advertised target, verify final tag/source/manifest/
digests/trust, install and replay the offline journey, agent install and backup restore.
Public README/manual/latest links must agree. Failed public replay requires an honest
unsafe-release disposition and corrected version; never move tags or replace immutable
published bytes silently.

## Corpus and independent quality

Select provenance-preserving licensed inputs and unseen tasks before results. Use
[context evaluation](evaluating-context.md), [capacity protocol](testing-large-vaults.md)
and [critic workflow](testing-usability.md). Record complete returned facts/citations,
errors, regressions, resource use and observed costs. Mock vectors test mechanics;
real model quality and supported scale need their own declared evidence.
