//! Fictional source-only fixtures; no question text, expected answers or provider.
use super::*;
use serde_json::{Value, json};
use std::time::Duration;

struct Fixture {
    text: String,
    source: SourceBinding,
    preparation: PreparationBinding,
    envelope: Envelope,
}
impl Fixture {
    fn new() -> Self {
        let text = format!(
            "General rule permits two days.\n{}\nApplication requires written notice.\nIf appointed abroad, a separate procedure applies.\nNationals of Exampleland are exempt.\n",
            "unrelated background\n".repeat(180)
        );
        let source = SourceBinding {
            source_id: id("source_one"),
            source_revision: id("revision_one"),
            raw_hash: Blake3Hash::digest(&text),
        };
        let preparation = PreparationBinding {
            recipe_id: id("source_only_v1"),
            recipe_hash: hash("recipe"),
            prompt_hash: hash("prompt"),
            schema_hash: hash("schema"),
            model_identity_hash: hash("mock-model"),
            settings_hash: hash("settings"),
            retained_generation_id: id("generation_one"),
            retained_generation_hash: hash("retained-generation"),
        };
        let node = |name: &str, kind, quote: &str| Node {
            id: id(name),
            kind,
            scope: format!("Generated navigation label for {name}"),
            evidence: span(&text, quote),
        };
        let nodes = vec![
            node("rule", NodeKind::Rule, "General rule permits two days."),
            node(
                "notice",
                NodeKind::Requirement,
                "Application requires written notice.",
            ),
            node(
                "abroad",
                NodeKind::Condition,
                "If appointed abroad, a separate procedure applies.",
            ),
            node(
                "exemption",
                NodeKind::Exception,
                "Nationals of Exampleland are exempt.",
            ),
        ];
        let edges = vec![
            Edge {
                from: id("rule"),
                to: id("notice"),
                kind: RelationKind::Requires,
                evidence: nodes[1].evidence.clone(),
            },
            Edge {
                from: id("rule"),
                to: id("abroad"),
                kind: RelationKind::Alternative,
                evidence: nodes[2].evidence.clone(),
            },
            Edge {
                from: id("notice"),
                to: id("exemption"),
                kind: RelationKind::Excepts,
                evidence: nodes[3].evidence.clone(),
            },
        ];
        let envelope = Envelope {
            schema: SCHEMA.into(),
            source: source.clone(),
            preparation: preparation.clone(),
            nodes,
            edges,
        };
        Self {
            text,
            source,
            preparation,
            envelope,
        }
    }
    fn original(&self) -> AdmittedOriginal<'_> {
        AdmittedOriginal {
            binding: &self.source,
            text: &self.text,
        }
    }
    fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.envelope).unwrap()
    }
    fn parse(&self) -> Result<ValidatedRelations<'_>> {
        parse(
            &self.bytes(),
            self.original(),
            &self.preparation,
            validation_limits(),
        )
    }
    fn value(&self) -> Value {
        serde_json::to_value(&self.envelope).unwrap()
    }
    fn reject(&self, value: Value) {
        assert!(
            parse(
                &serde_json::to_vec(&value).unwrap(),
                self.original(),
                &self.preparation,
                validation_limits(),
            )
            .is_err()
        );
    }
}
fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn hash(s: &str) -> Blake3Hash {
    Blake3Hash::digest(s)
}
fn span(text: &str, quote: &str) -> SpanBinding {
    let start = text.find(quote).unwrap() as u64;
    SpanBinding {
        span: ByteSpan::new(start, start + quote.len() as u64).unwrap(),
        quote_hash: hash(quote),
    }
}
fn validation_limits() -> ValidationLimits {
    ValidationLimits {
        max_hash_bytes: 64 * 1024 * 1024,
        deadline: Instant::now() + Duration::from_secs(10),
    }
}
fn limits() -> NominationLimits {
    NominationLimits {
        max_candidates: 80,
        max_span_bytes: 1024,
        max_text_bytes: 12000,
        max_node_visits: 80,
        max_edge_visits: 80,
        max_depth: 4,
        deadline: Instant::now() + Duration::from_secs(10),
    }
}

