//! AC-11: `pf-workflow` names no planning method and depends on no `pf-` crate but `pf-engine`.
//!
//! The test reads every file under `pf-workflow/src/` and fails, ignoring case and treating `_`
//! and a space alike, on the words of a planning method as a whole word or an identifier part,
//! or on a call to the planning tool. The matching rules are those of `engine_is_generic` in
//! `pf-engine`. It also reads `pf-workflow`'s `Cargo.toml` and fails on any `pf-` dependency
//! except `pf-engine`. Each check has a case over an inline bad sample that must fire.
//!
//! The banned words sit in the lists below, so this file is not under `src/` on purpose.

use std::path::{Path, PathBuf};

// ------------------------------------------------------------------ manifest check

fn is_pf(name: &str) -> bool {
    let n = name.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
    n.starts_with("pf-") || n.starts_with("pf_")
}

/// The text between the first pair of quotes after `package`, in a value like
/// `{ package = "pf-x", path = "y" }`.
fn renamed_package(value: &str) -> Option<String> {
    let at = value.find("package")?;
    let rest = &value[at + "package".len()..];
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let inner = &rest[1..];
    let end = inner.find(quote)?;
    Some(inner[..end].to_string())
}

/// The last path component of `path = "../x"` in a value.
fn path_target(value: &str) -> Option<String> {
    let at = value.find("path")?;
    let rest = value[at + "path".len()..]
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let inner = &rest[1..];
    let end = inner.find(quote)?;
    inner[..end]
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .map(str::to_string)
}

/// Names of workspace dependencies that are `pf-` crates, by key, rename or path.
fn workspace_pf_names(workspace_manifest: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_table = false;
    for line in workspace_manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_table = line.trim_matches(['[', ']']) == "workspace.dependencies";
            continue;
        }
        if !in_table || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().split('.').next().unwrap_or("").trim_matches('"');
            let hit = is_pf(key)
                || renamed_package(value).is_some_and(|p| is_pf(&p))
                || path_target(value).is_some_and(|p| is_pf(&p));
            if hit {
                names.push(key.to_string());
            }
        }
    }
    names
}

/// Every dependency of the manifest that is a `pf-` crate.
fn pf_dependencies(manifest: &str, workspace_manifest: &str) -> Vec<String> {
    let inherited = workspace_pf_names(workspace_manifest);
    let mut found = Vec::new();
    let mut flat_table = false; // inside [dependencies], [dev-dependencies], [target.x.dependencies]
    let mut named_table: Option<String> = None; // inside [dependencies.NAME]
    for raw in manifest.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            flat_table = false;
            named_table = None;
            let header = line.trim_matches(['[', ']']);
            if header.ends_with("dependencies") {
                flat_table = true;
            } else if let Some(at) = header.rfind("dependencies.") {
                let name = header[at + "dependencies.".len()..].trim_matches(['"', '\'']);
                if is_pf(name) {
                    found.push(name.to_string());
                }
                named_table = Some(name.to_string());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if let Some(name) = &named_table {
            if key.trim() == "package" && is_pf(value) {
                found.push(format!("{name} (package {})", value.trim()));
            }
            if key.trim() == "path" && path_target(line).is_some_and(|p| is_pf(&p)) {
                found.push(format!("{name} (path {})", value.trim()));
            }
            if key.trim() == "workspace" && inherited.contains(name) {
                found.push(format!("{name} (inherited from the workspace)"));
            }
            continue;
        }
        if !flat_table {
            continue;
        }
        let key = key.trim().trim_matches('"');
        let base = key.split('.').next().unwrap_or(key);
        let inherits = key.ends_with(".workspace") || value.contains("workspace");
        if is_pf(base) {
            found.push(base.to_string());
        } else if renamed_package(value).is_some_and(|p| is_pf(&p)) {
            found.push(format!(
                "{base} (renamed from {})",
                renamed_package(value).unwrap()
            ));
        } else if path_target(line).is_some_and(|p| is_pf(&p)) {
            found.push(format!("{base} (path)"));
        } else if inherits && inherited.iter().any(|n| n == base) {
            found.push(format!("{base} (inherited from the workspace)"));
        }
    }
    found
}

