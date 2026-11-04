//! Seed units (Approach 3).
//!
//! A seed is a unit the workflow file declares. It is admitted at the start of a run with no
//! queue record. A `repeat` re-admits it under a new ID, `<id>.2`, `<id>.3`, when the units it
//! queued are finished. The decision is a pure function of the run's stored state; the
//! controller calls it.
//!
//! A seed with `start_when` is not admitted at the start of a run. It is admitted once, on the first
//! tick at which its file exists and every unit of its track is done (and at least one such unit
//! exists). It never repeats, and a halted run admits nothing of it.
//!
//! A seed unit that ended `done` cannot park. So a seed that added no unit and left no
//! `until_file`, or one that reached its limit with no `until_file`, returns a halt reason, and
//! the controller halts the run through [`pf_engine::Engine::halt_for`].

use pf_engine::Engine;
use pf_engine::admission::{Admission, EngineError, RunView};
use pf_engine::port::store::NodeRow;
use pf_engine::port::unit_source::UnitRecord;
use pf_engine::status::Status;
use pf_engine::unit::UnitDetail;

use crate::file::{SeedDef, StartWhen};

/// The ID of a seed's unit: the seed ID, then `<id>.2`, `<id>.3`.
pub fn seed_unit_id(seed_id: &str, generation: u32) -> String {
    if generation <= 1 {
        seed_id.to_string()
    } else {
        format!("{seed_id}.{generation}")
    }
}

/// The generation that a unit ID stands for, when it is one of this seed's units.
fn generation_of(seed_id: &str, unit_id: &str) -> Option<u32> {
    if unit_id == seed_id {
        return Some(1);
    }
    let rest = unit_id.strip_prefix(seed_id)?.strip_prefix('.')?;
    // Only a whole number of 2 or more counts: `lane.x` and `lane.1` are other units.
    let n: u32 = rest.parse().ok()?;
    (n >= 2 && n.to_string() == rest).then_some(n)
}

/// The unit record of one generation of a seed.
pub fn seed_record(seed: &SeedDef, generation: u32) -> UnitRecord {
    UnitRecord::new(
        seed_unit_id(&seed.id, generation),
        seed.group.clone(),
        seed.order,
        seed.vars.clone(),
    )
}

/// What the controller does next for one seed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeedStep {
    /// Admit this unit onto this track.
    Admit { record: UnitRecord, track: String },
    /// Nothing yet: the seed runs, or its work runs, or the seed is finished.
    Wait,
    /// Halt the run with this reason through `halt_for`.
    Halt(String),
}

/// Does the seed's `start_when` hold? Its file exists, and the track it names has at least one
/// unit, every one of them done.
fn start_holds(start_when: &StartWhen, view: &RunView, start_file_exists: bool) -> bool {
    if !start_file_exists {
        return false;
    }
    let on_track: Vec<&NodeRow> = view
        .units()
        .filter(|n| {
            UnitDetail::from_text(&n.detail).is_ok_and(|d| d.track == start_when.units_done)
        })
        .collect();
    !on_track.is_empty() && on_track.iter().all(|n| n.status == Status::Done)
}

/// Decide the next step of one seed from the stored state of the run.
///
/// `until_file_exists` is whether the rendered `until_file` exists now.
pub fn decide(seed: &SeedDef, view: &RunView, until_file_exists: bool) -> SeedStep {
    decide_started(seed, view, until_file_exists, true)
}

/// [`decide`] for a seed that may carry `start_when`. `start_file_exists` is whether the rendered
/// `start_when.file` exists now; a seed with no `start_when` ignores it.
pub fn decide_started(
    seed: &SeedDef,
    view: &RunView,
    until_file_exists: bool,
    start_file_exists: bool,
) -> SeedStep {
    let mut generations: Vec<(u32, &NodeRow)> = view
        .units()
        .filter_map(|n| generation_of(&seed.id, &n.id).map(|g| (g, n)))
        .collect();
    generations.sort_by_key(|(g, _)| *g);
    let Some(&(generation, previous)) = generations.last() else {
        if let Some(start_when) = &seed.start_when
            && (view.is_halted() || !start_holds(start_when, view, start_file_exists))
        {
            return SeedStep::Wait;
        }
        return SeedStep::Admit {
            record: seed_record(seed, 1),
            track: seed.track.clone(),
        };
    };
    let Some(repeat) = &seed.repeat else {
        return SeedStep::Wait;
    };
    if previous.status != Status::Done || until_file_exists {
        return SeedStep::Wait;
    }
    let Ok(previous_detail) = UnitDetail::from_text(&previous.detail) else {
        return SeedStep::Wait;
    };

    let on_track: Vec<(&NodeRow, UnitDetail)> = view
        .units()
        .filter_map(|n| UnitDetail::from_text(&n.detail).ok().map(|d| (n, d)))
        .filter(|(_, d)| d.track == repeat.when_units_done)
        .collect();
    let added_after = on_track
        .iter()
        .any(|(_, d)| d.ingest_seq > previous_detail.ingest_seq);
    if !added_after {
        return SeedStep::Halt(format!("seed {} added no units", seed.id));
    }
    if on_track.iter().any(|(n, _)| n.status != Status::Done) {
        return SeedStep::Wait;
    }
    if generation >= repeat.max {
        return SeedStep::Halt(format!("seed {} reached its limit", seed.id));
    }
    SeedStep::Admit {
        record: seed_record(seed, generation + 1),
        track: seed.track.clone(),
    }
}

/// Admit the first unit of each seed. The same call again is a replay. A seed with `start_when`
/// starts later, through [`advance_seed_started`].
pub fn start_seeds(engine: &Engine, seeds: &[SeedDef]) -> Result<Vec<Admission>, EngineError> {
    seeds
        .iter()
        .filter(|s| s.start_when.is_none())
        .map(|s| engine.admit_unit(seed_record(s, 1), &s.track))
        .collect()
}

/// What `advance_seed` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeedAction {
    Idle,
    /// A seed unit was admitted.
    Admitted(String),
    /// The run was halted with this reason.
    Halted(String),
}

/// Decide one seed's next step and carry it out.
pub fn advance_seed(
    engine: &Engine,
    seed: &SeedDef,
    until_file_exists: bool,
) -> Result<SeedAction, EngineError> {
    advance_seed_started(engine, seed, until_file_exists, true)
}

/// [`advance_seed`] for a seed that may carry `start_when`: `start_file_exists` is whether the
/// rendered `start_when.file` exists now.
pub fn advance_seed_started(
    engine: &Engine,
    seed: &SeedDef,
    until_file_exists: bool,
    start_file_exists: bool,
) -> Result<SeedAction, EngineError> {
    let view = engine.view()?;
    match decide_started(seed, &view, until_file_exists, start_file_exists) {
        SeedStep::Wait => Ok(SeedAction::Idle),
        SeedStep::Admit { record, track } => {
            let id = record.id.clone();
            let admission = engine.admit_unit(record, &track)?;
            Ok(if admission.replayed {
                SeedAction::Idle
            } else {
                SeedAction::Admitted(id)
            })
        }
        SeedStep::Halt(reason) => {
            engine.halt_for(&format!("seed:{}", seed.id), &reason)?;
            Ok(SeedAction::Halted(reason))
        }
    }
}
