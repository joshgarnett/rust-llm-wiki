# Implemented lwiki 0.1.2 commands

Generated from the release command registry and argument parser. Run `lwiki --json capabilities` before use.

## capabilities

```text
List implemented commands, schemas and supported modes

Usage: lwiki capabilities [OPTIONS]

Options:
      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## skill export

```text
Write host-specific skill files to a new output directory

Usage: lwiki skill export [OPTIONS] --target <TARGET> --output <OUTPUT>

Options:
      --target <TARGET>
          Host format for the exported skill

          [possible values: codex, claude-code, cursor]

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --output <OUTPUT>
          New directory for exported skill files; existing files are not overwritten

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## schema

```text
Print a published JSON input or output schema

Usage: lwiki schema [OPTIONS] <NAME>

Arguments:
  <NAME>
          Published schema name; inspect capabilities for available schemas

Options:
      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## init

```text
Create a new Markdown wiki in a directory that does not exist

Usage: lwiki init [OPTIONS] <PATH>

Arguments:
  <PATH>
          New wiki directory; initialization refuses an existing path

Options:
      --title <TITLE>
          Human-readable title

          [default: "Local wiki"]

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## read

```text
Read a record by ID or vault-relative path

Usage: lwiki read [OPTIONS] <--id <ID>|--path <PATH>>

Options:
      --id <ID>
          Select a record by its stable ID

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --path <PATH>
          Select a record by its vault-relative path

      --json
          Emit one structured JSON envelope

      --max-bytes <MAX_BYTES>
          Maximum UTF-8 bytes to return

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --start <START>
          Start of the zero-based, half-open UTF-8 byte range

      --end <END>
          End of the zero-based, half-open UTF-8 byte range

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --no-sync
          Read the existing index snapshot without syncing; freshness is not verified

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## page put

```text
Create a page or replace one using its expected content hash

Usage: lwiki page put [OPTIONS] --file <FILE>

Options:
      --file <FILE>
          Markdown page file with a valid page envelope; use - for stdin

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --path <PATH>
          Destination vault-relative path; defaults to pages/<record-id>.md

      --if-match <IF_MATCH>
          Required current BLAKE3 hash when replacing an existing page

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## page init

```text
Initialize plain Markdown with a stable page identity and draft envelope

Usage: lwiki page init [OPTIONS] --file <FILE> --title <TITLE>

Options:
      --file <FILE>
          Plain UTF-8 Markdown body; use - for bounded standard input

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --title <TITLE>
          Human-readable title for the new draft page

      --id <ID>
          Stable page identity; omit to allocate one

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --path <PATH>
          New vault-relative path; defaults to pages/<record-id>.md

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## page batch

```text
Prepare/apply 1–16 coupled page updates, each with its own author hash

Usage: lwiki page batch [OPTIONS] --file <FILE>

Options:
      --file <FILE>
          JSON request matching schema page-batch; use - for stdin

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## page rename

```text
Move a page and update known links while preserving its ID

Usage: lwiki page rename [OPTIONS] --to <TO> --if-match <IF_MATCH> <ID>

Arguments:
  <ID>
          Stable record or changeset ID returned by an earlier command

Options:
      --to <TO>
          New vault-relative page path

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --if-match <IF_MATCH>
          Expected current BLAKE3 content hash; reject intervening edits

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## source add

```text
Capture a local file as a new source with an immutable revision

Usage: lwiki source add [OPTIONS] <FILE>

Arguments:
  <FILE>
          Input file; use - to read bounded standard input

Options:
      --title <TITLE>
          Human-readable title

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --media-type <MEDIA_TYPE>
          Explicit media type for the captured input

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## source refresh

```text
Capture changed content as a new revision of an existing source

Usage: lwiki source refresh [OPTIONS] --file <FILE> <ID>

Arguments:
  <ID>
          Stable record or changeset ID returned by an earlier command

Options:
      --file <FILE>
          Input file; use - to read bounded standard input

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --title <TITLE>
          Human-readable title

      --json
          Emit one structured JSON envelope

      --media-type <MEDIA_TYPE>
          Explicit media type for the captured input

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## source withdraw

```text
Withdraw a source from current support while retaining its history

Usage: lwiki source withdraw [OPTIONS] --reason <REASON> <ID>

Arguments:
  <ID>
          Stable record or changeset ID returned by an earlier command

Options:
      --reason <REASON>
          Reason recorded with the withdrawal

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## evidence revalidate

```text
Stage successor evidence when its quotation uniquely matches the target revision

Usage: lwiki evidence revalidate [OPTIONS] --to-revision <TO_REVISION> --if-match <IF_MATCH> <ID>

Arguments:
  <ID>
          Stable record or changeset ID returned by an earlier command

Options:
      --to-revision <TO_REVISION>
          Target immutable source revision ID

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --if-match <IF_MATCH>
          Expected current BLAKE3 content hash; reject intervening edits

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## index sync

```text
Refresh the index from changed Markdown and source records

Usage: lwiki index sync [OPTIONS]

Options:
      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## index rebuild

```text
Recreate the disposable index from canonical files without provider calls

Usage: lwiki index rebuild [OPTIONS]

Options:
      --normalized
          Use the normalized catalog; currently supports cached lexical reads and source refresh

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## embeddings check

```text
Inspect local embedding coverage; --probe explicitly contacts the provider

Usage: lwiki embeddings check [OPTIONS]

