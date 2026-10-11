# Experimental Source relation preparation

Source relation preparation is an explicit experiment for retaining a rule with
its distant conditions, requirements, exceptions and alternatives in cited
context. It is disabled by default and currently requires normalized storage.
There is no demonstrated development-corpus quality improvement: native
completeness remains **13/32 positive tasks and 79/129 required facts**.

## Retrieval-only evaluation plan, 2026-10-11

The current priority is retrieval completeness. Scale experiments are deferred;
their resource or platform failures do not block this evaluation. Paid generation
is authorized up to **USD 20 total**, including pilot work, failed attempts and
unknown accounting holds. No generation request has been executed under this
authorization. Earlier statements that generation consent is pending are
superseded. Existing embedding acquisition and its accounting remain separate.

The hypothesis is that source-only preparation can connect a rule to a distant
condition, exception or alternative, so deterministic retrieval returns the
complete applicable evidence. It remains untested on the development corpus.
The existing implementation nominates fragments of up to 1,024 bytes, ranks scope
labels by query-term overlap, and operates within already admitted Sources.
It cannot repair owner-discovery losses. A useful relationship also fails to help
when its complete original support cannot fit the unchanged passage limits.

The [Dense X Retrieval study](https://arxiv.org/abs/2312.06648) provides evidence
that retrieval-unit granularity can affect retrieval and downstream task results.
Its proposition-based results motivate inspecting our representation; they do
not validate the current fragment-relation design or predict our task score.
We will test the implemented representation before changing it or acquiring the
whole corpus.

1. **Test fidelity and usable support on eight Sources, at most USD 5.** Freeze
   two longest encoded requests, two Sources with distant conditions/exceptions,
   two with alternative procedures, and two ordinary Sources, chosen from source
   content alone with deterministic ID tie-breaking. Keep questions and answer
   labels out of selection and generation. Before dispatch, an independent critic
   records expected source-only relationships, exact supporting ranges and the
   limits. Freeze the supported model, recipe and 4,096-output-token ceiling.
   Require eight valid, untruncated retained responses, no unsupported,
   reversed or wrong-scenario emitted relationships, and at least three declared
   distant rule/qualification examples across three Sources whose complete support
   fits the existing limits. Record omissions too. Stop on a structural or fidelity
   failure; do not retune the prompt or representation within this trial. This is
   a representation diagnostic, not a retrieval-quality score.
2. **Complete all 994 Sources only if the pilot warrants it.** Reuse the eight
   successful preparations without paying for them again. Use disjoint manifests
   for eight pilot Sources and the remaining 986, with separate Run lifetime caps
   of USD 5 and USD 15. Source inspection confirms that retained annotations remain
   eligible across Runs; a dedicated public-command reuse check remains required
   before paid dispatch. The provider does not deduplicate across Runs: verify the
   sets are disjoint and their union is exactly the original 994 Sources. Do not
   create a third full-corpus Run. Use one attempt per Source, serial execution,
   Run request ceilings of eight and 986, and a finite three-hour campaign window;
   continuation receives only the time remaining in that window.
   Refresh prices and inspect actual sealed inputs. Forecast remaining input cost
   and maximum output cost separately from conservative reservation ceilings;
   require the remainder forecast, a 25% contingency, its spent/unknown charges
   and the next reservation to fit the USD 15 continuation cap. Unused pilot
   allowance is not transferred. Reassess before continuing. Unknown charges are
   retained. Incomplete acquisition fails the
   coverage requirement and cannot become a partial quality success.
3. **Compare actual returned evidence on all 40 development tasks.** Only after
   994/994 successful preparations and reconciled accounting, freeze source,
   publication and cache identities. Reuse the validated paired offline executor
   and existing query vectors. Compare baseline with the explicitly named
   model-prepared, deterministic-query workflow under identical evidence/proof
   budgets. Require at least **17/32 complete positives**, preservation of every
   one of the **79 baseline facts**, all **eight absent controls**, exact applicable
   citations and an independent **at least 9/10 with no correctness blocker** for
   the retrieval workflow, using a rubric frozen before dispatch. Report every
   regression, cost and measured latency.
   A failure closes this candidate without another parameter sweep. A pass permits
   a separately frozen unseen evaluation; it does not change the unprepared native
   baseline, prove answer generation, promote defaults or qualify a release.

The immediate action is to freeze the pilot's exact Sources, critic checklist,
fresh quote and reusable acquisition arrangement. No additional user approval is
needed for that experiment within the USD 20 ceiling. Holdout labels remain
private. No new model, harness, scale benchmark or build is justified by this
planning checkpoint alone.

## Ordinary command path

`relations prepare` authenticates complete current Sources and returns an exact
manifest without calling a provider or persisting preparation. It requires a
configured generation service, including in offline mode, because the manifest
binds its profile, model, settings and configuration identity. A dry run is not
an input-count or cost forecast. Add `--quote` to seal every actual request and
return a finite monetary allowance without reading credentials, sending requests
or persisting a Run. Quoting requires a supported bound and complete valid rate
card; a missing rate, expired price or oversized request refuses the whole quote.

For one Source, a caller can save the JSON response and extract
`data.manifest_json` **verbatim**, without parsing and re-encoding that string:

```sh
lwiki --wiki WIKI --json --offline --profile primary relations prepare \
  --source-id SOURCE_ID --max-output-tokens 1024 \
  --providers-config PROVIDERS.toml --max-requests 1 \
  --attempts-per-task 1 --concurrency 1 --deadline-ms 900000 > plan.json
jq -jr '.data.manifest_json' plan.json > manifest.json
```

For a priced preparation, add `--quote --max-cost 10` to the preparation command.
The exact manifest is unchanged. `data.quote.entries` binds each Source, task,
descriptor and prepared request to its byte, token and cost ceilings.
`aggregate_worst_case` sums one attempt per Source; each individual attempt must
fit the proposed empty Run's limits. The aggregate may exceed the lifetime cap:
cheap settled attempts can permit progress, while expensive attempts or unknown
charges can stop an incomplete batch. `aggregate_fits_lifetime_limits` describes
that conservative total, and `complete_acquisition_guaranteed` is always false.
Execution rechecks prices, inputs and all retained charges before sending.
Prepared-wire fingerprints precede the later retained codec/profile fields.
The quote is limited to 16 MiB and its complete response to 64 MiB; output is
refused whole when it cannot fit.

Review complete input coverage and an enforceable finite provider allowance
before starting paid work. The configured provider attempt timeout must fit
inside the remaining invocation and Run deadlines. Preparation uses generation;
an embedding-only profile and embedding authorization do not cover it.

```sh
lwiki --wiki WIKI --json --profile primary relations run \
  --file manifest.json --run run_relations_example --max-tasks 1 \
  --providers-config PROVIDERS.toml --max-requests 1 \
  --attempts-per-task 1 --concurrency 1 --deadline-ms 900000
```

Continue with the same Run, configuration and exact manifest bytes. The finite
invocation task allowance does not reset lifetime request, token, cost or unknown
accounting holds. A completed retained run can be replayed offline. Inspect its
accounting with `jobs status --run run_relations_example`; missing billing data
remains unknown rather than becoming a zero cost.

The experiment is selected separately at query time:

```sh
lwiki --wiki WIKI --json --offline context 'harbor permit' \
  --mode lexical --scope indexed-documents --excerpt-bytes 1024 \
  --max-bytes 12000 --max-tokens 3000 --experimental-source-relations
```

The hidden experimental flag accepts automatic lexical indexed-document
selection. It does not qualify other modes, the ordinary 240-byte excerpt
default, or a model-assisted answer workflow. The comparison uses identical
excerpt, output and proof budgets for both arms.

## Evidence, freshness and reconstruction

Generated links are unassessed navigation metadata, not accepted assertions.
Returned evidence remains original Source byte ranges with exact revision,
span and quote hashes. Complete relation groups must fit the existing evidence
budget; over-budget groups are omitted whole. Source refresh and withdrawal
invalidate current navigation while preserving historical preparation records.

Raw responses, their complete input descriptors, annotations and receipts are
immutable Run records. Querying and normalized cache reconstruction use those
retained records offline. Rebuilding a lost cache does not require provider
credentials, its descriptor cache, or another provider request. Publication and
reconstruction legitimately change discovery snapshot generations.

## Local validation, 2026-10-11 UTC

The integrated synthetic API workflow passes public preparation, one mock
request, three distant original facts, exact citations, atomic budget omission,
complete-cache-loss reconstruction, refresh, withdrawal and historical-byte
preservation. The normalized 17-Source continuation passes the same Run across
1+8+8 tasks with unchanged 60-second invocation limits and all 17 attempts and
unknown holds retained. Two earlier legacy-layout schedules exceeded that
limit; those failures remain unqualified.

A serialized CLI fixture completes twelve executable commands with one bounded
Responses-v1 loopback request: normalized rebuild, baseline and experimental
context, offline prepare, run, status, offline replay, default context, config-free
experimental context, cache-loss rebuild, repeated context and final status.
It authenticates the exact manifest, outbound source-only descriptor, immutable
artifacts, original citations and unchanged financial holds. Its whole test
interval was 5.220 seconds; this is a tiny mock workflow, not provider latency or
production capacity. A separate committed-before-acknowledgment fixture passes
missing-checkpoint publication followed by exact no-growth replay.

Validation reuses unaffected passing checks rather than claiming a new full
suite: the final two-case replay passed, following seven other focused passes
and the earlier passing publication, catalog, proof, accounting and CLI groups.
Earlier namespace, ordering, fixture and timeout failures remain recorded.
There were no external generation calls. Mock request accounting deliberately
retains unknown cost because the fictional service has no rate card.

The shipping offline quotation also completes all 994 public development Sources.
Its manifest is byte-identical to ordinary preparation: 2,645,558 bytes binding
8,483,469 descriptor bytes. Actual encoded requests total 8,595,791 bytes, with
a maximum of 119,628 bytes; every Source has a distinct authenticated wire hash.
All 3,987 pinned Source, publication and cache files preserve their bytes.
The command takes 6.293 seconds and peaks at 55,705,600 bytes resident memory on
the observed host. These are preparation measurements, not retrieval or provider
performance evidence.

At the quoted rates and 4,096 output tokens, the supported conservative bound is
USD 0.5303416 per attempt and USD 527.1595504 for all 994 attempts. A proposed
USD 10 serial lifetime cap cannot guarantee complete acquisition. This is a
worst-case reservation bound, not an estimate of actual usage. No paid request
was made, and incomplete acquisition cannot qualify retrieval quality.

The filesystem checks distinguish retained-data writes from SQLite reader
coordination: an existing catalog SHM timestamp may change even when every file's
contents, identity and directory membership stay fixed. Initial strict timestamp
assertions failed on this difference; their diagnostic evidence is retained.
The corrected two-case replay passes, completing 27 distinct affected cases
across the retained check groups. Independent review accepts all six local quote
gates, including the actual 994-Source output; this is no whole-goal acceptance.

The full 40-question development gate, every baseline fact, all absent controls,
exact citations, independent unseen/HIGH acceptance, semantic retrieval and
representative 25K/~2.5GB qualification remain open. No default promotion or
0.2.0 release qualification follows from this local workflow.
