//! AC-3: the item PR reviewer seat. After `close` opens the PR, a `review` stage writes a verdict.
//! A pass on the PR head merges the item. A block sends the unit to `fix` and back to `review`.
//! A pass whose `head:` is not the PR's head reviews again. A third block parks the item.
//!
//! The driver runs on a `FakeAdapter` and a bare forge, as in `clear_resume.rs`. The test plays the
//! reviewer and the fixer: it writes `review.md` or `fix-done` and ends the session. The two
//! scripts the stages call (`review-prep.sh`, `stale-pass.sh`) are the shipped ones.

mod common;

use std::sync::{Arc, Mutex};

use common::rig::*;
use common::*;
use pf_adapter_fake::{FakeAdapter, Step};
use pf_engine::admission::PushOutcome;
use pf_engine::claim::sha256_hex;
use pf_engine::port::agent::*;
use pf_engine::port::store::{RequestKind, RequestRow, RequestStatus};
use pf_engine::status::Status;
use pf_forge_bare::PrChecks;

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

struct Seat {
    r: Rig,
    plans: Arc<Mutex<Vec<LaunchPlan>>>,
    rules: Arc<Mutex<Vec<(String, RuleSet)>>>,
}

const UNIT: &str = "u1";
const BRANCH: &str = "work/u1";

/// A rig whose item track is `implement, verify, close, review, fix`.
fn seat() -> Seat {
    let plans = Arc::new(Mutex::new(Vec::new()));
    let rules = Arc::new(Mutex::new(Vec::new()));
    let o = Options {
        review_loop: true,
        scripts_from: Some(real_scripts_dir()),
        ..Options::default()
    };
    let (p, ru) = (plans.clone(), rules.clone());
    let r = Rig::build(
        &o,
        |_| {},
        move |fake| {
            Arc::new(Spy {
                inner: fake,
                plans: p,
                rules: ru,
            }) as Arc<dyn AgentPort>
        },
    );
    Seat { r, plans, rules }
}

/// Run `u1` to an open `review` session. `close` printed the PR. Returns the PR head.
fn to_review(s: &Seat) -> String {
    let r = &s.r;
    let head = r.item(UNIT, 1, 1, PrChecks::Green);
    r.reach_close(UNIT);
    // The review seat pushes nothing: its script only ends the turn.
    r.script(UNIT, vec![Step::TurnEnd]);
    r.close(UNIT, 1);
    assert_eq!(r.unit(UNIT).0, Status::Running, "{}", r.ledger_text());
    assert_eq!(review_launches(r), 1, "{}", r.ledger_text());
    head
}

fn review_launches(r: &Rig) -> usize {
    r.stage_node(UNIT, "review").len()
}

fn fix_launches(r: &Rig) -> usize {
    r.stage_node(UNIT, "fix").len()
}

/// Make a new commit on the PR branch, as a fixer would. Returns the new head.
fn advance_pr(r: &Rig) -> String {
    git(&r.work, &["fetch", "--quiet", "origin"]);
    git(
        &r.work,
        &[
            "checkout",
            "--quiet",
            "-B",
            BRANCH,
            &format!("origin/{BRANCH}"),
        ],
    );
    let n = git(
        &r.bare,
        &["rev-list", "--count", &format!("refs/heads/{BRANCH}")],
    );
    std::fs::write(r.work.join(format!("fix-{n}.txt")), "fixed\n").unwrap();
    git(&r.work, &["add", "-A"]);
    git(&r.work, &["commit", "--quiet", "-m", &format!("fix {n}")]);
    git(&r.work, &["push", "--quiet", "origin", BRANCH]);
    git(&r.bare, &["rev-parse", &format!("refs/heads/{BRANCH}")])
}

/// Make the commit the next `fix` session will push, and script that session: it pushes the
/// commit and ends the turn. Call it before the tick that launches `fix`. Returns the new head.
fn prepare_fix(r: &Rig) -> String {
    let sha = advance_pr(r);
    r.script(
        UNIT,
        vec![
            Step::Push {
                branch: BRANCH.to_string(),
                sha: sha.clone(),
                outcome: PushOutcome::Pushed,
            },
            Step::TurnEnd,
        ],
    );
    sha
}

fn review_text(verdict: &str, head: &str) -> String {
    format!("verdict: {verdict}\nhead: {head}\n\n- a finding about {head}\n")
}