Options:
      --document-prefix <DOCUMENT_PREFIX>
          Text prepended to document embedding inputs; part of the embedding space identity

          [default: ""]

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --query-prefix <QUERY_PREFIX>
          Text prepended to query embedding inputs; part of the embedding space identity

          [default: ""]

      --json
          Emit one structured JSON envelope

      --max-input-bytes <MAX_INPUT_BYTES>
          Maximum UTF-8 bytes per embedding input

          [default: 12000]

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --quality-target-bytes <QUALITY_TARGET_BYTES>
          Preferred segment size below the hard embedding input ceiling

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --dry-run
          Preview without writes, provider requests or credential resolution

      --max-requests <MAX_REQUESTS>
          Lifetime request-attempt ceiling, including retries

      --concurrency <CONCURRENCY>
          Maximum simultaneous provider requests

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --attempts-per-task <ATTEMPTS_PER_TASK>
          Maximum attempts for an individual provider task

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --deadline-ms <DEADLINE_MS>
          Overall remote operation deadline in milliseconds from startup

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

      --max-request-bytes <MAX_REQUEST_BYTES>
          Lifetime ceiling on outgoing request bytes

      --max-response-bytes <MAX_RESPONSE_BYTES>
          Lifetime ceiling on incoming response bytes

      --max-input-units <MAX_INPUT_UNITS>
          Lifetime ceiling for each input and cached-input billable class

      --max-output-units <MAX_OUTPUT_UNITS>
          Lifetime ceiling for each output and reasoning billable class

      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          Currency code used with --max-cost

          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>
          Maximum provider requests dispatched per minute

      --tokens-per-minute <TOKENS_PER_MINUTE>
          Maximum accounted provider tokens per minute

      --retry-uncertain
          Opt into retrying uncertain work under retained accounting; prior attempts may be billed

      --retain-http-error-body
          Retain a bounded private HTTP error-body diagnostic for explicit inspection

      --probe
          Explicitly contact the selected provider within the supplied request limits

  -h, --help
          Print help

```

## embeddings sync

```text
Generate missing embeddings through a trusted remote provider

Usage: lwiki embeddings sync [OPTIONS]

Options:
      --document-prefix <DOCUMENT_PREFIX>
          Text prepended to document embedding inputs; part of the embedding space identity

          [default: ""]

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --query-prefix <QUERY_PREFIX>
          Text prepended to query embedding inputs; part of the embedding space identity

          [default: ""]

      --json
          Emit one structured JSON envelope

      --max-input-bytes <MAX_INPUT_BYTES>
          Maximum UTF-8 bytes per embedding input

          [default: 12000]

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --quality-target-bytes <QUALITY_TARGET_BYTES>
          Preferred segment size below the hard embedding input ceiling

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --dry-run
          Preview without writes, provider requests or credential resolution

      --max-requests <MAX_REQUESTS>
          Lifetime request-attempt ceiling, including retries

      --concurrency <CONCURRENCY>
          Maximum simultaneous provider requests

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --attempts-per-task <ATTEMPTS_PER_TASK>
          Maximum attempts for an individual provider task

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --deadline-ms <DEADLINE_MS>
          Overall remote operation deadline in milliseconds from startup

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

      --max-request-bytes <MAX_REQUEST_BYTES>
          Lifetime ceiling on outgoing request bytes

      --max-response-bytes <MAX_RESPONSE_BYTES>
          Lifetime ceiling on incoming response bytes

      --max-input-units <MAX_INPUT_UNITS>
          Lifetime ceiling for each input and cached-input billable class

      --max-output-units <MAX_OUTPUT_UNITS>
          Lifetime ceiling for each output and reasoning billable class

      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          Currency code used with --max-cost

          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>
          Maximum provider requests dispatched per minute

      --tokens-per-minute <TOKENS_PER_MINUTE>
          Maximum accounted provider tokens per minute

      --retry-uncertain
          Opt into retrying uncertain work under retained accounting; prior attempts may be billed

      --retain-http-error-body
          Retain a bounded private HTTP error-body diagnostic for explicit inspection

  -h, --help
          Print help

```

## search

```text
Find text with literal, lexical, semantic or hybrid retrieval

Usage: lwiki search [OPTIONS] <QUERY>

Arguments:
  <QUERY>
          Search text; lexical terms are treated as data, not query operators

