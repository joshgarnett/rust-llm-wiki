#[path = "fixtures/p17/common.rs"]
mod common;
#[path = "../test_support/paths.rs"]
mod test_paths;

use common::Fixture;
use lwiki::{
    catalog::DocumentRow,
    domain::*,
    retrieval::{render, segment, spaces::EmbeddingSettings},
};

fn source_document(body: String) -> DocumentRow {
    DocumentRow {
        path: VaultRelativePath::new("sources/article/content.md").unwrap(),
        hash: Blake3Hash::digest(body.as_bytes()),
        record_id: None,
        kind: None,
        title: "Restaurants".into(),
        aliases: vec![],
        headings: String::new(),
        tags: vec![],
        body: body.clone(),
        raw_text: body,
        source_id: Some(RecordId::new("source_article").unwrap()),
        owner_revision: Some(RecordId::new("revision_article").unwrap()),
        eligibility: Eligibility::Current,
        reasons: vec![],
    }
}

#[test]
fn restaurant_article_preserves_all_15254_bytes_with_seventeen_headings() {
    let fixture = Fixture::new();
    let reader = fixture.reader();
    let mut article = String::new();
    for n in 0..17 {
        article.push_str(&format!("## Restaurant {n}\n\n"));
        article.push_str(&"café 🦀 menu and seating. ".repeat(28));
        article.push_str("\n\n");
    }
    let tail = "\nFinal restaurant evidence: late reservations accepted.\n";
    assert!(article.len() + tail.len() < 15_254);
    article.push_str(&"x".repeat(15_254 - article.len() - tail.len()));
    article.push_str(tail);
    assert_eq!(article.len(), 15_254);
    assert_eq!(
        article
            .lines()
            .filter(|line| line.starts_with("## "))
            .count(),
        17
    );
    let document = source_document(article);
    let mut counts = vec![];
    for (hard, quality) in [(12_000, None), (12_000, Some(3000)), (20_000, None)] {
        let settings = EmbeddingSettings {
            max_input_bytes: hard,
            quality_target_bytes: quality,
            ..Default::default()
        };
        let units = render::render_document(&reader, &document, &settings).unwrap();
        assert_eq!(
            units,
            render::render_document(&reader, &document, &settings).unwrap()
        );
        counts.push(units.len());
        let mut cursor = 0;
        let mut reconstructed = String::new();
        for unit in &units {
            let input = unit.input();
            assert_eq!(input.utf8, unit.utf8);
            assert_eq!(input.input_hash, Blake3Hash::digest(input.utf8.as_bytes()));
            assert!(input.utf8.len() <= quality.unwrap_or(hard));
            let span = unit.source_span.unwrap();
            assert_eq!(span.start(), cursor);
            let body = document
                .raw_text
                .get(span.start() as usize..span.end() as usize)
                .unwrap();
            assert!(input.utf8.ends_with(body));
            reconstructed.push_str(body);
            cursor = span.end();
        }
        assert_eq!(cursor, 15_254);
        assert_eq!(reconstructed, document.raw_text);
        assert!(units.last().unwrap().utf8.ends_with(tail));
    }
    assert!(counts[0] > 1);
    assert!(counts[1] > counts[0]);
    assert_eq!(counts[2], 1);
}

#[test]
fn hard_split_inside_a_line_does_not_fabricate_an_oversized_heading() {
    let fixture = Fixture::new();
    let reader = fixture.reader();
    let header = "Title: \"Restaurants\"\nHeadings: []\n\n";
    let bound = 130;
    let capacity = bound - header.len();
    let body = format!("{}## {}", "a".repeat(capacity), "x".repeat(capacity * 3));
    let document = source_document(body);
    let settings = EmbeddingSettings {
        max_input_bytes: bound,
        ..Default::default()
    };
    let units = render::render_document(&reader, &document, &settings).unwrap();
    assert!(units.len() > 2);
    assert!(units[1].utf8[header.len()..].starts_with("## "));
    for unit in &units {
        assert!(unit.utf8.starts_with(header));
        assert!(unit.utf8.len() <= bound);
    }
    let reconstructed: String = units
        .iter()
        .map(|unit| {
            let span = unit.source_span.unwrap();
            &document.raw_text[span.start() as usize..span.end() as usize]
        })
        .collect();
    assert_eq!(reconstructed, document.raw_text);
}

#[test]
fn repeated_h2_labels_are_siblings_even_without_an_h1() {
    let raw = format!(
        "## First\n{}\n## Second\n{}\n### Child\n{}",
        "a".repeat(45),
        "b".repeat(45),
        "c".repeat(45)
    );
    let units = segment::split(&raw, 0, 0, 65, None).unwrap();
    let second = units
        .iter()
        .find(|unit| unit.text.starts_with("## Second"))
        .unwrap();
    assert_eq!(second.headings, ["Second"]);
    let child = units
        .iter()
        .find(|unit| unit.text.starts_with("### Child"))
        .unwrap();
    assert_eq!(child.headings, ["Second", "Child"]);
}

#[test]
fn heading_split_retains_the_complete_original_label() {
    let raw = format!("## {}\nremaining body", "é".repeat(30));
    let units = segment::split(&raw, 0, 0, 20, None).unwrap();
    for unit in &units {
        assert_eq!(unit.headings, ["é".repeat(30)]);
        assert!(raw.is_char_boundary(unit.span.start() as usize));
        assert!(raw.is_char_boundary(unit.span.end() as usize));
    }
}
