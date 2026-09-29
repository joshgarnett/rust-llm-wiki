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

## embeddings check

```text
Usage: lwiki embeddings check [OPTIONS]

Options:
      --document-prefix <DOCUMENT_PREFIX>
          [default: ""]

      --wiki <WIKI>


      --format <FORMAT>
          [possible values: human, json, jsonl]

      --query-prefix <QUERY_PREFIX>
          [default: ""]

      --json


      --max-input-bytes <MAX_INPUT_BYTES>
          [default: 12000]

      --jsonl


      --quality-target-bytes <QUALITY_TARGET_BYTES>


      --offline


      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --dry-run


      --max-requests <MAX_REQUESTS>
          [default: 60]

      --concurrency <CONCURRENCY>
          [default: 2]

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --attempts-per-task <ATTEMPTS_PER_TASK>
          [default: 3]

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --deadline-ms <DEADLINE_MS>
          [default: 900000]

      --profile <PROFILE>


      --lock-timeout-ms <LOCK_TIMEOUT_MS>


      --max-request-bytes <MAX_REQUEST_BYTES>


      --max-response-bytes <MAX_RESPONSE_BYTES>


      --max-input-units <MAX_INPUT_UNITS>


      --max-output-units <MAX_OUTPUT_UNITS>


      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>


      --tokens-per-minute <TOKENS_PER_MINUTE>


      --retry-uncertain


      --probe


  -h, --help
          Print help

```

## embeddings sync

```text
Usage: lwiki embeddings sync [OPTIONS]

Options:
      --document-prefix <DOCUMENT_PREFIX>
          [default: ""]

      --wiki <WIKI>


      --format <FORMAT>
          [possible values: human, json, jsonl]

      --query-prefix <QUERY_PREFIX>
          [default: ""]

      --json


      --max-input-bytes <MAX_INPUT_BYTES>
          [default: 12000]

      --jsonl


      --quality-target-bytes <QUALITY_TARGET_BYTES>


      --offline


      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --dry-run


      --max-requests <MAX_REQUESTS>
          [default: 60]

      --concurrency <CONCURRENCY>
          [default: 2]

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --attempts-per-task <ATTEMPTS_PER_TASK>
          [default: 3]

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --deadline-ms <DEADLINE_MS>
          [default: 900000]

      --profile <PROFILE>


      --lock-timeout-ms <LOCK_TIMEOUT_MS>


      --max-request-bytes <MAX_REQUEST_BYTES>


      --max-response-bytes <MAX_RESPONSE_BYTES>


      --max-input-units <MAX_INPUT_UNITS>


      --max-output-units <MAX_OUTPUT_UNITS>


      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>


      --tokens-per-minute <TOKENS_PER_MINUTE>


      --retry-uncertain


  -h, --help
          Print help

```

## search

