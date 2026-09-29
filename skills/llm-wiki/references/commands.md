# Implemented lwiki 0.1.0 commands

Generated from the release command registry and argument parser. Run `lwiki --json capabilities` before use.

## capabilities

```text
Usage: lwiki capabilities [OPTIONS]

Options:
      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## skill export

```text
Usage: lwiki skill export [OPTIONS] --target <TARGET> --output <OUTPUT>

Options:
      --target <TARGET>
          [possible values: codex, claude-code, cursor]

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --output <OUTPUT>
          

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## schema

```text
Usage: lwiki schema [OPTIONS] <NAME>

Arguments:
  <NAME>
          

Options:
      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## init

```text
Usage: lwiki init [OPTIONS] <PATH>

Arguments:
  <PATH>
          

Options:
      --title <TITLE>
          [default: "Local wiki"]

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## read

```text
Usage: lwiki read [OPTIONS] <--id <ID>|--path <PATH>>

Options:
      --id <ID>
          

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --path <PATH>
          

      --json
          

      --max-bytes <MAX_BYTES>
          

      --jsonl
          

      --start <START>
          

      --end <END>
          

      --offline
          

      --dry-run
          

      --no-sync
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## page put

```text
Usage: lwiki page put [OPTIONS] --file <FILE>

Options:
      --file <FILE>
          

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --path <PATH>
          

      --if-match <IF_MATCH>
          

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## page rename

```text
Usage: lwiki page rename [OPTIONS] --to <TO> --if-match <IF_MATCH> <ID>

Arguments:
  <ID>
          

Options:
      --to <TO>
          

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --if-match <IF_MATCH>
          

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## source add

```text
Usage: lwiki source add [OPTIONS] <FILE>

Arguments:
  <FILE>
          

Options:
      --title <TITLE>
          

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --media-type <MEDIA_TYPE>
          

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## source refresh

```text
Usage: lwiki source refresh [OPTIONS] --file <FILE> <ID>

Arguments:
  <ID>
          

Options:
      --file <FILE>
          

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --title <TITLE>
          

      --json
          

      --media-type <MEDIA_TYPE>
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## source withdraw

```text
Usage: lwiki source withdraw [OPTIONS] --reason <REASON> <ID>

Arguments:
  <ID>
          

Options:
      --reason <REASON>
          

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## evidence revalidate

```text
Usage: lwiki evidence revalidate [OPTIONS] --to-revision <TO_REVISION> --if-match <IF_MATCH> <ID>

Arguments:
  <ID>
          

Options:
      --to-revision <TO_REVISION>
          

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --if-match <IF_MATCH>
          

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## index sync

```text
Usage: lwiki index sync [OPTIONS]

Options:
      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## index rebuild

```text
Usage: lwiki index rebuild [OPTIONS]

Options:
      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## search

```text
Usage: lwiki search [OPTIONS] <QUERY>

Arguments:
  <QUERY>
          

Options:
      --mode <MODE>
          [default: lexical]
          [possible values: literal, lexical]

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --kind <KINDS>
          

      --json
          

      --tag <TAGS>
          

      --jsonl
          

      --source-id <SOURCE_IDS>
          

      --offline
          

      --path-prefix <PATH_PREFIX>
          

      --dry-run
          

      --status <AUTHORED_STATUSES>
          

      --include-proposed
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --include-historical
          

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --limit <LIMIT>
          [default: 10]

      --profile <PROFILE>
          

      --candidates <CANDIDATES>
          [default: 80]

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

      --excerpt-bytes <EXCERPT_BYTES>
          [default: 240]

      --cursor <CURSOR>
          

      --no-sync
          

  -h, --help
          Print help

```

## context

```text
Usage: lwiki context [OPTIONS] <QUERY>

Arguments:
  <QUERY>
          

