//! `arm` (client side): check a plan, show what it will spend, and send it to the controller.
//!
//! The client computes the whole [`ArmPlan`] (the checks, the forge facts and the build target)
//! and sends it. A refusal prints `pulse-factory: arm refused: <cause>` and records nothing: this
//! code does not even create the state directory. Only the controller writes (ADR-0003).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pf_engine::render::RenderContext;
use pf_workflow::commands::{run_argv, run_plan_check};
use pf_workflow::package::Package;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::ArmArgs;
use crate::forge;

/// The summary schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// Parameters that `arm` itself sets.
pub const ARM_SET: [&str; 3] = ["spine", "briefs", "forge_repo"];

/// Everything the controller needs to start a run. The client computes it all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArmPlan {
    pub spine: String,
    pub workflow_dir: PathBuf,
    pub ledger: PathBuf,
    /// The parameters, with `spine` and `forge_repo`. The controller adds `briefs`.
    pub params: BTreeMap<String, String>,
    pub plugin_roots: Vec<PathBuf>,
    pub paper: Option<PathBuf>,
    pub replace: Option<String>,
    pub forge_repo: String,
    pub default_branch: String,
    /// The build target of the repo, or empty.
    pub target_dir: String,
}

/// An earlier run, as the controller reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunInfo {
    pub id: String,
    #[serde(default)]
    pub spine: String,
    /// `active` or `ended`.
    pub state: String,
    #[serde(default)]
    pub running_sessions: u64,
}

fn refuse(cause: impl std::fmt::Display) -> String {
    format!("pulse-factory: arm refused: {cause}")
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// The share directory: `$PF_SHARE_DIR`, else `~/.local/share/pulse-factory`.
pub fn share_dir() -> PathBuf {
    match std::env::var_os("PF_SHARE_DIR").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map_or_else(|| PathBuf::from("."), PathBuf::from)
            .join(".local/share/pulse-factory"),
    }
}

fn valid_spine(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn parse_params(raw: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for item in raw {
        let (k, v) = item
            .split_once('=')
            .ok_or_else(|| refuse(format!("--param {item:?} is not key=value")))?;
        if k.is_empty() {
            return Err(refuse(format!("--param {item:?} has no key")));
        }
        if out.insert(k.to_string(), v.to_string()).is_some() {
            return Err(refuse(format!("parameter {k} is given twice")));
        }
    }
    Ok(out)
}

/// What the plan will do, as the operator reads it.
#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub schema_version: u32,
    pub spine: String,
    pub items: Option<u64>,
    pub workflow: Value,
    pub roles: Vec<Value>,
    pub caps: Value,
    pub plugins: Vec<Value>,
    pub most_launches: Option<u64>,
    pub merges: Vec<Value>,
    pub replaces: Option<String>,
    pub paper: Option<String>,
    pub forge_repo: String,
    pub default_branch: String,
}

impl Summary {
    /// The text form, one fact per line.
    pub fn text(&self) -> String {
        let mut t = String::new();
        let line = |t: &mut String, s: String| {
            t.push_str(&s);
            t.push('\n');
        };
        line(&mut t, format!("spine: {}", self.spine));
        line(
            &mut t,
            match self.items {
                Some(n) => format!("items: {n}"),
                None => "items: unknown (the workflow has no plan_check)".to_string(),
            },
        );
        line(
            &mut t,
            format!(
                "workflow: {} (file version {})",
                self.workflow["name"].as_str().unwrap_or(""),
                self.workflow["version"]
            ),
        );
        for r in &self.roles {
            line(
                &mut t,
                format!(
                    "role {}: runtime {}, model {}",
                    r["name"].as_str().unwrap_or(""),
                    r["runtime"].as_str().unwrap_or(""),
                    r["model"].as_str().unwrap_or("")
                ),
            );
        }
        line(
            &mut t,
            format!(
                "caps: model_sessions {}, cold_builds {}",
                self.caps["model_sessions"], self.caps["cold_builds"]
            ),
        );
        for s in self.caps["seeds"].as_array().into_iter().flatten() {
            line(
                &mut t,
                format!(
                    "caps: seed {} runs at most {} times",
                    s["id"].as_str().unwrap_or(""),
                    s["max"]
                ),
            );
        }
        line(
            &mut t,
            format!("caps: {}", self.caps["relaunch"].as_str().unwrap_or("")),
        );
        for p in &self.plugins {
            line(
                &mut t,
                format!(
                    "plugin {} {}",
                    p["name"].as_str().unwrap_or(""),
                    p["version"].as_str().unwrap_or("")
                ),
            );
        }
        line(
            &mut t,
            match self.most_launches {
                Some(n) => format!("most launches: {n}"),
                None => "most launches: unknown (the workflow has no plan_check)".to_string(),
            },
        );
        for m in &self.merges {
            line(&mut t, m["sentence"].as_str().unwrap_or("").to_string());
        }
        line(
            &mut t,
            format!(
                "forge: {} (default branch {})",
                self.forge_repo, self.default_branch
            ),
        );
        if let Some(r) = &self.replaces {
            line(&mut t, format!("replaces: {r}"));
        }
        if let Some(p) = &self.paper {
            line(
                &mut t,
                format!("paper env: {p} (no model, no spend, no network)"),
            );
        }
        t
    }
}

