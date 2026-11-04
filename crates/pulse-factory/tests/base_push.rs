//! AC-2: the base push before a stage. A stage with `base_push: {dir, branch}` finds the branch on
//! `origin` before it launches, and pushes the local branch there when `origin` lacks it. The
//! driver runs on a `FakeAdapter` and a bare forge, as in `clear_resume.rs`; the local repo is the
//! rig's clone, whose `origin` is the bare forge. No network and no model.

mod common;

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use common::rig::*;
use common::*;
use pf_adapter_fake::{FakeAdapter, Step};
use pf_engine::admission::PushOutcome;
use pf_engine::claim::sha256_hex;
use pf_engine::port::agent::*;
use pf_engine::port::store::RequestKind;
use pf_engine::status::Status;
use pf_forge_bare::PrChecks;

const UNIT: &str = "u1";

/// The item `close` stage: the base branch and its repo come from the unit's record, so a test
/// picks them per unit. `before` asks the bare forge for the branch, so it passes only when the
/// push came first.
const CLOSE_EXTRA: &str = r#"        base_push:
          dir: "{unit.vars.base_dir}"
          branch: "{unit.vars.base}"
        before:
          - [git, -C, "{param.forge_repo}", rev-parse, --verify, "refs/heads/{unit.vars.base}"]
"#;

fn options() -> Options {
    Options {
        item_vars: "[worktree_path, branch, base, base_dir]".to_string(),
        close_extra: CLOSE_EXTRA.to_string(),
        ..Options::default()
    }
}

/// A rig and an item `u1` whose base branch is `base` in the rig's clone.
fn rig_for(base: &str) -> Rig {
    let r = Rig::with(&options(), |_| {});
    let dir = r.work.clone();
    item(&r, UNIT, 1, base, &dir);
    r
}

/// An item with a PR and a script that pushes the PR's head, as `Rig::item` does, but with the
/// base branch and its repo in the record.
fn item(r: &Rig, id: &str, number: u64, base: &str, base_dir: &Path) -> String {
    let head = r.pr(number, &format!("work/{id}"), PrChecks::Green);
    r.script(
        id,
        vec![
            Step::Push {
                branch: format!("work/{id}"),
                sha: head.clone(),
                outcome: PushOutcome::Pushed,
            },
            Step::TurnEnd,
        ],
    );
    r.queue_item_with(
        id,
        1,
        &[
            ("base", base),
            ("base_dir", &base_dir.display().to_string()),
        ],
    );
    head
}

