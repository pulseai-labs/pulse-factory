//! AC-2: `workflows/ossify-spine/` is a real workflow package. It parses, it names every brief
//! and rule file it needs, its rule files parse, and it holds no host path and no AI-workspace
//! content.

use std::path::{Path, PathBuf};

use pf_workflow::package::Package;
use pf_workflow::rules::{Allow, RoleRules};
use pf_workflow::{DoneCond, WorkflowFile};

fn package_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../workflows/ossify-spine")
}

fn load() -> Package {
    Package::load(&package_dir()).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn the_workflow_file_parses() {
    let text = std::fs::read_to_string(package_dir().join("workflow.yaml")).unwrap();
    let file = WorkflowFile::parse(&text).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(file.name, "ossify-spine");
    assert_eq!(file.version, 1);
}

#[test]
fn the_repo_on_the_forge_is_a_parameter_apart_from_the_local_repo_path() {
    let file = load().file;
    for name in ["repo", "forge_repo", "briefs"] {
        assert!(file.params.iter().any(|p| p == name), "param {name}");
    }
    let merge = file.track("item").unwrap().merge.as_ref().unwrap();
    assert_eq!(merge.repo, "{param.forge_repo}");
    // `git -C` still takes the local path.
    assert!(
        merge
            .after_merge
            .iter()
            .any(|c| c.argv.iter().any(|a| a == "{param.repo}")),
        "after_merge keeps {{param.repo}}"
    );
}

#[test]
fn it_declares_the_ossify_plugin_with_a_minimum_version() {
    let file = load().file;
    let plugin = file
        .plugins
        .iter()
        .find(|p| p.name == "ossify")
        .expect("the ossify plugin is declared");
    assert!(!plugin.min_version.is_empty());
    assert!(
        plugin.min_version.split('.').count() >= 2,
        "{}",
        plugin.min_version
    );
}

#[test]
fn it_has_a_lane_track_and_an_item_track_with_verify_before_close() {
    let file = load().file;
    assert!(file.track("lane").is_some(), "lane track");
    let item = file.track("item").expect("item track");
    let names: Vec<&str> = item.stages.iter().map(|s| s.name.as_str()).collect();
    let at = |n: &str| {
        names
            .iter()
            .position(|x| *x == n)
            .unwrap_or_else(|| panic!("stage {n}"))
    };
    assert!(at("implement") < at("verify"), "{names:?}");
    assert!(at("verify") < at("close"), "{names:?}");
    assert_eq!(file.track("lane").unwrap().stages.len(), 1);
    assert_eq!(names, ["implement", "verify", "close", "review", "fix"]);
    assert!(
        at("close") < at("review") && at("review") < at("fix"),
        "{names:?}"
    );
}

#[test]
fn only_the_item_track_checks_the_head_of_its_merge() {
    let file = load().file;
    let with_merge: Vec<&str> = file
        .tracks
        .iter()
        .filter(|t| t.merge.is_some())
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(with_merge, ["item", "spine"]);
    let with_head_check: Vec<&str> = file
        .tracks
        .iter()
        .filter(|t| t.merge.as_ref().is_some_and(|m| m.head_check))
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(with_head_check, ["item"]);
}

#[test]
fn it_has_a_repeating_seed_with_a_max() {
    let file = load().file;
    let seed = file
        .seeds
        .iter()
        .find(|s| s.repeat.is_some())
        .expect("a repeating seed");
    let repeat = seed.repeat.as_ref().unwrap();
    assert!(repeat.max >= 1);
    assert_eq!(repeat.when_units_done, "item");
    assert_eq!(seed.track, "lane");
    assert!(file.track(&seed.track).is_some());
}

#[test]
fn every_brief_and_rule_file_it_names_exists_and_each_rule_file_parses() {
    let package = load();
    let dir = package_dir();
    assert!(!package.briefs.is_empty());
    for brief in &package.briefs {
        assert!(dir.join(brief).is_file(), "brief {}", brief.display());
    }
    for name in ["common", "lane", "implement", "verify", "close"] {
        assert!(
            package
                .briefs
                .iter()
                .any(|b| b == Path::new(&format!("briefs/{name}.md"))),
            "brief {name}.md is named by a stage"
        );
    }
    for role in &package.file.roles {
        let path = dir.join(&role.rules);
        assert!(path.is_file(), "rule file {}", path.display());
        let text = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let rules = RoleRules::parse(&text, &name).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(rules.role, role.name);
        assert!(package.rules.contains_key(&role.name));
    }
}

#[test]
fn a_package_with_a_missing_brief_or_rule_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let from = package_dir();
    copy_dir(&from, dir.path());
    assert!(Package::load(dir.path()).is_ok());

    std::fs::remove_file(dir.path().join("briefs/lane.md")).unwrap();
    let e = Package::load(dir.path()).unwrap_err().to_string();
    assert!(e.contains("briefs/lane.md"), "{e}");

    copy_dir(&from, dir.path());
    std::fs::remove_file(dir.path().join("rules/close.yaml")).unwrap();
    let e = Package::load(dir.path()).unwrap_err().to_string();
    assert!(e.contains("rules/close.yaml"), "{e}");
}

