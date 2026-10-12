use super::*;
use clap::Parser;

#[test]
fn omitted_mode_preserves_the_explicit_local_boundary() {
    for (arguments, omitted, mode) in [
        (
            vec!["lwiki", "search", "meeting"],
            true,
            crate::retrieval::SearchMode::Hybrid,
        ),
        (
            vec!["lwiki", "search", "meeting", "--mode", "lexical"],
            false,
            crate::retrieval::SearchMode::Lexical,
        ),
        (
            vec!["lwiki", "search", "meeting", "--mode", "literal"],
            false,
            crate::retrieval::SearchMode::Literal,
        ),
    ] {
        let args = Arguments::try_parse_from(arguments).unwrap();
        let super::super::Command::Search(command) = args.command else {
            panic!("search");
        };
        assert_eq!(command.search.mode.is_none(), omitted);
        assert_eq!(command.search.plan().mode, mode);
    }
}

#[test]
fn preview_reports_admission_limits_without_inventing_work_or_usage() {
    let mut document = config::LocalConfigDocument::default();
    document.remote.max_requests = Some(1);
    document.remote.deadline_ms = Some(12);
    document.remote.max_request_bytes = Some(200);
    let value = preview(
        true,
        PreparationTrigger::Lookup {
            query: "private question".into(),
        },
        &document,
    );
    assert_eq!(value["planned_limits"]["requests"], 1);
    assert_eq!(value["planned_limits"]["deadline_ms"], 12);
    assert_eq!(value["planned_limits"]["request_bytes"], 200);
    assert_eq!(value["deadline_scope"], "starting_additional_work");
    assert_eq!(value["credentials_resolved"], false);
    assert!(value["inputs"].is_null());
    assert!(!value.to_string().contains("private question"));
}

#[test]
fn status_preserves_saved_data_and_reports_actual_network_separately() {
    let mut envelope = Envelope::success(
        "source add",
        json!({"allocated_ids":{"source":"Source.Saved"},"status":"committed"}),
    );
    let saved = envelope.data.clone();
    attach(
        &mut envelope,
        json!({"state":"failed","observations":{"network_used":true},"error":{"code":"PROVIDER_RESPONSE"}}),
        false,
    );
    assert!(envelope.ok);
    assert!(envelope.meta.network_used);
    assert_eq!(envelope.data["allocated_ids"], saved["allocated_ids"]);
    assert_eq!(envelope.data["status"], "committed");
    let mut output = Vec::new();
    present(&envelope.data["automatic_embeddings"], &mut output).unwrap();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("saved notes remain available")
    );
}

#[test]
fn committed_owner_selection_rejects_proposals_and_operational_records() {
    let temp = tempfile::tempdir().unwrap();
    let wiki = temp.path().join("wiki");
    crate::app::offline::init(&wiki, "Owner selection", OperationOptions::default()).unwrap();
    let fs = crate::vault::VaultFs::new(crate::vault::VaultRoot::explicit(&wiki).unwrap());
    let app = OfflineApp::new(fs, OperationOptions::default()).unwrap();
    let page = "---\nwiki_schema: '1'\nwiki_id: Page.Saved\nwiki_kind: page\ntitle: Saved\nwiki_status: reviewed\n---\nSaved fact.\n";
    let outcome = app
        .page_put(
            VaultRelativePath::new("knowledge/pages/saved.md").unwrap(),
            page.as_bytes().to_vec(),
            None,
        )
        .unwrap();
    let mut data = serde_json::to_value(outcome).unwrap();
    assert_eq!(
        committed_owners(&app, &data),
        vec![VaultRelativePath::new("knowledge/pages/saved.md").unwrap()]
    );
    data["status"] = json!("prepared");
    assert!(committed_owners(&app, &data).is_empty());
    data["status"] = json!("committed");
    data["reused"] = json!(true);
    assert!(committed_owners(&app, &data).is_empty());
}