/// A local branch with a commit of its own in `repo`, off `origin/spine`. Returns its SHA.
fn local_branch(repo: &Path, name: &str) -> String {
    git(repo, &["fetch", "--quiet", "origin"]);
    git(
        repo,
        &["checkout", "--quiet", "-B", name, &format!("origin/{BASE}")],
    );
    std::fs::write(repo.join(format!("{name}.txt").replace('/', "_")), "base\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "--quiet", "-m", name]);
    git(repo, &["rev-parse", "HEAD"])
}

/// The SHA of a branch of the bare forge, or `None`.
fn on_origin(r: &Rig, branch: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(&r.bare)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Put `local` on `origin` as `branch`.
fn put_on_origin(repo: &Path, local: &str, branch: &str) {
    git(
        repo,
        &[
            "push",
            "--quiet",
            "origin",
            &format!("{local}:refs/heads/{branch}"),
        ],
    );
}

/// Pass `implement` and `verify`: the tick of the second pass is the one that launches `close`.
fn through_verify(r: &Rig, unit: &str) {
    r.tick();
    r.pass(unit, &r.report(unit));
    r.pass(unit, &r.out(unit, "verdict.json"));
}

/// Every push line of the ledger that names `branch`.
fn push_lines(r: &Rig, branch: &str) -> Vec<String> {
    r.ledger_lines("push: repo=")
        .into_iter()
        .filter(|l| l.contains(&format!("branch={branch} ")))
        .collect()
}

/// Every push line of the ledger that names the run's forge: the seats' own pushes name another
/// repo (the fake adapter's), so these are the controller's.
fn forge_push_lines(r: &Rig) -> Vec<String> {
    r.ledger_lines("push: repo=")
        .into_iter()
        .filter(|l| l.contains(&format!("repo={} ", r.forge.repo())))
        .collect()
}

/// The reason of the open `park` request of a parked unit.
fn park_reason(r: &Rig, unit: &str) -> String {
    let (status, reason) = r.unit(unit);
    assert_eq!(status, Status::Parked, "{reason:?}");
    let req = r
        .view()
        .open_request_of(unit)
        .cloned()
        .expect("an open request");
    assert_eq!(req.kind, RequestKind::Park);
    assert_eq!(Some(req.reason.clone()), reason);
    req.reason
}

fn close_started(r: &Rig, unit: &str) -> bool {
    !r.stage_node(unit, "close").is_empty()
}

#[test]
fn a_branch_origin_lacks_is_pushed_once_before_the_stage_and_it_launches_in_the_same_tick() {
    let r = rig_for("base-x");
    let sha = local_branch(&r.work, "base-x");
    assert_eq!(
        on_origin(&r, "base-x"),
        None,
        "the branch starts local only"
    );

    // `before` asks the forge for the branch: the close stage launches only when the push came first.
    r.reach_close(UNIT);

    assert_eq!(on_origin(&r, "base-x"), Some(sha.clone()));
    let lines = push_lines(&r, "base-x");
    assert_eq!(lines.len(), 1, "one push, one line: {lines:?}");
    assert!(
        lines[0].contains(&format!(
            "push: repo={} branch=base-x sha={sha} outcome=pushed",
            r.forge.repo()
        )),
        "{}",
        lines[0]
    );
    // Later ticks push nothing and write nothing.
    r.tick();
    r.tick();
    assert_eq!(push_lines(&r, "base-x").len(), 1);
    assert_eq!(on_origin(&r, "base-x"), Some(sha));
}

#[test]
fn a_branch_origin_holds_is_not_pushed_at_the_local_sha_or_at_another() {
    for same in [true, false] {
        let r = rig_for("base-x");
        let local = local_branch(&r.work, "base-x");
        let held = if same {
            put_on_origin(&r.work, "base-x", "base-x");
            local.clone()
        } else {
            let other = local_branch(&r.work, "other");
            put_on_origin(&r.work, "other", "base-x");
            other
        };
        assert_eq!(on_origin(&r, "base-x"), Some(held.clone()));
        assert_eq!(
            held == local,
            same,
            "the held SHA is the local one only when same"
        );

        r.reach_close(UNIT);

        assert!(push_lines(&r, "base-x").is_empty(), "same={same}");
        assert_eq!(
            on_origin(&r, "base-x"),
            Some(held),
            "same={same}: unchanged"
        );
    }
}

#[test]
fn the_default_branch_and_a_name_check_ref_format_refuses_park_the_unit_with_no_push() {
    // The forge's default branch is `spine`.
    let r = rig_for(BASE);
    through_verify(&r, UNIT);
    let reason = park_reason(&r, UNIT);
    assert!(
        reason.contains(&format!("`{BASE}`")) && reason.contains("default branch"),
        "{reason}"
    );
    assert!(!close_started(&r, UNIT));
    assert!(forge_push_lines(&r).is_empty());

    let r = rig_for("bad..name");
    through_verify(&r, UNIT);
    let reason = park_reason(&r, UNIT);
    assert!(
        reason.contains("`bad..name`") && reason.contains("check-ref-format"),
        "{reason}"
    );
    assert!(push_lines(&r, "bad..name").is_empty());
    assert!(!close_started(&r, UNIT));
}

#[test]
fn a_branch_that_starts_with_a_dash_or_is_empty_is_refused_with_no_push() {
    for (base, rule) in [("-x", "must not start with `-`"), ("", "must not be empty")] {
        let r = Rig::with(&options(), |_| {});
        let dir = r.work.clone();
        let rec = pf_engine::port::unit_source::UnitRecord::new(
            UNIT,
            "g1",
            1,
            [
                (
                    "worktree_path".to_string(),
                    r.spine_dir.join("wt").display().to_string(),
                ),
                ("branch".to_string(), "work/u1".to_string()),
                ("base".to_string(), base.to_string()),
                ("base_dir".to_string(), dir.display().to_string()),
            ]
            .into(),
        );
        std::fs::create_dir_all(r.spine_dir.join("wt")).unwrap();
        r.script(UNIT, vec![Step::TurnEnd]);
        r.publish("u1.json", &rec.to_json());
        r.tick();
        assert!(r.has_unit(UNIT), "{base:?}: the record is admitted");
        r.pass(UNIT, &r.report(UNIT));
        r.pass(UNIT, &r.out(UNIT, "verdict.json"));
        let reason = park_reason(&r, UNIT);
        assert!(reason.contains(rule), "{base:?}: {reason}");
        assert!(forge_push_lines(&r).is_empty(), "{base:?}");
    }
}

#[test]
fn a_dir_whose_origin_is_not_the_runs_forge_is_refused_naming_both() {
    let r = Rig::with(&options(), |_| {});
    // Another bare repo, and a clone of it that holds the base branch.
    let other_bare = r.root.path().join("other.git");
    std::fs::create_dir_all(&other_bare).unwrap();
    git(
        &other_bare,
        &["init", "--quiet", "--bare", "--initial-branch=spine"],
    );
    let other = r.root.path().join("other");
    git(
        r.root.path(),
        &[
            "clone",
            "--quiet",
            other_bare.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    git(&other, &["config", "user.name", "Test"]);
    git(&other, &["config", "user.email", "test@example.invalid"]);
    git(&other, &["checkout", "--quiet", "-b", "base-x"]);
    std::fs::write(other.join("f"), "x\n").unwrap();
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "--quiet", "-m", "x"]);
    item(&r, UNIT, 1, "base-x", &other);

    through_verify(&r, UNIT);

    let reason = park_reason(&r, UNIT);
    assert!(
        reason.contains(other_bare.to_str().unwrap()) && reason.contains(r.forge.repo()),
        "both values are named: {reason}"
    );
    assert!(!close_started(&r, UNIT));
    assert_eq!(on_origin(&r, "base-x"), None);
    let in_other = Command::new("git")
        .arg("-C")
        .arg(&other_bare)
        .args(["rev-parse", "--verify", "--quiet", "refs/heads/base-x"])
        .output()
        .unwrap();
    assert!(!in_other.status.success(), "nothing reached the other repo");
    assert!(forge_push_lines(&r).is_empty());
}

#[test]
fn a_failed_push_writes_one_failed_line_and_parks_naming_the_cause() {
    // No local branch.
    let r = rig_for("no-such-branch");
    through_verify(&r, UNIT);
    let reason = park_reason(&r, UNIT);
    assert_eq!(
        reason,
        "base push of no-such-branch to origin failed: no local branch no-such-branch"
    );
    let lines = push_lines(&r, "no-such-branch");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("sha= outcome=failed: no local branch no-such-branch"),
        "{}",
        lines[0]
    );
    assert!(!close_started(&r, UNIT));

    // A forge hook that rejects the push.
    let r = rig_for("base-x");
    let sha = local_branch(&r.work, "base-x");
    std::fs::create_dir_all(r.bare.join("hooks")).unwrap();
    write_script(
        &r.bare.join("hooks/update"),
        "#!/bin/sh\necho 'rejected by the forge hook' >&2\nexit 1\n",
    );
    through_verify(&r, UNIT);
    let reason = park_reason(&r, UNIT);
    assert!(
        reason.starts_with("base push of base-x to origin failed: ")
            && reason.contains("rejected by the forge hook"),
        "{reason}"
    );
    let lines = push_lines(&r, "base-x");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains(&format!("sha={sha} outcome=failed: "))
            && lines[0].contains("rejected by the forge hook"),
        "{}",
        lines[0]
    );
    assert_eq!(on_origin(&r, "base-x"), None, "the hook refused it");
}

