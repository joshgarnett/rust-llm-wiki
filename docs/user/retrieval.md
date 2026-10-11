# Search and citations

Find relevant passages in your notes, then read the text behind an answer.
Your agent can do this for you. The commands below use your selected wiki;
add `--wiki "/absolute/path/to/Wiki"` when you want a different one.

## Search and inspect cited context

Search for an exact phrase or command:

```sh
lwiki --offline search 'a phrase from your note' --mode literal
```

Literal search is case sensitive and includes punctuation. It does not use
wildcards or regular expressions. Use word search when you remember the topic
rather than the exact wording:

```sh
lwiki --offline search 'project release decisions' --mode lexical
```

For passages that help answer a question:

```sh
lwiki --offline context 'What did we decide about the release?' --max-bytes 6000 --max-tokens 1500
```

Context returns text and citations. It does not write an answer. Give the text
to your agent, or read it yourself. A source citation identifies a saved note
version and the exact quoted passage. Check that the passage supports the claim,
including any conditions or exceptions.

Search and context have output limits, so they can omit useful facts. If a fact
is missing, search for its wording, narrow to the relevant source, or read more
of that note. An empty result does not prove the information is absent.

Run `index sync` after editing wiki files outside lwiki. To check all managed
files for errors, use `check`. On a normalized index, add `--verify-selected` to
search when you need citations; plain search there is for finding notes. Context
checks its selected evidence. Search by meaning uses `--mode semantic` or `hybrid` and needs
[prepared embeddings](sources-and-providers.md#optional-remote-providers).

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

On normalized indexes, context defaults to `indexed-documents`, which checks
the selected files. `--scope indexed-evidence --mode lexical` searches only
indexed captured sources without syncing. `--include-historical` can include
earlier captured versions in literal or word context; read their labels before
using them as current facts. Historical semantic and hybrid selection are
unsupported.

An older index may use whole-wiki `current` verification instead. If a command
reports that a scope is unsupported, use its suggested scope. To use the
normalized layout, back up the wiki and run `index rebuild --normalized`.
Snapshot context and `read --no-sync` use cached text without current source
citations. They are useful for inspection, not for claiming the text is current.

## Read an exact captured range

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