/// The most launches the plan shows: `2 × (Σ seeds max × stages + Σ unit-source tracks items ×
/// stages)`. Each session stage counts, and the 2 is each stall's one relaunch.
pub fn most_launches(pkg: &Package, items: Option<u64>) -> Option<u64> {
    let stages = |track: &str| -> u64 {
        pkg.file
            .track(track)
            .map_or(0, |t| u64::try_from(t.stages.len()).unwrap_or(u64::MAX))
    };
    let mut total: u64 = 0;
    for seed in &pkg.file.seeds {
        let max = seed.repeat.as_ref().map_or(1, |r| u64::from(r.max));
        total = total.saturating_add(max.saturating_mul(stages(&seed.track)));
    }
    let mut tracks: Vec<&str> = pkg
        .file
        .unit_sources
        .iter()
        .map(|u| u.track.as_str())
        .collect();
    tracks.sort();
    tracks.dedup();
    if !tracks.is_empty() {
        let n = items?;
        for t in tracks {
            total = total.saturating_add(n.saturating_mul(stages(t)));
        }
    }
    Some(total.saturating_mul(2))
}

fn build_summary(
    plan: &ArmPlan,
    pkg: &Package,
    items: Option<u64>,
    plugins: &[(String, String)],
    ctx: &RenderContext,
) -> Summary {
    let merges = pkg
        .file
        .tracks
        .iter()
        .filter_map(|t| t.merge.as_ref().map(|m| (t, m)))
        .map(|(t, m)| {
            // The base renders with the run's values and no unit. A base that needs a unit shows
            // as written.
            let base =
                pf_engine::render::render(&m.base, ctx).map_or_else(|_| m.base.clone(), |r| r.text);
            json!({
                "track": t.name,
                "base": base,
                "sentence": format!("item PRs merge into {base} on green checks with no ack"),
            })
        })
        .collect();
    Summary {
        schema_version: SCHEMA_VERSION,
        spine: plan.spine.clone(),
        items,
        workflow: json!({ "name": pkg.file.name, "version": pkg.file.version }),
        roles: pkg
            .file
            .roles
            .iter()
            .map(|r| json!({ "name": r.name, "runtime": r.runtime, "model": r.model }))
            .collect(),
        caps: json!({
            "model_sessions": pkg.file.settings.model_sessions,
            "cold_builds": pkg.file.settings.cold_builds,
            "seeds": pkg.file.seeds.iter().map(|s| json!({
                "id": s.id,
                "max": s.repeat.as_ref().map_or(1, |r| u64::from(r.max)),
            })).collect::<Vec<_>>(),
            "relaunch": "one relaunch per stage",
        }),
        plugins: plugins
            .iter()
            .map(|(n, v)| json!({ "name": n, "version": v }))
            .collect(),
        most_launches: most_launches(pkg, items),
        merges,
        replaces: plan.replace.clone(),
        paper: plan.paper.as_ref().map(|p| p.display().to_string()),
        forge_repo: plan.forge_repo.clone(),
        default_branch: plan.default_branch.clone(),
    }
}

/// Everything `arm` works out before it asks the controller anything.
pub struct Prepared {
    pub plan: ArmPlan,
    pub package: Package,
    pub items: Option<u64>,
    pub plugins: Vec<(String, String)>,
}

fn load_package(args: &ArmArgs) -> Result<(PathBuf, Package), String> {
    let dir = if args.workflow.contains('/') {
        absolute(Path::new(&args.workflow))
    } else {
        share_dir().join("workflows").join(&args.workflow)
    };
    let pkg =
        Package::load(&dir).map_err(|e| refuse(format!("the workflow does not load: {e}")))?;
    Ok((dir, pkg))
}