/// A repository with a commit on `base-x` and no `origin` remote.
fn lonely_repo(r: &Rig) -> std::path::PathBuf {
    let lonely = r.root.path().join("lonely");
    std::fs::create_dir_all(&lonely).unwrap();
    git(&lonely, &["init", "--quiet", "--initial-branch=base-x"]);
    git(&lonely, &["config", "user.name", "Test"]);
    git(&lonely, &["config", "user.email", "test@example.invalid"]);
    std::fs::write(lonely.join("f"), "x\n").unwrap();
    git(&lonely, &["add", "-A"]);
    git(&lonely, &["commit", "--quiet", "-m", "x"]);
    lonely
}

#[test]
fn an_unreadable_origin_is_refused_with_the_cause_and_pushes_nothing() {
    let r = Rig::with(&options(), |_| {});
    let lonely = lonely_repo(&r);
    item(&r, UNIT, 1, "base-x", &lonely);

    through_verify(&r, UNIT);

    let reason = park_reason(&r, UNIT);
    assert!(
        reason.starts_with(
            "base push of branch `base-x` refused: cannot read the fetch URL of origin of "
        ) && reason.contains("No such remote"),
        "{reason}"
    );
    assert!(!close_started(&r, UNIT));
    assert!(forge_push_lines(&r).is_empty());
    assert_eq!(on_origin(&r, "base-x"), None);
}

