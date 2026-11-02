//! Seed units (Approach 3).
//!
//! A seed is a unit the workflow file declares. It is admitted at the start of a run with no
//! queue record. A `repeat` re-admits it under a new ID, `<id>.2`, `<id>.3`, when the units it
//! queued are finished. The decision is a pure function of the run's stored state; the
//! controller calls it.
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

use crate::file::SeedDef;

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

/// Decide the next step of one seed from the stored state of the run.
///
/// `until_file_exists` is whether the rendered `until_file` exists now.
pub fn decide(seed: &SeedDef, view: &RunView, until_file_exists: bool) -> SeedStep {
    let mut generations: Vec<(u32, &NodeRow)> = view
        .units()
        .filter_map(|n| generation_of(&seed.id, &n.id).map(|g| (g, n)))
        .collect();
    generations.sort_by_key(|(g, _)| *g);
    let Some(&(generation, previous)) = generations.last() else {
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

/// Admit the first unit of each seed. The same call again is a replay.
pub fn start_seeds(engine: &Engine, seeds: &[SeedDef]) -> Result<Vec<Admission>, EngineError> {
    seeds
        .iter()
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
    let view = engine.view()?;
    match decide(seed, &view, until_file_exists) {
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
