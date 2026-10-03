# Getting started with lwiki

This tutorial captures deployment notes, finds a command, and returns cited text for a person or agent to use. Everything in the tutorial works offline with a disposable wiki and no provider credentials. An authored page and a source refresh then show how to maintain useful knowledge over time.

## Before you start

Use a macOS/Linux shell. Windows vault writes remain unsupported. From the repository root, install the development tools listed in [builds and CI](builds.md), then run:

```sh
just deps-fetch
just candidate .artifacts/candidate-001
```

The candidate output directory must be new; use a different suffix if it already exists. Building requires development tools and initial dependency downloads. Running the resulting binary does not require Python, Node, a database server or a model runtime. If you already have a binary, set `LWIKI` below to its absolute path instead. The repository's current behavior is described here; use a historical release's [matching guide](testing-0.1.2.md) for that release.

```sh
LWIKI="$PWD/.artifacts/candidate-001/lwiki"
"$LWIKI" --version
"$LWIKI" --help
"$LWIKI" source add --help
```

Use the same shell for the remaining steps so it retains `LWIKI`, `DEMO` and `WIKI`. All paths are quoted, including paths with spaces.

## Capture and find deployment notes

Create a temporary workspace. `mktemp` creates the parent directory; `lwiki init` will create the new `wiki` child. Do not initialize over an existing wiki.

```sh
DEMO="$(mktemp -d)"
WIKI="$DEMO/wiki"
cat > "$DEMO/deployment-notes.md" <<'EOF'
# Atlas deployment notes

The Atlas service uses Postgres for its primary database.
Run atlas_migrate before deploying a new release.
Rollback restores the previous release and keeps the database unchanged.
EOF
"$LWIKI" --offline init "$WIKI"
"$LWIKI" --wiki "$WIKI" --offline --json source add "$DEMO/deployment-notes.md" --title 'Atlas deployment notes'
```

The capture uses `--json` to expose stable machine fields: the envelope's `data.allocated_ids.source` and `data.allocated_ids.revision`. Keep the source ID for future refreshes. These values are allocated for your wiki; the example IDs in other documents cannot substitute for them. For this Markdown input, expect `extraction_status: complete` and `citable: true`. Capturing copies the input into an immutable revision; editing the original file later does not change that captured revision.

Find the exact deployment command:

```sh
"$LWIKI" --wiki "$WIKI" --offline search 'atlas_migrate' --mode literal --limit 5
```

Expect a current result titled `Atlas deployment notes` whose excerpt includes `Run atlas_migrate before deploying a new release.` Literal search finds exact bytes. The default lexical mode searches terms and ranks results; it works locally and is useful when you remember a subject rather than an exact identifier:

```sh
"$LWIKI" --wiki "$WIKI" --offline search 'Atlas deployment' --limit 5
```

Both commands automatically sync the rebuildable local index. Semantic and hybrid modes require remote embeddings and compatible cached vectors; see [providers](providers.md) before using them. A lexical search does not infer synonyms or answer a question.

## Give an agent cited context

```sh
"$LWIKI" --wiki "$WIKI" --offline context 'Atlas deployment' --max-bytes 6000 --max-tokens 1500
"$LWIKI" --wiki "$WIKI" --offline check
```

Context allows up to 1,024 bytes per excerpt by default; search uses 240. Use `--excerpt-bytes` to choose an explicit bound, within the total context budget. Longer multi-source questions may require a larger `--max-bytes`/`--max-tokens` budget. Read the actual passages: a relevant source title is not enough if the supporting sentence is absent.

Context returns the deployment note with a `Citation (Current)`, a source ID, an immutable revision ID, a byte span and a quote hash. It assembles evidence rather than generating an answer. The token bound uses an estimate from UTF-8 bytes; it is not exact tokenizer accounting. Read warnings and omissions when bounds exclude text. `check` should report `error_count: 0` and an empty diagnostics list.

To inspect a cited record, copy the actual revision ID from the context output and set it below; replace the placeholder text before running the read command:

```sh
REVISION_ID='paste the revision_ ID from your context result'
"$LWIKI" --wiki "$WIKI" --offline read --id "$REVISION_ID" --max-bytes 6000
```

An exact citation establishes which bytes were quoted. You or the agent must still assess whether those bytes support a claim. Source capture alone does not create or accept graph assertions.

## Write a readable wiki page

Create your own concise runbook, then wrap it in a page record:

```sh
cat > "$DEMO/runbook.md" <<'EOF'
# Atlas release checklist

1. Run atlas_migrate before deploying.
2. Deploy the new Atlas release.
3. If rollback is needed, restore the previous release and leave the database unchanged.

This is an authored summary of the captured deployment notes. Review it when those notes change.
EOF
"$LWIKI" --wiki "$WIKI" --offline page init --file "$DEMO/runbook.md" --title 'Atlas release checklist' --path pages/atlas-runbook.md
"$LWIKI" --wiki "$WIKI" --offline read --path pages/atlas-runbook.md --max-bytes 6000
"$LWIKI" --wiki "$WIKI" --offline search 'atlas_migrate' --mode literal --kind page
```

