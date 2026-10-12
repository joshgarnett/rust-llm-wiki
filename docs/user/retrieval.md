# Search and citations

Find relevant passages in your notes, then read the text behind an answer.
Your agent can do this for you. The commands below use your selected wiki;
add `--wiki "/absolute/path/to/Wiki"` when you want a different one.

## Search and inspect cited context

Search for a topic:

```sh
lwiki search 'project release decisions'
```

To find an exact phrase or command, use literal search:

```sh
lwiki search 'a phrase from your note' --mode literal
```

Literal search is case sensitive and includes punctuation. It does not use
wildcards or regular expressions.

For passages that help answer a question:

```sh
lwiki context 'What did we decide about the release?'
```

Context returns text and citations. It does not write an answer. Give the text
to your agent, or read it yourself. A source citation identifies a saved note
version and the exact quoted passage. Check that the passage supports the claim,
including any conditions or exceptions.

Search and context have output limits, so they can omit useful facts. If a fact
is missing, search for its wording, narrow to the relevant source, or read more
of that note. An empty result does not prove the information is absent.

If you enabled automatic search by meaning, ordinary search and context combine
word matches with similar meanings. They can send eligible note text and your
question to the approved embedding service. Preparation has finite limits and
can remain incomplete. When search by meaning is unavailable, the command uses
local search and reports why. You can keep working and inspect warnings if a needed fact is missing.

For local word search, use `--mode lexical`. Literal search also stays local.
Add `--offline` to block all provider requests, including credential helper calls.
Reads do not need a provider. A wiki saved as your default is not, by itself,
permission to send its notes.

Ordinary local lookups pick up edits to files in a small wiki without writing
to the folder. An agent only needs read access for this local route. It also
works when the saved search index is missing. After changing an original note
outside the wiki, [capture it again](everyday-use.md#capture-and-refresh-notes)
to use the update as cited evidence.

For larger wikis, a lookup may use the saved index and warn that new or edited
notes could be missing. Run `lwiki index sync` from a session with write access
to update it. Read warnings before treating a result as current. Use `lwiki check`
to check all managed files for errors.

Explicit `--mode semantic` or `--mode hybrid` requires
[compatible embeddings](sources-and-providers.md#optional-remote-providers).
These modes report a missing prerequisite instead of switching to word search.
Offline use requires an identical cached query embedding.

## Scope, filters and bounds

Narrow a search with `--source-id ID` or `--path-prefix PATH`. Repeat
`--source-id` for several sources. You can also filter managed notes by kind,
tag or status. For example, find draft pages:

```sh
lwiki --offline search 'project summary' --kind page --status draft
```

Drafts appear in search but are excluded from default cited context. A source
filter searches the captured note's text. `--kind source` searches its metadata
record instead.

Increase both context bounds when you need more text:

```sh
lwiki --offline context 'What steps and exceptions apply?' --max-bytes 12000 --max-tokens 3000
```

The first bound reached limits the result. Tokens are estimated from byte size;
your agent may count them differently. Read warnings and omissions as well as
the passages. If using JSON, `data.text` is the rendered evidence; the full
response also includes metadata and may repeat passages.

For explicit control on normalized indexes, `--scope indexed-documents`
searches the saved index and checks the selected files.
`--scope indexed-evidence --mode lexical` searches only indexed captured sources
without syncing. `--include-historical` can include
earlier captured versions in literal or word context; read their labels before
using them as current facts. Historical semantic and hybrid selection are
unsupported.

Ordinary local context checks current notes when the wiki fits its read limits.
If it uses the saved index instead, its warnings explain that choice.
Explicit `--scope current` requires the current check and fails if it cannot
complete within those limits. Snapshot context and `read --no-sync` use cached
text without current source citations. They are useful for inspection, not for
claiming the text is current. `--no-sync` is not a workaround for denied write
access; ordinary local lookup is the read-only route.

## Read an exact captured range

Read a saved source using the ID returned by capture:

```sh
lwiki --offline source read SOURCE_ID --max-bytes 12000
```

This returns its saved text and a citation. For an earlier copy, add
`--revision REVISION_ID` using a returned revision ID. To continue a long read,
follow the returned `--start` instruction and keep that revision fixed. Each
range has its own citation. `read --id SOURCE_ID` reads source metadata rather
than the saved text.

Copy the payload path and byte range returned by search or context:

```sh
lwiki --offline --json read --path 'RETURNED_PAYLOAD_PATH' --start START --end END --max-bytes 4096
```

Replace the path and `START`/`END` with the actual values. For captured notes,
the positions count UTF-8 bytes from zero and exclude the end position. Use the
payload path, not the revision metadata ID.

To read onward within a limit, omit `--end`:

```sh
lwiki --offline --json read --path 'RETURNED_PAYLOAD_PATH' --start START --max-bytes 8192
```

If more text remains, use the returned continuation position for the next read.
Each returned range has its own citation. Historical and withdrawn text remains
readable, with its status reported; reading it does not make it current.

Ordinary page `read` positions count from the text body, after frontmatter.
Search positions for authored pages can count from the whole file. Use
[inspect](everyday-use.md#inspect-and-edit-exact-bytes) for physical file positions,
and do not copy one kind of position into the other.

## Inspect named neighbors

If your wiki contains reviewed entities and relationships, find an entity and
inspect the facts connected to it:

```sh
lwiki --offline --json search 'the entity name' --mode lexical --kind entity --verify-selected
lwiki --offline --json graph neighbors ENTITY_ID --depth 2
```

Copy the exact entity ID from the selected hit's `record_ref.record_id`.
Similar names can refer to different things. Read the cited supporting and
contradicting passages before using a relationship in an answer. Direction,
negation, dates and other qualifiers matter; a two-step connection does not
establish a new fact.

The default depth is one; the maximum is two. Results can be truncated and have
no continuation cursor. Narrow the query or inspect each entity separately when
you need more detail. `--no-sync` returns cached relationships without citations;
add `--verify-selected` to check the selected evidence. General graph queries
and historical graph views are unavailable on normalized storage.

## Select passages with a host

An agent can help choose complementary passages when automatic context misses
facts. This advanced route prepares a selection task, lets your agent choose
from it, then verifies the original text. lwiki does not call the agent for you.

On a normalized index, prepare the task:

```sh
lwiki --offline context 'Which steps and exceptions apply?' --mode lexical --scope indexed-documents --limit 5 --candidates 80 --max-bytes 6000 --max-tokens 1500 --prepare-selection > selection-task.json
```

Give the successful task output to your agent and ask for a reply matching
`lwiki schema context-selection`. Save that reply as `selection-reply.json`.
If you used `--json`, give the agent only
`data.selection_packet.selector_input`. Then repeat the same question, filters
and limits with the reply:

```sh
lwiki --offline context 'Which steps and exceptions apply?' --mode lexical --scope indexed-documents --limit 5 --candidates 80 --max-bytes 6000 --max-tokens 1500 --selection selection-reply.json
```

The reply chooses supplied passage IDs; it must keep the task fingerprint
unchanged. If the evidence changed, prepare a new task. Check the final passages
and any omissions before asking for an answer. Selection still uses the output
limits and cannot guarantee a complete answer.

For an agent to choose ranges from complete original notes instead, use
`--prepare-original-selection --discover-originals`, then replay with
`--discover-originals --selection REPLY.json`. Inspect
`schema context-original-selection-auto` for that reply format. Keep this
separate from passage selection; their replies are not interchangeable.
