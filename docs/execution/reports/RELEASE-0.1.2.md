# lwiki 0.1.2 release verification

Source: `d8a82a5a345549fada06987dec986801c6842252`, annotated tag `v0.1.2`. All implementation and release source is on main. A final documentation-only stamp records this evidence without changing tagged product bytes.

- [Release build 36641341259](https://github.com/joshgarnett/rust-llm-wiki/actions/runs/36641341259): all six native Linux GNU, macOS and Windows jobs passed, x64 and ARM64.
- [CI 36641320077](https://github.com/joshgarnett/rust-llm-wiki/actions/runs/36641320077): all three jobs passed on the same source.
- All six archives and twelve assets passed `scripts/verify_release.py`: clean expected source, package 0.1.2, native target/host, exact archive members, archive/binary checksums and successful capabilities/version. Native executable architectures were checked separately.
- The downloaded Apple Silicon binary passed strict `codesign --verify --strict`, version and 30 offline manual regression commands. No network or provider call occurred in that smoke.
- [Draft v0.1.2](https://github.com/joshgarnett/rust-llm-wiki/releases/tag/untagged-c3572f140a7cf1d674b4) contains the twelve verified assets; every GitHub SHA256 digest matches its local verified asset. The documented `gh release download v0.1.2` command returned the byte-identical tested macOS archive/checksum. Existing v0.1.0/v0.1.1 drafts and tags were preserved.

Local scope: 230 integration parent cases across 18 affected targets plus the packet unit case, strict all-source Clippy/format, final 0.1.2 seven-target package/CLI gate, 41 Python tooling cases, 36-step exported skill recipe and 30-command manual smoke. Earlier fixture/lint failures and corrected continuations are retained in [DEEP-checks.json](DEEP-checks.json) and [DEEP-followup.md](DEEP-followup.md); no single initially-green full historical suite is claimed. Detailed native/artifact evidence is [RELEASE-0.1.2-checks.json](RELEASE-0.1.2-checks.json).

User-facing instructions: [0.1.2 test-agent guide](../../testing-0.1.2.md) and [release notes](../../release-notes-0.1.2.md). All B1–B13 dispositions are explicit, including local-only offline follow-ups, byte-exact literal search, original-only HTML capture and the corrected B12 observation.

Limits: Windows vault writes remain unsupported; native builds do not qualify full crash recovery on every host. No live gateway/auth helper/paid requests, real vaults, host installation or publication. The older installed-copy macOS SIGKILL remains undiagnosed. The release stays a draft for owner inspection.