#[test]
fn a_directory_named_origin_does_not_stand_in_for_a_missing_origin_remote() {
    // With no remote named `origin`, git reads `origin` as a path: `ls-remote` and `push` would go
    // to this bare repository inside `dir`.
    let r = Rig::with(&options(), |_| {});
    let lonely = lonely_repo(&r);
    let decoy = lonely.join("origin");
    std::fs::create_dir_all(&decoy).unwrap();
    git(&decoy, &["init", "--quiet", "--bare"]);
    item(&r, UNIT, 1, "base-x", &lonely);

    through_verify(&r, UNIT);

    let reason = park_reason(&r, UNIT);
    assert!(
        reason.contains("refused: cannot read the fetch URL of origin of "),
        "{reason}"
    );
    assert!(!close_started(&r, UNIT));
    let in_decoy = Command::new("git")
        .arg("-C")
        .arg(&decoy)
        .args(["rev-parse", "--verify", "--quiet", "refs/heads/base-x"])
        .output()
        .unwrap();
    assert!(!in_decoy.status.success(), "nothing reached the directory");
    assert!(forge_push_lines(&r).is_empty());
}

#[test]
fn a_push_url_that_is_not_the_runs_forge_is_refused_and_that_repo_gets_no_ref() {
    // `remote.origin.pushurl` redirects `git push` and leaves the fetch URL (what `ls-remote`
    // reads) at the forge. The second case adds the redirect after the forge as a second push URL.
    for first_is_forge in [false, true] {
        let r = rig_for("base-x");
        local_branch(&r.work, "base-x");
        let other_bare = r.root.path().join("elsewhere.git");
        std::fs::create_dir_all(&other_bare).unwrap();
        git(
            &other_bare,
            &["init", "--quiet", "--bare", "--initial-branch=spine"],
        );
        let other = other_bare.to_str().unwrap();
        if first_is_forge {
            git(
                &r.work,
                &["config", "remote.origin.pushurl", r.bare.to_str().unwrap()],
            );
            git(
                &r.work,
                &["config", "--add", "remote.origin.pushurl", other],
            );
        } else {
            git(&r.work, &["config", "remote.origin.pushurl", other]);
        }

        through_verify(&r, UNIT);

        let reason = park_reason(&r, UNIT);
        assert!(
            reason.contains("pushes to") && reason.contains(other),
            "{first_is_forge}: {reason}"
        );
        assert!(!close_started(&r, UNIT), "{first_is_forge}");
        assert_eq!(on_origin(&r, "base-x"), None, "{first_is_forge}");
        let in_other = Command::new("git")
            .arg("-C")
            .arg(&other_bare)
            .args(["for-each-ref", "refs/heads"])
            .output()
            .unwrap();
        assert!(
            in_other.stdout.is_empty(),
            "{first_is_forge}: nothing reached the other repo"
        );
        assert!(forge_push_lines(&r).is_empty(), "{first_is_forge}");
    }
}

