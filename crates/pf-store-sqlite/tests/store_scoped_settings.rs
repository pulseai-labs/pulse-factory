//! The scoped settings through the store port on the real store (ADR-0003, ADR-0016): the two
//! effects, the two reads, the scope chain, and the rows that stay after a project is removed.

mod common;

use common::{add_project, applied, commit, open, rejected, remove_project};
use pf_engine::port::store::{
    AdmissionOutcome, Commit, Committed, Effect, ORCHESTRATOR_AGENT, ProjectKind, SettingRow,
    SettingScope, SettingScopeKind, Store,
};
use pf_store_sqlite::SqliteStore;

const KEY: &str = "chat.self_turn_cap";

fn row(scope: SettingScope, key: &str, value: &str, by: &str, at: &str) -> SettingRow {
    SettingRow {
        scope,
        key: key.to_string(),
        value: value.to_string(),
        set_by: by.to_string(),
        set_at: at.to_string(),
    }
}

fn set_commit(id: &str, r: SettingRow) -> Commit {
    let line = format!(
        "setting_set {} admitted scope={} command={id}",
        r.key,
        r.scope.text()
    );
    commit(
        "setting_set",
        id,
        &format!("{{\"key\":\"{}\",\"value\":{}}}", r.key, r.value),
        AdmissionOutcome::Admitted,
        None,
        Some(&format!("{}@{}", r.key, r.scope.text())),
        vec![Effect::SetSetting(r)],
        Some(line),
    )
}

fn unset_commit(id: &str, scope: SettingScope, key: &str) -> Commit {
    let line = format!(
        "setting_unset {key} admitted scope={} command={id}",
        scope.text()
    );
    commit(
        "setting_unset",
        id,
        &format!("{{\"key\":\"{key}\"}}"),
        AdmissionOutcome::Admitted,
        None,
        Some(&format!("{key}@{}", scope.text())),
        vec![Effect::UnsetSetting {
            scope,
            key: key.to_string(),
        }],
        Some(line),
    )
}

fn scope(project: Option<&str>, agent: Option<&str>) -> SettingScope {
    SettingScope::new(project, agent)
}

fn setup() -> (tempfile::TempDir, SqliteStore, String) {
    let (dir, store) = open();
    let p1 = add_project(
        &store,
        "add-1",
        "alpha",
        "/work/alpha",
        ProjectKind::Folder,
        None,
    );
    assert_eq!(p1, "P-1");
    (dir, store, p1)
}

fn outbox(store: &SqliteStore) -> usize {
    store.unwritten_ledger_lines(common::LEDGER).unwrap().len()
}

#[test]
fn set_setting_stores_one_row_at_each_of_the_four_scopes_and_a_second_set_replaces_it() {
    let (_dir, store, p1) = setup();
    let scopes = [
        scope(None, None),
        scope(Some(&p1), None),
        scope(None, Some(ORCHESTRATOR_AGENT)),
        scope(Some(&p1), Some(ORCHESTRATOR_AGENT)),
    ];
    for (i, s) in scopes.iter().enumerate() {
        applied(
            store
                .commit(set_commit(
                    &format!("set-{i}"),
                    row(s.clone(), KEY, &format!("{i}"), "cli", "t1"),
                ))
                .unwrap(),
        );
    }
    assert_eq!(store.setting_rows().unwrap().len(), 4);
    // The same scope and key again: one row, with the new value, set_by and set_at.
    applied(
        store
            .commit(set_commit(
                "set-again",
                row(scopes[1].clone(), KEY, "9", "D-1", "t2"),
            ))
            .unwrap(),
    );
    let rows = store.setting_rows().unwrap();
    assert_eq!(rows.len(), 4);
    let r = rows.iter().find(|r| r.scope == scopes[1]).unwrap();
    assert_eq!(
        (r.value.as_str(), r.set_by.as_str(), r.set_at.as_str()),
        ("9", "D-1", "t2")
    );
}