// ------------------------------------------------------------------ source check

struct Word {
    text: String,
    /// The text between the end of the previous word and the start of this one.
    gap: String,
    after_dot: bool,
    before_paren: bool,
}

/// Split text into identifier parts: break on every character that is not a letter or a digit
/// (so `_` and a space alike), at a lower-to-upper case change, and between letters and digits.
fn words(text: &str) -> Vec<Word> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Word> = Vec::new();
    let mut i = 0;
    let mut gap_start = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < chars.len() && chars[i].is_ascii_alphanumeric() {
            let (p, c) = (chars[i - 1], chars[i]);
            let boundary = (p.is_ascii_lowercase() && c.is_ascii_uppercase())
                || (p.is_ascii_digit() != c.is_ascii_digit())
                || (p.is_ascii_uppercase()
                    && c.is_ascii_uppercase()
                    && chars.get(i + 1).is_some_and(char::is_ascii_lowercase));
            if boundary {
                break;
            }
            i += 1;
        }
        out.push(Word {
            text: chars[start..i]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase(),
            gap: chars[gap_start..start].iter().collect(),
            after_dot: start > 0 && chars[start - 1] == '.',
            before_paren: chars.get(i) == Some(&'('),
        });
        gap_start = i;
    }
    out
}

/// What is wrong with this text, as short descriptions.
fn banned_in(text: &str) -> Vec<String> {
    let ws = words(text);
    let mut hits = Vec::new();
    for (i, w) in ws.iter().enumerate() {
        match w.text.as_str() {
            "ossify" | "spine" | "workitem" | "rounds" => hits.push(w.text.clone()),
            "oss" => hits.push("oss".to_string()),
            "round" if !(w.after_dot && w.before_paren) => hits.push("round".to_string()),
            "item"
                if i > 0
                    && ws[i - 1].text == "work"
                    && w.gap.chars().all(|c| c == '_' || c == ' ') =>
            {
                hits.push("work item".to_string());
            }
            _ => {}
        }
    }
    hits
}

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            source_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The `pf-` dependencies of a manifest, leaving out `pf-engine` itself (a direct dependency
/// by name, path or workspace inheritance). A renamed `pf-engine` still counts as a finding.
fn forbidden_pf_dependencies(manifest: &str, workspace_manifest: &str) -> Vec<String> {
    pf_dependencies(manifest, workspace_manifest)
        .into_iter()
        .filter(|entry| {
            let base = entry.split(" (").next().unwrap_or(entry);
            !(base == "pf-engine" || base == "pf_engine")
        })
        .collect()
}

// ------------------------------------------------------------------ tests

#[test]
fn the_workflow_manifest_has_no_pf_dependency_but_the_engine() {
    let manifest = std::fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    let workspace = std::fs::read_to_string(crate_dir().join("../../Cargo.toml")).unwrap();
    assert_eq!(
        forbidden_pf_dependencies(&manifest, &workspace),
        Vec::<String>::new()
    );
    // The check reads real dependencies: it sees the engine and serde in the manifest.
    assert!(pf_dependencies(&manifest, &workspace).contains(&"pf-engine".to_string()));
    assert!(manifest.contains("serde.workspace"));
}

#[test]
fn every_kind_of_other_pf_dependency_fires_on_an_inline_bad_sample() {
    let workspace = "[workspace.dependencies]\nserde = \"1\"\nstore = { package = \"pf-store-sqlite\", path = \"crates/x\" }\npf-engine = { path = \"crates/pf-engine\" }\n";
    let bad = [
        (
            "normal",
            "[dependencies]\npf-store-sqlite = { path = \"../pf-store-sqlite\" }\n",
        ),
        ("normal, version", "[dependencies]\npf-x = \"1\"\n"),
        (
            "a name that starts like the engine",
            "[dependencies]\npf-engine-extra = \"1\"\n",
        ),
        ("dev", "[dev-dependencies]\npf-x = \"1\"\n"),
        ("build", "[build-dependencies]\npf_x = \"1\"\n"),
        (
            "target-specific",
            "[target.'cfg(unix)'.dependencies]\npf-x = \"1\"\n",
        ),
        (
            "renamed",
            "[dependencies]\nstorage = { package = \"pf-store-sqlite\", path = \"x\" }\n",
        ),
        ("table form", "[dependencies.pf-x]\nversion = \"1\"\n"),
        ("dotted key", "[dependencies]\npf-x.path = \"../pf-x\"\n"),
        (
            "inherited rename",
            "[dependencies]\nstore.workspace = true\n",
        ),
    ];
    for (name, manifest) in bad {
        assert!(
            !forbidden_pf_dependencies(manifest, workspace).is_empty(),
            "the {name} sample must fire:\n{manifest}"
        );
    }
    let good = [
        "[dependencies]\npf-engine = { path = \"../pf-engine\" }\nserde.workspace = true\n",
        "[dependencies]\npf-engine.workspace = true\n",
        "[package]\nname = \"pf-workflow\"\ndescription = \"pf-store-sqlite is a neighbour\"\n[dependencies]\nserde_json = \"1\"\n[dev-dependencies]\ntempfile = \"3\"\n",
    ];
    for manifest in good {
        assert_eq!(
            forbidden_pf_dependencies(manifest, workspace),
            Vec::<String>::new(),
            "{manifest}"
        );
    }
}

#[test]
fn the_workflow_source_names_no_planning_method_and_calls_no_planning_tool() {
    let mut files = Vec::new();
    source_files(&crate_dir().join("src"), &mut files);
    assert!(files.len() >= 8, "the walk found {} files", files.len());
    let mut findings = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for (n, line) in text.lines().enumerate() {
            for hit in banned_in(line) {
                findings.push(format!("{}:{}: {hit}", file.display(), n + 1));
            }
        }
    }
    assert!(findings.is_empty(), "{}", findings.join("\n"));
}