#[test]
fn a_halted_run_runs_no_check_and_pushes_nothing() {
    let r = rig_for("base-x");
    local_branch(&r.work, "base-x");
    r.tick();
    r.pass(UNIT, &r.report(UNIT));
    r.engine.halt("kill switch").unwrap();
    r.pass(UNIT, &r.out(UNIT, "verdict.json"));
    r.tick();
    r.tick();
    assert!(!close_started(&r, UNIT), "nothing launches under a halt");
    assert_eq!(on_origin(&r, "base-x"), None);
    assert!(forge_push_lines(&r).is_empty());

    // A check that ran would park this unit: its dir has no `origin`.
    let r = Rig::with(&options(), |_| {});
    let lonely = r.root.path().join("lonely");
    std::fs::create_dir_all(&lonely).unwrap();
    git(&lonely, &["init", "--quiet"]);
    item(&r, UNIT, 1, "base-x", &lonely);
    r.tick();
    r.pass(UNIT, &r.report(UNIT));
    r.engine.halt("kill switch").unwrap();
    r.pass(UNIT, &r.out(UNIT, "verdict.json"));
    r.tick();
    assert_eq!(r.unit(UNIT).0, Status::Running, "the check did not run");
    assert!(r.view().open_request_of(UNIT).is_none());
}

#[test]
fn a_repeat_of_the_event_id_is_a_replay_and_a_clear_gives_a_new_attempt_a_new_id() {
    let r = rig_for("base-x");
    // No local branch yet: the first attempt fails and parks the unit.
    through_verify(&r, UNIT);
    let req = r.view().open_request_of(UNIT).cloned().expect("a request");
    assert_eq!(push_lines(&r, "base-x").len(), 1);
    assert!(push_lines(&r, "base-x")[0].contains("outcome=failed"));

    // The same event ID again is a replay: no second line.
    let again = r
        .engine
        .record_push(
            UNIT,
            "base-push:u1:0:0",
            r.forge.repo(),
            "base-x",
            "",
            PushOutcome::Failed("no local branch base-x".to_string()),
        )
        .unwrap();
    assert!(again.replayed);
    assert_eq!(push_lines(&r, "base-x").len(), 1);

    // The operator fixes the cause and clears the unit: the attempt has a new generation.
    let sha = local_branch(&r.work, "base-x");
    let generation = r
        .engine
        .claim("ann@host", &sha256_hex(&[7u8; 32]))
        .unwrap()
        .generation;
    r.driver
        .clear_unit(UNIT, None, generation, &req.id)
        .unwrap();
    r.tick();
    assert!(close_started(&r, UNIT), "the cleared unit launches close");
    let lines = push_lines(&r, "base-x");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[1].contains(&format!("sha={sha} outcome=pushed")),
        "{}",
        lines[1]
    );
    assert_eq!(on_origin(&r, "base-x"), Some(sha));
    // Two more ticks: still two lines.
    r.tick();
    r.tick();
    assert_eq!(push_lines(&r, "base-x").len(), 2);
}