```text
Usage: lwiki search [OPTIONS] <QUERY>

Arguments:
  <QUERY>


Options:
      --graph <GRAPH>
          Include graph evidence ranks in hybrid search

          [possible values: entities]

      --wiki <WIKI>


      --format <FORMAT>
          [possible values: human, json, jsonl]

      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --json


      --max-requests <MAX_REQUESTS>
          [default: 60]

      --concurrency <CONCURRENCY>
          [default: 2]

      --jsonl


      --attempts-per-task <ATTEMPTS_PER_TASK>
          [default: 3]

      --offline


      --deadline-ms <DEADLINE_MS>
          [default: 900000]

      --dry-run


      --max-request-bytes <MAX_REQUEST_BYTES>


      --stage
          Retain a guarded preparation for a later explicit changes apply

      --max-response-bytes <MAX_RESPONSE_BYTES>


      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-input-units <MAX_INPUT_UNITS>


      --profile <PROFILE>


      --lock-timeout-ms <LOCK_TIMEOUT_MS>


      --max-output-units <MAX_OUTPUT_UNITS>


      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>


      --tokens-per-minute <TOKENS_PER_MINUTE>


      --retry-uncertain


      --lexical-fallback
          Use lexical results when compatible embeddings are unavailable

      --mode <MODE>
          [default: lexical]
          [possible values: literal, lexical, semantic, hybrid]

      --kind <KINDS>


      --tag <TAGS>


      --source-id <SOURCE_IDS>


      --path-prefix <PATH_PREFIX>


      --status <AUTHORED_STATUSES>


      --include-proposed


      --include-historical


      --limit <LIMIT>
          [default: 10]

      --candidates <CANDIDATES>
          [default: 80]

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
      --graph <GRAPH>
          Include graph evidence ranks in hybrid search

          [possible values: entities]

      --wiki <WIKI>


      --format <FORMAT>
          [possible values: human, json, jsonl]

      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --json


      --max-requests <MAX_REQUESTS>
          [default: 60]

      --concurrency <CONCURRENCY>
          [default: 2]

      --jsonl


      --attempts-per-task <ATTEMPTS_PER_TASK>
          [default: 3]

      --offline


      --deadline-ms <DEADLINE_MS>
          [default: 900000]

      --dry-run


      --max-request-bytes <MAX_REQUEST_BYTES>


      --stage
          Retain a guarded preparation for a later explicit changes apply

      --max-response-bytes <MAX_RESPONSE_BYTES>


      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-input-units <MAX_INPUT_UNITS>


      --profile <PROFILE>


      --lock-timeout-ms <LOCK_TIMEOUT_MS>


      --max-output-units <MAX_OUTPUT_UNITS>


      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>


      --tokens-per-minute <TOKENS_PER_MINUTE>


      --retry-uncertain


      --lexical-fallback
          Use lexical results when compatible embeddings are unavailable

      --mode <MODE>
          [default: lexical]
          [possible values: literal, lexical, semantic, hybrid]

      --kind <KINDS>


      --tag <TAGS>


      --source-id <SOURCE_IDS>


      --path-prefix <PATH_PREFIX>


      --status <AUTHORED_STATUSES>


      --include-proposed


      --include-historical


      --limit <LIMIT>
          [default: 10]

      --candidates <CANDIDATES>
          [default: 80]

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
          [possible values: lexical, semantic]

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
      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --wiki <WIKI>


      --format <FORMAT>
          [possible values: human, json, jsonl]

      --max-requests <MAX_REQUESTS>
          [default: 60]

      --concurrency <CONCURRENCY>
          [default: 2]

      --json


      --attempts-per-task <ATTEMPTS_PER_TASK>
          [default: 3]

      --jsonl


      --deadline-ms <DEADLINE_MS>
          [default: 900000]

      --offline


      --dry-run


      --max-request-bytes <MAX_REQUEST_BYTES>


      --max-response-bytes <MAX_RESPONSE_BYTES>


      --stage
          Retain a guarded preparation for a later explicit changes apply

      --max-input-units <MAX_INPUT_UNITS>


      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-output-units <MAX_OUTPUT_UNITS>


      --profile <PROFILE>


      --lock-timeout-ms <LOCK_TIMEOUT_MS>


      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>


      --tokens-per-minute <TOKENS_PER_MINUTE>


      --retry-uncertain


      --run <RUN>


      --max-output-tokens <MAX_OUTPUT_TOKENS>
          [default: 4096]

      --new-extraction


      --source-id <SOURCE_ID>


      --revision-id <REVISION_ID>


      --executor <EXECUTOR>
          [default: agent]
          [possible values: agent, api]

      --window <WINDOWS>
          Exact UTF-8 source byte window START:END; repeat up to16 times

      --max-mentions <MAX_MENTIONS>
          [default: 64]

      --max-assertions <MAX_ASSERTIONS>
          [default: 128]

      --max-output-bytes <MAX_OUTPUT_BYTES>
          [default: 262144]

      --candidate-id <CANDIDATE_IDS>


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
      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --wiki <WIKI>


      --format <FORMAT>
          [possible values: human, json, jsonl]

      --max-requests <MAX_REQUESTS>
          [default: 60]

      --concurrency <CONCURRENCY>
          [default: 2]

      --json


      --attempts-per-task <ATTEMPTS_PER_TASK>
          [default: 3]

      --jsonl


      --deadline-ms <DEADLINE_MS>
          [default: 900000]

      --offline


      --dry-run


      --max-request-bytes <MAX_REQUEST_BYTES>


      --max-response-bytes <MAX_RESPONSE_BYTES>


      --stage
          Retain a guarded preparation for a later explicit changes apply

      --max-input-units <MAX_INPUT_UNITS>


      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-output-units <MAX_OUTPUT_UNITS>


      --profile <PROFILE>


      --lock-timeout-ms <LOCK_TIMEOUT_MS>


      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>


      --tokens-per-minute <TOKENS_PER_MINUTE>


      --retry-uncertain


      --lexical-fallback
          Use lexical results when compatible embeddings are unavailable

      --strategy <STRATEGY>
          [default: combined]
          [possible values: entity, relationship, combined]

      --seed <SEED>
          [default: lexical]
          [possible values: lexical, semantic]

      --kind <KINDS>


      --tag <TAGS>


      --source-id <SOURCE_IDS>


      --path-prefix <PATH_PREFIX>


      --status <AUTHORED_STATUSES>


      --include-proposed


      --include-historical


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
      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --wiki <WIKI>


      --format <FORMAT>
          [possible values: human, json, jsonl]

      --max-requests <MAX_REQUESTS>
          [default: 60]

      --concurrency <CONCURRENCY>
          [default: 2]

      --json


      --attempts-per-task <ATTEMPTS_PER_TASK>
          [default: 3]

      --jsonl


      --deadline-ms <DEADLINE_MS>
          [default: 900000]

      --offline


      --dry-run


      --max-request-bytes <MAX_REQUEST_BYTES>


      --max-response-bytes <MAX_RESPONSE_BYTES>


      --stage
          Retain a guarded preparation for a later explicit changes apply

      --max-input-units <MAX_INPUT_UNITS>


      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-output-units <MAX_OUTPUT_UNITS>


      --profile <PROFILE>


      --lock-timeout-ms <LOCK_TIMEOUT_MS>


      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>


      --tokens-per-minute <TOKENS_PER_MINUTE>


      --retry-uncertain


      --lexical-fallback
          Use lexical results when compatible embeddings are unavailable

      --strategy <STRATEGY>
          [default: combined]
          [possible values: entity, relationship, combined]

      --seed <SEED>
          [default: lexical]
          [possible values: lexical, semantic]

      --kind <KINDS>


      --tag <TAGS>


      --source-id <SOURCE_IDS>


      --path-prefix <PATH_PREFIX>


      --status <AUTHORED_STATUSES>


      --include-proposed


      --include-historical


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

      --role <ROLE>
          [default: embed]
          [possible values: embed, generate, search]

      --json


      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --jsonl


      --max-requests <MAX_REQUESTS>
          [default: 60]

      --concurrency <CONCURRENCY>
          [default: 2]

      --offline


      --attempts-per-task <ATTEMPTS_PER_TASK>
          [default: 3]

      --dry-run


      --deadline-ms <DEADLINE_MS>
          [default: 900000]

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --max-request-bytes <MAX_REQUEST_BYTES>


      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --max-response-bytes <MAX_RESPONSE_BYTES>


      --profile <PROFILE>


      --lock-timeout-ms <LOCK_TIMEOUT_MS>


      --max-input-units <MAX_INPUT_UNITS>


      --max-output-units <MAX_OUTPUT_UNITS>


      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>


      --tokens-per-minute <TOKENS_PER_MINUTE>


      --retry-uncertain


  -h, --help
          Print help

```