Options:
      --graph <GRAPH>
          Include graph evidence ranks in hybrid search

          [possible values: entities]

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --json
          Emit one structured JSON envelope

      --max-requests <MAX_REQUESTS>
          Lifetime request-attempt ceiling, including retries

      --concurrency <CONCURRENCY>
          Maximum simultaneous provider requests

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --attempts-per-task <ATTEMPTS_PER_TASK>
          Maximum attempts for an individual provider task

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --deadline-ms <DEADLINE_MS>
          Overall remote operation deadline in milliseconds from startup

      --dry-run
          Preview without writes, provider requests or credential resolution

      --max-request-bytes <MAX_REQUEST_BYTES>
          Lifetime ceiling on outgoing request bytes

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --max-response-bytes <MAX_RESPONSE_BYTES>
          Lifetime ceiling on incoming response bytes

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-input-units <MAX_INPUT_UNITS>
          Lifetime ceiling for each input and cached-input billable class

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

      --max-output-units <MAX_OUTPUT_UNITS>
          Lifetime ceiling for each output and reasoning billable class

      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          Currency code used with --max-cost

          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>
          Maximum provider requests dispatched per minute

      --tokens-per-minute <TOKENS_PER_MINUTE>
          Maximum accounted provider tokens per minute

      --retry-uncertain
          Opt into retrying uncertain work under retained accounting; prior attempts may be billed

      --retain-http-error-body
          Retain a bounded private HTTP error-body diagnostic for explicit inspection

      --lexical-fallback
          Use lexical results when compatible embeddings are unavailable

      --mode <MODE>
          Retrieval mode; semantic and hybrid modes require compatible embeddings

          [default: lexical]
          [possible values: literal, lexical, semantic, hybrid]

      --kind <KINDS>
          Filter by record kind; repeat for multiple kinds

      --tag <TAGS>
          Filter by tag; repeat for multiple tags

      --source-id <SOURCE_IDS>
          Filter by source ID before applying result limits; repeat as needed

      --path-prefix <PATH_PREFIX>
          Restrict results to a vault-relative path prefix

      --status <AUTHORED_STATUSES>
          Filter by authored status; this does not change derived eligibility

      --include-proposed
          Include labeled proposed assertions without accepting them

      --include-historical
          Include labeled historical, withdrawn and otherwise ineligible records

      --limit <LIMIT>
          Maximum result count

          [default: 10]

      --candidates <CANDIDATES>
          Maximum ranked candidates considered before final selection

          [default: 80]

      --excerpt-bytes <EXCERPT_BYTES>
          Maximum UTF-8 bytes in each excerpt (default: search 240, context 1024)

      --cursor <CURSOR>
          Continuation cursor from an identical query on the same index generation

      --no-sync
          Read the existing index snapshot without syncing; freshness is not verified

  -h, --help
          Print help

```

## context

```text
Assemble cited context within byte and token budgets

Usage: lwiki context [OPTIONS] <QUERY>

Arguments:
  <QUERY>
          Search text; lexical terms are treated as data, not query operators

Options:
      --graph <GRAPH>
          Include graph evidence ranks in hybrid search

          [possible values: entities]

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --json
          Emit one structured JSON envelope

      --max-requests <MAX_REQUESTS>
          Lifetime request-attempt ceiling, including retries

      --concurrency <CONCURRENCY>
          Maximum simultaneous provider requests

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --attempts-per-task <ATTEMPTS_PER_TASK>
          Maximum attempts for an individual provider task

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --deadline-ms <DEADLINE_MS>
          Overall remote operation deadline in milliseconds from startup

      --dry-run
          Preview without writes, provider requests or credential resolution

      --max-request-bytes <MAX_REQUEST_BYTES>
          Lifetime ceiling on outgoing request bytes

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --max-response-bytes <MAX_RESPONSE_BYTES>
          Lifetime ceiling on incoming response bytes

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-input-units <MAX_INPUT_UNITS>
          Lifetime ceiling for each input and cached-input billable class

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

      --max-output-units <MAX_OUTPUT_UNITS>
          Lifetime ceiling for each output and reasoning billable class

      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          Currency code used with --max-cost

          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>
          Maximum provider requests dispatched per minute

      --tokens-per-minute <TOKENS_PER_MINUTE>
          Maximum accounted provider tokens per minute

      --retry-uncertain
          Opt into retrying uncertain work under retained accounting; prior attempts may be billed

      --retain-http-error-body
          Retain a bounded private HTTP error-body diagnostic for explicit inspection

      --lexical-fallback
          Use lexical results when compatible embeddings are unavailable

      --mode <MODE>
          Retrieval mode; semantic and hybrid modes require compatible embeddings

          [default: lexical]
          [possible values: literal, lexical, semantic, hybrid]

      --kind <KINDS>
          Filter by record kind; repeat for multiple kinds

      --tag <TAGS>
          Filter by tag; repeat for multiple tags

      --source-id <SOURCE_IDS>
          Filter by source ID before applying result limits; repeat as needed

      --path-prefix <PATH_PREFIX>
          Restrict results to a vault-relative path prefix

      --status <AUTHORED_STATUSES>
          Filter by authored status; this does not change derived eligibility

      --include-proposed
          Include labeled proposed assertions without accepting them

      --include-historical
          Include labeled historical, withdrawn and otherwise ineligible records

      --limit <LIMIT>
          Maximum result count

          [default: 10]

      --candidates <CANDIDATES>
          Maximum ranked candidates considered before final selection

          [default: 80]

      --excerpt-bytes <EXCERPT_BYTES>
          Maximum UTF-8 bytes in each excerpt (default: search 240, context 1024)

      --cursor <CURSOR>
          Continuation cursor from an identical query on the same index generation

      --no-sync
          Read the existing index snapshot without syncing; freshness is not verified

      --prepare-selection
          Prepare a bounded candidate packet for one host-agent selection; does not run a model

      --selection <FILE>
          Apply an ID-only host reply to the exact current candidate packet (file or - for stdin)

      --scope <SCOPE>
          Evidence scope: current, historical, snapshot, or indexed-evidence (captured sources only)

          [default: current]
          [possible values: current, historical, snapshot, indexed-evidence]

      --target <TARGET>
          Retrieve document passages, graph evidence or both

          [default: documents]
          [possible values: documents, graph, combined]

      --strategy <STRATEGY>
          Rank entities, relationships or both when traversing the graph

          [default: combined]
          [possible values: entity, relationship, combined]

      --seed <SEED>
          Seed the graph with lexical matches or compatible semantic embeddings

          [default: lexical]
          [possible values: lexical, semantic]

      --seeds <SEEDS>
          Maximum graph seed records

          [default: 12]

      --depth <DEPTH>
          Maximum graph traversal depth

          [default: 1]

      --incident-per-seed <INCIDENT_PER_SEED>
          Maximum incident assertions examined per graph seed

          [default: 16]

      --assertions <ASSERTIONS>
          Maximum assertions examined during graph traversal

          [default: 128]

      --navigation
          Include separately labeled page and provenance links

      --max-bytes <MAX_BYTES>
          Maximum total context bytes, including reserved instructions and output

          [default: 12000]

      --max-tokens <MAX_TOKENS>
          Maximum estimated context tokens, including reserved instructions and output

          [default: 3000]

      --instruction-bytes <INSTRUCTION_BYTES>
          Bytes reserved for caller instructions within the context ceiling

          [default: 0]

      --instruction-tokens <INSTRUCTION_TOKENS>
          Estimated tokens reserved for caller instructions

          [default: 0]

      --output-bytes <OUTPUT_BYTES>
          Bytes reserved for the model response within the context ceiling

          [default: 0]

      --output-tokens <OUTPUT_TOKENS>
          Estimated tokens reserved for the model response

          [default: 0]

      --verification-max-bytes <VERIFICATION_MAX_BYTES>
          Maximum bytes read while verifying context evidence

          [default: 67108864]

      --verification-max-files <VERIFICATION_MAX_FILES>
          Maximum files read while verifying context evidence

          [default: 4096]

      --verification-max-entries <VERIFICATION_MAX_ENTRIES>
          Maximum directory entries inspected during evidence verification

          [default: 16384]

      --verification-max-elapsed-ms <VERIFICATION_MAX_ELAPSED_MS>
          Maximum elapsed milliseconds for evidence verification

          [default: 2000]

  -h, --help
          Print help