#[test]
fn source_only_parser_roundtrip_and_exact_distant_requirement() {
    let f = Fixture::new();
    let valid = f.parse().unwrap();
    assert_eq!(valid.envelope(), &f.envelope);
    assert_eq!(valid.annotation_bytes(), f.bytes().len());
    let out = valid.nominate(&[id("rule")], limits()).unwrap();
    assert_eq!(out.source, f.source);
    assert_eq!(out.node_visits, 4);
    assert_eq!(out.edge_visits, 3);
    assert_eq!(out.spans.len(), 4);
    assert_eq!(out.unexpanded_nodes, 0);
    assert!(out.omissions.is_empty());
    assert!(out.spans.iter().any(
        |p| p.evidence.span.start() > 3000 && p.text == "Application requires written notice."
    ));
    for proposal in &out.spans {
        assert_eq!(
            proposal.text,
            proposal.evidence.span.slice(&f.text).unwrap()
        );
        assert_eq!(hash(proposal.text), proposal.evidence.quote_hash);
        assert!(!proposal.text.contains("Generated navigation"));
    }
    assert_eq!(
        out.text_bytes,
        out.spans.iter().map(|s| s.text.len()).sum::<usize>()
    );
    for edge in &out.dependencies {
        assert_eq!(edge.assessment, Assessment::Unassessed);
        assert!(edge.from_candidate < out.spans.len());
        assert!(edge.to_candidate < out.spans.len());
        assert!(edge.relation_candidate < out.spans.len());
    }
}

#[test]
fn condition_is_unresolved_never_assumed_false_and_alternative_is_retained() {
    let f = Fixture::new();
    let valid = f.parse().unwrap();
    let out = valid.nominate(&[id("rule")], limits()).unwrap();
    let alternative = out
        .dependencies
        .iter()
        .find(|d| d.to == id("abroad"))
        .unwrap();
    assert_eq!(alternative.from, id("rule"));
    assert_eq!(alternative.kind, RelationKind::Alternative);
    assert_eq!(alternative.assessment, Assessment::Unassessed);
    assert!(
        out.spans[alternative.to_candidate]
            .text
            .starts_with("If appointed abroad")
    );
    let mut fabricated_truth = f.value();
    fabricated_truth["nodes"][2]["applicable"] = json!(false);
    f.reject(fabricated_truth);
}

#[test]
fn strict_wire_rejects_unknown_duplicate_missing_truncated_and_versions() {
    let f = Fixture::new();
    for path in ["root", "source", "preparation", "node", "edge", "span"] {
        let mut value = f.value();
        match path {
            "root" => value["query"] = json!("forbidden query input"),
            "source" => value["source"]["extra"] = json!(1),
            "preparation" => value["preparation"]["extra"] = json!(1),
            "node" => value["nodes"][0]["quote"] = json!("generated invented evidence"),
            "edge" => value["edges"][0]["confidence"] = json!(1),
            "span" => value["nodes"][0]["evidence"]["span"]["extra"] = json!(1),
            _ => unreachable!(),
        }
        f.reject(value);
    }
    let raw = String::from_utf8(f.bytes()).unwrap();
    for raw in [
        raw.replacen(
            "\"schema\":",
            "\"schema\":\"lwiki.source-relations.v1\",\"schema\":",
            1,
        ),
        raw.replacen("\"start\":", "\"start\":0,\"start\":", 1),
        format!("{raw} {{}}"),
        raw[..raw.len() - 1].to_owned(),
        format!("```json\n{raw}\n```"),
    ] {
        assert!(
            parse(
                raw.as_bytes(),
                f.original(),
                &f.preparation,
                validation_limits()
            )
            .is_err()
        );
    }
    for field in ["schema", "source", "preparation", "nodes", "edges"] {
        let mut value = f.value();
        value.as_object_mut().unwrap().remove(field);
        f.reject(value);
    }
    let mut value = f.value();
    value["schema"] = json!("lwiki.source-relations.v2");
    f.reject(value);
    let mut value = f.value();
    value["nodes"][0]["id"] = json!("bad node!");
    f.reject(value);
}