#[test]
fn a_brief_path_that_leaves_the_package_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&package_dir(), dir.path());
    let path = dir.path().join("workflow.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replacen("briefs/lane.md", "../outside.md", 1)).unwrap();
    let e = Package::load(dir.path()).unwrap_err().to_string();
    assert!(e.contains("../outside.md"), "{e}");
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

// ------------------------------------------------------------------ rule files

#[test]
fn inline_bad_rule_files_are_refused() {
    let good = "version: 1\nrole: implementer\nallow:\n  - delete_own_scratch\n";
    assert!(RoleRules::parse(good, "implementer.yaml").is_ok());

    let unknown_allow =
        "version: 1\nrole: implementer\nallow:\n  - delete_own_scratch\n  - push_to_main\n";
    let e = RoleRules::parse(unknown_allow, "implementer.yaml").unwrap_err();
    assert!(e.message.contains("push_to_main"), "{e}");
    assert_eq!(e.line, 5);

    let wrong_version = "version: 2\nrole: implementer\nallow: []\n";
    let e = RoleRules::parse(wrong_version, "implementer.yaml").unwrap_err();
    assert!(e.message.contains("supported version is 1"), "{e}");

    let mismatched = "version: 1\nrole: verifier\nallow: []\n";
    let e = RoleRules::parse(mismatched, "implementer.yaml").unwrap_err();
    assert!(
        e.message.contains("verifier") && e.message.contains("implementer"),
        "{e}"
    );

    let unknown_field = "version: 1\nrole: implementer\nallow: []\nextra: 1\n";
    let e = RoleRules::parse(unknown_field, "implementer.yaml").unwrap_err();
    assert!(e.message.contains("unknown field `extra`"), "{e}");

    let duplicate = "version: 1\nrole: implementer\nrole: implementer\nallow: []\n";
    let e = RoleRules::parse(duplicate, "implementer.yaml").unwrap_err();
    assert!(e.message.contains("duplicate key"), "{e}");

    let missing = "version: 1\nrole: implementer\n";
    let e = RoleRules::parse(missing, "implementer.yaml").unwrap_err();
    assert!(e.message.contains("missing required field `allow`"), "{e}");

    let twice = "version: 1\nrole: implementer\nallow: [push_own_tag, push_own_tag]\n";
    let e = RoleRules::parse(twice, "implementer.yaml").unwrap_err();
    assert!(e.message.contains("push_own_tag"), "{e}");
}

#[test]
fn the_implementer_and_the_verifier_list_no_push_and_close_pushes_its_branch() {
    let package = load();
    let allows = |role: &str| -> Vec<Allow> { package.rules[role].allow.clone() };
    for role in ["implementer", "verifier"] {
        let list = allows(role);
        assert!(!list.contains(&Allow::PushOwnBranch), "{role}");
        assert!(!list.contains(&Allow::PushOwnTag), "{role}");
    }
    assert!(allows("close").contains(&Allow::PushOwnBranch));
}

// ------------------------------------------------------------------ the dispatch count

fn flatten(argv: &[String]) -> String {
    argv.join(" ")
}

#[test]
fn the_dispatch_count_runs_after_the_first_turn_and_never_before_a_launch() {
    let file = load().file;
    let mut after_first_turn = 0;
    for track in &file.tracks {
        for stage in &track.stages {
            for command in &stage.before {
                assert!(
                    !flatten(&command.argv).contains("work_item_dispatched"),
                    "stage {} counts a dispatch in `before`",
                    stage.name
                );
            }
            for command in &stage.after_first_turn {
                if flatten(&command.argv).contains("work_item_dispatched") {
                    after_first_turn += 1;
                }
            }
        }
    }
    assert!(
        after_first_turn >= 1,
        "implement counts its dispatch after the first turn"
    );
    let implement = &file.track("item").unwrap().stages[0];
    assert!(
        implement
            .after_first_turn
            .iter()
            .any(|c| flatten(&c.argv).contains("work_item_dispatched"))
    );
    // `before` caps launches by a read of `dispatches`.
    assert!(
        implement
            .before
            .iter()
            .any(|c| flatten(&c.argv).contains("dispatches")),
        "before reads the dispatch count"
    );
}

#[test]
fn the_close_stage_is_done_when_a_pr_is_recorded() {
    let file = load().file;
    let close = &file.track("item").unwrap().stages[2];
    assert!(close.done.iter().any(|d| matches!(d, DoneCond::Pr { .. })));
    assert!(!close.park_on.is_empty());
}

// ------------------------------------------------------------------ no host path, no AI workspace

/// What is wrong with this text: a host path, or a name from the AI workspace.
fn forbidden_in(text: &str) -> Vec<String> {
    let mut hits = Vec::new();
    let lower = text.to_ascii_lowercase();
    for name in [
        "pulse-factory-ai",
        "ai-workspace",
        "ai_workspace",
        "ai workspace",
        "memory-bank",
        "memory_bank",
        ".claude",
        "draco",
        "praveen",
    ] {
        if lower.contains(name) {
            hits.push(format!("AI-workspace name `{name}`"));
        }
    }
    for prefix in [
        "/home/", "/users/", "/root/", "/tmp/", "/var/", "/etc/", "/opt/", "/usr/", "/mnt/",
        "/srv/", "/data/", "/run/", "~/",
    ] {
        let mut from = 0;
        while let Some(at) = lower[from..].find(prefix) {
            let at = from + at;
            let before = lower[..at].chars().last();
            // A relative path such as `docs/tmp/x` is not a host path.
            let relative = before
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '}'));
            if !relative {
                hits.push(format!("host path `{prefix}`"));
            }
            from = at + prefix.len();
        }
    }
    if text.contains(":\\") {
        hits.push("host path (drive letter)".to_string());
    }
    hits
}

fn package_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            package_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn no_file_of_the_package_holds_a_host_path_or_an_ai_workspace_name() {
    let mut files = Vec::new();
    package_files(&package_dir(), &mut files);
    assert!(files.len() >= 10, "the walk found {} files", files.len());
    let mut findings = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for hit in forbidden_in(&text) {
            findings.push(format!("{}: {hit}", file.display()));
        }
    }
    assert!(findings.is_empty(), "{}", findings.join("\n"));
}

#[test]
fn the_package_check_fires_on_inline_bad_samples() {
    let bad = [
        ("a home path", "cwd: /home/someone/project"),
        ("a home path in quotes", "path: \"/home/dev/x\""),
        ("a tilde path", "dir: ~/work"),
        ("a tmp path", "out: /tmp/run"),
        (
            "an AI workspace repo name",
            "see pulse-factory-ai for the plan",
        ),
        ("the words AI workspace", "Read the AI workspace first."),
        ("a memory bank", "read .claude/memory-bank/00.md"),
        ("a drive letter", "dir: C:\\work"),
        ("the host name", "runs on draco-desk"),
    ];
    for (name, sample) in bad {
        assert!(
            !forbidden_in(sample).is_empty(),
            "the {name} sample must fire: {sample}"
        );
    }
    let good = [
        "cwd: \"{param.repo}\"",
        "out: \"{run.dir}/out/{unit.id}\"",
        "a relative path docs/tmp/x and src/home/y",
    ];
    for sample in good {
        assert_eq!(forbidden_in(sample), Vec::<String>::new(), "{sample}");
    }
}

/// The keys ossify's work-item close reads from a caller's verdict (`close --verdict`). A missing
/// key halts the close with a halt that names the key.
const VERDICT_KEYS: [&str; 8] = [
    "mode",
    "model",
    "agent",
    "work_item_id",
    "identity",
    "rule",
    "lenses",
    "mutation",
];