#[test]
fn a_set_naming_a_removed_project_an_unknown_project_or_an_unknown_agent_is_rejected() {
    let (_dir, store, p1) = setup();
    let p2 = add_project(
        &store,
        "add-2",
        "beta",
        "/work/beta",
        ProjectKind::Folder,
        None,
    );
    applied(remove_project(&store, "rm-2", &p2));
    let before = outbox(&store);
    for (i, s) in [
        scope(Some(&p2), None),
        scope(Some("P-9"), None),
        scope(None, Some("ghost")),
        scope(Some(&p1), Some("ghost")),
    ]
    .into_iter()
    .enumerate()
    {
        let why = rejected(
            store
                .commit(set_commit(
                    &format!("bad-{i}"),
                    row(s, KEY, "3", "cli", "t1"),
                ))
                .unwrap(),
        );
        assert!(!why.is_empty());
    }
    let why = rejected(
        store
            .commit(set_commit(
                "bad-removed",
                row(scope(Some(&p2), None), KEY, "3", "cli", "t1"),
            ))
            .unwrap(),
    );
    assert!(why.contains(&p2), "{why}");
    assert!(store.setting_rows().unwrap().is_empty());
    assert_eq!(
        outbox(&store),
        before,
        "a rejected commit has no outbox line"
    );
    // Nothing was written: no admission either, so the same ID commits later.
    assert!(store.admission("@controller", "bad-0").unwrap().is_none());
}

#[test]
fn unset_deletes_exactly_its_row_and_an_unset_with_no_row_is_rejected() {
    let (_dir, store, p1) = setup();
    for (i, s) in [scope(None, None), scope(Some(&p1), None)]
        .into_iter()
        .enumerate()
    {
        applied(
            store
                .commit(set_commit(&format!("s{i}"), row(s, KEY, "3", "cli", "t1")))
                .unwrap(),
        );
    }
    applied(
        store
            .commit(unset_commit("u1", scope(Some(&p1), None), KEY))
            .unwrap(),
    );
    let left = store.setting_rows().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].scope, scope(None, None));
    let why = rejected(
        store
            .commit(unset_commit("u2", scope(Some(&p1), None), KEY))
            .unwrap(),
    );
    assert_eq!(why, format!("no value of {KEY} is set at project:{p1}"));
    assert_eq!(store.setting_rows().unwrap().len(), 1);
}

#[test]
fn each_applied_commit_has_one_admission_and_one_outbox_line_and_a_rejection_has_neither() {
    let (_dir, store, _p1) = setup();
    let base = outbox(&store);
    let (admission, line) = applied(
        store
            .commit(set_commit(
                "s1",
                row(scope(None, None), KEY, "3", "cli", "t1"),
            ))
            .unwrap(),
    );
    assert_eq!(admission.kind, "setting_set");
    assert_eq!(
        admission.unit_id.as_deref(),
        Some("chat.self_turn_cap@server")
    );
    assert!(
        line.unwrap()
            .body
            .contains("setting_set chat.self_turn_cap admitted scope=server")
    );
    assert_eq!(outbox(&store), base + 1);
    rejected(
        store
            .commit(unset_commit(
                "u-none",
                scope(None, Some(ORCHESTRATOR_AGENT)),
                KEY,
            ))
            .unwrap(),
    );
    assert_eq!(outbox(&store), base + 1);
    assert!(store.admission("@controller", "u-none").unwrap().is_none());
    // The same ID with the same content is a replay and changes nothing.
    let again = store
        .commit(set_commit(
            "s1",
            row(scope(None, None), KEY, "3", "cli", "t1"),
        ))
        .unwrap();
    assert!(matches!(again, Committed::Replayed(_)), "{again:?}");
    assert_eq!(outbox(&store), base + 1);
}