/// The open review session ends with this text in `review.md`.
fn end_review(r: &Rig, text: &str) {
    std::fs::write(r.out(UNIT, "review.md"), text).unwrap();
    r.fake
        .finish(&r.session_ref(UNIT).expect("an open review session"));
    r.tick();
}

/// The open fix session ends, with `fix-done` written. The next launch is a review that pushes
/// nothing.
fn end_fix(r: &Rig) {
    r.script(UNIT, vec![Step::TurnEnd]);
    r.touch(&r.out(UNIT, "fix-done"));
    r.fake
        .finish(&r.session_ref(UNIT).expect("an open fix session"));
    r.tick();
}

fn claim(r: &Rig) -> u64 {
    r.engine
        .claim("ann@host", &sha256_hex(&[7u8; 32]))
        .unwrap()
        .generation
}

fn open_request(r: &Rig) -> RequestRow {
    r.view()
        .open_request_of(UNIT)
        .cloned()
        .unwrap_or_else(|| panic!("no open request: {}", r.ledger_text()))
}

fn assert_merged(r: &Rig) {
    assert_eq!(r.unit(UNIT).0, Status::Done, "{}", r.ledger_text());
    assert!(
        r.ledger_text().contains("merge of unit u1"),
        "{}",
        r.ledger_text()
    );
    assert!(
        std::fs::read_to_string(r.run_dir.join("after_merge.log"))
            .unwrap()
            .contains(UNIT)
    );
}

// ------------------------------------------------------------------ pass and merge

#[test]
fn a_pass_on_the_pr_head_merges_the_item() {
    let s = seat();
    let head = to_review(&s);
    let r = &s.r;
    // The unit stands at `review`: close is done, and the PR does not merge before the review.
    assert_eq!(r.unit(UNIT).0, Status::Running);
    assert!(!r.run_dir.join("after_merge.log").exists());

    end_review(r, &review_text("pass", &head));
    r.tick();
    assert_merged(r);
    assert_eq!(fix_launches(r), 0, "a passing review skips `fix`");
    assert_eq!(review_launches(r), 1);
    assert!(
        r.ledger_lines("sends the unit to").is_empty(),
        "{:?}",
        r.ledger_lines("sends the unit to")
    );
    // The merge stayed pinned to the head the close seat pushed.
    let pushed = r.engine.last_successful_push(UNIT).unwrap().unwrap();
    assert_eq!(pushed.sha, head);
}

// ------------------------------------------------------------------ block, fix, pass

#[test]
fn a_block_runs_fix_and_a_pass_on_the_fixed_head_merges_with_the_fix_push_recorded() {
    let s = seat();
    let h0 = to_review(&s);
    let r = &s.r;

    let h1 = prepare_fix(r);
    assert_ne!(h0, h1);
    end_review(r, &review_text("block", &h0));
    assert_eq!(fix_launches(r), 1, "{}", r.ledger_text());
    assert_eq!(r.unit(UNIT).0, Status::Running);
    assert!(
        !r.run_dir.join("after_merge.log").exists(),
        "a block does not merge"
    );

    end_fix(r);
    // The fix's push is recorded, and it is the new head of the PR.
    let pushed = r.engine.last_successful_push(UNIT).unwrap().unwrap();
    assert_eq!(pushed.sha, h1);
    assert_eq!(pushed.branch, BRANCH);
    assert_eq!(review_launches(r), 2, "{}", r.ledger_text());

    end_review(r, &review_text("pass", &h1));
    r.tick();
    assert_merged(r);
    assert_eq!(fix_launches(r), 1, "one fix round");

    let sends = r.ledger_lines("sends the unit to");
    assert_eq!(sends.len(), 2, "{sends:?}");
    assert!(
        sends[0].contains("outcome 1 sends the unit to fix (visit 1 of 2)"),
        "{}",
        sends[0]
    );
    assert!(
        sends[1].contains("outcome 1 sends the unit to review (visit 1 of 2)"),
        "{}",
        sends[1]
    );
}

// ------------------------------------------------------------------ stale pass

