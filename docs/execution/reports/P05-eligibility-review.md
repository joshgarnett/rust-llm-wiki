# Independent P05 eligibility review

Reviewer: gpt-6-sol P05-B worker, separately reviewing P05-A-owned scan/eligibility code after publication handoff. The runtime could not create/restart Astra; root authorized the playbook's independent Sol fallback. Review is read-only; root/A own fixes and test execution. Status: all blocking findings resolved by inspected final A code and targeted regression evidence. No source edits made by reviewer.

Scope: complete identity registry (malformed/future/duplicate IDs), typed companion links, original/content dependencies, source head advance/withdrawal, current support versus historical/retracted evidence, closure/cycles, identity/description split, decisions/conflicts/superseders, immutable assertion proposition, whole-overlay validation. Selected storage/record-schema contracts and source successor behavior read.

## Findings requiring regression evidence

**A-R1: malformed copies with safely readable YAML IDs could leave a winner.** `scan::readable_ids` fallback initially fed the entire bare `wiki_id` value into `RecordId::new`, so `wiki_id: alpha # explanation` in an envelope with a separate malformed YAML field reserved no ID. Quoted scalar trailing comments and quoted YAML key variants also bypassed fallback. Repro: copy `entities/alpha.md`, add an inline comment to its ID and break another YAML field; scan must exclude alpha and diagnose both paths. Recommended reuse of the existing YAML adapter on individual top-level field lines rather than diverging scalar grammar. Resolved: `isolated_fields` parses each independent root field plus indented continuation through the existing strict adapter; tests include comments, quotes, escapes, quoted/spaced keys, and folded/literal block scalar IDs. The adjacent adopted-marker fallback now shares that helper, closing the quoted semantic-key bypass by inspection.

**A-R2: unrelated evidence chains could claim supersession.** Initially only predecessor kind/cycle was validated; evidence could supersede a predecessor for another assertion/source. Same-revision successors also could alter the supposedly corrected source span. Required shared assertion/source; same-revision locator/span/hash equality. New-revision unique-quote revalidation remains legitimate under P04 and must not be frozen to old revision/span. Root accepted this distinction; A added `evidence_successor_chain_disagreement` checks. Resolved: final A regression checks unrelated source chains, same-revision span disagreement, and legitimate new-revision revalidation.

**A-R3: accept/reject decisions with no affected IDs remained valid.** Empty input/output sets produced an empty target loop and an apparently current decision. Explicit affected canonical IDs are required. Resolved: A added `decision_missing_assertion_outcome`; final A regression asserts an empty acceptance decision is Invalid.

## Other evidence

Reviewed declaration-only support graph, cycle detection, structural invalid propagation separated from entity description support, all evidence membership closure, intact-current support/dispute classification, decision conflict sets using inputs and outputs, invalid superseders excluded, complete original/content/absence dependencies, exact assertion proposition fields with normalized negation/modality defaults, overlay acceptance requiring current support and rejection of new invalidity/duplicates. Canonical alias outcomes are already explicit; no prose inference. Source/decision/control hashes cover complete notes and new membership.

B's real SQL Engine capture/page recovery exercises the same A validator/projector (11 B tests passed before root refinements). Root owns full suite and actual SIGKILL qualification. This review does not claim live providers, power loss, other OS behavior or completion of later P09/P10 mutation workflows.

## Final reviewed fingerprint and qualification

Observed SHA256 matches A handoff: scan `7998238165c40f29a375da6469dd6c6dc50d28f73f825c5338c3412a595950c0`; eligibility `baab394437abdb4a1410565997652cd2720d2c6ced4d0b1ddd843def1e971d99`; tests `2277fa1d84044f1b0b7b6ec717294cfc34d62cb094c95b337c893331940dd5fc`. A reports final `cargo test --locked --offline --test catalog_scan_eligibility`: 13 passed, 0.44s; `cargo clippy --locked --offline --lib -- -D warnings`: passed, 0.74s. Reviewer inspected exact artifacts and regression assertions; did not rerun Cargo without a lease. Root's integrated gate remains authoritative.

Nonblocking conservative behavior: root-field reservations can be recovered even from an unterminated initial `---` envelope. This may exclude an identity claimed by malformed literal text, but never creates typed identity or grants authority. The complete control manifest remains the raw-note hash; no freshness guarantee is weakened.