```

## graph extract

```text
Export an agent packet or run bounded API extraction; never accept assertions

Usage: lwiki graph extract [OPTIONS] --source-id <SOURCE_ID>

Options:
      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --max-requests <MAX_REQUESTS>
          Lifetime request-attempt ceiling, including retries

      --concurrency <CONCURRENCY>
          Maximum simultaneous provider requests

      --json
          Emit one structured JSON envelope

      --attempts-per-task <ATTEMPTS_PER_TASK>
          Maximum attempts for an individual provider task

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --deadline-ms <DEADLINE_MS>
          Overall remote operation deadline in milliseconds from startup

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --max-request-bytes <MAX_REQUEST_BYTES>
          Lifetime ceiling on outgoing request bytes

      --max-response-bytes <MAX_RESPONSE_BYTES>
          Lifetime ceiling on incoming response bytes

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --max-input-units <MAX_INPUT_UNITS>
          Lifetime ceiling for each input and cached-input billable class

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-output-units <MAX_OUTPUT_UNITS>
          Lifetime ceiling for each output and reasoning billable class

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          Currency code used with --max-cost

          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>
          Maximum provider requests dispatched per minute

      --tokens-per-minute <TOKENS_PER_MINUTE>
          Maximum accounted provider tokens per minute

      --retry-uncertain
          Opt into retrying uncertain work under retained accounting; prior attempts may be billed

      --retain-http-error-body
          Retain a bounded private HTTP error-body diagnostic for explicit inspection

      --run <RUN>
          Existing extraction run ID to resume with retained accounting

      --max-output-tokens <MAX_OUTPUT_TOKENS>
          Maximum generated output tokens for API extraction

          [default: 4096]

      --new-extraction
          Explicitly permit a separate extraction for a different response to the same packet

      --source-id <SOURCE_ID>
          Captured source ID to extract from

      --revision-id <REVISION_ID>
          Explicit source revision ID; defaults to the source head

      --executor <EXECUTOR>
          Export a packet for an agent or execute through a trusted generation API

          [default: agent]
          [possible values: agent, api]

      --window <WINDOWS>
          Exact UTF-8 source byte window START:END; repeat up to16 times

      --max-mentions <MAX_MENTIONS>
          Maximum source-local mentions allowed in the extraction response

          [default: 64]

      --max-assertions <MAX_ASSERTIONS>
          Maximum assertions allowed in the extraction response

          [default: 128]

      --max-output-bytes <MAX_OUTPUT_BYTES>
          Maximum extraction response bytes

          [default: 262144]

      --candidate-id <CANDIDATE_IDS>
          Existing entity ID to include as candidate context; repeat as needed

  -h, --help
          Print help

```

## graph import

```text
Validate a packet-bound extraction response and stage proposed records

Usage: lwiki graph import [OPTIONS] --file <FILE>

Options:
      --file <FILE>
          Packet-bound extraction response JSON; use - for stdin

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --new-extraction
          Explicitly permit a separate extraction for a different response to the same packet

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## graph resolve

```text
Stage explicit mention bindings or new entities using expected hashes

Usage: lwiki graph resolve [OPTIONS] --file <FILE>

Options:
      --file <FILE>
          Strict lwiki.graph-resolution.v1 JSON; use - for stdin

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## graph decide

```text
Stage explicit entity merges, splits or aliases with complete remaps

Usage: lwiki graph decide [OPTIONS] --file <FILE>

Options:
      --file <FILE>
          Strict lwiki.entity-decisions.v1 JSON; use - for stdin

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## graph review