#[test]
fn the_verify_brief_writes_every_verdict_key_the_close_reads() {
    let text = std::fs::read_to_string(package_dir().join("briefs/verify.md")).unwrap();
    let shape = text
        .lines()
        .find(|l| l.contains(r#"{"mode":"verified""#))
        .expect("the verify brief states the verdict's shape");
    for key in VERDICT_KEYS {
        assert!(
            shape.contains(&format!("\"{key}\":")),
            "verdict key {key} missing from: {shape}"
        );
    }
}

#[test]
fn the_minimum_ossify_version_is_one_whose_close_reads_that_verdict() {
    let file = load().file;
    let plugin = file.plugins.iter().find(|p| p.name == "ossify").unwrap();
    let mut parts = plugin
        .min_version
        .split('.')
        .map(|p| p.parse::<u32>().unwrap());
    let version = (parts.next().unwrap(), parts.next().unwrap());
    // ossify 1.30.0 is the first close with `--verdict`, and it requires the `agent` key.
    assert!(version >= (1, 30), "min_version {}", plugin.min_version);
}

// ------------------------------------------------------------------ answers that resume (r0.s2.w2)

/// What is wrong with the way a package routes an answer. Each line is one defect.
fn answer_route_problems(package: &Package, common_brief: &str) -> Vec<String> {
    let mut p = Vec::new();
    for track in &package.file.tracks {
        for stage in &track.stages {
            if !stage
                .message
                .contains("Rulings: {run.dir}/out/{unit.id}/rulings.md")
            {
                p.push(format!("stage {} does not name `Rulings:`", stage.name));
            }
            let rule = stage
                .stop_on_answer
                .iter()
                .find(|c| c.argv.iter().any(|a| a == "{run.package}/scripts/rule.sh"));
            match rule {
                None => p.push(format!("stage {} has no rule.sh stop hook", stage.name)),
                Some(c) => {
                    if c.argv.last().map(String::as_str) != Some(stage.name.as_str()) {
                        p.push(format!(
                            "the stop hook of stage {} does not name its stage",
                            stage.name
                        ));
                    }
                }
            }
            for entry in &stage.park_on {
                if entry.reason.contains("start a new run") {
                    p.push(format!(
                        "a park_on reason of stage {} says to start a new run",
                        stage.name
                    ));
                }
            }
        }
    }
    let implement = package
        .file
        .track("item")
        .and_then(|t| t.stages.iter().find(|s| s.name == "implement"));
    let gaps_hook = implement
        .and_then(|s| s.park_on.iter().find(|e| e.file.ends_with("/gaps.json")))
        .map(|e| &e.on_answer);
    match gaps_hook {
        Some(h)
            if h.iter().any(|c| {
                c.argv
                    .iter()
                    .any(|a| a == "{run.package}/scripts/clarify.sh")
            }) => {}
        _ => p.push("implement's gaps.json park has no clarify.sh hook".to_string()),
    }
    if !common_brief.contains("rulings.md")
        || !common_brief.contains("Its last entry answers your last stop")
    {
        p.push("common.md holds no rulings rule".to_string());
    }
    p
}

#[test]
fn the_package_loads_with_the_answer_hooks_and_pins_its_scripts() {
    let package = load();
    for script in [
        "scripts/clarify.sh",
        "scripts/rule.sh",
        "scripts/review-prep.sh",
        "scripts/stale-pass.sh",
        "scripts/verdict.sh",
    ] {
        assert!(
            package.scripts.contains(&PathBuf::from(script)),
            "{script} in {:?}",
            package.scripts
        );
        assert!(package.pinned_files().contains(&PathBuf::from(script)));
    }
    let item = package.file.track("item").unwrap();
    let implement = &item.stages[0];
    let gaps = &implement.park_on[0];
    assert!(gaps.file.ends_with("/gaps.json"));
    assert_eq!(gaps.on_answer.len(), 1);
    // clarify.sh <handoff> <body_file> <answer_file> <request> <generation> <spine_dir>
    assert_eq!(
        gaps.on_answer[0].argv,
        [
            "sh",
            "{run.package}/scripts/clarify.sh",
            "{unit.vars.handoff_path}",
            "{request.body_file}",
            "{answer.file}",
            "{answer.request}",
            "{answer.generation}",
            "{param.spine_dir}",
        ]
    );
    // rule.sh <rulings_file> <stop_file> <answer_file> <request> <generation> <stage>
    for track in &package.file.tracks {
        for stage in &track.stages {
            assert_eq!(
                stage.stop_on_answer.len(),
                1,
                "stage {} has a stop hook",
                stage.name
            );
            assert_eq!(
                stage.stop_on_answer[0].argv,
                [
                    "sh",
                    "{run.package}/scripts/rule.sh",
                    "{run.dir}/out/{unit.id}/rulings.md",
                    "{run.dir}/out/{unit.id}/stop",
                    "{answer.file}",
                    "{answer.request}",
                    "{answer.generation}",
                    stage.name.as_str(),
                ]
            );
            // The hook retires the file that the stage's stop_file names.
            assert_eq!(stage.stop_file, "{run.dir}/out/{unit.id}/stop");
        }
    }
    // The close stage's correction packet has a hook too: without it, an answer resumes the stage
    // and the packet, still on disk, parks it again at once.
    let close = package
        .file
        .track("item")
        .unwrap()
        .stages
        .iter()
        .find(|s| s.name == "close")
        .expect("the close stage");
    let packet = close
        .park_on
        .iter()
        .find(|e| e.file.ends_with("/correction.txt"))
        .expect("the close stage parks on the correction packet");
    assert_eq!(
        packet.on_answer.len(),
        1,
        "the correction packet's park has an answer hook"
    );
    assert_eq!(
        packet.on_answer[0].argv,
        [
            "sh",
            "{run.package}/scripts/rule.sh",
            "{run.dir}/out/{unit.id}/rulings.md",
            "{run.dir}/out/{unit.id}/correction.txt",
            "{answer.file}",
            "{answer.request}",
            "{answer.generation}",
            "close",
        ]
    );
    // The redispatch counts toward the cap of 3 that `before` already checks.
    assert!(
        implement
            .before
            .iter()
            .any(|c| flatten(&c.argv).contains("(.dispatches // 0) < 3")),
        "implement's before keeps the dispatch cap"
    );
    assert!(
        implement
            .before
            .iter()
            .any(|c| c.argv == ["rm", "-f", "{run.dir}/out/{unit.id}/gaps.json"]),
        "implement's before removes gaps.json, so the park file does not park the unit again"
    );
}

#[test]
fn every_session_stage_names_the_rulings_file_and_no_park_says_start_a_new_run() {
    let package = load();
    let common = std::fs::read_to_string(package_dir().join("briefs/common.md")).unwrap();
    assert_eq!(
        answer_route_problems(&package, &common),
        Vec::<String>::new()
    );
    for track in &package.file.tracks {
        for stage in &track.stages {
            assert!(
                stage
                    .message
                    .contains("Rulings: {run.dir}/out/{unit.id}/rulings.md."),
                "stage {}",
                stage.name
            );
        }
    }
    // The close gate's park names the verbs that answer or clear it.
    let close = &package.file.track("item").unwrap().stages[2];
    let reason = &close.park_on[0].reason;
    assert!(
        reason.contains("pulse-factory answer") && reason.contains("pulse-factory clear"),
        "{reason}"
    );
    let gaps = &package.file.track("item").unwrap().stages[0].park_on[0].reason;
    assert!(gaps.contains("pulse-factory answer"), "{gaps}");
    // The rule reads as the spec words it.
    assert!(common.contains(
        "If `<your OUT>/rulings.md` exists, read it first. Its last entry answers your last stop; earlier entries still bind. Never edit it."
    ));
}

#[test]
fn the_answer_route_check_fires_on_inline_bad_samples() {
    let package = load();
    let common = std::fs::read_to_string(package_dir().join("briefs/common.md")).unwrap();
    // A stage message with no `Rulings:`.
    let mut bad = package.clone();
    bad.file.tracks[0].stages[0].message = "Read the brief.".to_string();
    assert!(
        answer_route_problems(&bad, &common)
            .iter()
            .any(|m| m.contains("does not name `Rulings:`"))
    );
    // A stage with no stop hook, and one whose hook names another stage.
    let mut bad = package.clone();
    bad.file.tracks[1].stages[1].stop_on_answer.clear();
    assert!(
        answer_route_problems(&bad, &common)
            .iter()
            .any(|m| m.contains("no rule.sh stop hook"))
    );
    let mut bad = package.clone();
    let hook = &mut bad.file.tracks[1].stages[1].stop_on_answer[0];
    *hook.argv.last_mut().unwrap() = "implement".to_string();
    assert!(
        answer_route_problems(&bad, &common)
            .iter()
            .any(|m| m.contains("does not name its stage"))
    );
    // A park reason that sends the operator to a new run.
    let mut bad = package.clone();
    bad.file.tracks[1].stages[2].park_on[0].reason =
        "fix it, then start a new run to clear this park".to_string();
    assert!(
        answer_route_problems(&bad, &common)
            .iter()
            .any(|m| m.contains("start a new run"))
    );
    // No gaps hook.
    let mut bad = package.clone();
    bad.file.tracks[1].stages[0].park_on[0].on_answer.clear();
    assert!(
        answer_route_problems(&bad, &common)
            .iter()
            .any(|m| m.contains("no clarify.sh hook"))
    );
    // A common brief without the rule.
    assert!(
        answer_route_problems(&package, "# Every seat\n")
            .iter()
            .any(|m| m.contains("no rulings rule"))
    );
}

#[test]
fn every_script_is_posix_sh_and_calls_no_method_tool() {
    for name in [
        "clarify.sh",
        "rule.sh",
        "review-prep.sh",
        "stale-pass.sh",
        "verdict.sh",
    ] {
        let path = package_dir().join("scripts").join(name);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("#!/bin/sh\n"), "{name}: POSIX sh");
        for line in text.lines().filter(|l| !l.trim_start().starts_with('#')) {
            let words: Vec<&str> = line
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .collect();
            assert!(!words.contains(&"oss"), "{name} calls `oss`: {line}");
            assert!(
                !words.contains(&"bash") && !words.contains(&"eval"),
                "{name}: {line}"
            );
        }
        assert!(text.contains("set -eu"), "{name} stops on an error");
    }
}

// ------------------------------------------------------------------ the item PR reviewer seat

fn item_stage(file: &WorkflowFile, name: &str) -> pf_workflow::StageSpec {
    file.track("item")
        .unwrap()
        .stages
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("stage {name}"))
        .clone()
}