#[test]
fn a_second_real_push_in_one_park_generation_gets_its_own_ledger_line() {
    use pf_engine::render::RenderContext;
    use pf_workflow::file::BasePush;
    use pulse_factory::controller::base_push::{Forge, ensure};

    let r = rig_for("base-x");
    r.tick();
    assert!(r.has_unit(UNIT));
    let stage = r
        .package
        .file
        .tracks
        .iter()
        .flat_map(|t| &t.stages)
        .find(|s| s.name == "close")
        .expect("a close stage");
    let base = BasePush {
        dir: r.work.display().to_string(),
        branch: "base-x".to_string(),
    };
    let forge = Forge {
        repo: r.forge.repo(),
        default_branch: BASE,
        paper: true,
    };
    let run_dir = r.run_dir.display().to_string();
    let ctx = RenderContext {
        run_id: "r",
        run_dir: &run_dir,
        params: &r.params,
        unit: None,
    };
    let go = || ensure(&r.engine, &forge, stage, &base, UNIT, 0, &ctx);
    let lose = || {
        git(&r.bare, &["update-ref", "-d", "refs/heads/base-x"]);
        assert_eq!(on_origin(&r, "base-x"), None);
    };

    // One generation. The same SHA pushed twice: both pushes have a line.
    let first = local_branch(&r.work, "base-x");
    go().unwrap();
    assert_eq!(on_origin(&r, "base-x"), Some(first.clone()));
    lose();
    go().unwrap();
    assert_eq!(on_origin(&r, "base-x"), Some(first.clone()));
    assert_eq!(
        push_lines(&r, "base-x").len(),
        2,
        "{:?}",
        push_lines(&r, "base-x")
    );

    // Another SHA in the same generation: a third line, and no park.
    lose();
    std::fs::write(r.work.join("more.txt"), "more\n").unwrap();
    git(&r.work, &["add", "-A"]);
    git(&r.work, &["commit", "--quiet", "-m", "more"]);
    let second = git(&r.work, &["rev-parse", "HEAD"]);
    assert_ne!(first, second);
    go().unwrap();
    assert_eq!(on_origin(&r, "base-x"), Some(second.clone()));
    let lines = push_lines(&r, "base-x");
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(
        lines.iter().all(|l| l.contains("outcome=pushed")),
        "{lines:?}"
    );
    assert!(lines[2].contains(&format!("sha={second} ")), "{}", lines[2]);

    // A repeat of an attempt's own command ID is still a replay.
    let again = r
        .engine
        .record_push(
            UNIT,
            "base-push:u1:0:2",
            r.forge.repo(),
            "base-x",
            &second,
            PushOutcome::Pushed,
        )
        .unwrap();
    assert!(again.replayed);
    assert_eq!(push_lines(&r, "base-x").len(), 3);
}

#[test]
fn the_item_merges_on_the_seats_push_not_the_base_push() {
    let r = rig_for("base-x");
    local_branch(&r.work, "base-x");
    r.reach_close(UNIT);
    let base = r.engine.last_successful_push(UNIT).unwrap().unwrap();
    assert_eq!(base.branch, "base-x", "the base push is the newest so far");

    // The seat pushes its own branch and prints its PR: `head_check` reads the seat's push.
    r.close(UNIT, 1);
    let seat = r.engine.last_successful_push(UNIT).unwrap().unwrap();
    assert_eq!(seat.branch, "work/u1");
    assert_eq!(r.unit(UNIT).0, Status::Done, "{:?}", r.unit(UNIT));
    // The ledger holds the base push before the seat's push.
    let text = r.ledger_text();
    let base_at = text.find("branch=base-x ").expect("the base push line");
    let seat_at = text.rfind("branch=work/u1 ").expect("the seat push line");
    assert!(base_at < seat_at);
}

/// The fake adapter, with every launch plan and every rule set it was handed written down.
struct Spy {
    inner: Arc<FakeAdapter>,
    plans: Arc<Mutex<Vec<LaunchPlan>>>,
    rules: Arc<Mutex<Vec<(String, RuleSet)>>>,
}