```text
Stage assertion decisions and assessments of every active evidence item

Usage: lwiki graph review [OPTIONS] --file <FILE>

Options:
      --file <FILE>
          Strict lwiki.graph-review.v1 JSON; use - for stdin

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## graph query

```text
Search and traverse eligible graph assertions with cited evidence

Usage: lwiki graph query [OPTIONS] <QUERY>

Arguments:
  <QUERY>
          Search text; lexical terms are treated as data, not query operators

Options:
      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --max-requests <MAX_REQUESTS>
          Lifetime request-attempt ceiling, including retries

      --concurrency <CONCURRENCY>
          Maximum simultaneous provider requests

      --json
          Emit one structured JSON envelope

      --attempts-per-task <ATTEMPTS_PER_TASK>
          Maximum attempts for an individual provider task

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --deadline-ms <DEADLINE_MS>
          Overall remote operation deadline in milliseconds from startup

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --max-request-bytes <MAX_REQUEST_BYTES>
          Lifetime ceiling on outgoing request bytes

      --max-response-bytes <MAX_RESPONSE_BYTES>
          Lifetime ceiling on incoming response bytes

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --max-input-units <MAX_INPUT_UNITS>
          Lifetime ceiling for each input and cached-input billable class

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-output-units <MAX_OUTPUT_UNITS>
          Lifetime ceiling for each output and reasoning billable class

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          Currency code used with --max-cost

          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>
          Maximum provider requests dispatched per minute

      --tokens-per-minute <TOKENS_PER_MINUTE>
          Maximum accounted provider tokens per minute

      --retry-uncertain
          Opt into retrying uncertain work under retained accounting; prior attempts may be billed

      --retain-http-error-body
          Retain a bounded private HTTP error-body diagnostic for explicit inspection

      --lexical-fallback
          Use lexical results when compatible embeddings are unavailable

      --strategy <STRATEGY>
          Rank entities, relationships or both when traversing the graph

          [default: combined]
          [possible values: entity, relationship, combined]

      --seed <SEED>
          Seed the graph with lexical matches or compatible semantic embeddings

          [default: lexical]
          [possible values: lexical, semantic]

      --kind <KINDS>
          Filter by record kind; repeat for multiple kinds

      --tag <TAGS>
          Filter by tag; repeat for multiple tags

      --source-id <SOURCE_IDS>
          Filter by source ID before applying result limits; repeat as needed

      --path-prefix <PATH_PREFIX>
          Restrict results to a vault-relative path prefix

      --status <AUTHORED_STATUSES>
          Filter by authored status; this does not change derived eligibility

      --include-proposed
          Include labeled proposed assertions without accepting them

      --include-historical
          Include labeled historical, withdrawn and otherwise ineligible records

      --navigation
          Include separately labeled page and provenance links

      --candidates <CANDIDATES>
          Maximum ranked candidates considered before final selection

          [default: 80]

      --seeds <SEEDS>
          Maximum graph seed records

          [default: 12]

      --depth <DEPTH>
          Maximum graph traversal depth

          [default: 1]

      --incident-per-seed <INCIDENT_PER_SEED>
          Maximum incident assertions examined per graph seed

          [default: 16]

      --assertions <ASSERTIONS>
          Maximum assertions examined during graph traversal

          [default: 128]

      --limit <LIMIT>
          Maximum result count

          [default: 10]

      --excerpt-bytes <EXCERPT_BYTES>
          Maximum UTF-8 bytes in each excerpt

          [default: 240]

      --support-per-assertion <SUPPORT_PER_ASSERTION>
          Maximum supporting evidence items returned per assertion

          [default: 2]

      --contradictions-per-assertion <CONTRADICTIONS_PER_ASSERTION>
          Maximum contradicting evidence items returned per assertion

          [default: 1]

      --cursor <CURSOR>
          Continuation cursor from an identical query on the same index generation

      --no-sync
          Read the existing index snapshot without syncing; freshness is not verified

  -h, --help
          Print help

```

## graph neighbors

```text
Inspect bounded assertion and navigation links around a record

Usage: lwiki graph neighbors [OPTIONS] <ID>

Arguments:
  <ID>
          Stable record or changeset ID returned by an earlier command