Options:
      --mode <MODE>
          [default: lexical]
          [possible values: literal, lexical]

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --kind <KINDS>
          

      --json
          

      --tag <TAGS>
          

      --jsonl
          

      --source-id <SOURCE_IDS>
          

      --offline
          

      --path-prefix <PATH_PREFIX>
          

      --dry-run
          

      --status <AUTHORED_STATUSES>
          

      --include-proposed
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --include-historical
          

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --limit <LIMIT>
          [default: 10]

      --profile <PROFILE>
          

      --candidates <CANDIDATES>
          [default: 80]

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

      --excerpt-bytes <EXCERPT_BYTES>
          [default: 240]

      --cursor <CURSOR>
          

      --no-sync
          

      --scope <SCOPE>
          [default: current]
          [possible values: current, historical, snapshot]

      --target <TARGET>
          [default: documents]
          [possible values: documents, graph, combined]

      --strategy <STRATEGY>
          [default: combined]
          [possible values: entity, relationship, combined]

      --seed <SEED>
          [default: lexical]
          [possible values: lexical]

      --seeds <SEEDS>
          [default: 12]

      --depth <DEPTH>
          [default: 1]

      --incident-per-seed <INCIDENT_PER_SEED>
          [default: 16]

      --assertions <ASSERTIONS>
          [default: 128]

      --navigation
          

      --max-bytes <MAX_BYTES>
          [default: 12000]

      --max-tokens <MAX_TOKENS>
          [default: 3000]

      --instruction-bytes <INSTRUCTION_BYTES>
          [default: 0]

      --instruction-tokens <INSTRUCTION_TOKENS>
          [default: 0]

      --output-bytes <OUTPUT_BYTES>
          [default: 0]

      --output-tokens <OUTPUT_TOKENS>
          [default: 0]

      --verification-max-bytes <VERIFICATION_MAX_BYTES>
          [default: 67108864]

      --verification-max-files <VERIFICATION_MAX_FILES>
          [default: 4096]

      --verification-max-entries <VERIFICATION_MAX_ENTRIES>
          [default: 16384]

      --verification-max-elapsed-ms <VERIFICATION_MAX_ELAPSED_MS>
          [default: 2000]

  -h, --help
          Print help

```

## graph extract

```text
Usage: lwiki graph extract [OPTIONS] --source-id <SOURCE_ID>

Options:
      --source-id <SOURCE_ID>
          

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --revision-id <REVISION_ID>
          

      --executor <EXECUTOR>
          [default: agent]
          [possible values: agent, api]

      --json
          

      --jsonl
          

      --window <WINDOWS>
          Exact UTF-8 source byte window START:END; repeat up to16 times

      --max-mentions <MAX_MENTIONS>
          [default: 64]

      --offline
          

      --dry-run
          

      --max-assertions <MAX_ASSERTIONS>
          [default: 128]

      --max-output-bytes <MAX_OUTPUT_BYTES>
          [default: 262144]

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --candidate-id <CANDIDATE_IDS>
          

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## graph import

```text
Usage: lwiki graph import [OPTIONS] --file <FILE>

Options:
      --file <FILE>
          

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --new-extraction
          

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## graph resolve

```text
Usage: lwiki graph resolve [OPTIONS] --file <FILE>

Options:
      --file <FILE>
          Strict lwiki.graph-resolution.v1 JSON; use - for stdin

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## graph decide

```text
Usage: lwiki graph decide [OPTIONS] --file <FILE>

Options:
      --file <FILE>
          Strict lwiki.entity-decisions.v1 JSON; use - for stdin

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## graph review

```text
Usage: lwiki graph review [OPTIONS] --file <FILE>

Options:
      --file <FILE>
          Strict lwiki.graph-review.v1 JSON; use - for stdin

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## graph query