#[test]
fn a_pass_whose_head_is_not_the_pr_head_reviews_again() {
    let s = seat();
    let h0 = to_review(&s);
    let r = &s.r;
    // The reviewer read an older commit: the base branch tip.
    let old = git(&r.bare, &["rev-parse", "refs/heads/spine"]);
    assert_ne!(old, h0);

    end_review(r, &review_text("pass", &old));
    assert_eq!(
        r.unit(UNIT).0,
        Status::Running,
        "a stale pass does not merge"
    );
    assert!(!r.run_dir.join("after_merge.log").exists());
    assert_eq!(review_launches(r), 2, "{}", r.ledger_text());
    assert_eq!(fix_launches(r), 0);
    let sends = r.ledger_lines("sends the unit to");
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert!(
        sends[0].contains("outcome 2 sends the unit to review (visit 1 of 2)"),
        "{}",
        sends[0]
    );

    end_review(r, &review_text("pass", &h0));
    r.tick();
    assert_merged(r);
    assert_eq!(review_launches(r), 2);
}

#[test]
fn a_pass_that_names_no_head_is_a_stale_pass() {
    let s = seat();
    to_review(&s);
    let r = &s.r;
    end_review(r, "verdict: pass\n\nlooks fine\n");
    assert_eq!(review_launches(r), 2, "{}", r.ledger_text());
    assert_eq!(r.unit(UNIT).0, Status::Running);
}

#[test]
fn three_stale_passes_park_the_item_with_the_review_cap() {
    let s = seat();
    to_review(&s);
    let r = &s.r;
    let old = git(&r.bare, &["rev-parse", "refs/heads/spine"]);
    for round in 1..=2 {
        end_review(r, &review_text("pass", &old));
        assert_eq!(review_launches(r), 1 + round, "{}", r.ledger_text());
    }
    end_review(r, &review_text("pass", &old));
    let (status, reason) = r.unit(UNIT);
    assert_eq!(status, Status::Parked, "{}", r.ledger_text());
    assert_eq!(reason.as_deref(), Some("review: review cap 2 reached"));
    assert_eq!(open_request(r).kind, RequestKind::Park);
    assert!(!r.run_dir.join("after_merge.log").exists());
}

// ------------------------------------------------------------------ line 1 is the verdict

#[test]
fn a_pass_that_quotes_a_block_on_a_later_line_merges_with_no_fix() {
    let s = seat();
    let head = to_review(&s);
    let r = &s.r;
    // Line 1 is the verdict. The findings quote `verdict: block` further down.
    let text = format!(
        "verdict: pass\nhead: {head}\n\n- the old review said:\nverdict: block\n- it is fixed\n"
    );
    end_review(r, &text);
    r.tick();
    assert_merged(r);
    assert_eq!(fix_launches(r), 0, "a quoted block does not start `fix`");
    assert_eq!(review_launches(r), 1, "{}", r.ledger_text());
    assert!(
        r.ledger_lines("sends the unit to").is_empty(),
        "{:?}",
        r.ledger_lines("sends the unit to")
    );
}

#[test]
fn a_block_that_quotes_a_pass_on_a_later_line_still_runs_fix() {
    let s = seat();
    let h0 = to_review(&s);
    let r = &s.r;
    let h1 = prepare_fix(r);
    let text = format!("verdict: block\nhead: {h0}\n\n- the last review said:\nverdict: pass\n");
    end_review(r, &text);
    assert_eq!(fix_launches(r), 1, "{}", r.ledger_text());
    assert!(!r.run_dir.join("after_merge.log").exists());
    end_fix(r);
    assert_eq!(review_launches(r), 2, "{}", r.ledger_text());
    end_review(r, &review_text("pass", &h1));
    r.tick();
    assert_merged(r);
}

#[test]
fn a_malformed_line_one_over_a_later_pass_does_not_end_the_stage() {
    let s = seat();
    let h0 = to_review(&s);
    let r = &s.r;
    // The file holds `verdict: pass` and the right head, but line 1 is not the verdict.
    let text = format!("# Review of {h0}\nverdict: pass\nhead: {h0}\n");
    end_review(r, &text);
    r.tick();
    assert_ne!(
        r.unit(UNIT).0,
        Status::Done,
        "a later verdict line must not end the stage: {}",
        r.ledger_text()
    );
    // The stage is not done, so the ended seat is a seat with no done signal: the unit parks.
    let (status, reason) = r.unit(UNIT);
    assert_eq!(status, Status::Parked, "{}", r.ledger_text());
    assert_eq!(reason.as_deref(), Some("seat ended with no done signal"));
    assert!(
        !r.run_dir.join("after_merge.log").exists(),
        "the item does not merge"
    );
    assert_eq!(fix_launches(r), 0, "no outcome fired");
    assert_eq!(review_launches(r), 1, "{}", r.ledger_text());
    assert!(
        r.ledger_lines("sends the unit to").is_empty(),
        "{:?}",
        r.ledger_lines("sends the unit to")
    );
}