Options:
      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --max-requests <MAX_REQUESTS>
          Lifetime request-attempt ceiling, including retries

      --concurrency <CONCURRENCY>
          Maximum simultaneous provider requests

      --json
          Emit one structured JSON envelope

      --attempts-per-task <ATTEMPTS_PER_TASK>
          Maximum attempts for an individual provider task

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --deadline-ms <DEADLINE_MS>
          Overall remote operation deadline in milliseconds from startup

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --max-request-bytes <MAX_REQUEST_BYTES>
          Lifetime ceiling on outgoing request bytes

      --max-response-bytes <MAX_RESPONSE_BYTES>
          Lifetime ceiling on incoming response bytes

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --max-input-units <MAX_INPUT_UNITS>
          Lifetime ceiling for each input and cached-input billable class

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-output-units <MAX_OUTPUT_UNITS>
          Lifetime ceiling for each output and reasoning billable class

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          Currency code used with --max-cost

          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>
          Maximum provider requests dispatched per minute

      --tokens-per-minute <TOKENS_PER_MINUTE>
          Maximum accounted provider tokens per minute

      --retry-uncertain
          Opt into retrying uncertain work under retained accounting; prior attempts may be billed

      --retain-http-error-body
          Retain a bounded private HTTP error-body diagnostic for explicit inspection

      --lexical-fallback
          Use lexical results when compatible embeddings are unavailable

      --strategy <STRATEGY>
          Rank entities, relationships or both when traversing the graph

          [default: combined]
          [possible values: entity, relationship, combined]

      --seed <SEED>
          Seed the graph with lexical matches or compatible semantic embeddings

          [default: lexical]
          [possible values: lexical, semantic]

      --kind <KINDS>
          Filter by record kind; repeat for multiple kinds

      --tag <TAGS>
          Filter by tag; repeat for multiple tags

      --source-id <SOURCE_IDS>
          Filter by source ID before applying result limits; repeat as needed

      --path-prefix <PATH_PREFIX>
          Restrict results to a vault-relative path prefix

      --status <AUTHORED_STATUSES>
          Filter by authored status; this does not change derived eligibility

      --include-proposed
          Include labeled proposed assertions without accepting them

      --include-historical
          Include labeled historical, withdrawn and otherwise ineligible records

      --navigation
          Include separately labeled page and provenance links

      --candidates <CANDIDATES>
          Maximum ranked candidates considered before final selection

          [default: 80]

      --seeds <SEEDS>
          Maximum graph seed records

          [default: 12]

      --depth <DEPTH>
          Maximum graph traversal depth

          [default: 1]

      --incident-per-seed <INCIDENT_PER_SEED>
          Maximum incident assertions examined per graph seed

          [default: 16]

      --assertions <ASSERTIONS>
          Maximum assertions examined during graph traversal

          [default: 128]

      --limit <LIMIT>
          Maximum result count

          [default: 10]

      --excerpt-bytes <EXCERPT_BYTES>
          Maximum UTF-8 bytes in each excerpt

          [default: 240]

      --support-per-assertion <SUPPORT_PER_ASSERTION>
          Maximum supporting evidence items returned per assertion

          [default: 2]

      --contradictions-per-assertion <CONTRADICTIONS_PER_ASSERTION>
          Maximum contradicting evidence items returned per assertion

          [default: 1]

      --cursor <CURSOR>
          Continuation cursor from an identical query on the same index generation

      --no-sync
          Read the existing index snapshot without syncing; freshness is not verified

  -h, --help
          Print help

```

## check

```text
Check canonical records, references and source integrity

Usage: lwiki check [OPTIONS]

Options:
      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## doctor

```text
Inspect local status without a full audit; probe providers only with --probe

Usage: lwiki doctor [OPTIONS]

Options:
      --probe
          Explicitly contact the selected provider within the supplied request limits

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --role <ROLE>
          Provider capability to probe: embed or generate

          [default: embed]
          [possible values: embed, generate]

      --extraction-schema
          Probe the real extraction JSON schema; requires --probe --role generate

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --max-requests <MAX_REQUESTS>
          Lifetime request-attempt ceiling, including retries

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --concurrency <CONCURRENCY>
          Maximum simultaneous provider requests

      --dry-run
          Preview without writes, provider requests or credential resolution

      --attempts-per-task <ATTEMPTS_PER_TASK>
          Maximum attempts for an individual provider task

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --deadline-ms <DEADLINE_MS>
          Overall remote operation deadline in milliseconds from startup

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-request-bytes <MAX_REQUEST_BYTES>
          Lifetime ceiling on outgoing request bytes

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

      --max-response-bytes <MAX_RESPONSE_BYTES>
          Lifetime ceiling on incoming response bytes

      --max-input-units <MAX_INPUT_UNITS>
          Lifetime ceiling for each input and cached-input billable class

      --max-output-units <MAX_OUTPUT_UNITS>
          Lifetime ceiling for each output and reasoning billable class

      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          Currency code used with --max-cost

          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>
          Maximum provider requests dispatched per minute

      --tokens-per-minute <TOKENS_PER_MINUTE>
          Maximum accounted provider tokens per minute

      --retry-uncertain
          Opt into retrying uncertain work under retained accounting; prior attempts may be billed

      --retain-http-error-body
          Retain a bounded private HTTP error-body diagnostic for explicit inspection

  -h, --help
          Print help

```

## research plan

```text
Preview agent tasks and current passages without persisting a run

Usage: lwiki research plan [OPTIONS] <QUESTION>

Arguments:
  <QUESTION>
          Question for the host agent to investigate

Options:
      --url <URLS>
          Suggested origin for the host agent; lwiki does not fetch it

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --exclude <EXCLUSIONS>
          Scope exclusion to include in the agent packet

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --source-id <SOURCE_IDS>
          Include the current captured text of this source; repeat as needed

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --source-range <SOURCE_RANGES>
          Exact current UTF-8 source range SOURCE:START:END; repeat for selected passages

      --max-rounds <MAX_ROUNDS>
          Maximum collection/answer rounds (1–8), including the initial round

          [default: 3]

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --max-sources <MAX_SOURCES>
          Maximum total sources accepted from the agent (0–64)

          [default: 15]

      --max-source-bytes <MAX_SOURCE_BYTES>
          Maximum lifetime source-content bytes accepted locally (up to 4 MiB)

          [default: 524288]

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --run-id <RUN_ID>
          New run ID; omit to allocate one. Existing IDs require resume

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## research run

```text
Start a local research handoff for an agent with its own tools

Usage: lwiki research run [OPTIONS] <QUESTION>

Arguments:
  <QUESTION>
          Question for the host agent to investigate