impl AgentPort for Spy {
    fn launch(&self, p: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.plans.lock().unwrap().push(p.clone());
        self.inner.launch(p)
    }
    fn status(&self, s: &SessionRef) -> Result<SessionStatus, AgentError> {
        self.inner.status(s)
    }
    fn deliver(&self, s: &SessionRef, m: &str) -> Result<(), AgentError> {
        self.inner.deliver(s, m)
    }
    fn interrupt(&self, s: &SessionRef) -> Result<(), AgentError> {
        self.inner.interrupt(s)
    }
    fn usage(&self, s: &SessionRef) -> Result<Usage, AgentError> {
        self.inner.usage(s)
    }
    fn answer_dialog(&self, s: &SessionRef, d: &Dialog) -> Result<DialogAnswer, AgentError> {
        self.inner.answer_dialog(s, d)
    }
    fn apply_rules(&self, p: &mut LaunchPlan, r: &RuleSet) -> Result<(), AgentError> {
        self.rules
            .lock()
            .unwrap()
            .push((p.stage.clone(), r.clone()));
        self.inner.apply_rules(p, r)
    }
    fn remove_ask_tool(&self, p: &mut LaunchPlan) -> Result<(), AgentError> {
        self.inner.remove_ask_tool(p)
    }
    fn hand_off(&self, s: &SessionRef, p: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.inner.hand_off(s, p)
    }
    fn is_alive(&self, s: &SessionRef) -> Result<bool, AgentError> {
        self.inner.is_alive(s)
    }
    fn stop(&self, s: &SessionRef) -> Result<(), AgentError> {
        self.inner.stop(s)
    }
    fn resolve_plugin(&self, n: &str, v: &str) -> Result<ResolvedPlugin, AgentError> {
        self.inner.resolve_plugin(n, v)
    }
    fn drain_events(&self, s: &SessionRef) -> Result<Vec<SessionEvent>, AgentError> {
        self.inner.drain_events(s)
    }
}

/// The plan and the rule set the stage `close` launched with, on a rig built from `o`.
fn close_launch(o: &Options, with_base: bool) -> (LaunchPlan, RuleSet) {
    let plans = Arc::new(Mutex::new(Vec::new()));
    let rules = Arc::new(Mutex::new(Vec::new()));
    let (p, ru) = (plans.clone(), rules.clone());
    let r = Rig::build(
        o,
        |_| {},
        move |fake| {
            Arc::new(Spy {
                inner: fake,
                plans: p,
                rules: ru,
            }) as Arc<dyn AgentPort>
        },
    );
    let dir = r.work.clone();
    item(&r, UNIT, 1, "base-x", &dir);
    if with_base {
        local_branch(&r.work, "base-x");
    }
    r.reach_close(UNIT);
    let plan = plans
        .lock()
        .unwrap()
        .iter()
        .find(|p| p.stage == "close")
        .cloned()
        .expect("close launched");
    let rule = rules
        .lock()
        .unwrap()
        .iter()
        .find(|(stage, _)| stage == "close")
        .map(|(_, r)| r.clone())
        .expect("the close rules were applied");
    (plan, rule)
}

#[test]
fn the_close_seats_own_branch_and_rule_set_do_not_change() {
    let (with, with_rules) = close_launch(&options(), true);
    // The same package with no `base_push`, and no `before` that needs the branch on `origin`.
    let plain = Options {
        close_extra: String::new(),
        ..options()
    };
    let (without, without_rules) = close_launch(&plain, false);

    assert_eq!(with.settings["own_branch"], "work/u1");
    assert_eq!(with.settings["push_branch"], "work/u1");
    // The two rigs have their own temporary roots: compare the settings with the root cut off.
    let rooted = |plan: &LaunchPlan, key: &str| {
        let text = plan.settings.get(key).cloned();
        text.map(|t| {
            t.rsplit_once("/spine")
                .map_or(t.clone(), |(_, tail)| tail.to_string())
        })
    };
    for key in [
        "own_branch",
        "push_branch",
        "push_dir",
        "worktree",
        "report_dir",
    ] {
        assert_eq!(rooted(&with, key), rooted(&without, key), "{key}");
    }
    assert_eq!(with.role, without.role);
    assert_eq!(with_rules, without_rules, "the rule set is the same");
}