fn check_params(pkg: &Package, given: &BTreeMap<String, String>) -> Result<(), String> {
    for (k, v) in given {
        if ARM_SET.contains(&k.as_str()) {
            return Err(refuse(format!(
                "parameter {k} is set by arm itself; do not pass it"
            )));
        }
        if !pkg.file.params.contains(k) {
            return Err(refuse(format!(
                "parameter {k} is not declared by workflow {}",
                pkg.file.name
            )));
        }
        if v == "." || v.starts_with("./") || v.starts_with("../") {
            return Err(refuse(format!(
                "parameter {k} is the relative path {v:?}; the controller runs in another directory, so give an absolute path"
            )));
        }
    }
    for k in &pkg.file.params {
        if !ARM_SET.contains(&k.as_str()) && !given.contains_key(k) {
            return Err(refuse(format!("parameter {k} is declared but not given")));
        }
    }
    Ok(())
}

fn git_stdout(argv: &[&str]) -> Result<String, String> {
    let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
    let out = run_argv(&argv, None, 30).map_err(|f| f.to_string())?;
    if out.exit_code != Some(0) {
        let line = String::from_utf8_lossy(&out.stderr)
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string();
        return Err(line);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Read the forge facts: from GitHub, or from the paper env's bare repo.
fn forge_facts(
    paper: Option<&Path>,
    params: &BTreeMap<String, String>,
) -> Result<forge::Facts, String> {
    if let Some(paper) = paper {
        let bare = paper.join("forge.git");
        let canon = std::fs::canonicalize(&bare).map_err(|e| {
            refuse(format!(
                "the paper env has no forge.git at {}: {e}",
                bare.display()
            ))
        })?;
        let head = git_stdout(&[
            "git",
            "-C",
            &canon.display().to_string(),
            "symbolic-ref",
            "--short",
            "HEAD",
        ])
        .map_err(|e| refuse(format!("cannot read the HEAD of {}: {e}", canon.display())))?;
        return Ok(forge::Facts {
            forge_repo: canon.display().to_string(),
            default_branch: head,
        });
    }
    let repo = params.get("repo").ok_or_else(|| {
        refuse("the workflow declares no parameter `repo`, so arm cannot find the GitHub remote")
    })?;
    let origin = git_stdout(&["git", "-C", repo, "remote", "get-url", "origin"])
        .map_err(|e| refuse(format!("cannot read the origin of {repo}: {e}")))?;
    let owner_name = forge::parse_origin(&origin)
        .ok_or_else(|| refuse(format!("the origin of {repo} is not on GitHub: {origin}")))?;
    forge::gh_facts(&owner_name).map_err(refuse)
}

/// `target_directory` of the repo's cargo metadata, or empty on any failure.
fn target_dir(params: &BTreeMap<String, String>) -> String {
    let Some(repo) = params.get("repo") else {
        return String::new();
    };
    git_stdout(&[
        "env",
        "-C",
        repo,
        "cargo",
        "metadata",
        "--no-deps",
        "--format-version",
        "1",
        "--locked",
    ])
    .ok()
    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    .and_then(|v| v["target_directory"].as_str().map(str::to_string))
    .unwrap_or_default()
}

/// Run every check that needs no controller.
pub fn prepare(args: &ArmArgs) -> Result<Prepared, String> {
    if std::env::var_os("PF_SEAT_TOKEN").is_some() {
        return Err(refuse(
            "PF_SEAT_TOKEN is set, so this is a seat; a seat cannot arm a run",
        ));
    }
    if args.dry_run && args.yes {
        return Err(refuse("--dry-run and --yes cannot go together"));
    }
    if !valid_spine(&args.spine) {
        return Err(refuse(format!(
            "spine name {:?} must match [A-Za-z0-9._-]+",
            args.spine
        )));
    }
    let (workflow_dir, package) = load_package(args)?;
    let given = parse_params(&args.params)?;
    check_params(&package, &given)?;

    let ledger = absolute(&args.ledger);
    let plugin_roots: Vec<PathBuf> = args.plugin_roots.iter().map(|p| absolute(p)).collect();
    for root in &plugin_roots {
        if !root.is_dir() {
            return Err(refuse(format!(
                "--plugin-root {} does not exist",
                root.display()
            )));
        }
    }
    let paper = match &args.paper {
        Some(p) => {
            let p = absolute(p);
            if !p.is_dir() {
                return Err(refuse(format!("--paper {} does not exist", p.display())));
            }
            Some(p)
        }
        None => None,
    };

    let mut params = given;
    params.insert("spine".to_string(), args.spine.clone());

    // plan_check has a run ID and a directory of no value yet: it renders with the parameters.
    let items = match &package.file.plan_check {
        Some(spec) => {
            let ctx = RenderContext {
                run_id: "",
                run_dir: "",
                params: &params,
                unit: None,
            };
            Some(run_plan_check(spec, &ctx).map_err(refuse)?)
        }
        None => None,
    };

    let mut plugins = Vec::new();
    for req in &package.file.plugins {
        let found = pf_adapter_omp::plugin::resolve(&plugin_roots, &req.name, &req.min_version)
            .map_err(|e| refuse(format!("plugin {}: {e}", req.name)))?;
        plugins.push((found.name, found.version));
    }

    let facts = forge_facts(paper.as_deref(), &params)?;
    params.insert("forge_repo".to_string(), facts.forge_repo.clone());
    let target_dir = target_dir(&params);

    let plan = ArmPlan {
        spine: args.spine.clone(),
        workflow_dir,
        ledger,
        params,
        plugin_roots,
        paper,
        replace: args.replace.clone(),
        forge_repo: facts.forge_repo,
        default_branch: facts.default_branch,
        target_dir,
    };
    Ok(Prepared {
        plan,
        package,
        items,
        plugins,
    })
}

/// Check the earlier runs the controller reports.
pub fn check_earlier(runs: &[RunInfo], replace: Option<&str>) -> Result<(), String> {
    if let Some(name) = replace {
        match runs.iter().find(|r| r.id == name) {
            None => return Err(refuse(format!("--replace {name}: no such run"))),
            Some(r) if r.state != "active" => {
                return Err(refuse(format!(
                    "--replace {name}: that run is at an end state, and blocks nothing"
                )));
            }
            Some(_) => {}
        }
    }
    if let Some(r) = runs.iter().find(|r| r.running_sessions > 0) {
        return Err(refuse(format!(
            "run {} has a running session; only one run may have a live seat at a time",
            r.id
        )));
    }
    if let Some(r) = runs
        .iter()
        .find(|r| r.state == "active" && Some(r.id.as_str()) != replace)
    {
        return Err(refuse(format!(
            "run {} is not at an end state; name it with --replace to replace it",
            r.id
        )));
    }
    Ok(())
}

enum Earlier {
    Known(Vec<RunInfo>),
    NoController(String),
}

fn earlier_runs(state: &Path) -> Result<Earlier, String> {
    match pf_ipc::call(state, "runs", json!({})) {
        Ok(resp) if resp.ok => {
            let runs: Vec<RunInfo> =
                serde_json::from_value(resp.result.get("runs").cloned().unwrap_or(json!([])))
                    .map_err(|e| {
                        refuse(format!("the controller's list of runs is unreadable: {e}"))
                    })?;
            Ok(Earlier::Known(runs))
        }
        Ok(resp) => Err(refuse(format!(
            "the controller refused to list runs: {}",
            resp.error.unwrap_or_default()
        ))),
        Err(e @ pf_ipc::ClientError::NoController { .. }) => {
            Ok(Earlier::NoController(e.to_string()))
        }
        Err(e) => Err(refuse(e)),
    }
}

/// The `arm` verb. It returns the exit code.
pub fn run(args: &ArmArgs) -> i32 {
    match run_inner(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}");
            1
        }
    }
}

