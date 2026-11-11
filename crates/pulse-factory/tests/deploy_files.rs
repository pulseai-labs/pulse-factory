//! AC-13: the unit file, the scripts and the skill. Each check has a case over an inline bad
//! sample that must fire.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

// ---- the checks, as functions over text, so a bad sample can make each one fire

fn unit_problems(unit: &str) -> Vec<String> {
    let mut p = Vec::new();
    let line = |key: &str| {
        unit.lines()
            .find(|l| l.trim_start().starts_with(key))
            .map(str::to_string)
    };
    match line("ExecStart=") {
        Some(l) if l.split_whitespace().last() == Some("controller") => {}
        _ => p.push("ExecStart does not run `controller`".into()),
    }
    if !unit.lines().any(|l| l.trim() == "Restart=on-failure") {
        p.push("no Restart=on-failure".into());
    }
    if !unit.lines().any(|l| l.trim() == "WantedBy=default.target") {
        p.push("no WantedBy=default.target".into());
    }
    match unit.lines().find(|l| l.contains("PATH=")) {
        Some(l) if l.contains(".local/bin") && l.contains(".cargo/bin") => {}
        _ => p.push("PATH lacks .local/bin or .cargo/bin".into()),
    }
    if unit.contains("CARGO_") {
        p.push("the unit sets a CARGO_ variable".into());
    }
    // The chat processes are children of the controller. systemd must end any child that is left
    // in the unit's cgroup, such as a background `Bash` in its own session (ADR-0001, ADR-0015).
    if !unit.lines().any(|l| l.trim() == "KillMode=control-group") {
        p.push("no KillMode=control-group".into());
    }
    p
}

fn install_problems(script: &str) -> Vec<String> {
    let mut p = Vec::new();
    if script.contains("CARGO_PROFILE_") {
        p.push("sets CARGO_PROFILE_".into());
    }
    for l in script.lines().filter(|l| !l.trim_start().starts_with('#')) {
        let writes = l.contains(">")
            || l.contains("tee ")
            || l.contains("sed -i")
            || l.contains("cp ")
            || l.contains("mv ");
        if writes && l.contains(".cargo") {
            p.push(format!("writes to .cargo: {l}"));
        }
    }
    p
}

fn skill_problems(skill: &str, verbs: &[String]) -> Vec<String> {
    let mut p = Vec::new();
    for v in verbs {
        if !skill.contains(&format!("`{v}`")) && !skill.contains(&format!("pulse-factory {v}")) {
            p.push(format!("does not name the verb {v}"));
        }
    }
    for l in skill.lines() {
        let words: Vec<&str> = l
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .collect();
        if words.iter().any(|w| matches!(*w, "sleep" | "watch"))
            || l.contains("while true")
            || l.contains("until ") && l.contains("; do")
        {
            p.push(format!("a wait or polling instruction: {l}"));
        }
    }
    // `oss` has no `spine_branch` verb (`oss branch_name <id> <slug>` prints the branch), and
    // `oss spine_dir` takes `<release> <spine> <slug>`.
    if skill.contains("oss spine_branch") {
        p.push("calls `oss spine_branch`, which is no verb".into());
    }
    if skill.contains("oss spine_dir \"$spine\"") {
        p.push("calls `oss spine_dir \"$spine\"` with one argument".into());
    }
    p
}

/// A guide that tells the operator what to do with a parked unit: answer it or clear it, in the
/// same run. A guide that sends them to a new run is out of date since `r0.s2`.
fn parked_problems(doc: &str) -> Vec<String> {
    let mut p = Vec::new();
    for l in doc.lines() {
        if l.contains("parked") && l.contains("new run") {
            p.push(format!("a parked unit needs a new run: {l}"));
        }
    }
    let names = |verb: &str| {
        doc.contains(&format!("`{verb}`")) || doc.contains(&format!("pulse-factory {verb}"))
    };
    if !doc.contains("parked") || !names("answer") || !names("clear") {
        p.push("does not name `answer` and `clear` for a parked unit".into());
    }
    p
}

fn verbs_of_cli_contract(cli: &str) -> Vec<String> {
    cli.lines()
        .filter_map(|l| l.strip_prefix("## `")?.strip_suffix('`'))
        .map(str::to_string)
        .collect()
}

// ---- the real files

#[test]
fn the_unit_file_is_as_the_contract_says() {
    let unit = read("deploy/systemd/pulse-factory.service");
    assert_eq!(unit_problems(&unit), Vec::<String>::new());
}

