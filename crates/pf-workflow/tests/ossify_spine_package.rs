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
    assert_eq!(names, ["implement", "verify", "close"]);
}

#[test]
fn only_the_item_track_merges_and_it_checks_the_head() {
    let file = load().file;
    let with_merge: Vec<&str> = file
        .tracks
        .iter()
        .filter(|t| t.merge.is_some())
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(with_merge, ["item"]);
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
