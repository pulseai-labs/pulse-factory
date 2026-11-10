//! The spine-close rig of `spine_close.rs` and `device_requests_api.rs` (`r1.s4.w1`): a live controller on
//! a `FakeAdapter` and a `BareForge`, a run of the fixture spine with the `claim_ack` merge, and the
//! helpers that drive an item to its merge and the spine seat to its PR. Nothing here starts a seat
//! process, spends or reaches the network.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use pf_adapter_fake::{Scenario, Step};
use pf_engine::admission::PushOutcome;
use pf_engine::port::agent::{SessionRef, Usage};
use pf_engine::port::store::RequestKind;
use pf_engine::status::Status;
use pf_forge_bare::{BareForge, PrChecks};

use super::ctl::Live;
use super::{BIN, Options, git};

pub const SPINE_BRANCH: &str = "spine/s1";

pub fn cost(micros: u64) -> Step {
    Step::Usage(Usage {
        input_tokens: 10,
        cost_micros: micros,
        ..Usage::default()
    })
}

pub struct Spine {
    pub live: Live,
    pub work: PathBuf,
    pub forge: BareForge,
}

impl Spine {
    pub fn start() -> Spine {
        Spine::start_with(|_| {})
    }

    /// Start with `tweak` adjusting the controller's config (the API address, for one).
    pub fn start_with(
        tweak: impl FnOnce(&mut pulse_factory::controller::Config) + Send + 'static,
    ) -> Spine {
        let o = Options {
            spine_close: true,
            ..Options::default()
        };
        let live = Live::start_package(BIN.into(), &o, tweak);
        let work = live.sb.root.path().join("work");
        git(
            live.sb.root.path(),
            &[
                "clone",
                "--quiet",
                live.bare.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );
        git(&work, &["config", "user.name", "Test"]);
        git(&work, &["config", "user.email", "test@example.invalid"]);
        git(&work, &["checkout", "--quiet", "-b", "main"]);
        std::fs::write(work.join("README"), "start\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "--quiet", "-m", "start"]);
        git(&work, &["push", "--quiet", "origin", "main"]);
        git(
            &work,
            &["push", "--quiet", "origin", &format!("main:{SPINE_BRANCH}")],
        );
        let forge = BareForge::open(&live.bare).unwrap();

        let mut plan = live.plan();
        plan.params.insert("base_branch".into(), "main".into());
        live.controller.arm(plan).unwrap();
        Spine { live, work, forge }
    }

    pub fn tick(&self) {
        self.live.controller.tick_all();
    }

    pub fn run_dir(&self) -> PathBuf {
        self.live.run_dir()
    }

    pub fn out(&self, unit: &str, name: &str) -> PathBuf {
        self.run_dir().join("out").join(unit).join(name)
    }

    pub fn touch(&self, path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x\n").unwrap();
    }

    pub fn has_unit(&self, unit: &str) -> bool {
        self.live
            .handle()
            .engine
            .view()
            .unwrap()
            .node(unit)
            .is_some()
    }

    pub fn status_of(&self, unit: &str) -> Status {
        self.live
            .handle()
            .engine
            .view()
            .unwrap()
            .node(unit)
            .unwrap_or_else(|| panic!("no unit {unit}"))
            .status
    }

    pub fn session_of(&self, unit: &str) -> SessionRef {
        self.live
            .handle()
            .engine
            .view()
            .unwrap()
            .sessions
            .iter()
            .rfind(|s| s.unit_id == unit && !s.ended)
            .and_then(|s| s.process_ref.clone())
            .map(SessionRef)
            .unwrap_or_else(|| panic!("no open session for {unit}"))
    }

    /// Write the stage's done file, end the turn and tick.
    pub fn pass(&self, unit: &str, done: &Path) {
        self.touch(done);
        self.live.ports.fake.finish(&self.session_of(unit));
        self.tick();
    }

    pub fn report(&self, unit: &str) -> PathBuf {
        self.live
            .sb
            .spine_dir
            .join(format!("work-{unit}"))
            .join("report.md")
    }

    /// Commit on `branch` off `from` in the bare repo and write PR `number`. Returns the head.
    pub fn pr(&self, number: u64, branch: &str, from: &str, base: &str) -> String {
        git(&self.work, &["fetch", "--quiet", "origin"]);
        git(
            &self.work,
            &[
                "checkout",
                "--quiet",
                "-B",
                branch,
                &format!("origin/{from}"),
            ],
        );
        std::fs::write(
            self.work.join(format!("{branch}.txt").replace('/', "_")),
            "x\n",
        )
        .unwrap();
        git(&self.work, &["add", "-A"]);
        git(&self.work, &["commit", "--quiet", "-m", branch]);
        git(&self.work, &["push", "--quiet", "origin", branch]);
        self.forge
            .write_pr(number, branch, base, PrChecks::Green)
            .unwrap();
        self.head(branch)
    }

    pub fn head(&self, branch: &str) -> String {
        git(
            &self.live.bare,
            &["rev-parse", &format!("refs/heads/{branch}")],
        )
    }

    /// An item: a PR, a script that pushes its head and spends, and a queue record.
    pub fn item(&self, id: &str, order: u32, number: u64) {
        let branch = format!("work/{id}");
        let head = self.pr(number, &branch, SPINE_BRANCH, SPINE_BRANCH);
        self.live.ports.fake.script(
            id,
            Scenario::Script(vec![
                Step::Push {
                    branch,
                    sha: head,
                    outcome: PushOutcome::Pushed,
                },
                cost(100),
                Step::TurnEnd,
            ]),
        );
        self.live.queue_item(id, order);
    }

    /// Pass `implement` and `verify`, then print the PR and end `close`: the item merges.
    pub fn finish_item(&self, id: &str, number: u64) {
        self.tick();
        self.pass(id, &self.report(id));
        self.pass(id, &self.out(id, "verdict.json"));
        std::fs::create_dir_all(self.out(id, "")).unwrap();
        std::fs::write(
            self.out(id, "pr.url"),
            format!("file://{}#{number}\n", self.forge.repo()),
        )
        .unwrap();
        self.live.ports.fake.finish(&self.session_of(id));
        self.tick();
    }

    pub fn requests(&self, kind: RequestKind) -> Vec<pf_engine::port::store::RequestRow> {
        self.live
            .handle()
            .engine
            .view()
            .unwrap()
            .requests
            .into_iter()
            .filter(|r| r.kind == kind)
            .collect()
    }

    pub fn ledger(&self) -> String {
        self.live.sb.read_ledger()
    }

    pub fn token(&self) -> String {
        std::fs::read_to_string(self.live.sb.state.join("claim-token"))
            .unwrap()
            .trim()
            .to_string()
    }
}