fn argv_has(cond: &DoneCond, word: &str) -> bool {
    match cond {
        DoneCond::Exit0(c) => c.argv.iter().any(|a| a == word),
        _ => false,
    }
}

#[test]
fn the_reviewer_role_runs_deepseek_v4_1_flash_under_its_own_read_only_rules() {
    let package = load();
    let role = package
        .file
        .roles
        .iter()
        .find(|r| r.name == "reviewer")
        .expect("roles.reviewer");
    assert_eq!(role.runtime, "omp");
    assert_eq!(role.model, "ollama-daemon/deepseek-v4.1-flash:1017-cloud");
    assert_eq!(role.rules, "rules/reviewer.yaml");
    assert_eq!(
        package.rules["reviewer"].allow,
        [Allow::WriteOwnReportAndRunFiles],
        "the reviewer writes only its report and run files"
    );
    let review = item_stage(&package.file, "review");
    assert_eq!(review.role, "reviewer");
    assert_eq!(item_stage(&package.file, "fix").role, "close");
}

#[test]
fn the_review_and_fix_briefs_exist_and_each_is_named_by_its_stage() {
    let package = load();
    for (stage, brief) in [("review", "briefs/review.md"), ("fix", "briefs/fix.md")] {
        assert!(package_dir().join(brief).is_file(), "{brief}");
        let spec = item_stage(&package.file, stage);
        assert!(
            spec.brief.iter().any(|b| b == brief),
            "{stage}: {:?}",
            spec.brief
        );
        assert!(
            spec.brief.iter().any(|b| b == "briefs/common.md"),
            "{stage}"
        );
    }
    let review = std::fs::read_to_string(package_dir().join("briefs/review.md")).unwrap();
    for needed in [
        "verdict: pass",
        "verdict: block",
        "head:",
        "previous:",
        "redirection",
    ] {
        assert!(review.contains(needed), "review.md lacks `{needed}`");
    }
    let fix = std::fs::read_to_string(package_dir().join("briefs/fix.md")).unwrap();
    for needed in ["review", "Fix done file", "Rulings"] {
        assert!(fix.contains(needed), "fix.md lacks `{needed}`");
    }
}