#[test]
fn setting_chain_lists_the_rows_most_specific_first_and_skips_the_scopes_a_context_lacks() {
    let (_dir, store, p1) = setup();
    for (i, s) in [
        scope(None, None),
        scope(None, Some(ORCHESTRATOR_AGENT)),
        scope(Some(&p1), None),
        scope(Some(&p1), Some(ORCHESTRATOR_AGENT)),
    ]
    .into_iter()
    .enumerate()
    {
        applied(
            store
                .commit(set_commit(
                    &format!("s{i}"),
                    row(s, KEY, &format!("{i}"), "cli", "t1"),
                ))
                .unwrap(),
        );
    }
    let texts =
        |rows: Vec<SettingRow>| -> Vec<String> { rows.iter().map(|r| r.scope.text()).collect() };
    assert_eq!(
        texts(
            store
                .setting_chain(KEY, Some(&p1), Some(ORCHESTRATOR_AGENT))
                .unwrap()
        ),
        [
            "project:P-1/agent:orchestrator",
            "project:P-1",
            "agent:orchestrator",
            "server"
        ]
    );
    assert_eq!(
        texts(
            store
                .setting_chain(KEY, None, Some(ORCHESTRATOR_AGENT))
                .unwrap()
        ),
        ["agent:orchestrator", "server"]
    );
    assert_eq!(
        texts(store.setting_chain(KEY, Some(&p1), None).unwrap()),
        ["project:P-1", "server"]
    );
    assert_eq!(
        texts(store.setting_chain(KEY, None, None).unwrap()),
        ["server"]
    );
    assert!(
        store
            .setting_chain("chat.deny_commands", Some(&p1), None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn setting_rows_lists_every_row_by_key_then_scope_and_reads_every_back_as_none() {
    let (_dir, store, p1) = setup();
    for (i, (s, key)) in [
        (scope(Some(&p1), None), "chat.deny_commands"),
        (scope(None, None), "chat.deny_commands"),
        (scope(None, Some(ORCHESTRATOR_AGENT)), KEY),
        (scope(None, None), KEY),
    ]
    .into_iter()
    .enumerate()
    {
        let value = if key == KEY { "3" } else { "[]" };
        applied(
            store
                .commit(set_commit(
                    &format!("s{i}"),
                    row(s, key, value, "cli", "t1"),
                ))
                .unwrap(),
        );
    }
    let rows = store.setting_rows().unwrap();
    let got: Vec<(String, String)> = rows
        .iter()
        .map(|r| (r.key.clone(), r.scope.text()))
        .collect();
    assert_eq!(
        got,
        [
            ("chat.deny_commands".to_string(), "project:P-1".to_string()),
            ("chat.deny_commands".to_string(), "server".to_string()),
            (KEY.to_string(), "agent:orchestrator".to_string()),
            (KEY.to_string(), "server".to_string()),
        ]
    );
    let server = rows
        .iter()
        .find(|r| r.key == KEY && r.scope.kind() == SettingScopeKind::Server);
    let server = server.unwrap();
    assert_eq!(
        (&server.scope.project_id, &server.scope.agent_id),
        (&None, &None)
    );
}

#[test]
fn the_scope_chain_and_the_text_forms_match_the_approach_for_all_four_kinds() {
    let kinds = [
        (scope(None, None), SettingScopeKind::Server, "server"),
        (
            scope(Some("P-1"), None),
            SettingScopeKind::Project,
            "project:P-1",
        ),
        (
            scope(None, Some("orchestrator")),
            SettingScopeKind::Agent,
            "agent:orchestrator",
        ),
        (
            scope(Some("P-1"), Some("orchestrator")),
            SettingScopeKind::ProjectAgent,
            "project:P-1/agent:orchestrator",
        ),
    ];
    for (s, kind, text) in kinds {
        assert_eq!(s.kind(), kind);
        assert_eq!(s.text(), text);
    }
    let chain = SettingScope::chain(Some("P-1"), Some("orchestrator"));
    assert_eq!(
        chain,
        [
            scope(Some("P-1"), Some("orchestrator")),
            scope(Some("P-1"), None),
            scope(None, Some("orchestrator")),
            scope(None, None),
        ]
    );
    assert_eq!(
        SettingScope::chain(None, Some("orchestrator")),
        [scope(None, Some("orchestrator")), scope(None, None)]
    );
    assert_eq!(
        SettingScope::chain(Some("P-1"), None),
        [scope(Some("P-1"), None), scope(None, None)]
    );
    assert_eq!(SettingScope::chain(None, None), [scope(None, None)]);
    assert_eq!(SettingScopeKind::ProjectAgent.as_str(), "project_agent");
}

#[test]
fn after_a_project_is_removed_its_rows_stay_and_a_set_naming_it_is_rejected() {
    let (_dir, store, p1) = setup();
    applied(
        store
            .commit(set_commit(
                "s1",
                row(scope(Some(&p1), None), KEY, "2", "cli", "t1"),
            ))
            .unwrap(),
    );
    applied(remove_project(&store, "rm-1", &p1));
    let rows = store.setting_rows().unwrap();
    assert_eq!(rows.len(), 1, "the row stays");
    assert_eq!(rows[0].scope.project_id.as_deref(), Some("P-1"));
    let why = rejected(
        store
            .commit(set_commit(
                "s2",
                row(scope(Some(&p1), None), KEY, "4", "cli", "t2"),
            ))
            .unwrap(),
    );
    assert!(why.contains("P-1") && why.contains("removed"), "{why}");
}