// ------------------------------------------------------------------ the cap and the clear

/// Block three reviews in a row. The third block parks the unit. Returns the PR head and the
/// request.
fn three_blocks(s: &Seat) -> (String, RequestRow) {
    let r = &s.r;
    let mut head = to_review(s);
    for round in 1..=2 {
        let next = prepare_fix(r);
        end_review(r, &review_text("block", &head));
        assert_eq!(fix_launches(r), round, "{}", r.ledger_text());
        end_fix(r);
        assert_eq!(review_launches(r), 1 + round, "{}", r.ledger_text());
        head = next;
    }
    end_review(r, &review_text("block", &head));
    (head, open_request(r))
}

#[test]
fn three_blocks_park_the_item_with_a_request_naming_the_cap() {
    let s = seat();
    let (_, request) = three_blocks(&s);
    let r = &s.r;
    let (status, reason) = r.unit(UNIT);
    assert_eq!(status, Status::Parked, "{}", r.ledger_text());
    assert_eq!(reason.as_deref(), Some("review: fix cap 2 reached"));
    assert_eq!(request.kind, RequestKind::Park);
    assert_eq!(request.reason, "review: fix cap 2 reached");
    assert_eq!(request.status, RequestStatus::Open);
    assert_eq!(fix_launches(r), 2, "no third fix launched");
    assert_eq!(review_launches(r), 3);
    assert!(!r.run_dir.join("after_merge.log").exists());
    assert!(
        r.ledger_text().contains("review: fix cap 2 reached"),
        "{}",
        r.ledger_text()
    );
}

#[test]
fn a_clear_to_fix_resumes_the_parked_item_with_new_counters() {
    let s = seat();
    let (_, request) = three_blocks(&s);
    let r = &s.r;

    // The operator clears the park to `fix`: a new fix round starts. The fixer pushes a commit.
    let h3 = prepare_fix(r);
    let generation = claim(r);
    r.driver
        .clear_unit(UNIT, Some("fix"), generation, &request.id)
        .unwrap();
    assert_eq!(r.unit(UNIT).0, Status::Running);
    {
        let view = r.view();
        let detail = view.unit_detail(view.node(UNIT).unwrap()).unwrap();
        assert!(
            detail.visits.is_empty(),
            "a clear resets every counter: {:?}",
            detail.visits
        );
    }
    r.tick();
    assert_eq!(fix_launches(r), 3, "{}", r.ledger_text());
    end_fix(r);
    assert_eq!(review_launches(r), 4, "{}", r.ledger_text());

    // The counters are new: this block is visit 1 of 2, not visit 3.
    let before = r.ledger_lines("sends the unit to").len();
    let h4 = prepare_fix(r);
    end_review(r, &review_text("block", &h3));
    let sends = r.ledger_lines("sends the unit to");
    assert_eq!(sends.len(), before + 1, "{sends:?}");
    assert!(
        sends
            .last()
            .unwrap()
            .contains("sends the unit to fix (visit 1 of 2)"),
        "{}",
        sends.last().unwrap()
    );
    end_fix(r);
    end_review(r, &review_text("pass", &h4));
    r.tick();
    assert_merged(r);
}

// ------------------------------------------------------------------ review-prev.md

#[test]
fn review_prev_holds_the_last_review_on_each_later_round() {
    let s = seat();
    let h0 = to_review(&s);
    let r = &s.r;
    // Review 1 has no previous review.
    assert!(!r.out(UNIT, "review-prev.md").exists());

    let first = review_text("block", &h0);
    let h1 = prepare_fix(r);
    end_review(r, &first);
    end_fix(r);
    // Review 2: the last review is the previous one, and `review.md` is gone until it is written.
    assert_eq!(
        std::fs::read_to_string(r.out(UNIT, "review-prev.md")).unwrap(),
        first
    );
    assert!(!r.out(UNIT, "review.md").exists());

    let second = review_text("block", &h1);
    let h2 = prepare_fix(r);
    end_review(r, &second);
    end_fix(r);
    // Review 3: the second review replaced the first.
    assert_eq!(
        std::fs::read_to_string(r.out(UNIT, "review-prev.md")).unwrap(),
        second
    );

    end_review(r, &review_text("pass", &h2));
    r.tick();
    assert_merged(r);
}