#[test]
fn provenance_and_span_hashes_are_never_self_authorizing() {
    let f = Fixture::new();
    for field in ["source_id", "source_revision", "raw_hash"] {
        let mut value = f.value();
        value["source"][field] = if field == "raw_hash" {
            json!(hash("stale"))
        } else {
            json!("different")
        };
        f.reject(value);
    }
    for field in [
        "recipe_id",
        "recipe_hash",
        "prompt_hash",
        "schema_hash",
        "model_identity_hash",
        "settings_hash",
        "retained_generation_id",
        "retained_generation_hash",
    ] {
        let mut value = f.value();
        value["preparation"][field] = if field.ends_with("_id") {
            json!("different")
        } else {
            json!(hash("different"))
        };
        f.reject(value);
    }
    let mut value = f.value();
    value["nodes"][0]["evidence"]["quote_hash"] = json!(hash("invented"));
    f.reject(value);
    let mut value = f.value();
    value["edges"][0]["evidence"]["quote_hash"] = json!(hash("invented"));
    f.reject(value);
    let changed = format!("{}changed", f.text);
    assert!(
        parse(
            &f.bytes(),
            AdmittedOriginal {
                binding: &f.source,
                text: &changed
            },
            &f.preparation,
            validation_limits(),
        )
        .is_err()
    );
}

#[test]
fn reference_ids_and_duplicate_relations_fail_closed() {
    let f = Fixture::new();
    for field in ["from", "to"] {
        let mut value = f.value();
        value["edges"][0][field] = json!("unknown");
        f.reject(value);
    }
    let mut value = f.value();
    value["nodes"][1]["id"] = value["nodes"][0]["id"].clone();
    f.reject(value);
    let mut value = f.value();
    let edge = value["edges"][0].clone();
    value["edges"].as_array_mut().unwrap().push(edge);
    f.reject(value);
    let mut value = f.value();
    value["nodes"][0]["scope"] = json!("x".repeat(257));
    f.reject(value);
    let valid = f.parse().unwrap();
    assert!(valid.nominate(&[id("unknown")], limits()).is_err());
    assert!(valid.nominate(&[id("rule"), id("rule")], limits()).is_err());
}

#[test]
fn invalid_ranges_utf8_splits_and_non_utf8_wire_fail() {
    let f = Fixture::new();
    for (start, end) in [
        (2, 1),
        (0, 0),
        (0, f.text.len() as u64 + 1),
        (u64::MAX, u64::MAX),
    ] {
        let mut value = f.value();
        value["nodes"][0]["evidence"]["span"] = json!({"start":start,"end":end});
        f.reject(value);
    }
    let mut unicode = Fixture::new();
    unicode.text = "é rule".into();
    unicode.source.raw_hash = hash(&unicode.text);
    unicode.envelope.source = unicode.source.clone();
    unicode.envelope.nodes.truncate(1);
    unicode.envelope.edges.clear();
    unicode.envelope.nodes[0].evidence = SpanBinding {
        span: ByteSpan::new(1, 2).unwrap(),
        quote_hash: hash("invalid"),
    };
    assert!(unicode.parse().is_err());
    let mut bytes = f.bytes();
    bytes[0] = 0xff;
    assert!(parse(&bytes, f.original(), &f.preparation, validation_limits()).is_err());
}