#[test]
fn the_review_stage_is_read_only_and_names_its_files() {
    let file = load().file;
    let review = item_stage(&file, "review");
    assert!(
        !review.settings.contains_key("worktree"),
        "no worktree write"
    );
    assert_eq!(
        review.cwd, "{unit.vars.worktree_path}",
        "cwd is for reading"
    );
    assert_eq!(review.settings["report_dir"], "{run.dir}/out/{unit.id}");
    assert!(review.slots == ["session"], "{:?}", review.slots);
    assert!(
        review
            .message
            .contains("previous: {run.dir}/out/{unit.id}/review-prev.md"),
        "{}",
        review.message
    );
    assert!(review.message.starts_with("/review "), "{}", review.message);
    let before: Vec<String> = review.before.iter().map(|c| flatten(&c.argv)).collect();
    assert!(
        before.iter().any(|c| c.contains("scripts/review-prep.sh")
            && c.contains("out/{unit.id}/review.md")
            && c.contains("out/{unit.id}/review-prev.md")),
        "{before:?}"
    );
    assert_eq!(review.done.len(), 1);
    // The verdict is line 1 of the review file, read by `verdict.sh`; no condition calls `grep`.
    assert_eq!(
        exit0_argv(&review.done[0]),
        [
            "sh",
            "{run.package}/scripts/verdict.sh",
            "{run.dir}/out/{unit.id}/review.md",
            "any"
        ]
    );
}

/// The argv of an `exit0` condition. Any other kind of condition panics.
fn exit0_argv(cond: &DoneCond) -> Vec<String> {
    match cond {
        DoneCond::Exit0(c) => c.argv.clone(),
        other => panic!("not an exit0 condition: {other:?}"),
    }
}

#[test]
fn no_review_condition_calls_grep() {
    let file = load().file;
    let review = item_stage(&file, "review");
    let conds = review
        .done
        .iter()
        .chain(review.outcomes.iter().flat_map(|o| o.when.iter()));
    for cond in conds {
        let argv = exit0_argv(cond);
        assert!(
            !argv.iter().any(|a| a == "grep"),
            "a review condition calls grep: {argv:?}"
        );
    }
}