#[test]
fn a_malformed_review_does_not_replace_the_previous_one() {
    let s = seat();
    let h0 = to_review(&s);
    let r = &s.r;
    let first = review_text("block", &h0);
    let h1 = prepare_fix(r);
    end_review(r, &first);
    end_fix(r);
    // Review 2 ends with a file that has no verdict line: the stage is not done, so the unit
    // stays in `review`. A relaunch (here a clear to `review`) must keep the last good review.
    std::fs::write(r.out(UNIT, "review.md"), "no verdict here\n").unwrap();
    r.fake.finish(&r.session_ref(UNIT).unwrap());
    r.tick();
    assert_eq!(review_launches(r), 2, "{}", r.ledger_text());
    assert_eq!(
        std::fs::read_to_string(r.out(UNIT, "review-prev.md")).unwrap(),
        first
    );
    // Run the prep step by hand, as the next launch does.
    let prep = r.package_dir.join("scripts/review-prep.sh");
    let status = std::process::Command::new("sh")
        .arg(&prep)
        .arg(r.out(UNIT, "review.md"))
        .arg(r.out(UNIT, "review-prev.md"))
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(r.out(UNIT, "review-prev.md")).unwrap(),
        first,
        "a review with no verdict line never overwrites review-prev.md"
    );
    assert!(!r.out(UNIT, "review.md").exists());
    let _ = h1;
}

// ------------------------------------------------------------------ the launch plans

#[test]
fn the_review_seat_launches_with_no_worktree_and_the_reviewer_rules() {
    let s = seat();
    let h0 = to_review(&s);
    let r = &s.r;
    let h1 = prepare_fix(r);
    end_review(r, &review_text("block", &h0));
    end_fix(r);
    end_review(r, &review_text("pass", &h1));
    r.tick();
    assert_merged(r);

    let plans = s.plans.lock().unwrap().clone();
    let wt = r.spine_dir.join("wt-u1").display().to_string();
    let reviews: Vec<&LaunchPlan> = plans.iter().filter(|p| p.stage == "review").collect();
    assert_eq!(reviews.len(), 2);
    for p in &reviews {
        assert_eq!(p.role, "reviewer");
        assert_eq!(p.model, "model-e");
        // No worktree setting, so a write inside the worktree is not allowed.
        assert!(
            p.settings.get("worktree").is_none_or(String::is_empty),
            "{:?}",
            p.settings.get("worktree")
        );
        // The worktree is the working directory, for reading.
        assert_eq!(p.settings["cwd"], wt);
        assert_eq!(
            p.settings["report_dir"],
            r.run_dir.join("out").join(UNIT).display().to_string()
        );
        let message = &p.settings["message"];
        assert!(
            message.starts_with(&format!("/review {} {BRANCH} previous: ", r.forge.repo())),
            "{message}"
        );
        assert!(
            message.ends_with(&format!(
                "{}/out/{UNIT}/review-prev.md",
                r.run_dir.display()
            )),
            "{message}"
        );
        assert!(!p.settings.contains_key("push_dir"));
        assert!(!p.settings.contains_key("push_branch"));
    }

    // The rule set each stage got: the review stage got `reviewer`, the fix stage got `close`.
    let rules = s.rules.lock().unwrap().clone();
    let of = |stage: &str| -> Vec<&RuleSet> {
        rules
            .iter()
            .filter(|(st, _)| st == stage)
            .map(|(_, r)| r)
            .collect()
    };
    let review_rules = of("review");
    assert_eq!(review_rules.len(), 2);
    assert!(
        review_rules
            .iter()
            .all(|r| r.name == "reviewer" && r.prompts_off)
    );
    assert!(of("fix").iter().all(|r| r.name == "close"));

    // The fix seat works in the item's worktree and may push its branch, as `close` does.
    let fixes: Vec<&LaunchPlan> = plans.iter().filter(|p| p.stage == "fix").collect();
    assert_eq!(fixes.len(), 1);
    assert_eq!(fixes[0].role, "close");
    assert_eq!(fixes[0].settings["worktree"], wt);
    assert_eq!(fixes[0].settings["push_dir"], wt);
    assert_eq!(fixes[0].settings["push_branch"], BRANCH);
}

// ------------------------------------------------------------------ a halted run

#[test]
fn a_halted_run_takes_no_outcome_launch() {
    let s = seat();
    let h0 = to_review(&s);
    let r = &s.r;
    prepare_fix(r);
    r.engine.halt("operator stop").unwrap();
    end_review(r, &review_text("block", &h0));
    assert_eq!(fix_launches(r), 0, "{}", r.ledger_text());
}
