//! AC-1: the launch command line, the seat token and the version check. A stub `omp` stands in
//! for the real program, so this test spends nothing.

mod common;

use common::*;
use pf_engine::port::agent::{AgentError, AgentPort, RuleSet};

#[test]
fn argv_holds_each_flag_of_the_adr() {
    let dir = tempfile::tempdir().unwrap();
    let omp = stub_omp(dir.path(), "18.11.3", STUB_STARTS_A_TURN);
    let a = adapter(dir.path(), omp);
    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let plan = plan(
        &work,
        &[
            ("plugins", "/plugins/ossify,/plugins/other"),
            ("skills", "run-spine,work-item"),
            ("thinking", "low"),
        ],
    );
    a.launch(&plan).expect("the launch succeeds");

    let argv = lines(&dir.path().join("argv"));
    let value_of = |flag: &str| -> Vec<String> {
        argv.iter()
            .enumerate()
            .filter(|(_, w)| w.as_str() == flag)
            .map(|(i, _)| argv[i + 1].clone())
            .collect()
    };
    assert_eq!(value_of("--model"), vec!["zai/glm-5.3-flash".to_string()]);
    assert!(argv.contains(&"--no-extensions".to_string()));
    assert!(argv.contains(&"--no-rules".to_string()));
    assert_eq!(
        value_of("--plugin-dir"),
        vec!["/plugins/ossify".to_string(), "/plugins/other".to_string()],
        "one --plugin-dir for each pinned plugin"
    );
    assert!(argv.contains(&"--skills=run-spine,work-item".to_string()));
    assert_eq!(value_of("--approval-mode"), vec!["yolo".to_string()]);
    assert_eq!(value_of("--thinking"), vec!["low".to_string()]);
    let hook = value_of("--hook");
    assert_eq!(hook.len(), 1);
    assert!(hook[0].ends_with("pf-seat.ts"), "{hook:?}");
    assert!(
        std::path::Path::new(&hook[0]).is_file(),
        "the extension file exists"
    );
    let tools = value_of("--tools");
    assert_eq!(tools.len(), 1);
    let tools: Vec<&str> = tools[0].split(',').collect();
    assert!(!tools.contains(&"ask"), "{tools:?}");
    assert!(
        !tools.contains(&"task"),
        "subagents are not tracked, so `task` stays out: {tools:?}"
    );
    assert_eq!(
        argv.last().map(String::as_str),
        Some("Do the work item."),
        "the first message is the last argument"
    );
}

#[test]
fn no_declared_skill_means_no_skill() {
    let dir = tempfile::tempdir().unwrap();
    let a = adapter(
        dir.path(),
        stub_omp(dir.path(), "18.11.3", STUB_STARTS_A_TURN),
    );
    a.launch(&plan(dir.path(), &[])).unwrap();
    let argv = lines(&dir.path().join("argv"));
    assert!(argv.contains(&"--no-skills".to_string()));
    assert!(!argv.iter().any(|w| w.starts_with("--skills")));
}

#[test]
fn the_seat_token_is_set_and_differs_between_launches() {
    let dir = tempfile::tempdir().unwrap();
    let a = adapter(
        dir.path(),
        stub_omp(dir.path(), "18.11.3", STUB_STARTS_A_TURN),
    );
    a.launch(&plan(dir.path(), &[("env.EXTRA", "1")])).unwrap();
    a.launch(&plan(dir.path(), &[])).unwrap();
    let tokens = lines(&dir.path().join("tokens"));
    assert_eq!(tokens.len(), 2);
    assert!(tokens.iter().all(|t| t.len() == 32), "{tokens:?}");
    assert_ne!(tokens[0], tokens[1]);
    let env = std::fs::read_to_string(dir.path().join("env")).unwrap();
    assert!(env.contains("PF_SEAT_TOKEN="));
    assert!(env.contains("PF_EVENT_FILE="));
}

#[test]
fn the_plan_env_reaches_the_seat_and_reserved_names_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let a = adapter(
        dir.path(),
        stub_omp(dir.path(), "18.11.3", STUB_STARTS_A_TURN),
    );
    a.launch(&plan(dir.path(), &[("env.PF_HOOK_CMD", "/bin/true")]))
        .unwrap();
    let env = std::fs::read_to_string(dir.path().join("env")).unwrap();
    assert!(env.contains("PF_HOOK_CMD=/bin/true"));
    let err = a
        .launch(&plan(dir.path(), &[("env.PF_SEAT_TOKEN", "mine")]))
        .unwrap_err();
    assert!(matches!(err, AgentError::Refused(_)), "{err:?}");
}