Options:
      --url <URLS>
          Suggested origin for the host agent; lwiki does not fetch it

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --exclude <EXCLUSIONS>
          Scope exclusion to include in the agent packet

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --source-id <SOURCE_IDS>
          Include the current captured text of this source; repeat as needed

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --source-range <SOURCE_RANGES>
          Exact current UTF-8 source range SOURCE:START:END; repeat for selected passages

      --max-rounds <MAX_ROUNDS>
          Maximum collection/answer rounds (1–8), including the initial round

          [default: 3]

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --max-sources <MAX_SOURCES>
          Maximum total sources accepted from the agent (0–64)

          [default: 15]

      --max-source-bytes <MAX_SOURCE_BYTES>
          Maximum lifetime source-content bytes accepted locally (up to 4 MiB)

          [default: 524288]

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --run-id <RUN_ID>
          New run ID; omit to allocate one. Existing IDs require resume

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## research resume

```text
Return the outstanding agent packet; never execute tools

Usage: lwiki research resume [OPTIONS] <RUN_ID>

Arguments:
  <RUN_ID>
          Research run ID returned by run

Options:
      --refresh
          Replace a stale packet using current sources; invalidate its old fingerprint

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## research import

```text
Validate an agent submission and publish captures/report through guarded recovery

Usage: lwiki research import [OPTIONS] --file <FILE>

Options:
      --file <FILE>
          JSON submission path, or - for standard input

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## research status

```text
Read local handoff progress and remaining work

Usage: lwiki research status [OPTIONS] <RUN_ID>

Arguments:
  <RUN_ID>
          Research run ID returned by run

Options:
      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## research report

```text
Read the most recent imported answer and its unresolved gaps

Usage: lwiki research report [OPTIONS] <RUN_ID>

Arguments:
  <RUN_ID>
          Research run ID returned by run

Options:
      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## research maintenance

```text
List concrete host repair tasks for stale support, contradictions and missing synthesis

Usage: lwiki research maintenance [OPTIONS] <RUN_ID>

Arguments:
  <RUN_ID>
          Research run ID whose sources and retained answers define repair scope

Options:
      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## storage inventory

```text
Measure bounded file, logical-byte and duplicate-content totals

Usage: lwiki storage inventory [OPTIONS]

Options:
      --retain-undo-changes <RETAIN_UNDO_CHANGES>
          Completed changes to retain for ordinary undo; dependencies remain protected

          [default: 20]

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --max-files <MAX_FILES>
          Maximum physical files to inspect; partial inventory is disclosed

          [default: 100000]

      --json
          Emit one structured JSON envelope

      --max-bytes <MAX_BYTES>
          Maximum aggregate bytes to hash during inventory and cleanup planning

          [default: 1073741824]

      --expected-plan <EXPECTED_PLAN>
          Bind cleanup to the exact hash returned by storage plan

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## storage plan

```text
Preview exact retained/protected data and migration blockers without writes

Usage: lwiki storage plan [OPTIONS]

Options:
      --retain-undo-changes <RETAIN_UNDO_CHANGES>
          Completed changes to retain for ordinary undo; dependencies remain protected

          [default: 20]

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --max-files <MAX_FILES>
          Maximum physical files to inspect; partial inventory is disclosed

          [default: 100000]

      --json
          Emit one structured JSON envelope

      --max-bytes <MAX_BYTES>
          Maximum aggregate bytes to hash during inventory and cleanup planning

          [default: 1073741824]

      --expected-plan <EXPECTED_PLAN>
          Bind cleanup to the exact hash returned by storage plan

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## storage cleanup

```text
Migrate and compact retained operational data; keep full-vault backups

Usage: lwiki storage cleanup [OPTIONS]

Options:
      --retain-undo-changes <RETAIN_UNDO_CHANGES>
          Completed changes to retain for ordinary undo; dependencies remain protected

          [default: 20]

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --max-files <MAX_FILES>
          Maximum physical files to inspect; partial inventory is disclosed

          [default: 100000]

      --json
          Emit one structured JSON envelope

      --max-bytes <MAX_BYTES>
          Maximum aggregate bytes to hash during inventory and cleanup planning

          [default: 1073741824]

      --expected-plan <EXPECTED_PLAN>
          Bind cleanup to the exact hash returned by storage plan

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## changes show

```text
Inspect a retained changeset and its exact proposed operations

Usage: lwiki changes show [OPTIONS] <ID>

Arguments:
  <ID>
          Stable record or changeset ID returned by an earlier command

Options:
      --operation <OPERATION>
          Zero-based operation index to inspect within the changeset

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## changes apply

```text
Apply a prepared changeset with expected-hash guards and recovery

Usage: lwiki changes apply [OPTIONS] <ID>

Arguments:
  <ID>
          Stable record or changeset ID returned by an earlier command

Options:
      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## changes abort

```text
Discard an unapplied preparation while retaining its history

Usage: lwiki changes abort [OPTIONS] <ID>

Arguments:
  <ID>
          Stable record or changeset ID returned by an earlier command

Options:
      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## changes rollback

```text
Stage a guarded inverse; immutable source captures remain retained

Usage: lwiki changes rollback [OPTIONS] <ID>

Arguments:
  <ID>
          Stable record or changeset ID returned by an earlier command

Options:
      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## changes resolve