#[test]
fn every_banned_word_fires_on_an_inline_bad_sample() {
    let bad = [
        ("ossify", "use ossify::Plan;"),
        ("ossify, upper case", "let x = OSSIFY;"),
        ("ossify as an identifier part", "struct OssifyRun;"),
        ("spine", "let spine = 1;"),
        ("spine as an identifier part", "fn spine_branch() {}"),
        ("workitem", "let workitem = 1;"),
        ("work item, with a space", "// each work item is a unit"),
        ("work item, with an underscore", "let work_item = 1;"),
        ("work item, in camel case", "struct WorkItem;"),
        ("round", "let round = 1;"),
        ("round as an identifier part", "fn round_robin() {}"),
        ("round as a free call", "let r = round(1.5);"),
        ("round as a field", "let r = x.round;"),
        ("rounds", "let rounds = 3;"),
        ("rounds, upper case", "const ROUNDS: u32 = 3;"),
        (
            "a call to oss",
            "Command::new(\"oss\").arg(\"verify_acs\");",
        ),
        ("a call to oss in a script", "oss verify_step"),
    ];
    for (name, sample) in bad {
        assert!(
            !banned_in(sample).is_empty(),
            "the {name} sample must fire: {sample}"
        );
    }
}

#[test]
fn a_method_call_round_and_the_word_workarounds_pass() {
    let good = [
        "let r = (1.5f64).round();",
        "let r = x.round();",
        "// there are workarounds for this",
        "let workarounds = 1;",
        "let around = 1;",
        "let ground = 1;",
        "let boss = 1; let cross = 2; let gloss = 3;",
        "// The worker did the work. Item two follows.",
        "let item = 1; let work = 2;",
        "fn processing() {}",
        "let spineless = 1;",
    ];
    for sample in good {
        assert_eq!(banned_in(sample), Vec::<String>::new(), "{sample}");
    }
}
