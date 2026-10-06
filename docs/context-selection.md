# Selecting context with a host agent

Use this workflow when a question needs several complementary passages and automatic context leaves out required facts. `lwiki` prepares candidate evidence, your host agent selects passage IDs, and `lwiki` verifies and packs the selected original text. The host agent must already be available in your environment; the CLI does not launch it or call a generation provider.

The selection mechanism and model-assisted completeness remain under evaluation; the separate maintained question-to-Page workflow must be judged independently. Development pilots recovered missing facts, but the independent acceptance gate remains open. The first host pilots took roughly 98–438 seconds of observed wall time; these are upper bounds, not measured model inference time. Actual host token usage and cost were unavailable. Allow for preparation, agent selection and final validation when assessing whether it suits your task.

## Prepare, select and verify

For a checked brief and searchable draft Page, follow the maintained [cited-Page recipe](../skills/llm-wiki/references/cited-page.md). It records requested conditions, verifies actual packed support, retains explicit gaps, permits at most one budgeted verified gap read and reconciles author conflicts. This is host work; it does not launch a model or establish native completeness.

Start with an initialized wiki containing captured sources. Check the selected layout. These commands use lexical retrieval on an already activated normalized index, with `--scope indexed-documents`; they work offline without an embedding provider. For a legacy index, use `--scope current` in both commands. Normalized activation remains explicit; do not rebuild to change layout merely to follow this recipe. Global advertised modes do not imply normalized literal, semantic/hybrid or strict current/historical parity. Report an unsupported requested mode instead of silently switching it. Replace the wiki path and question with your own, using the same values and retrieval limits in both commands.

```sh
lwiki --wiki ./my-wiki --offline context \
  'Which setup steps and exceptions apply to this task?' \
  --mode lexical --scope indexed-documents --limit 5 --candidates 80 \
  --max-bytes 6000 --max-tokens 1500 \
  --prepare-selection > selection-task.json
```

Check that the command succeeded before giving the file to an agent. Without `--json`, the output is the exact JSON task for the selector. With `--json`, it is an output envelope: only `data.selection_packet.selector_input` is the selector task. Prepared candidates are not final answer context.

Give one fresh agent the task file and ask it to follow the included instructions. It should inspect only the supplied question and candidates, then return the exact `packet_fingerprint` and an ordered list of existing card IDs. Keep a requirement/support/gap ledger as a separate local sidecar; never add it to the core selector reply. Save its unmodified reply as `selection-reply.json`. The reply schema is available through `lwiki schema context-selection`; do not copy IDs or fingerprints from another task.

```sh
lwiki --wiki ./my-wiki --offline context \
  'Which setup steps and exceptions apply to this task?' \
  --mode lexical --scope indexed-documents --limit 5 --candidates 80 \
  --max-bytes 6000 --max-tokens 1500 \
  --selection selection-reply.json
```

The final output contains original passages and citations. Selection order determines packing priority; selected passages can still be omitted when the final byte, token or per-source limits are reached. Inspect the returned omissions and use only the final verified context when answering. For the bounded cited-Page recipe, declare the gap before at most one verified read within the chosen remaining bound; any unsupported or unread conditions stay explicit in the brief and draft. Further research is a separate scope choice.

For model input, use the final command's plain stdout, or extract `data.text` after checking a successful JSON response. That is the budgeted rendered evidence, including its citations. Retain the full JSON for application checks and audit; feeding it all to a model also includes structured copies of the passages and metadata, so it can substantially exceed the rendered-context budget. Inspect omissions and warnings separately before deciding whether the evidence is adequate. Separating model content from retained artifacts is also an explicit [LangChain tool-message pattern](https://github.com/langchain-ai/langchain/blob/master/libs/core/langchain_core/messages/tool.py); no LangChain dependency is required here.

On a legacy layout that supports hybrid selection, if a complete embedding cache already exists for the question and sources, use `--mode hybrid` in **both** commands. Offline operation never acquires missing vectors. Keep other filters, budget and verification options identical as well.

## Limits and failures

The examples use the historical evaluation budget of 6,000 bytes / 1,500 estimated tokens for final context. The CLI default is 12,000 / 3,000. To allow up to 16 KiB, pass `--max-bytes 16384 --max-tokens 4096` in **both** commands. Increasing only the byte limit leaves the token limit in force. These are ceilings, not output targets, and include any instruction/output reservations. They bound the rendered context, not the complete JSON envelope or the intermediate selector task. A larger budget still needs evaluation for completeness and irrelevant material.

The task contains at most 80 candidate cards and 130,048 UTF-8 bytes, with 1,024 additional bytes reserved by the evaluation protocol for transport instructions. Byte limits include metadata and JSON escaping. Owners are interleaved before truncation to preserve source coverage; a bounded pool can still omit required evidence. The byte/4 token estimate is not actual model usage.

The v2 task shows a shared source table and exact passage text, spans, IDs and rendered costs. Full citations and ranking metadata remain local. An authority commitment binds those retained details; the packet fingerprint also binds the displayed task and instructions. This removes repeated metadata from model input without changing evidence bytes or final verification. Local preparation separately limits supplied authority to 1,600 cards, 8 MiB of serialized metadata in total and 130,048 bytes per card, including candidates later omitted. These are work bounds, not model token or peak-memory guarantees.

A reply may contain at most 20 unique supplied IDs and 4,096 bytes. Unknown IDs, duplicate fields or IDs, extra fields and malformed replies fail validation. `--selection -` reads a reply from standard input. File input must be a regular file.

Changes to the question, request limits, candidate evidence or snapshot invalidate an old reply. Prepare a new task and select again after an intentional change; editing the fingerprint cannot make a stale selection valid. Keep the old failure in evaluation records rather than silently replacing it with a retry.

An empty selection produces empty context. It does not prove that the wiki lacks an answer. Verified citations establish source identity, exact text and freshness; neither citations nor model selection certify answer completeness. Selection is supported for lexical indexed-document context on normalized vaults and supported current document context on legacy vaults, not literal, graph or historical/snapshot context. Normalized proof authenticates selected document dependencies against the discovery generation and reports `global_membership_verified: false`; it is not strict global Current freshness. Draft Pages are discoverable with lexical `search TITLE --status draft` and readable by their actual ID/path; default search can exclude drafts. `page init` keeps draft status, while guarded `page put` requires full Markdown with the returned record metadata plus body and the complete file hash. A stale guard requires substantive rereading/reconciliation, preserving author text. For an intended external author edit on a normalized vault, count one `index sync` within the remaining bounds before verified rereading. A selected read otherwise can refuse with `FRESHNESS_CONFLICT`; preserve that refusal, inspect that the external edit is intended, then sync and reread. Discovery refresh does not reconcile the body or justify blind guard replacement.

For controlled comparisons, follow [the context evaluation protocol](evaluating-context.md). Its selector isolation, timeout and resource accounting are responsibilities of the evaluation harness, not properties enforced by the CLI.
