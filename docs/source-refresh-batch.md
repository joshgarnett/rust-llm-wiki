# Guarded Source refresh batches

On an already normalized vault, `source refresh-batch --file REQUEST.json`
refreshes 1–16 existing active Sources in one retained Change and catalog
publication. Results keep request order. Shared dependencies use the combined
final Source heads; previous revisions and authored Page bytes remain preserved.
See [functional validation](validation-source-refresh-batch.md) for the measured
scope and remaining performance gates.

Inspect the running command and schema before constructing a request:

```sh
lwiki source refresh-batch --help
lwiki schema source-refresh-batch
lwiki --offline --json --wiki DIR read --id SOURCE_ID --max-bytes 65536
```

For each member, copy the observed `data.hash` into `if_match` and
`data.record.wiki_current_revision` into `expected_revision`. Compute `input_hash`
as the BLAKE3 digest of the exact new input bytes. This JSON is a template: replace
the Source/Revision IDs and both hash placeholders with actual observed values.

```json
{
  "items": [
    {
      "source_id": "source_example",
      "file": "updated.md",
      "if_match": "blake3:<64 lowercase hexadecimal digits>",
      "expected_revision": "revision_example",
      "input_hash": "blake3:<64 lowercase hexadecimal digits>",
      "media_type": "text/markdown"
    }
  ]
}
```

Optional `title` and `media_type` override capture metadata. Member file paths
resolve relative to the request file's directory. Inputs must be distinct regular
UTF-8 files; symlinks, stdin, duplicate Sources or paths, unknown fields and stale
guards refuse. Request JSON is limited to 1 MiB and aggregate input bytes to 4 MiB.
An unchanged member still authenticates its Source guards. An entirely unchanged
batch allocates no Change or publication; a historical capture can reuse its
retained Revision, with `reused` and `no_op` reported separately.

Preview, stage and apply through the ordinary commands:

```sh
lwiki --offline --json --wiki DIR --dry-run source refresh-batch --file REQUEST.json
lwiki --offline --json --wiki DIR --stage source refresh-batch --file REQUEST.json
lwiki --offline --wiki DIR changes show CHANGE_ID --summary
lwiki --offline --json --wiki DIR changes show CHANGE_ID --operation 0
lwiki --offline --json --wiki DIR changes apply CHANGE_ID
```

`--summary` reviews authenticated Change metadata: every operation's zero-based
index, path, create/update/delete kind, expected and proposed hashes, declared
retained byte counts, application dependencies and read guards. It does not read
retained file bodies or current targets. Payload availability, payload integrity
and current target freshness are explicitly **not checked**; a summary establishes
neither apply readiness nor undo availability. Its size grows with operation and
guard metadata rather than file contents; there is no universal small-output limit.
Editable note status is diagnostic and cannot override recorded Change status.

Use `--operation N` to inspect the exact before/proposed bytes of a chosen operation,
or omit both flags for the existing full inspection. These endpoints verify retained
bytes separately and return JSON byte arrays. For unresolved Changes they also
perform broader retained-body verification. `--summary` and `--operation` conflict.
Summary inspection is read-only with `--offline` and `--dry-run`; apply continues to
check the original guards and recovery rules.

Dry-run validates the request and input files before accessing the catalog or
writer lock. Source existence, current head and historical reuse remain unresolved
and result fields are null. Staging retains exact inputs; apply does not need the
external files afterward. Apply the returned Change ID after an interruption to
recover that exact intent, and preserve any reported conflict rather than
replanning over changed guards. A repeated completed apply returns the retained
terminal outcome.

Read the resulting Source/Revision and actual citations after applying. A current
capture does not establish that every statement is approved guidance. This
command does not prepare remote embeddings or rewrite an authored answer/Page;
use the existing explicit preparation and reconciliation workflows when needed.
