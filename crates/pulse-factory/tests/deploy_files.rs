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
fn the_skill_names_every_verb_and_waits_for_nothing() {
    let verbs = verbs_of_cli_contract(&read("docs/contracts/cli.md"));
    assert_eq!(verbs, vec!["arm", "status", "halt", "hook", "controller"]);
    assert_eq!(
        skill_problems(&read("skill/SKILL.md"), &verbs),
        Vec::<String>::new()
    );
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
}