#[test]
fn parsing_ceilings_refuse_instead_of_truncate() {
    let f = Fixture::new();
    assert!(parse(&[], f.original(), &f.preparation, validation_limits()).is_err());
    assert!(
        parse(
            &vec![b' '; MAX_ANNOTATION_BYTES + 1],
            f.original(),
            &f.preparation,
            validation_limits(),
        )
        .is_err()
    );
    let mut too_many = f.envelope.clone();
    too_many.nodes = (0..MAX_NODES + 1)
        .map(|i| {
            let mut n = f.envelope.nodes[0].clone();
            n.id = id(&format!("node_{i}"));
            n
        })
        .collect();
    // Small enough to exercise count validation rather than only byte refusal.
    for n in &mut too_many.nodes {
        n.scope = "s".into();
    }
    let bytes = serde_json::to_vec(&too_many).unwrap();
    assert!(bytes.len() <= MAX_ANNOTATION_BYTES);
    assert!(parse(&bytes, f.original(), &f.preparation, validation_limits()).is_err());
    too_many = f.envelope.clone();
    too_many.edges = vec![f.envelope.edges[0].clone(); MAX_EDGES + 1];
    assert!(
        parse(
            &serde_json::to_vec(&too_many).unwrap(),
            f.original(),
            &f.preparation,
            validation_limits(),
        )
        .is_err()
    );
}

#[test]
fn candidate_byte_depth_work_caps_and_cycles_disclose_omissions() {
    let mut f = Fixture::new();
    f.envelope.edges.push(Edge {
        from: id("notice"),
        to: id("rule"),
        kind: RelationKind::Requires,
        evidence: f.envelope.nodes[0].evidence.clone(),
    });
    let valid = f.parse().unwrap();
    let out = valid.nominate(&[id("rule")], limits()).unwrap();
    assert_eq!(out.node_visits, 4);
    assert_eq!(out.edge_visits, 4);
    assert_eq!(out.spans.len(), 4);
    for (reason, lim) in [
        (
            OmissionReason::CandidateLimit,
            NominationLimits {
                max_candidates: 1,
                ..limits()
            },
        ),
        (
            OmissionReason::SpanLimit,
            NominationLimits {
                max_span_bytes: 1,
                ..limits()
            },
        ),
        (
            OmissionReason::TextLimit,
            NominationLimits {
                max_text_bytes: 1,
                ..limits()
            },
        ),
        (
            OmissionReason::NodeWorkLimit,
            NominationLimits {
                max_node_visits: 1,
                ..limits()
            },
        ),
        (
            OmissionReason::EdgeWorkLimit,
            NominationLimits {
                max_edge_visits: 0,
                ..limits()
            },
        ),
        (
            OmissionReason::DepthLimit,
            NominationLimits {
                max_depth: 0,
                ..limits()
            },
        ),
    ] {
        let out = valid.nominate(&[id("rule")], lim).unwrap();
        assert!(out.omissions.iter().any(|(r, n)| *r == reason && *n > 0));
        assert!(out.spans.len() <= lim.max_candidates);
        assert!(out.text_bytes <= lim.max_text_bytes);
        assert!(out.node_visits <= lim.max_node_visits);
        assert!(out.edge_visits <= lim.max_edge_visits);
    }
    assert!(
        valid
            .nominate(
                &[id("rule")],
                NominationLimits {
                    deadline: Instant::now(),
                    ..limits()
                }
            )
            .is_err()
    );
    assert!(
        valid
            .nominate(
                &[id("rule")],
                NominationLimits {
                    max_candidates: 81,
                    ..limits()
                }
            )
            .is_err()
    );
}

#[test]
fn relation_support_span_costs_a_separate_candidate_and_failed_dependency_does_not_expand() {
    let mut f = Fixture::new();
    f.envelope.edges.truncate(1);
    f.envelope.edges[0].evidence = span(&f.text, "unrelated background");
    let valid = f.parse().unwrap();
    let out = valid
        .nominate(
            &[id("rule")],
            NominationLimits {
                max_candidates: 2,
                ..limits()
            },
        )
        .unwrap();
    assert_eq!(out.spans.len(), 1);
    assert!(out.dependencies.is_empty());
    assert_eq!(out.edge_visits, 1);
    assert!(out.omissions.contains(&(OmissionReason::CandidateLimit, 1)));
    let out = valid
        .nominate(
            &[id("rule")],
            NominationLimits {
                max_candidates: 3,
                ..limits()
            },
        )
        .unwrap();
    assert_eq!(out.spans.len(), 3);
    assert_eq!(out.dependencies.len(), 1);
    let edge = &out.dependencies[0];
    assert_ne!(edge.to_candidate, edge.relation_candidate);
}