Expect a draft page at `pages/atlas-runbook.md` and a search result for that page. Draft pages are searchable but excluded from verified current context until reviewed. `page init` allocates its stable identity and frontmatter; use it for plain Markdown bodies. You can open this file in a Markdown editor or open `$WIKI` as an Obsidian vault. Preserve the page ID and frontmatter when editing. Renaming a heading does not change identity. Use [guarded page workflows](../skills/llm-wiki/references/workflows.md) for scripted updates and batches; existing pages require their observed hashes to preserve intervening author edits.

## Refresh a source when its instructions change

Use the source ID from the original capture result, replacing the placeholder first:

```sh
SOURCE_ID='paste the source_ ID from your capture result'
cat > "$DEMO/deployment-notes.md" <<'EOF'
# Atlas deployment notes

The Atlas service uses Postgres for its primary database.
Run atlas_migrate_v2 before deploying a new release.
Rollback restores the previous release and keeps the database unchanged.
EOF
"$LWIKI" --wiki "$WIKI" --offline source refresh "$SOURCE_ID" --file "$DEMO/deployment-notes.md"
"$LWIKI" --wiki "$WIKI" --offline search 'atlas_migrate_v2' --mode literal --source-id "$SOURCE_ID"
"$LWIKI" --wiki "$WIKI" --offline search 'atlas_migrate' --mode literal --source-id "$SOURCE_ID" --include-historical
"$LWIKI" --wiki "$WIKI" --offline check
```

Refresh retains the source identity and creates a new immutable revision. Expect current source text to contain `atlas_migrate_v2`; historical search can also show the previous revision with a historical label. Literal substring search for `atlas_migrate` also matches `atlas_migrate_v2`, so inspect the excerpt and scope labels. The source filter keeps the authored page out of these results.

The authored runbook still says `atlas_migrate`: refresh does not rewrite your prose. Review and update authored summaries yourself. Previously reviewed evidence may need explicit `evidence revalidate` and a new review. To remove a source from current support while preserving history, use `source withdraw SOURCE_ID --reason REASON`; read `source withdraw --help` and the [maintained workflows](../skills/llm-wiki/references/workflows.md) first.

## Export a workflow for your coding agent

```sh
"$LWIKI" --offline skill export --target codex --output "$DEMO/codex-skill"
```

Use a new export directory. Choose `claude-code` or `cursor` instead for those hosts. Export writes instructions, executable examples and the running binary's command reference; it does not install the skill or launch a host agent. Read the exported `SKILL.md` and generated `references/commands.md`, then use your host's skill installation workflow. The [maintained skill](../skills/llm-wiki/SKILL.md) describes authorized use.

For agent-led research, [the maintained research workflow](../skills/llm-wiki/references/workflows.md) explains `research run`, packet stages, citation-bound imports and explicit gaps. The CLI persists packets and validates submitted evidence; the host agent handles external tools and its own usage budget. Provider API extraction and embeddings have separate explicit configuration and accounting requirements in [providers](providers.md).

## When something goes wrong

| Symptom | Next step |
| --- | --- |
| `init` refuses the directory | Choose a new child directory; for an existing wiki, use `--wiki` instead of initializing it again. |
| Input file cannot be opened | Check the input path and current shell directory. The tutorial creates its file before capture. |
| Wiki is not found | Pass the absolute `--wiki "$WIKI"` path. Automatic discovery searches from the current directory. |
| Search has no hits | Check spelling and mode. Try a known exact word from the captured input and inspect capture warnings and `citable`. |
| Semantic search reports unavailable embeddings | Use default lexical or literal search for local work, or configure and explicitly authorize embeddings. |
| Context reports a final-proof budget exceeded | Follow the resource-specific hint. After inspecting vault size, explicitly raise the named `--verification-max-*` limit; for example, `--verification-max-entries 65536`. Entry counts include repeated path checks, not just unique files. Verification limits are separate from output limits. |
| A guarded change conflicts | Read the current page, incorporate the author edit, and prepare a new change with current hashes. See [workflows](../skills/llm-wiki/references/workflows.md). |

Use `--json` for a structured machine envelope. Successful command data is under `data`; read errors and warnings as well as the process exit code. Human output is intended for reading and can change between versions. `check` and `graph neighbors` currently display formatted JSON even in human mode. Inspect `"$LWIKI" --json capabilities` and `"$LWIKI" COMMAND --help` for the commands supported by your binary.

Keep the temporary wiki as a learning fixture or choose a new permanent wiki directory for your own notes. Make a complete backup including `.wiki` before storage cleanup; [the cleanup guide](testing-cleanup.md) covers retention, recovery and restore. Local examples establish command behavior on disposable data; they do not qualify live providers, host discovery or real-model quality.
