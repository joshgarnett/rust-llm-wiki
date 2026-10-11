use super::*;
use crate::domain::ErrorCode;
use rusqlite::{params, types::Value};
use std::time::Duration;

fn presence(connection: &Connection, text: &str, query: &str) -> (bool, bool) {
    connection
        .query_row(
            &format!("SELECT instr(?1,?2)>0,{FUNCTION}(?1,?2)"),
            params![text, query],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[test]
fn original_text_presence_matches_instr_without_case_or_unicode_normalization() {
    let connection = Connection::open_in_memory().unwrap();
    for (query, texts) in [
        ("café", vec!["first\ncafé\nlast", "cafe\u{301}", "CAFÉ", ""]),
        ("🦀", vec!["α🦀ω", "🦀🦀", "a crab", ""]),
        ("one\ntwo", vec!["zero\none\ntwo\nthree", "one two"]),
        ("aa", vec!["aaa", "aaaaa", "AA", "a"]),
        ("missing", vec!["", "a source containing something else"]),
        (
            "target",
            vec!["before\0target\0after", "target\0after", "none\0"],
        ),
    ] {
        let registration = register(&connection, query, None).unwrap();
        for text in texts {
            let (baseline, candidate) = presence(&connection, text, query);
            assert_eq!(candidate, baseline, "query={query:?}, text={text:?}");
            assert_eq!(candidate, text.contains(query));
        }
        registration.finish().unwrap();
    }
}

#[test]
fn presence_does_not_change_nonoverlapping_original_byte_offsets_or_64_match_cap() {
    let connection = Connection::open_in_memory().unwrap();
    let registration = register(&connection, "aa", None).unwrap();
    let text = format!("🦀\0{}", "aa ".repeat(100));
    assert_eq!(presence(&connection, &text, "aa"), (true, true));
    let offsets = super::super::literal::literal_matches(&text, "aa");
    assert_eq!(offsets.len(), 64);
    assert_eq!(offsets[0], 5..7);
    assert_eq!(offsets[63], 194..196);
    assert_eq!(
        super::super::literal::literal_matches("aaaaa", "aa"),
        vec![0..2, 2..4]
    );
    registration.finish().unwrap();
}

#[test]
fn count_and_capped_selection_share_presence_filters_and_binary_order() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE documents(path TEXT,raw_text TEXT,current INTEGER,source INTEGER);\
             INSERT INTO documents VALUES\
             ('z','needle',1,1),('A','needle after a line\nbreak',1,1),\
             ('a','needle',1,1),('old','needle',0,1),\
             ('note','needle',1,0),('absent','other',1,1);",
        )
        .unwrap();
    let registration = register(&connection, "needle", None).unwrap();
    for presence in [
        "instr(raw_text,?1)>0".to_owned(),
        format!("{FUNCTION}(raw_text,?1)"),
    ] {
        let predicate = format!("current=1 AND source=1 AND {presence}");
        let count: i64 = connection
            .query_row(
                &format!("SELECT count(*) FROM documents WHERE {predicate}"),
                ["needle"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
        let mut statement = connection
            .prepare(&format!(
                "SELECT path FROM documents WHERE {predicate} ORDER BY path COLLATE BINARY LIMIT 2"
            ))
            .unwrap();
        let paths = statement
            .query_map(["needle"], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(paths, ["A", "a"]);
    }
    registration.finish().unwrap();
}

#[test]
fn guarded_sql_preserves_instr_on_non_text_while_callback_rejects_wrong_types() {
    let connection = Connection::open_in_memory().unwrap();
    let registration = register(&connection, "12", None).unwrap();
    for value in [
        Value::Null,
        Value::Integer(123),
        Value::Real(12.5),
        Value::Blob(b"x12y\0".to_vec()),
        Value::Blob(b"absent".to_vec()),
    ] {
        let (baseline, candidate): (Option<bool>, Option<bool>) = connection
            .query_row(
                &format!("SELECT instr(?1,?2)>0,CASE WHEN typeof(?1)='text' THEN {FUNCTION}(?1,?2) ELSE instr(?1,?2)>0 END"),
                params![value, "12"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(candidate, baseline);
        let error = connection
            .query_row::<bool, _, _>(
                &format!("SELECT {FUNCTION}(?1,?2)"),
                params![value, "12"],
                |row| row.get(0),
            )
            .unwrap_err();
        assert_eq!(
            crate::catalog::sql::sql_error(error).code,
            ErrorCode::IndexCorrupt
        );
    }
    for needle in [
        Value::Integer(12),
        Value::Blob(b"12".to_vec()),
        Value::Text("other".into()),
    ] {
        let error = connection
            .query_row::<bool, _, _>(
                &format!("SELECT {FUNCTION}(?1,?2)"),
                params!["12", needle],
                |row| row.get(0),
            )
            .unwrap_err();
        assert_eq!(
            crate::catalog::sql::sql_error(error).code,
            ErrorCode::IndexCorrupt
        );
    }
    registration.finish().unwrap();
}

#[test]
fn expired_shared_deadline_maps_to_budget_exceeded_without_reset() {
    let connection = Connection::open_in_memory().unwrap();
    let registration = register(
        &connection,
        "needle",
        Some(Instant::now() - Duration::from_secs(1)),
    )
    .unwrap();
    let error = connection
        .query_row::<bool, _, _>(
            &format!("SELECT {FUNCTION}(?1,?2)"),
            params!["needle", "needle"],
            |row| row.get(0),
        )
        .unwrap_err();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::OperationInterrupted)
    );
    assert_eq!(
        crate::catalog::sql::sql_error(error).code,
        ErrorCode::BudgetExceeded
    );
    registration.finish().unwrap();
}

fn registered(connection: &Connection) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_function_list WHERE name=?1 AND narg=2)",
            [FUNCTION],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn registrations_cleanup_on_success_and_error_and_do_not_replace_live_owner() {
    let connection = Connection::open_in_memory().unwrap();
    assert!(register(&connection, "", None).is_err());
    assert!(!registered(&connection));
    let registration = register(&connection, "first", None).unwrap();
    assert!(register(&connection, "second", None).is_err());
    assert_eq!(presence(&connection, "first", "first"), (true, true));
    registration.finish().unwrap();
    assert!(!registered(&connection));
    {
        let _registration = register(&connection, "second", None).unwrap();
        let mut statement = connection
            .prepare(&format!("SELECT {FUNCTION}(?1,?2)"))
            .unwrap();
        assert!(
            statement
                .query_row(params!["second", "wrong"], |row| row.get::<_, bool>(0))
                .is_err()
        );
    }
    assert!(!registered(&connection));
    let registration = register(&connection, "third", None).unwrap();
    assert_eq!(presence(&connection, "third", "third"), (true, true));
    registration.finish().unwrap();
    assert!(!registered(&connection));
}