```text
Resolve a durable conflict using an inspected, hash-bound request

Usage: lwiki changes resolve [OPTIONS] <ID>

Arguments:
  <ID>
          Conflicted changeset ID to inspect or resolve

Options:
      --mode <MODE>
          Produce a resolution request with --dry-run

          [default: resume]
          [possible values: resume, abandon]

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --file <FILE>
          JSON request returned by changes resolve --dry-run; use - for stdin

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## jobs amend

```text
Explicitly preserve or raise cumulative limits of a planned/paused/stopped job

Usage: lwiki jobs amend [OPTIONS] --run <RUN> --reason <REASON>

Options:
      --run <RUN>
          Retained provider run ID whose cumulative limits will be raised

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --reason <REASON>
          Brief explanation retained with the amendment

      --json
          Emit one structured JSON envelope

      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --max-requests <MAX_REQUESTS>
          Lifetime request-attempt ceiling, including retries

      --concurrency <CONCURRENCY>
          Maximum simultaneous provider requests

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --attempts-per-task <ATTEMPTS_PER_TASK>
          Maximum attempts for an individual provider task

      --dry-run
          Preview without writes, provider requests or credential resolution

      --deadline-ms <DEADLINE_MS>
          Overall remote operation deadline in milliseconds from startup

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --max-request-bytes <MAX_REQUEST_BYTES>
          Lifetime ceiling on outgoing request bytes

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-response-bytes <MAX_RESPONSE_BYTES>
          Lifetime ceiling on incoming response bytes

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

      --max-input-units <MAX_INPUT_UNITS>
          Lifetime ceiling for each input and cached-input billable class

      --max-output-units <MAX_OUTPUT_UNITS>
          Lifetime ceiling for each output and reasoning billable class

      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          Currency code used with --max-cost

          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>
          Maximum provider requests dispatched per minute

      --tokens-per-minute <TOKENS_PER_MINUTE>
          Maximum accounted provider tokens per minute

      --retry-uncertain
          Opt into retrying uncertain work under retained accounting; prior attempts may be billed

      --retain-http-error-body
          Retain a bounded private HTTP error-body diagnostic for explicit inspection

  -h, --help
          Print help

```

## jobs status

```text
Inspect retained progress, effective limits and unknown holds without provider access

Usage: lwiki jobs status [OPTIONS] --run <RUN>

Options:
      --run <RUN>
          Retained provider run ID returned by the original operation

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## jobs diagnostics inspect

```text
Metadata is safe by default; --raw explicitly includes private provider text

Usage: lwiki jobs diagnostics inspect [OPTIONS] --run <RUN> --attempt <ATTEMPT> --kind <KIND>

Options:
      --run <RUN>
          Retained provider run ID

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --attempt <ATTEMPT>
          Attempt ID from jobs status; identifies the private diagnostic

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --kind <KIND>
          Diagnostic family to inspect

          [possible values: semantic-rejection, http-error]

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --raw
          Include bounded private provider text instead of safe metadata only

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## jobs diagnostics prune

```text
Prune only a settled diagnostic with known billing; unknown holds remain protected

Usage: lwiki jobs diagnostics prune [OPTIONS] --run <RUN> --attempt <ATTEMPT> --kind <KIND>

Options:
      --run <RUN>
          Retained provider run ID

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --attempt <ATTEMPT>
          Settled, known-billing attempt ID from jobs status

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --kind <KIND>
          Diagnostic family to remove explicitly

          [possible values: semantic-rejection, http-error]

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## recover

```text
Reconcile interrupted changes while preserving unfamiliar edits

Usage: lwiki recover [OPTIONS]

Options:
      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## migrate

```text
Stage a compatible schema migration; future schemas are never downgraded

Usage: lwiki migrate [OPTIONS] --if-match <IF_MATCH> <--id <ID>|--path <PATH>>

Options:
      --id <ID>
          Select a record by its stable ID

      --wiki <WIKI>
          Wiki root containing WIKI.md; defaults to discovery from the current directory

      --format <FORMAT>
          Output format: human-readable text, a JSON envelope or JSON Lines events

          [possible values: human, json, jsonl]

      --path <PATH>
          Select a record by its vault-relative path

      --if-match <IF_MATCH>
          Expected current BLAKE3 content hash; reject intervening edits

      --json
          Emit one structured JSON envelope

      --jsonl
          Emit JSON Lines events for supported streaming commands

      --to-schema <TO_SCHEMA>
          Target schema version; future schemas cannot be downgraded

          [default: 1]

      --offline
          Prevent provider requests and credential helper calls; local operations remain available

      --dry-run
          Preview without writes, provider requests or credential resolution

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          Trusted provider profile name from the private provider configuration

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          Maximum writer-lock wait in milliseconds (default: 5000; preferences may override)

  -h, --help
          Print help

```

## Implemented schemas

- `lwiki --json schema context-selection`
- `lwiki --json schema output`
- `lwiki --json schema record`
- `lwiki --json schema page`
- `lwiki --json schema page-batch`
- `lwiki --json schema stream`
- `lwiki --json schema extraction`
- `lwiki --json schema extraction-packet`
- `lwiki --json schema extraction-state`
- `lwiki --json schema graph-resolution`
- `lwiki --json schema graph-resolution-receipt`
- `lwiki --json schema run`
- `lwiki --json schema run-event`
- `lwiki --json schema usage-receipt`
- `lwiki --json schema entity-decisions`
- `lwiki --json schema entity-decision-receipt`
- `lwiki --json schema graph-review`
- `lwiki --json schema graph-review-receipt`
- `lwiki --json schema research-packet`
- `lwiki --json schema research-submission`