#[test]
fn an_unqualified_version_is_refused_and_no_session_starts() {
    let dir = tempfile::tempdir().unwrap();
    let a = adapter(
        dir.path(),
        stub_omp(dir.path(), "19.0.0", STUB_STARTS_A_TURN),
    );
    let err = a.launch(&plan(dir.path(), &[])).unwrap_err();
    match err {
        AgentError::Refused(why) => {
            assert!(why.contains("19.0.0") && why.contains("18.11.3"), "{why}");
        }
        other => panic!("expected Refused, got {other:?}"),
    }
    assert!(
        !dir.path().join("argv").exists(),
        "the stub never ran as a session"
    );
    assert!(!dir.path().join("state/sessions").exists());
}

#[test]
fn rules_must_keep_prompts_off_and_the_ask_tool_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let a = adapter(
        dir.path(),
        stub_omp(dir.path(), "18.11.3", STUB_STARTS_A_TURN),
    );
    let mut p = plan(dir.path(), &[]);
    let on = RuleSet {
        name: "implementer".into(),
        prompts_off: false,
        deny: vec![],
    };
    assert!(matches!(
        a.apply_rules(&mut p, &on),
        Err(AgentError::Refused(_))
    ));
    let off = RuleSet {
        name: "implementer".into(),
        prompts_off: true,
        deny: vec!["merge".into()],
    };
    a.apply_rules(&mut p, &off).unwrap();
    a.remove_ask_tool(&mut p).unwrap();
    assert_eq!(p.settings["rule_set"], "implementer");
    assert_eq!(p.settings["ask_tool"], "off");
}

/// A stub that reports what the seat holds, as the extension does before the first turn.
fn stub_with_inventory(skills: &str, tools: &str) -> String {
    format!(
        r#"echo '{{"ev":"inventory","t":1,"activeTools":{tools},"skills":{skills}}}' >> "$PF_EVENT_FILE"
echo '{{"ev":"agent_start","t":2}}' >> "$PF_EVENT_FILE"
exec sleep 600"#
    )
}

#[test]
fn an_undeclared_skill_reaching_the_seat_refuses_the_launch() {
    let dir = tempfile::tempdir().unwrap();
    let omp = stub_omp(
        dir.path(),
        "18.11.3",
        &stub_with_inventory(r#"["run-spine","brainstorming"]"#, r#"["read","bash"]"#),
    );
    let a = adapter(dir.path(), omp);
    let err = a
        .launch(&plan(dir.path(), &[("skills", "run-*,work-item")]))
        .unwrap_err();
    match err {
        AgentError::Refused(why) => assert!(
            why.contains("brainstorming") && !why.contains("run-spine"),
            "{why}"
        ),
        other => panic!("expected Refused, got {other:?}"),
    }
}

#[test]
fn declared_skills_pass_and_a_glob_matches_by_star() {
    let dir = tempfile::tempdir().unwrap();
    let omp = stub_omp(
        dir.path(),
        "18.11.3",
        &stub_with_inventory(r#"["run-spine","work-item"]"#, r#"["read","bash"]"#),
    );
    let a = adapter(dir.path(), omp);
    a.launch(&plan(dir.path(), &[("skills", "run-*,work-item")]))
        .expect("every skill matches a declared glob");
}

#[test]
fn a_tool_outside_the_seat_list_refuses_the_launch() {
    let dir = tempfile::tempdir().unwrap();
    let omp = stub_omp(
        dir.path(),
        "18.11.3",
        &stub_with_inventory("[]", r#"["read","bash","mcp__canary_ping"]"#),
    );
    let a = adapter(dir.path(), omp);
    match a.launch(&plan(dir.path(), &[])).unwrap_err() {
        AgentError::Refused(why) => assert!(why.contains("mcp__canary_ping"), "{why}"),
        other => panic!("expected Refused, got {other:?}"),
    }
}

#[test]
fn a_plugin_that_brings_an_mcp_server_refuses_the_launch_before_anything_starts() {
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("plugin-a");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join(".mcp.json"), "{}").unwrap();
    let a = adapter(
        dir.path(),
        stub_omp(dir.path(), "18.11.3", STUB_STARTS_A_TURN),
    );
    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let err = a
        .launch(&plan(&work, &[("plugins", plugin.to_str().unwrap())]))
        .unwrap_err();
    assert!(
        matches!(&err, AgentError::Refused(why) if why.contains("MCP")),
        "{err:?}"
    );
    assert!(!dir.path().join("argv").exists(), "no process started");
    // A project MCP file in the seat's directory is refused the same way.
    std::fs::write(work.join(".mcp.json"), "{}").unwrap();
    let err = a.launch(&plan(&work, &[])).unwrap_err();
    assert!(
        matches!(&err, AgentError::Refused(why) if why.contains(".mcp.json")),
        "{err:?}"
    );
}