## research plan

```text
Usage: lwiki research plan [OPTIONS] <QUESTION>

Arguments:
  <QUESTION>


Options:
      --url <URLS>


      --wiki <WIKI>


      --exclude <EXCLUSIONS>


      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json


      --search-profile <SEARCH_PROFILE>


      --jsonl


      --max-rounds <MAX_ROUNDS>
          [default: 3]

      --max-sources <MAX_SOURCES>
          [default: 15]

      --offline


      --dry-run


      --stage-output-tokens <STAGE_OUTPUT_TOKENS>
          [default: 4096]

      --apply
          Apply the run's generated page proposals after their guarded preparation

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --run-id <RUN_ID>


      --profile <PROFILE>


      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --lock-timeout-ms <LOCK_TIMEOUT_MS>


      --max-requests <MAX_REQUESTS>
          [default: 60]

      --concurrency <CONCURRENCY>
          [default: 2]

      --attempts-per-task <ATTEMPTS_PER_TASK>
          [default: 3]

      --deadline-ms <DEADLINE_MS>
          [default: 900000]

      --max-request-bytes <MAX_REQUEST_BYTES>


      --max-response-bytes <MAX_RESPONSE_BYTES>


      --max-input-units <MAX_INPUT_UNITS>


      --max-output-units <MAX_OUTPUT_UNITS>


      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>


      --tokens-per-minute <TOKENS_PER_MINUTE>


      --retry-uncertain


  -h, --help
          Print help

```