fn run_inner(args: &ArmArgs) -> Result<i32, String> {
    let prepared = prepare(args)?;
    let state = pf_ipc::state_dir();
    let mut note = None;
    match earlier_runs(&state)? {
        Earlier::Known(runs) => check_earlier(&runs, args.replace.as_deref())?,
        Earlier::NoController(why) => {
            if args.yes {
                return Err(refuse(why));
            }
            if args.replace.is_some() {
                return Err(refuse(format!(
                    "--replace needs a controller to name the run. {why}"
                )));
            }
            note = Some(format!(
                "note: no controller answers, so the checks on earlier runs were skipped ({why})"
            ));
        }
    }

    let run_dir_unknown = RenderContext {
        run_id: "",
        run_dir: "",
        params: &prepared.plan.params,
        unit: None,
    };
    let summary = build_summary(
        &prepared.plan,
        &prepared.package,
        prepared.items,
        &prepared.plugins,
        &run_dir_unknown,
    );
    if !args.yes {
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&summary).unwrap_or_default()
            );
        } else {
            print!("{}", summary.text());
            if let Some(n) = &note {
                println!("{n}");
            }
        }
        if args.dry_run {
            return Ok(0);
        }
        eprintln!("pulse-factory: nothing was started; re-run with --yes to start");
        return Ok(2);
    }

    // The operator approves a summary they saw: print it, then start.
    if !args.json {
        print!("{}", summary.text());
    }
    let resp = pf_ipc::call(
        &state,
        "arm",
        serde_json::to_value(&prepared.plan).map_err(refuse)?,
    )
    .map_err(refuse)?;
    if !resp.ok {
        return Err(refuse(resp.error.unwrap_or_default()));
    }
    let run_id = resp.result["run_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if args.json {
        println!(
            "{}",
            json!({ "schema_version": SCHEMA_VERSION, "run_id": run_id, "summary": summary })
        );
    } else {
        println!("run: {run_id}");
    }
    Ok(0)
}