#[test]
fn every_script_parses_with_bash_n() {
    let mut scripts = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "sh") {
                out.push(p);
            }
        }
    }
    walk(&root().join("scripts"), &mut scripts);
    assert!(
        scripts.iter().any(|s| s.ends_with("install.sh")),
        "install.sh is missing: {scripts:?}"
    );
    for s in scripts {
        let out = Command::new("bash").arg("-n").arg(&s).output().unwrap();
        assert!(
            out.status.success(),
            "bash -n {}: {}",
            s.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn install_sh_sets_no_profile_and_writes_no_cargo_config() {
    assert_eq!(
        install_problems(&read("scripts/install.sh")),
        Vec::<String>::new()
    );
}

#[test]
fn the_skill_and_the_install_guide_say_how_to_answer_a_parked_unit() {
    for file in ["skill/SKILL.md", "docs/install.md"] {
        assert_eq!(parked_problems(&read(file)), Vec::<String>::new(), "{file}");
    }
    let skill = read("skill/SKILL.md");
    for want in [
        "pulse-factory requests",
        "pulse-factory show",
        "pulse-factory claim",
    ] {
        assert!(skill.contains(want), "SKILL.md names {want}");
    }
    // The branch and the directory come from verbs that exist.
    assert!(skill.contains("oss branch_name \"$spine\" \"$slug\""));
    assert!(skill.contains("oss spine_dir \"$release\" \"$spine\" \"$slug\""));
}

#[test]
fn the_skill_names_every_verb_and_waits_for_nothing() {
    let verbs = verbs_of_cli_contract(&read("docs/contracts/cli.md"));
    assert_eq!(
        verbs,
        vec![
            "arm",
            "propose",
            "status",
            "halt",
            "requests",
            "show",
            "claim",
            "answer",
            "clear",
            "ack",
            "project",
            "session",
            "settings",
            "chat-runtime",
            "device",
            "api",
            "hook",
            "controller"
        ]
    );
    let skill = read("skill/SKILL.md");
    assert_eq!(skill_problems(&skill, &verbs), Vec::<String>::new());
}

// ---- each check fires on a bad sample

#[test]
fn each_check_fires_on_a_bad_sample() {
    let good_unit = read("deploy/systemd/pulse-factory.service");
    for (bad, want) in [
        (good_unit.replace("controller", "daemon"), "ExecStart"),
        (
            good_unit.replace("Restart=on-failure", "Restart=always"),
            "Restart",
        ),
        (
            good_unit.replace("default.target", "multi-user.target"),
            "WantedBy",
        ),
        (good_unit.replace(".cargo/bin", "bin"), "PATH"),
        (format!("{good_unit}Environment=CARGO_HOME=/x\n"), "CARGO_"),
        // No `KillMode` at all, and a mode that leaves the children running.
        (
            good_unit.replace("KillMode=control-group\n", ""),
            "KillMode",
        ),
        (
            good_unit.replace("KillMode=control-group", "KillMode=process"),
            "KillMode",
        ),
    ] {
        let p = unit_problems(&bad);
        assert!(
            p.iter().any(|m| m.contains(want)),
            "{want} must fire: {p:?}"
        );
    }
    assert!(!install_problems("export CARGO_PROFILE_DEV_DEBUG=2\n").is_empty());
    assert!(!install_problems("echo x >> \"$HOME/.cargo/config.toml\"\n").is_empty());
    assert!(!install_problems("sed -i s/a/b/ ~/.cargo/config.toml\n").is_empty());
    let verbs = vec!["arm".to_string(), "status".to_string()];
    assert!(
        !skill_problems("only `arm` here", &verbs).is_empty(),
        "a missing verb must fire"
    );
    for bad in [
        "wait: sleep 5",
        "run `watch pulse-factory status`",
        "while true; do pulse-factory status; done",
        "until pulse-factory status; do x; done",
    ] {
        let p = skill_problems(&format!("`arm` `status`\n{bad}\n"), &verbs);
        assert!(!p.is_empty(), "{bad:?} must fire");
    }
    assert!(skill_problems("`arm` `status` and a watchful eye", &verbs).is_empty());
    for (bad, want) in [
        (
            "spine_branch=\"spine/$(oss spine_branch \"$spine\")\"",
            "spine_branch",
        ),
        ("spine_dir=\"$(oss spine_dir \"$spine\")\"", "spine_dir"),
    ] {
        let p = skill_problems(&format!("`arm` `status`\n{bad}\n"), &verbs);
        assert!(
            p.iter().any(|m| m.contains(want)),
            "{bad:?} must fire: {p:?}"
        );
    }
    let fixed =
        "`arm` `status`\nspine_dir=\"$(oss spine_dir \"$release\" \"$spine\" \"$slug\")\"\n";
    assert!(skill_problems(fixed, &verbs).is_empty());
    for bad in [
        "A parked unit needs a new run until r0.s2.",
        "A parked unit shows its reason.",
        "Use `answer` to resume it.",
        "A parked unit: `answer` it, or start a new run.",
    ] {
        assert!(!parked_problems(bad).is_empty(), "{bad:?} must fire");
    }
    assert!(parked_problems("A parked unit: use `answer` or `clear` in the same run.").is_empty());
}