#[test]
fn oversized_scope_and_typed_kinds_are_rejected_without_reinterpretation() {
    let f = Fixture::new();
    for scope in ["", "   ", "line\nline"] {
        let mut value = f.value();
        value["nodes"][0]["scope"] = json!(scope);
        f.reject(value);
    }
    let mut value = f.value();
    value["nodes"][0]["kind"] = json!("accepted_fact");
    f.reject(value);
    let mut value = f.value();
    value["edges"][0]["kind"] = json!("entails");
    f.reject(value);
    let mut value = f.value();
    value["nodes"][0]["evidence"]["span"]["start"] = json!(0.5);
    f.reject(value);
    let mut value = f.value();
    value["nodes"][0]["evidence"] = Value::Null;
    f.reject(value);
    let valid = f.parse().unwrap();
    assert!(
        valid
            .nominate(
                &[id("rule")],
                NominationLimits {
                    max_text_bytes: 12001,
                    ..limits()
                }
            )
            .is_err()
    );
    assert!(
        valid
            .nominate(
                &[id("rule")],
                NominationLimits {
                    max_span_bytes: 1025,
                    ..limits()
                }
            )
            .is_err()
    );
    assert!(
        valid
            .nominate(
                &[id("rule")],
                NominationLimits {
                    max_node_visits: 0,
                    ..limits()
                }
            )
            .is_err()
    );
}

#[test]
fn validation_hash_work_is_preflighted_and_occurrences_are_charged() {
    let f = Fixture::new();
    let bytes = f.bytes();
    let expected = f.text.len() as u64
        + f.envelope
            .nodes
            .iter()
            .map(|n| n.evidence.span.len())
            .sum::<u64>()
        + f.envelope
            .edges
            .iter()
            .map(|e| e.evidence.span.len())
            .sum::<u64>();
    let valid = parse(
        &bytes,
        f.original(),
        &f.preparation,
        ValidationLimits {
            max_hash_bytes: expected,
            ..validation_limits()
        },
    )
    .unwrap();
    assert_eq!(valid.validation_hash_bytes(), expected);
    let error = parse(
        &bytes,
        f.original(),
        &f.preparation,
        ValidationLimits {
            max_hash_bytes: expected - 1,
            ..validation_limits()
        },
    )
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    // Even a stale original is refused for insufficient work BEFORE hashing.
    let stale = format!("{}!", f.text);
    let error = parse(
        &bytes,
        AdmittedOriginal {
            binding: &f.source,
            text: &stale,
        },
        &f.preparation,
        ValidationLimits {
            max_hash_bytes: 0,
            ..validation_limits()
        },
    )
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert!(error.details.is_null());
    let error = parse(
        &bytes,
        AdmittedOriginal {
            binding: &f.source,
            text: &stale,
        },
        &f.preparation,
        validation_limits(),
    )
    .err()
    .unwrap();
    assert_eq!(
        error.details["source_relation_hash_bytes_consumed"],
        json!(stale.len())
    );
    assert!(
        parse(
            &bytes,
            f.original(),
            &f.preparation,
            ValidationLimits {
                deadline: Instant::now(),
                ..validation_limits()
            }
        )
        .is_err()
    );
}

#[test]
fn long_valid_annotation_remains_prepared_but_query_span_cap_is_disclosed() {
    let mut f = Fixture::new();
    f.envelope.nodes.truncate(1);
    f.envelope.edges.clear();
    f.envelope.nodes[0].evidence = SpanBinding {
        span: ByteSpan::new(0, 1500).unwrap(),
        quote_hash: hash(&f.text[..1500]),
    };
    let valid = f.parse().unwrap();
    let out = valid.nominate(&[id("rule")], limits()).unwrap();
    assert!(out.spans.is_empty());
    assert_eq!(out.omissions, vec![(OmissionReason::SpanLimit, 1)]);
    assert!(out.dependencies.is_empty());
}
