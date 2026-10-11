# Sources and providers reference

Use [everyday capture/refresh](everyday-use.md#capture-and-refresh-notes) for one
note. This reference covers collections, guarded batches and optional remote work.
Offline capture needs no credentials. Guided setup and consolidated provider/agent
configuration are requested work, not an available setup route yet. The following
private provider file is the current interface.

## Import a collection

Create a JSON Lines input list. Relative paths resolve from the list's directory;
title and media_type are optional. Choose an absent output in an existing directory.

~~~json
{"path":"deployment-notes.md","title":"Atlas deployment notes"}
{"path":"runbooks/rollback.txt","title":"Atlas rollback","media_type":"text/plain"}
~~~

Set DEMO to that input directory and WIKI to your wiki's absolute root:

~~~sh
lwiki --offline --json source import prepare --input-list "$DEMO/inputs.jsonl" --output "$DEMO/import.jsonl"
lwiki --wiki "$WIKI" --offline index rebuild --normalized
lwiki --wiki "$WIKI" --offline --json source import run --manifest "$DEMO/import.jsonl" --key atlas-notes --group-size 4 --max-groups 64
lwiki --wiki "$WIKI" --offline --json source import status --key atlas-notes
lwiki --wiki "$WIKI" --offline --json source import resume --key atlas-notes --max-groups 64
~~~

Import requires explicit normalized activation; the rebuild above changes layout,
so first back up an existing wiki and finish pending changes with their preparing
binary. Preparation needs no wiki and freezes original hashes, paths and extraction
policy. Reuse a key only with the same manifest/group size. Another key creates
new identities even for identical bytes. One invocation processes at most
max-groups; inspect `meta.partial` and `data.completed`. Resume uses the owned
manifest once retained; a failure before retention can require the hinted same-key
run with the original manifest.

Groups contain 1–8 items (default four), with 4 MiB aggregate bytes for multi-item
groups and 64 MiB per input. Larger inputs form size-one groups. Each group shares
one Change/publication; rollback affects the whole group. `--stage` is unsupported.
Manifest ceilings are 128 MiB, 200,000 items and 64 GiB total input, not measured
practical capacity. Progress, owned manifests and results are durable operational
state, not cache. See [storage](../developer/contracts/storage.md#capture-and-lifecycle).

UTF-8 text/Markdown retains exact bytes. HTML/HTM captures retain raw UTF-8 too;
lexical retrieval omits tags, attributes, comments, script/style, while citations
remain raw slices. Literal search can match markup. No browser rendering,
encoding conversion or external extractor occurs. Empty inputs have no nonempty
citation; unsupported formats retain originals without citable text. Media type
does not override filename admission. Frozen older extraction policy remains
unchanged on resume; refresh creates new extraction history rather than filling
an old original-only revision.

## Maintain imported Sources

`results_path` from import status names the append-only JSONL mapping artifact.
Join every `group_committed` event's `result.items` to original manifest items by
zero-based ordinal. Require complete unique coverage and count agreement;
`last_group` alone omits earlier groups. Status success does not certify every
mapping byte. Preserve inconsistent artifacts instead of editing operational state.

Inspect each returned Source/Revision ID and current head. Compare proposed input
with the current capture and extraction/title policy, not just the import's old
hash. Skip/report missing inputs and withdrawn Sources unless another action was
intended. Equal bytes or paths do not establish common identity.

~~~sh
lwiki --wiki "$WIKI" --offline --json read --id "$SOURCE_ID"
lwiki --wiki "$WIKI" --offline --json --stage source refresh "$SOURCE_ID" --file "$INPUT"
lwiki --wiki "$WIKI" --offline changes show "$CHANGE_ID" --operation 0
lwiki --wiki "$WIKI" --offline changes apply "$CHANGE_ID"
~~~

Use actual IDs/input paths. Review complete before/proposed bytes and dependencies;
a prior read/preview does not constrain a later stage. The retained reviewed input,
not a subsequently changed external file, is applied. An unchanged refresh can
reuse its revision without a Change. Keep unresolved outcomes for recovery.
Refresh never rewrites an authored Page. Read fresh cited context, reconcile its
actual facts and gaps, then use a guarded full-Page update as described in
[everyday use](everyday-use.md#create-edit-and-move-a-page).

`source withdraw ID --reason REASON` retains history, disables current support and
does not alter Page prose or immutable bytes. Refresh does not reactivate a withdrawn
Source. Historical access stays labeled; titles/origins never justify merging IDs.
Managed normalized capture/refresh publishes discovery; external wiki edits require
sync. Neither publication nor verified selected reads establish global completeness.

## Refresh a guarded batch

Normalized `source refresh-batch` refreshes 1–16 distinct active Sources in one
retained Change/publication. Inspect `schema source-refresh-batch`. For each member,
use the current Source read's `data.hash` and `wiki_current_revision`, and compute
the exact new input's BLAKE3 digest. Replace all placeholders:

~~~json
{
  "items": [{
    "source_id": "SOURCE_ID",
    "file": "updated.md",
    "if_match": "blake3:<64 lowercase hexadecimal digits>",
    "expected_revision": "REVISION_ID",
    "input_hash": "blake3:<64 lowercase hexadecimal digits>",
    "media_type": "text/markdown"
  }]
}
~~~

Member files resolve relative to the request file. Inputs must be distinct regular
UTF-8 files; duplicate Sources/paths, symlinks, stdin, unknown fields and stale
guards refuse. Request JSON caps 1 MiB and total input 4 MiB. Optional title/media
metadata overrides are explicit. Unchanged members still authenticate guards;
a wholly unchanged batch creates no Change/publication. Historical reuse and no-op
are separately reported.

~~~sh
lwiki --wiki "$WIKI" --offline --json --dry-run source refresh-batch --file REQUEST.json
lwiki --wiki "$WIKI" --offline --json --stage source refresh-batch --file REQUEST.json
lwiki --wiki "$WIKI" --offline changes show "$CHANGE_ID" --summary
lwiki --wiki "$WIKI" --offline --json changes show "$CHANGE_ID" --operation 0
lwiki --wiki "$WIKI" --offline changes apply "$CHANGE_ID"
~~~

Dry-run validates inputs but leaves Source/head/guard admission unresolved.
Summary verifies operation metadata, not payload availability/integrity or current
freshness; it proves neither apply readiness nor undo availability. Review selected
exact operations or full inspection, then apply. Summary and operation flags
conflict. Apply rechecks retained guards, can resume an interrupted exact intent,
and reuses a completed outcome. It does not prepare embeddings or revise a Page.

## Optional remote providers

Remote embeddings/extraction/probes require explicit task authorization and finite
limits. They are separate from host-agent research. `--providers-config` requires
an absolute path outside the wiki; the config and credential files must be private
regular files (for example owner-only 0600). Obtain the actual ID with `wiki show`
and replace all root/ID/credential placeholders:

~~~toml
version = 1
[profiles.primary]
embedding = "embed-small"

[services.embed-small]
adapter = "embeddings-v1"
url = "https://api.openai.com/v1/embeddings"
model = "text-embedding-3-small"
revision = "my-embedding-policy-v1"
dimensions = 1536
max_batch_items = 32
max_batch_bytes = 131072

[services.embed-small.auth]
kind = "static"
key_file = "/absolute/private/token"

[vault_bindings.personal]
root = "/absolute/wiki"
wiki_id = "paste-your-actual-wiki-id"
allowed_profiles = ["primary"]
~~~

Alternatively use key_env and provide that key in the process environment. Never
put secrets in notes, command arguments or public reports. Root/ID binding authorizes
that exact wiki; a copied wiki needs its own explicit binding. With WIKI and
PROVIDERS set to absolute paths, these commands make bounded paid requests:

~~~sh
lwiki --wiki "$WIKI" --json doctor --probe --role embed --providers-config "$PROVIDERS" --profile primary --max-requests 1
lwiki --wiki "$WIKI" --json embeddings sync --providers-config "$PROVIDERS" --profile primary --max-requests 20 --quality-target-bytes 3000 --max-input-bytes 8000
lwiki --wiki "$WIKI" search 'recover from a failed deployment' --mode semantic --providers-config "$PROVIDERS" --profile primary --max-requests 1
~~~

Sync reports coverage. Compatible inputs can reuse retained vectors. An identical
cached query can later run offline; a new query cannot. Dry-run sends no text and
opens no credentials, but authorization/cache state can remain unchecked. Hard
rendered embedding input defaults to 12,000 UTF-8 bytes including prefixes; quality
targets split long text while retaining exact byte mapping and can increase calls.
`embeddings check --offline --json` reports active settings/coverage, or null settings
without active cache. Deleting cache loses reusable vectors.

Request ceilings include retries; each embedding attempt reserves 8 MiB response
allowance. Limits compose with retained Run lifetime usage and unknown charges.
Unknown monetary cost is not zero. Inspect `jobs status --run RUN_ID` before an
explicit `jobs amend --run RUN_ID --reason REASON --max-requests N`. Repeating a
command can continue after its invocation allowance ends, but cannot reset retained
Run limits/deadlines/unknown holds. Retry an uncertain send only explicitly with
`--retry-uncertain`. Changed inputs refuse resend. Preserve accounting in backups.

Generation profiles name a separate service. New generation uses explicit
`adapter = "responses-v1"` with its exact Responses URL; embeddings use
`embeddings-v1`. Existing `chat-completions-v1` remains explicit. Responses sends
store:false, no tools and a user input. JSON-schema mode uses a bounded provider
grammar plus the complete local validator; text-json accepts bare JSON or one JSON
fence, without surrounding prose. Unsupported fields/schema fail rather than
converting endpoints. HTTP success can still contain refused/incomplete/invalid
output, and paid failures remain accounted for. Simple probes do not qualify
extraction schemas, live gateways or ranking quality. Full configuration, adapter,
limits and private diagnostics are in the [provider contract](../developer/contracts/providers-jobs.md).

## Experimental Source relations

Relation preparation is explicitly enabled, normalized-only and disabled by default.
No demonstrated development-corpus completeness improvement qualifies it. Generated
links are unassessed navigation, not accepted assertions; returned evidence remains
exact original Source spans. Refresh/withdrawal invalidates current navigation while
historical preparation survives.

`relations prepare` authenticates complete current Sources and binds a configured
generation profile even offline. `--quote` seals requests with a finite allowance
without credentials, sending or persistent Runs, and requires supported bounds and
a complete valid rate card. A quote is not guaranteed acquisition: attempts or
unknown charges can stop an incomplete batch. Review actual input coverage and
accounting before paid `relations run`. Use exact manifest bytes, the same Run/config
and lifetime limits for continuation. Querying requires explicit
`--experimental-source-relations` with supported lexical indexed-document context;
this does not qualify default context, other modes or automatic answers.

The [relation protocol](../developer/contracts/retrieval.md#experimental-source-relation-preparation)
contains exact opt-in prepare/run/query commands and quote ceilings. Raw responses,
descriptors and receipts remain immutable Run records for offline reconstruction.
Inspect actual citations/coverage warnings; local functional tests do not establish
retrieval completeness, large-wiki capacity or release qualification.