/// Run `verdict.sh` with these arguments. Returns its exit code and its stderr.
fn run_verdict(args: &[&std::ffi::OsStr]) -> (i32, String) {
    let out = std::process::Command::new("sh")
        .arg(package_dir().join("scripts/verdict.sh"))
        .args(args)
        .output()
        .expect("sh runs");
    (
        out.status
            .code()
            .expect("verdict.sh ends with an exit code"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The exit code of `verdict.sh <dir>/review.md <mode>` when the file holds exactly `bytes`.
fn verdict_of(bytes: &[u8], mode: &str) -> i32 {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("review.md");
    std::fs::write(&file, bytes).unwrap();
    run_verdict(&[file.as_os_str(), mode.as_ref()]).0
}

#[test]
fn verdict_sh_accepts_only_a_line_one_that_is_exactly_the_verdict_asked_for() {
    // (file bytes, expected exit in `pass`, in `block`, in `any`)
    let cases: &[(&str, &[u8], i32, i32, i32)] = &[
        ("pass", b"verdict: pass\nhead: abc\n", 0, 1, 0),
        ("block", b"verdict: block\nhead: abc\n", 1, 0, 0),
        ("pass, no final newline", b"verdict: pass", 0, 1, 0),
        ("block, no final newline", b"verdict: block", 1, 0, 0),
        (
            "pass that quotes a block",
            b"verdict: pass\nhead: abc\n\n- verdict: block\n",
            0,
            1,
            0,
        ),
        (
            "block that quotes a pass",
            b"verdict: block\nverdict: pass\n",
            1,
            0,
            0,
        ),
        (
            "a verdict only on a later line",
            b"review\nverdict: pass\n",
            1,
            1,
            1,
        ),
        (
            "a later block under a bad line 1",
            b"# Review\nverdict: block\n",
            1,
            1,
            1,
        ),
        ("a trailing space", b"verdict: pass \nhead: abc\n", 1, 1, 1),
        ("a trailing space, no newline", b"verdict: block ", 1, 1, 1),
        (
            "a carriage return",
            b"verdict: pass\r\nhead: abc\n",
            1,
            1,
            1,
        ),
        ("a leading space", b" verdict: pass\n", 1, 1, 1),
        ("a prefix", b"xverdict: pass\n", 1, 1, 1),
        ("a longer verdict", b"verdict: passed\n", 1, 1, 1),
        ("another verdict", b"verdict: maybe\n", 1, 1, 1),
        ("an empty file", b"", 1, 1, 1),
        ("a blank line 1", b"\nverdict: pass\n", 1, 1, 1),
    ];
    for (name, bytes, pass, block, any) in cases {
        for (mode, want) in [("pass", pass), ("block", block), ("any", any)] {
            assert_eq!(
                verdict_of(bytes, mode),
                *want,
                "{name}: mode {mode}: {:?}",
                String::from_utf8_lossy(bytes)
            );
        }
    }
}

#[test]
fn verdict_sh_fails_closed_for_no_file_a_directory_and_a_symbolic_link() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.md");
    let real = dir.path().join("real.md");
    std::fs::write(&real, "verdict: pass\n").unwrap();
    let folder = dir.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    let link = dir.path().join("link.md");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    for mode in ["pass", "block", "any"] {
        for (name, path) in [
            ("no file", &missing),
            ("a directory", &folder),
            ("a link", &link),
        ] {
            assert_eq!(
                run_verdict(&[path.as_os_str(), mode.as_ref()]).0,
                1,
                "{name}, mode {mode}"
            );
        }
    }
    // The same text in the plain file passes: only the link made the difference.
    assert_eq!(run_verdict(&[real.as_os_str(), "pass".as_ref()]).0, 0);
}

#[test]
fn verdict_sh_exits_2_with_a_line_on_stderr_for_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("review.md");
    std::fs::write(&file, "verdict: pass\n").unwrap();
    let f = file.as_os_str();
    let wrong: Vec<Vec<&std::ffi::OsStr>> = vec![
        vec![],
        vec![f],
        vec![f, "pass".as_ref(), "extra".as_ref()],
        vec![f, "maybe".as_ref()],
        vec![f, "".as_ref()],
        vec![f, "PASS".as_ref()],
    ];
    for args in wrong {
        let (code, stderr) = run_verdict(&args);
        assert_eq!(code, 2, "args {args:?}");
        assert_eq!(stderr.lines().count(), 1, "args {args:?}: {stderr:?}");
    }
}

/// The text of a package script, without its comment lines.
fn script_code(name: &str) -> String {
    let text = std::fs::read_to_string(package_dir().join("scripts").join(name)).unwrap();
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn stale_pass_and_review_prep_read_the_verdict_through_verdict_sh_alone() {
    for name in ["stale-pass.sh", "review-prep.sh"] {
        let code = script_code(name);
        assert!(
            code.contains("/verdict.sh\""),
            "{name} calls verdict.sh next to itself: {code}"
        );
        // No second copy of the line-1 test.
        for copy in ["verdict: pass", "verdict: block"] {
            assert!(!code.contains(copy), "{name} holds its own `{copy}` test");
        }
    }
    assert!(script_code("stale-pass.sh").contains("verdict.sh\" \"$review\" pass"));
    assert!(script_code("review-prep.sh").contains("verdict.sh\" \"$review\" any"));
}

/// Run `stale-pass.sh <file> <bare repo> main` and return its exit code. The bare repo has a
/// branch `main` at a known commit, built with local git only.
fn stale_pass_of(review: &[u8]) -> i32 {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str], cwd: &Path| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let bare = dir.path().join("bare.git");
    git(
        &[
            "init",
            "--quiet",
            "--bare",
            "-b",
            "main",
            bare.to_str().unwrap(),
        ],
        dir.path(),
    );
    let work = dir.path().join("work");
    git(
        &["init", "--quiet", "-b", "main", work.to_str().unwrap()],
        dir.path(),
    );
    git(&["commit", "--quiet", "--allow-empty", "-m", "c1"], &work);
    git(&["push", "--quiet", bare.to_str().unwrap(), "main"], &work);
    let head = git(&["rev-parse", "main"], &bare);
    let mut text = Vec::new();
    for part in review.split(|b| *b == b'@') {
        text.extend_from_slice(part);
        if part.len() != review.len() {
            text.extend_from_slice(head.as_bytes());
        }
    }
    let file = dir.path().join("review.md");
    std::fs::write(&file, &text).unwrap();
    std::process::Command::new("sh")
        .arg(package_dir().join("scripts/stale-pass.sh"))
        .arg(&file)
        .arg(&bare)
        .arg("main")
        .status()
        .unwrap()
        .code()
        .unwrap()
}

#[test]
fn stale_pass_reads_line_one_through_verdict_sh_and_line_two_for_the_head() {
    // `@` stands for the branch head. A pass on the head is not stale (1).
    assert_eq!(stale_pass_of(b"verdict: pass\nhead: @\n"), 1);
    // A pass on another head, or with no head, is stale (0).
    assert_eq!(stale_pass_of(b"verdict: pass\nhead: 0123abc\n"), 0);
    assert_eq!(stale_pass_of(b"verdict: pass\nno head\n"), 0);
    // Any other line 1 is not a pass, whatever follows (1).
    assert_eq!(stale_pass_of(b"verdict: block\nhead: 0123abc\n"), 1);
    assert_eq!(stale_pass_of(b"verdict: pass \nhead: 0123abc\n"), 1);
    assert_eq!(stale_pass_of(b"verdict: pass\r\nhead: 0123abc\n"), 1);
    assert_eq!(
        stale_pass_of(b"# review\nverdict: pass\nhead: 0123abc\n"),
        1
    );
    assert_eq!(stale_pass_of(b""), 1);
}

#[test]
fn review_prep_keeps_a_review_only_when_line_one_is_a_verdict_and_always_clears_review_md() {
    let prep = package_dir().join("scripts/review-prep.sh");
    let run = |review: &Path, prev: &Path| {
        let status = std::process::Command::new("sh")
            .arg(&prep)
            .arg(review)
            .arg(prev)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(!review.exists() && review.symlink_metadata().is_err());
    };
    let cases: &[(&str, &[u8], bool)] = &[
        ("pass", b"verdict: pass\nhead: abc\n", true),
        ("block, no final newline", b"verdict: block", true),
        (
            "a quoted block under a pass",
            b"verdict: pass\nverdict: block\n",
            true,
        ),
        (
            "a later verdict under a bad line 1",
            b"# Review\nverdict: pass\n",
            false,
        ),
        ("a trailing space", b"verdict: pass \n", false),
        ("a carriage return", b"verdict: block\r\n", false),
        ("an empty file", b"", false),
    ];
    for (name, bytes, kept) in cases {
        let dir = tempfile::tempdir().unwrap();
        let (review, prev) = (
            dir.path().join("review.md"),
            dir.path().join("review-prev.md"),
        );
        std::fs::write(&prev, "old\n").unwrap();
        std::fs::write(&review, bytes).unwrap();
        run(&review, &prev);
        let want: &[u8] = if *kept { bytes } else { b"old\n" };
        assert_eq!(std::fs::read(&prev).unwrap(), want, "{name}");
    }
    // A symbolic link is never followed.
    let dir = tempfile::tempdir().unwrap();
    let (review, prev) = (
        dir.path().join("review.md"),
        dir.path().join("review-prev.md"),
    );
    let target = dir.path().join("target.md");
    std::fs::write(&target, "verdict: pass\n").unwrap();
    std::fs::write(&prev, "old\n").unwrap();
    std::os::unix::fs::symlink(&target, &review).unwrap();
    run(&review, &prev);
    assert_eq!(std::fs::read_to_string(&prev).unwrap(), "old\n", "a link");
    assert!(target.exists(), "the link's target is untouched");
}

#[test]
fn the_review_outcomes_send_a_block_to_fix_and_a_stale_pass_to_review_each_at_most_twice() {
    let file = load().file;
    let review = item_stage(&file, "review");
    assert_eq!(review.outcomes.len(), 2);
    let block = &review.outcomes[0];
    assert_eq!((block.to.as_str(), block.max), ("fix", 2));
    assert_eq!(block.when.len(), 1);
    assert_eq!(
        exit0_argv(&block.when[0]),
        [
            "sh",
            "{run.package}/scripts/verdict.sh",
            "{run.dir}/out/{unit.id}/review.md",
            "block"
        ]
    );
    let stale = &review.outcomes[1];
    assert_eq!((stale.to.as_str(), stale.max), ("review", 2));
    assert_eq!(stale.when.len(), 1);
    assert!(argv_has(
        &stale.when[0],
        "{run.package}/scripts/stale-pass.sh"
    ));
    assert!(argv_has(&stale.when[0], "{param.forge_repo}"));
    assert!(argv_has(&stale.when[0], "{unit.vars.branch}"));
}

#[test]
fn the_fix_stage_runs_only_by_outcome_pushes_its_branch_and_returns_to_review() {
    let file = load().file;
    let fix = item_stage(&file, "fix");
    assert!(fix.entered_by_outcome, "a passing review skips fix");
    assert_eq!(fix.outcomes.len(), 1);
    assert_eq!(
        fix.outcomes[0].when,
        [DoneCond::File(
            "{run.dir}/out/{unit.id}/fix-done".to_string()
        )]
    );
    assert_eq!(
        (fix.outcomes[0].to.as_str(), fix.outcomes[0].max),
        ("review", 2)
    );
    assert_eq!(
        fix.done,
        [DoneCond::File(
            "{run.dir}/out/{unit.id}/fix-done".to_string()
        )]
    );
    assert_eq!(fix.settings["push_dir"], "{unit.vars.worktree_path}");
    assert_eq!(fix.settings["push_branch"], "{unit.vars.branch}");
    assert_eq!(fix.settings["worktree"], "{unit.vars.worktree_path}");
    assert_eq!(fix.cwd, "{unit.vars.worktree_path}");
    assert_eq!(fix.slots, ["session", "build"]);
    let before: Vec<String> = fix.before.iter().map(|c| flatten(&c.argv)).collect();
    assert!(
        before
            .iter()
            .any(|c| c.contains("rm -f") && c.ends_with("/fix-done")),
        "{before:?}"
    );
    // No other stage is entered by an outcome, and none of the older stages has an outcome.
    for stage in &file.track("item").unwrap().stages {
        assert_eq!(
            stage.entered_by_outcome,
            stage.name == "fix",
            "{}",
            stage.name
        );
        if stage.name != "review" && stage.name != "fix" {
            assert!(stage.outcomes.is_empty(), "{}", stage.name);
        }
    }
}

#[test]
fn review_and_fix_carry_the_stop_file_and_the_ruling_hook() {
    let file = load().file;
    for name in ["review", "fix"] {
        let stage = item_stage(&file, name);
        assert_eq!(stage.stop_file, "{run.dir}/out/{unit.id}/stop", "{name}");
        assert_eq!(
            stage.settings["env.SPINE_STOP_FILE"], "{run.dir}/out/{unit.id}/stop",
            "{name}"
        );
        assert_eq!(stage.stall_minutes, 20, "{name}");
        assert!(
            stage
                .message
                .contains("Rulings: {run.dir}/out/{unit.id}/rulings.md"),
            "{name}"
        );
        assert!(
            stage.stop_on_answer.iter().any(|c| {
                c.argv.iter().any(|a| a == "{run.package}/scripts/rule.sh")
                    && c.argv.last().map(String::as_str) == Some(name)
            }),
            "{name} has no rule.sh hook naming itself"
        );
    }
}

// ------------------------------------------------------------------ the spine close

const PR_ARGV: [&str; 13] = [
    "gh",
    "pr",
    "list",
    "--repo",
    "{param.forge_repo}",
    "--head",
    "{param.spine_branch}",
    "--base",
    "{param.base_branch}",
    "--state",
    "open",
    "--json",
    "url",
];

fn spine_close_stage(file: &WorkflowFile) -> pf_workflow::StageSpec {
    let track = file.track("spine").expect("the spine track");
    assert_eq!(track.stages.len(), 1, "one stage");
    let stage = track.stages[0].clone();
    assert_eq!(stage.name, "spine_close");
    stage
}

#[test]
fn the_package_takes_the_base_branch_of_the_spine_as_a_param() {
    let file = load().file;
    assert!(file.params.iter().any(|p| p == "base_branch"));
}

#[test]
fn the_spine_seed_starts_when_the_rounds_are_done_and_the_items_are_done() {
    let file = load().file;
    let seed = file
        .seeds
        .iter()
        .find(|s| s.id == "spine")
        .expect("the spine seed");
    assert_eq!(
        (seed.group.as_str(), seed.track.as_str()),
        ("spine", "spine")
    );
    assert!(seed.repeat.is_none());
    let start = seed.start_when.as_ref().expect("start_when");
    assert_eq!(start.file, "{run.dir}/out/lane/rounds.done");
    assert_eq!(start.units_done, "item");
    // The lane seed is the only repeating one, and it does not start late.
    let lane = file.seeds.iter().find(|s| s.id == "lane").unwrap();
    assert!(lane.start_when.is_none() && lane.repeat.is_some());
}

/// AC-4 (`r1.s1.w4`): the lane's two run-level files live under the lane's own directory, so no
/// seat of the package needs a write outside its own unit's directory.
#[test]
fn the_lane_keeps_its_queue_and_rounds_done_file_under_its_own_directory() {
    let file = load().file;
    assert_eq!(file.unit_sources.len(), 1);
    assert_eq!(file.unit_sources[0].dir, "{run.dir}/out/lane/queue");
    let lane = file.seeds.iter().find(|s| s.id == "lane").unwrap();
    assert_eq!(
        lane.repeat.as_ref().unwrap().until_file,
        "{run.dir}/out/lane/rounds.done"
    );
    let spine = file.seeds.iter().find(|s| s.id == "spine").unwrap();
    assert_eq!(
        spine.start_when.as_ref().unwrap().file,
        "{run.dir}/out/lane/rounds.done"
    );
    let stage = &file.track("lane").unwrap().stages[0];
    let makes_the_queue: Vec<String> = ["mkdir", "-p", "{run.dir}/out/lane/queue"]
        .iter()
        .map(|a| (*a).to_string())
        .collect();
    assert!(
        stage.before.iter().any(|c| c.argv == makes_the_queue),
        "the lane stage's `before` makes the queue directory: {:?}",
        stage.before
    );
    assert!(
        stage
            .message
            .contains("Queue directory: {run.dir}/out/lane/queue."),
        "{}",
        stage.message
    );
    assert!(
        stage
            .message
            .contains("Rounds done file: {run.dir}/out/lane/rounds.done."),
        "{}",
        stage.message
    );
}

/// AC-4 (`r1.s1.w4`): every `{run.dir}/` path in the workflow file starts with `{run.dir}/out/`.
/// The package's run-level files all live under `out/`, where a seat owns one directory.
#[test]
fn every_run_dir_path_in_the_workflow_file_lies_under_out() {
    let text = std::fs::read_to_string(package_dir().join("workflow.yaml")).unwrap();
    let mut seen = 0;
    for (n, line) in text.lines().enumerate() {
        for (at, _) in line.match_indices("{run.dir}") {
            seen += 1;
            assert!(
                line[at + "{run.dir}".len()..].starts_with("/out/"),
                "workflow.yaml:{}: a run-level path outside out/: {}",
                n + 1,
                line.trim()
            );
        }
    }
    assert!(seen > 20, "the scan read {seen} templates");
}

#[test]
fn the_spine_close_stage_runs_as_the_close_role_on_the_spine_branch() {
    let file = load().file;
    let stage = spine_close_stage(&file);
    assert_eq!(stage.role, "close");
    assert_eq!(stage.skills, ["close"]);
    assert_eq!(stage.brief, ["briefs/common.md", "briefs/spine-close.md"]);
    assert_eq!(stage.cwd, "{param.method_root}");
    assert_eq!(stage.slots, ["session"]);
    assert_eq!(stage.stall_minutes, 30);
    assert_eq!(stage.stop_file, "{run.dir}/out/{unit.id}/stop");
    assert_eq!(stage.settings["push_dir"], "{param.repo}");
    assert_eq!(stage.settings["push_branch"], "{param.spine_branch}");
    assert_eq!(stage.settings["own_branch"], "{param.spine_branch}");
    assert_eq!(stage.settings["report_dir"], "{param.spine_dir}");
    assert_eq!(
        stage.settings["env.SPINE_STOP_FILE"],
        "{run.dir}/out/{unit.id}/stop"
    );
    // The seat never has a worktree write: the close runs from the method's root.
    assert!(!stage.settings.contains_key("worktree"));
    assert!(
        stage.stop_on_answer.iter().any(|c| {
            c.argv.iter().any(|a| a == "{run.package}/scripts/rule.sh")
                && c.argv.last().map(String::as_str) == Some("spine_close")
        }),
        "no rule.sh hook naming the stage"
    );
    for want in [
        "{param.spine}",
        "Stop file: {run.dir}/out/{unit.id}/stop",
        "Rulings: {run.dir}/out/{unit.id}/rulings.md.",
        "Decisions file: {run.dir}/out/{unit.id}/decisions.md",
    ] {
        assert!(
            stage.message.contains(want),
            "{want:?} in {:?}",
            stage.message
        );
    }
    // The seat's own words never name the PR: the stage reads it from the forge.
    assert!(!stage.message.contains("pr.url") && !stage.message.contains("PR URL"));
}

#[test]
fn the_spine_close_stage_is_done_when_the_forge_lists_an_open_spine_pr() {
    let file = load().file;
    let stage = spine_close_stage(&file);
    assert_eq!(stage.done.len(), 1);
    let DoneCond::Pr { command, test } = &stage.done[0] else {
        panic!("{:?}", stage.done);
    };
    assert_eq!(*test, pf_workflow::PrTest::Url);
    assert_eq!(command.argv[..13], PR_ARGV);
    assert_eq!(command.argv[13], "--jq");
    assert_eq!(command.argv[14], ".[0].url // \"\"");
    assert_eq!(command.argv.len(), 15);
    // The merge reads the same PR.
    let merge = file.track("spine").unwrap().merge.as_ref().unwrap();
    assert_eq!(merge.pr.argv, command.argv);
}

#[test]
fn the_spine_merge_waits_for_the_ack_and_does_not_check_the_head() {
    let file = load().file;
    let merge = file.track("spine").unwrap().merge.as_ref().unwrap();
    assert_eq!(merge.rule, pf_engine::MergeRule::ClaimAck);
    assert_eq!(merge.repo, "{param.forge_repo}");
    assert_eq!(merge.base, "{param.base_branch}");
    assert_eq!(merge.head_branch.as_deref(), Some("{param.spine_branch}"));
    assert!(!merge.head_check, "the acked SHA replaces the head check");
    assert_eq!(merge.after_merge.len(), 1);
    assert_eq!(
        merge.after_merge[0].argv,
        [
            "git",
            "-C",
            "{param.repo}",
            "fetch",
            "origin",
            "{param.base_branch}"
        ]
    );
    // The item merge stays as it was.
    let item = file.track("item").unwrap().merge.as_ref().unwrap();
    assert_eq!(item.rule, pf_engine::MergeRule::GreenChecksInDeclaredOrder);
    assert!(item.head_check && item.head_branch.is_none());
}

#[test]
fn the_run_ends_when_the_method_records_the_close() {
    let file = load().file;
    let end = file.end_when.as_ref().expect("end_when");
    assert_eq!(end.when.len(), 1);
    let DoneCond::Exit0(c) = &end.when[0] else {
        panic!("{:?}", end.when);
    };
    assert_eq!(
        c.argv,
        [
            "jq",
            "-e",
            "--arg",
            "s",
            "{param.spine}",
            "any(.spines[]; .id == $s and .status == \"closed\")",
            "{param.state_file}"
        ]
    );
    assert_eq!(
        end.waiting,
        "the spine PR is merged: run /ossify:close {param.spine} for the second pass, in your own session or in the project's Orchestrator chat"
    );
    assert_eq!(end.reason, "spine {param.spine} closed");
}

#[test]
fn the_spine_close_brief_stops_after_the_pr_opens() {
    let dir = package_dir();
    let brief = std::fs::read_to_string(dir.join("briefs/spine-close.md")).unwrap();
    assert!(brief.contains("/ossify:close"), "{brief}");
    assert!(brief.contains("skill://close"), "{brief}");
    assert!(brief.contains("work-pr"), "{brief}");
    for never in ["merge", "review", "demo", "second pass"] {
        assert!(brief.contains(never), "the brief never says {never:?}");
    }
    // A halt before the PR opens is a stop.
    assert!(brief.contains("stop file"), "{brief}");
    assert!(
        load()
            .briefs
            .iter()
            .any(|b| b == Path::new("briefs/spine-close.md"))
    );
}

#[test]
fn the_spine_close_role_may_push_its_branch_and_nothing_else_is_added_to_its_rules() {
    let package = load();
    assert_eq!(
        package.rules["close"].allow,
        [
            Allow::DeleteOwnScratch,
            Allow::PushOwnBranch,
            Allow::WriteOwnReportAndRunFiles
        ]
    );
}

// ------------------------------------------------------------------ the base push

#[test]
fn only_the_item_close_stage_has_a_base_push_of_the_spine_branch() {
    let package = load();
    let close = item_stage(&package.file, "close");
    assert_eq!(
        close.base_push,
        Some(pf_workflow::BasePush {
            dir: "{param.repo}".to_string(),
            branch: "{param.spine_branch}".to_string(),
        }),
        "ossify's PR-mode close needs the spine branch on origin, and the seat may push only its own"
    );
    for track in &package.file.tracks {
        for stage in &track.stages {
            if track.name == "item" && stage.name == "close" {
                continue;
            }
            assert!(
                stage.base_push.is_none(),
                "{}.{} must not have a base push",
                track.name,
                stage.name
            );
        }
    }
}
