# New-family host reading and cited Page workflow

The frozen 24-task host-assisted pilot **failed** its complete-workflow gate: 15/20
complete cited Pages and independent score **6.83/10**, against ≥18/20 and ≥9/10. Two
tasks failed before a host was created; three other Pages had broken Markdown source
links. No failed task was retried or dropped.

The 18 positive tasks that ran all recovered their required facts through bounded
follow-up reads. Native initial context provided 38/58 required facts and completed 9/20
positive tasks; final host evidence and answers supplied 51/58 and completed 18/20
semantically. The other seven propositions belong to the two uncreated tasks. These
different outcomes identify useful host reading and incomplete initial evidence; the
unchanged historical native baseline remains 9/28 facts and 2/10 tasks. No default
retrieval improvement is claimed.

## Frozen corpus and workflow

Independent Astra authorship used 13 unused WixQA ExpertWritten-connected articles (76,037
rendered bytes) and eight fictional Bellwether Museum files (36,335 bytes): 21
files,112,372 bytes, six larger than 8 KiB. The public source snapshot is 2024-12-02 at
dataset revision `d662dc42479c14e202eccd832f8c4b66a035c4cc`. The complete earlier
exposure-connected closure of 47 questions/46 articles was excluded, leaving 153 eligible
question families. Raw public article text is a verbatim UTF-8 suffix after provenance
metadata with reversible offsets; labels and questions never entered the index. Fictional
Sources contain distant conditions, multisource facts, topical distractors,
approved/archived/draft editions and explicit reuse permission. See the [dataset
reference](eval-datasets-quick-reference.md#new-family-host-reading-pilot) for terms and
provenance.

Freeze: 20 positive+4 absent tasks,58 required propositions;
strict≥18/20,exact≥7/9,paraphrase≥5/6,multisource≥4/5 and overlapping new-document≥3/4;
all absent controls, all citations/current qualifications, no correctness/budget/isolation
blocker and critic≥9. Corpus-family counts are separate constraints, not extra success
denominators. Four source-only files were imported as a second batch before any task.

The unchanged native ARM64 optimized candidate-015 CLI uses accepted source `99a8da8`.
Every fresh Sol actor receives only its original task and fixed generic guide: normalized
offline lexical context (`sources/`,five owners,80 candidates,6,000 rendered bytes/1,500
estimated tokens), bounded verified search/deeper read, cited draft init, verified Page
read and exact-title discovery. These are the pilot's explicit bounds; actual CLI defaults
are ten owners/12,000 bytes/3,000 estimated tokens. Native HIGH and actual-default
profiles must not be conflated.

Per-task limits:240 observed UTC seconds,19 native attempts(initial 1+retrieval 12+Page
4+help 2),five seconds/command,49,152 full raw stdout/stderr bytes,8,192 fixed custom-input
bytes and 6,000 answer bytes. All errors consume bounds. No provider API calls or ID-only
selector. Two continuing vault arms serialize work in each arm; Sources stay fixed, only
the actor's own Page is accessible. Accumulated Pages can change index statistics; no
fresh-vault latency claim.

## Actual initial results

| Outcome | Actual | Gate |
| --- | ---: | ---: |
| Native initial supported facts |38/58|Diagnostic|
| Native initial complete positives |9/20|Separate from host|
| Final host supported facts |51/58|Diagnostic|
| Semantically complete positives |18/20|Separate from full workflow|
| Strict complete cited Pages |15/20|≥18/20:failed|
| Exact / paraphrase / multisource |7/9;5/6;3/5|7/9;5/6;4/5:multisource failed|
| New-document overlap |3/4|≥3/4|
| Qualified absence controls |4/4|4/4|
| Actual init/read/title mechanics |22/24|Not answer completeness|
| Final exact SourceRefs |37/37|All|
| Broken Markdown source links |4 across 3 Pages|Zero:failed|
| Independent score |6.83/10|≥9:failed|

All 122 native SourceRef occurrences and 37 final references authenticate exact
source/revision/span/quote hashes. No unsupported requested fact, stale revision or
archived/draft alternative was presented as current approved evidence. The broken links
are in final Page assembly: two Pages use vault-relative `sources/...` from `pages/`,
while another uses `../../sources/...` and leaves the vault. Required prose was supported,
but usable source navigation failed. The [maintained
guide](../skills/llm-wiki/references/cited-page.md) now explicitly derives Markdown links
relative to the actual saved Page directory. This documentation correction does not
rescore the failed trial or change candidate-015's frozen embedded guide.

Two uncreated jobs retain zero credit after literal `agent thread limit reached` failures;
at most two actors were active. Eight native usage errors and 23 pre-native admission
rejects are retained command friction. Preparation retained a wrapper serialization
failure after successful init and an incorrect check argument; only unrun work/corrected
check continued. No successful import/init or passing Rust check was replayed.

## Accounting and audit limits

The 22 actors made 161 native calls and returned 750,685 full raw native bytes. Sum of
native process-owned monotonic intervals:10.257 s; packet-to-terminal UTC
turnaround:2,123.942 s summed,94.394 s median,136.433 s maximum. Sum is not batch elapsed
time; host turnaround includes dispatch/tool/harness work and is not inference latency.
Tiny-corpus timing does not qualify 25K.

Available distinct-response usage:326 actor responses,10,503,225 input tokens(including
9,949,056 cached),91,022 output(including 4,652 reasoning),10,594,247 total. The
coordinator separately used 143 responses/6,209,712 total tokens; root/critic usage is not
included. Never add cumulative counters repeatedly or add reasoning/cache subsets again.
Fixed custom inputs total 171,863 bytes; visible ambient preambles total 1,065,554 bytes.
The 8 KiB/48 KiB limits do not cap total model context. Monetary cost, hidden input
partition, inference-only time and physical I/O/RSS remain unavailable.

Every actual actor filesystem/native call and result is preserved and independently
audited; no private label, prior Page, raw Source-file, provider, build or Git access was
observed. Some inter-agent message fields are encrypted: saved plaintext dispatch cannot
independently be compared with ciphertext. This is observed access auditing, not enforced
isolation or a plaintext-complete transport audit. Before these maintenance observations,
all 42 source heads and captured payloads match both initial vaults. The final head
snapshot preceded the last actor terminal; its remaining trace contains only its own
accounting work. Executable inode/size/mtime and retained hash binding match; the critic
did not reread the 31 MiB binary under its 32 MiB read cap.

The critic sealed initial grading at 04:27:25 UTC within 758.429 observed seconds/900
limit and ≤28 MiB known reads/32 MiB limit. Final report artifacts are under 1 MiB. A
temporary private extraction cache reached 1,138,851 bytes before compaction:90,275 bytes
over 1 MiB if that report cap also covers temporary working material. Preserve this
process deviation; it does not alter any candidate outcome. Initial-seal allocated trial
bytes 29,941,760/free 35,393,560,576, under 128 MiB new-owned ceiling and above unchanged
32 GiB floor.

## Separate maintenance observations

The independently frozen author-note reconciliation and substantive source-fact refresh
were executed only after initial assessment was sealed. Each is a separate fresh job with
240 UTC seconds,12 native attempts and 49,152 raw native bytes. Ordinary root setups are
bounded separately and retain whole Page before/after bytes plus all old immutable
revision-file hashes. Independent supplementary assessment is **0/2 full passes**, with
**2/2 semantic maintenance passes**. Both actors preserve author note, record, identity
and guarded read/edit/title behavior. All four final SourceRefs authenticate, and all
three old revision files remain unchanged. The substantive refresh changes the approved
freeze from Tuesday 14:00 to Wednesday 10:00 and reconciles the draft; both Pages still
retain the inherited broken link. No concurrent-author conflict was injected. These
observations cannot replace an initial task or change the original 24-task result.

| Additional observation | Native calls / admission errors | Raw bytes | Observed UTC turnaround | Full result |
| --- | ---: | ---: | ---: | --- |
| Author-note reconciliation |6 /3|29,743|78.487 s|Failed inherited link|
| Substantive Source refresh |7 /2|31,775|82.045 s|Failed inherited link|

Root setups use three calls each,0.363/0.620 owner-monotonic seconds and 8,137/8,599 raw
bytes, separately from host turnaround. Actor usage totals 983,678 input tokens(921,472
cached),6,412 output(including 240 reasoning). Supplied-recorder inspection is visible
additional input; no root syntax coaching was sent. All actor/setup bounds pass.
Supplementary grading seals in 186.451 UTC seconds against 300,known reads 5,212,596 bytes
against 8 MiB,with report/temporary artifacts below their 256 KiB/512 KiB caps. The same
encrypted-message audit limitation remains. Supplementary JSON SHA256
`a1961c971532f95840f6f77c68212b98124fc8544abb2c6466ec2dc13e0323c8`.

## Next decision and reproduction scope

A fresh Astra architecture review rejects another lexical weight/cap/allocator loop. A
cheaper topical distractor already dominates required writer-injection support on lexical
features. The next bounded development experiment first measures raw source → discovered
owner → public candidate packet → final context, then tests task-conditioned support
selection over matched same-owner representations. Structural preservation is justified
only by demonstrated representation loss; model-assisted selection earns separate credit.
No new product agent engine, database, local model or native quality gain is selected by
this failed pilot.

For a fresh run, follow the [context protocol](evaluating-context.md), [independent critic
workflow](testing-usability.md) and maintained cited-Page guide; obtain appropriately
licensed pinned source-only material and independently freeze new questions/gold before
outcomes. Exposed tasks are development thereafter. This historical source/task adaptation
is local evidence, not a newly shipped general WixQA evaluator or a fresh-checkout fixture
dependency. Native HIGH,semantic parity,normalized default activation,25K and full
release remain open; 100K is deferred.

Optional local evidence:`.artifacts/host-transfer-newfamilies-001`; original protocol SHA256 `f92d9d4f2acb9b25c0f5150e8570ab723867a5153a3d4e8acc1054d76bc40c78`,source manifest
`a82c568f4fc99667891d16cf9530734e5e5556a6be64557f3c004a429c855ebc`,task list
`3f886d804f9f6be91c82f6cae622c362998a38a461b0c03c38cebf0debca6782`,initial assessment JSON
`4af5d7ac4051923202522ce9ac088a632b8c7693c239069e6f028ff0ba286bab`. Raw logs/private gold
remain local and are not required to read this tracked summary.