## research run

```text
Usage: lwiki research run [OPTIONS] <QUESTION>

Arguments:
  <QUESTION>


Options:
      --url <URLS>


      --wiki <WIKI>


      --exclude <EXCLUSIONS>


      --format <FORMAT>
          [possible values: human, json, jsonl]

      --json


      --search-profile <SEARCH_PROFILE>


      --jsonl


      --max-rounds <MAX_ROUNDS>
          [default: 3]

      --max-sources <MAX_SOURCES>
          [default: 15]

      --offline


      --dry-run


      --stage-output-tokens <STAGE_OUTPUT_TOKENS>
          [default: 4096]

      --apply
          Apply the run's generated page proposals after their guarded preparation

      --stage
          Retain a guarded preparation for a later explicit changes apply

      --preferences <PREFERENCES>
          Explicit trusted local JSON preferences; never read ambient credentials

      --run-id <RUN_ID>


      --profile <PROFILE>


      --providers-config <PROVIDERS_CONFIG>
          Private TOML configuration; never discover provider files inside a vault

      --lock-timeout-ms <LOCK_TIMEOUT_MS>


      --max-requests <MAX_REQUESTS>
          [default: 60]

      --concurrency <CONCURRENCY>
          [default: 2]

      --attempts-per-task <ATTEMPTS_PER_TASK>
          [default: 3]

      --deadline-ms <DEADLINE_MS>
          [default: 900000]

      --max-request-bytes <MAX_REQUEST_BYTES>


      --max-response-bytes <MAX_RESPONSE_BYTES>


      --max-input-units <MAX_INPUT_UNITS>


      --max-output-units <MAX_OUTPUT_UNITS>


      --max-cost <MAX_COST>
          Checked decimal ceiling; requires a complete provable provider bound

      --currency <CURRENCY>
          [default: USD]

      --requests-per-minute <REQUESTS_PER_MINUTE>


      --tokens-per-minute <TOKENS_PER_MINUTE>


      --retry-uncertain


  -h, --help
          Print help

```

## research resume

```text
Usage: lwiki research resume [OPTIONS] <RUN_ID>

Arguments:
  <RUN_ID>


Options:
      --providers-config <PROVIDERS_CONFIG>


      --wiki <WIKI>


      --format <FORMAT>
          [possible values: human, json, jsonl]

      --retry-uncertain


      --amend-limits <AMEND_LIMITS>
          JSON with complete lifetime limits, an absolute UTC deadline, and reason. Omit this flag to preserve the run's effective limits and deadline

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

## research status

```text
Usage: lwiki research status [OPTIONS] <RUN_ID>

Arguments:
  <RUN_ID>


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

## research report

```text
Usage: lwiki research report [OPTIONS] <RUN_ID>

Arguments:
  <RUN_ID>


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
- `lwiki --json schema research-frontier`
- `lwiki --json schema research-gaps`
- `lwiki --json schema research-synthesis`
- `lwiki --json schema research-run-plan`
