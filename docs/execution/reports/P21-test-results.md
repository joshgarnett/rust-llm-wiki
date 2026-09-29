# P21 complete current native test results

Complete required all-target scope qualified by unchanged passing targets from the second whole invocation (exit101 at an obsolete API expectation), plus all corrected/unexecuted targets, binary and example harnesses in the exit0 continuation. Neither whole --all-targets invocation is relabeled a pass. Enabled parents are counted from the last summary of each Cargo target, excluding nested subprocess summaries. Ignored entries are test children explicitly invoked by enabled native recovery, concurrency or measurement parents. No required suite is skipped.

Final source fingerprint `325e425accaad00f69d658c68121d46469cac5e69d7fc3c2ef44d6b07912678a`; log SHA256 `e229ccaf0bc0c2b19557fb5f10a5da5d185ac253d99bb86bee56ae8c75c057b7`; log `/private/tmp/lwiki-p21-final/tests.log`.

| Cargo target | Passed parents | Explicit children | Seconds |
|---|---:|---:|---:|
| `unittests src/lib.rs` | 136 | 3 | 763.86 |
| `unittests src/main.rs` | 0 | 0 | 0.0 |
| `tests/api_extraction.rs` | 12 | 0 | 33.94 |
| `tests/catalog_crash_recovery.rs` | 2 | 1 | 2.23 |
| `tests/catalog_generations.rs` | 15 | 0 | 2.21 |
| `tests/catalog_scan_eligibility.rs` | 14 | 0 | 0.25 |
| `tests/change_read_preconditions.rs` | 8 | 0 | 1.01 |
| `tests/changes_prepare_journal.rs` | 17 | 0 | 24.2 |
| `tests/changes_recovery.rs` | 28 | 1 | 341.32 |
| `tests/context_cli.rs` | 5 | 0 | 8.11 |
| `tests/context_freshness.rs` | 18 | 0 | 3.8 |
| `tests/contracts.rs` | 4 | 0 | 0.05 |
| `tests/entity_decisions.rs` | 19 | 0 | 52.91 |
| `tests/entity_decisions_cli.rs` | 3 | 0 | 1.65 |
| `tests/entity_decisions_inverse.rs` | 10 | 0 | 66.16 |
| `tests/extraction_cli.rs` | 3 | 0 | 4.11 |
| `tests/extraction_import.rs` | 13 | 0 | 14.86 |
| `tests/graph_cli.rs` | 4 | 0 | 0.68 |
| `tests/graph_queries.rs` | 17 | 0 | 5.46 |
| `tests/graph_resolution.rs` | 16 | 0 | 74.93 |
| `tests/graph_review.rs` | 19 | 0 | 79.94 |
| `tests/job_accounting.rs` | 13 | 1 | 5.97 |
| `tests/m1_workflow.rs` | 1 | 0 | 8.05 |
| `tests/m2_workflow.rs` | 1 | 0 | 12.91 |
| `tests/machine_contract.rs` | 6 | 0 | 0.07 |
| `tests/offline_application.rs` | 14 | 0 | 9.13 |
| `tests/offline_cli.rs` | 10 | 0 | 3.49 |
| `tests/provider_dispatch.rs` | 4 | 0 | 1.67 |
| `tests/provider_trust.rs` | 6 | 0 | 0.02 |
| `tests/provider_wire.rs` | 10 | 0 | 13.53 |
| `tests/records_lossless.rs` | 8 | 0 | 0.02 |
| `tests/release_workflows.rs` | 1 | 0 | 55.15 |
| `tests/remote_cli.rs` | 5 | 0 | 7.11 |
| `tests/research_acquisition.rs` | 18 | 0 | 17.94 |
| `tests/research_cli.rs` | 11 | 0 | 13.11 |
| `tests/research_dispatch.rs` | 7 | 0 | 3.66 |
| `tests/research_extraction.rs` | 7 | 0 | 24.07 |
| `tests/research_planning.rs` | 8 | 0 | 0.04 |
| `tests/research_reports.rs` | 13 | 0 | 15.27 |
| `tests/research_stage_contracts.rs` | 7 | 0 | 0.03 |
| `tests/research_stages.rs` | 1 | 0 | 0.73 |
| `tests/research_workflow.rs` | 23 | 0 | 112.42 |
| `tests/resolution_cli.rs` | 4 | 0 | 4.39 |
| `tests/retrieval_baseline.rs` | 2 | 1 | 12.25 |
| `tests/retrieval_lexical.rs` | 13 | 0 | 1.97 |
| `tests/semantic_retrieval.rs` | 21 | 0 | 17.4 |
| `tests/skill_export.rs` | 4 | 0 | 7.35 |
| `tests/sources_evidence.rs` | 15 | 0 | 9.53 |
| `tests/vault_fs.rs` | 15 | 0 | 1.1 |
| `tests/windows_acl_policy.rs` | 7 | 0 | 0.0 |
| `unittests examples/fixture_hash.rs` | 0 | 0 | 0.0 |

Total: 618 passed parents; zero failures; 7 explicitly invoked children. Release-profile skill adds 4 parents and executes 32 maintained recipe steps.

Supporting earlier passing targets: `/private/tmp/lwiki-p21-final-second-failed/tests.log`, SHA256 `6bad232e1d90bcd0d63e410a2f7ad1f1f547a50fccbb31353e101605527a5cf3`, observed source fingerprint `ea627f8d6e0cad300f8f29398ab771d61ec32397f97544f0171862edc43d24ab`. Its whole invocation exited101; only its individually passing targets are reused. Final source differs only in the corrected extraction CLI test and qualification script; all product/Cargo/shared fixtures and reused target source bytes reconcile. Every target row has its actual log/hash/source association in P21-checks.json.
