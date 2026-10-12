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

Context returns readable passages, source states and links to saved notes when
available. Give the text to your agent, or read it yourself. Check that each
passage supports the claim, including any conditions or exceptions.

When you want your agent to save a cited answer, start with
`lwiki --verbose context 'YOUR QUESTION'`. This includes exact reference objects
that identify the saved note version and quoted passage. Your final answer can
still use readable source links. Use `--json` when you need the structured result.

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

The first limit reached determines how much text is returned. These limits
include exact references; ordinary readable output uses no more space. Tokens
are estimated from byte size, so your agent may count them differently. Read
warnings and omissions as well as the passages. If using JSON, `data.text` is the rendered evidence; the full
response also includes metadata and may repeat passages.

Check warnings before treating a result as current. If a lookup says newer notes
may be missing, run `lwiki index sync` from a session with write access and search
again. After changing an original note outside the wiki, capture it again to save
the updated text.

For an earlier saved copy, use the revision option in the next section. Check its
date and status before using it to describe the present.

## Read an exact captured range

For more text from a result, copy its `Read: lwiki source read ...` command. It
opens the same saved version without another search. Keep the original `--wiki`
and configuration options.

You can also read a saved source using the ID returned by capture:

```sh
lwiki --offline source read SOURCE_ID --max-bytes 12000
```

This returns its saved text, source state and a link when available. Add
`--verbose` before `source read` when you need the exact reference objects to
save a cited page. For an earlier copy, add
`--revision REVISION_ID` using a returned revision ID. To continue a long read,
follow the returned `--start` instruction and keep that revision fixed. Each
range has its own citation. `read --id SOURCE_ID` reads source metadata rather
than the saved text.

For an exact range, use the payload path and byte positions from
`--verbose context` or structured `--json search` output:

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

## Extract and review relationships

Ask your connected agent to turn a selected note into relationships you can look
up and check against the original text. For example:

> Capture my meeting note at `/absolute/path/to/meeting.md`. Extract its people,
> organizations and relationships. Show the proposed claims and their quoted
> support, keep similar names separate, and save only claims the note supports.
> Then show me the relationships around Ada.

Your agent saves a copy it can cite, proposes relationships, identifies the people
and organizations, and checks each claim against its quoted evidence before
accepting it. Ask for proposals only if you want to inspect them before acceptance.
The result should name the saved relationships, quote their support, and report
uncertain identities, unsupported claims or omitted text. A similar name does not
establish that two people are the same person.

Choose how to produce the proposals:

| Choice | What you need | What happens |
| --- | --- | --- |
| Your connected agent | The agent installed during setup and a selected note | Your agent reads the saved note and proposes relationships. No separate generation service is needed. |
| A generation API | A [configured generation model](sources-and-providers.md#add-an-optional-generation-model) and permission for explicit requests | lwiki sends the selected captured text to that service within your request limits and stages its proposals. Your agent still resolves identities and reviews the evidence. |

The agent route is the default. lwiki does not launch an agent itself. `--offline`
prevents lwiki's provider requests; your connected agent uses its own model and
account. Embeddings help search by meaning and cannot extract relationships.

For the API route, ask your agent to use your configured generation service with
a stated request limit. One short selected note can use:

```sh
lwiki --json graph extract --executor api --source-id SOURCE_ID --max-requests 1 --max-output-tokens 4096
```

Replace `SOURCE_ID` with the ID returned by capture. Keep the output allowance
within the saved service cap. The command proposes relationships for review.
Your agent then checks who each name refers to and reads the supporting and
contradicting passages before saving supported relationships. Ask it to report
the saved relationships, their citations and anything it could not resolve.

Your agent follows the [graph cookbook](../../skills/llm-wiki/references/workflows.md#extract-and-review-relationships).
You do not need to prepare its JSON requests yourself.

An API failure can still use paid tokens. Keep the reported run ID and inspect
`lwiki jobs status --run RUN_ID` before retrying. A reasoning model can consume its
output allowance before returning proposals; follow the reported limit adjustment.
Your [reasoning effort choice](sources-and-providers.md#choose-reasoning-effort)
controls thinking effort and does not increase that allowance.

After review, inspect a saved entity's neighbors as shown below. Read the quoted
passages before using a relationship in an answer. Partial extraction is useful,
but it does not mean every relationship in your wiki has been found.

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

## Find missing details

If the first passages miss an important condition or exception, ask your agent
to search for that detail and read more of the relevant notes. It can use the
[advanced retrieval workflow](../../skills/llm-wiki/references/workflows.md#advanced-retrieval-and-embedding-preparation)
when ordinary context is insufficient.

## Open a saved source

Cited context and `source read` can show a `Source:` Markdown link beside the
returned text. Copy the link into your answer or open it to inspect the saved
copy. The label tells you whether it is the current copy, an older copy or a
withdrawn source. For more text, use the supplied `Read: lwiki source read ...`
command. It opens the same saved version. Keep the original `--wiki` and
configuration options. Older links open the copy used by the citation.

The JSON output provides the same ready-to-copy link in `source_link.markdown`
for `source read`, or `source_links` on each context passage. Use
`--verbose context` or `--verbose source read` to keep the exact citation when
saving a cited page. A link helps you open a note; the
citation identifies the exact saved text.

If the generated copy is missing or has been edited, the command can still return
the captured text and citation without a link. Context keeps its supplied read
command when the link is absent. These links refer to
files in your local wiki; another computer needs its own local copy.