```text
Usage: lwiki graph query [OPTIONS] <QUERY>

Arguments:
  <QUERY>
          

Options:
      --strategy <STRATEGY>
          [default: combined]
          [possible values: entity, relationship, combined]

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --seed <SEED>
          [default: lexical]
          [possible values: lexical]

      --json
          

      --kind <KINDS>
          

      --jsonl
          

      --tag <TAGS>
          

      --offline
          

      --source-id <SOURCE_IDS>
          

      --dry-run
          

      --path-prefix <PATH_PREFIX>
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --status <AUTHORED_STATUSES>
          

      --include-proposed
          

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --include-historical
          

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

      --navigation
          

      --candidates <CANDIDATES>
          [default: 80]

      --seeds <SEEDS>
          [default: 12]

      --depth <DEPTH>
          [default: 1]

      --incident-per-seed <INCIDENT_PER_SEED>
          [default: 16]

      --assertions <ASSERTIONS>
          [default: 128]

      --limit <LIMIT>
          [default: 10]

      --excerpt-bytes <EXCERPT_BYTES>
          [default: 240]

      --support-per-assertion <SUPPORT_PER_ASSERTION>
          [default: 2]

      --contradictions-per-assertion <CONTRADICTIONS_PER_ASSERTION>
          [default: 1]

      --cursor <CURSOR>
          

      --no-sync
          

  -h, --help
          Print help

```

## graph neighbors

```text
Usage: lwiki graph neighbors [OPTIONS] <ID>

Arguments:
  <ID>
          

Options:
      --strategy <STRATEGY>
          [default: combined]
          [possible values: entity, relationship, combined]

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --seed <SEED>
          [default: lexical]
          [possible values: lexical]

      --json
          

      --kind <KINDS>
          

      --jsonl
          

      --tag <TAGS>
          

      --offline
          

      --source-id <SOURCE_IDS>
          

      --dry-run
          

      --path-prefix <PATH_PREFIX>
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --status <AUTHORED_STATUSES>
          

      --include-proposed
          

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --include-historical
          

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

      --navigation
          

      --candidates <CANDIDATES>
          [default: 80]

      --seeds <SEEDS>
          [default: 12]

      --depth <DEPTH>
          [default: 1]

      --incident-per-seed <INCIDENT_PER_SEED>
          [default: 16]

      --assertions <ASSERTIONS>
          [default: 128]

      --limit <LIMIT>
          [default: 10]

      --excerpt-bytes <EXCERPT_BYTES>
          [default: 240]

      --support-per-assertion <SUPPORT_PER_ASSERTION>
          [default: 2]

      --contradictions-per-assertion <CONTRADICTIONS_PER_ASSERTION>
          [default: 1]

      --cursor <CURSOR>
          

      --no-sync
          

  -h, --help
          Print help

```

## check

```text
Usage: lwiki check [OPTIONS]

Options:
      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## doctor

```text
Usage: lwiki doctor [OPTIONS]

Options:
      --probe
          

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## changes show

```text
Usage: lwiki changes show [OPTIONS] <ID>

Arguments:
  <ID>
          

Options:
      --operation <OPERATION>
          

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## changes apply

```text
Usage: lwiki changes apply [OPTIONS] <ID>

Arguments:
  <ID>
          

Options:
      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## changes abort

```text
Usage: lwiki changes abort [OPTIONS] <ID>

Arguments:
  <ID>
          

Options:
      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## changes rollback

```text
Usage: lwiki changes rollback [OPTIONS] <ID>

Arguments:
  <ID>
          

Options:
      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## recover

```text
Usage: lwiki recover [OPTIONS]

Options:
      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json
          

      --jsonl
          

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## migrate

```text
Stage a compatible schema migration; future schemas are never downgraded

Usage: lwiki migrate [OPTIONS] --if-match <IF_MATCH> <--id <ID>|--path <PATH>>

Options:
      --id <ID>
          

      --wiki <WIKI>
          

      --format <FORMAT>
          [possible values: human, json, jsonl]

      --path <PATH>
          

      --if-match <IF_MATCH>
          

      --json
          

      --jsonl
          

      --to-schema <TO_SCHEMA>
          [default: 1]

      --offline
          

      --dry-run
          

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --profile <PROFILE>
          

      --lock-timeout-ms <LOCK_TIMEOUT_MS>
          

  -h, --help
          Print help

```

## Implemented schemas

- `lwiki --json schema output`
- `lwiki --json schema record`
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
