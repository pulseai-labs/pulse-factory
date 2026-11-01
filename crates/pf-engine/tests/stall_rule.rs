//! AC-5: the stall rule of ADR-0006. No new turn, file, commit or PR within the limit is a
//! stall. A stall gets one relaunch, then a park.

use pf_engine::unit::{StallDecision, stall_decision};
use proptest::prelude::*;

const LIMIT: u64 = 20 * 60;
const NOW: u64 = 100_000;

fn d(
    turn: Option<u64>,
    file: Option<u64>,
    commit: Option<u64>,
    pr: Option<u64>,
    prior: u32,
) -> StallDecision {
    stall_decision(turn, file, commit, pr, prior, LIMIT, NOW)
}

#[test]
fn each_signal_alone_inside_the_limit_keeps_the_seat_active() {
    let recent = Some(NOW - LIMIT + 1);
    let old = Some(NOW - 10 * LIMIT);
    assert_eq!(
        d(recent, old, old, old, 0),
        StallDecision::Active,
        "a new turn"
    );
    assert_eq!(
        d(old, recent, old, old, 0),
        StallDecision::Active,
        "a new file"
    );
    assert_eq!(
        d(old, old, recent, old, 0),
        StallDecision::Active,
        "a commit"
    );
    assert_eq!(
        d(old, old, old, recent, 0),
        StallDecision::Active,
        "a PR change"
    );
    // A long tool call that writes files is active even when no turn ended.
    assert_eq!(d(old, recent, None, None, 1), StallDecision::Active);
}

#[test]
fn read_only_tool_calls_with_no_new_turn_are_not_a_signal() {
    // Reads change no file, commit or PR, and they start no turn: the controller passes the old
    // times, so the seat is stalled.
    let old = Some(NOW - 2 * LIMIT);
    assert_eq!(d(old, old, old, old, 0), StallDecision::Relaunch);
}

#[test]
fn none_of_them_past_the_limit_is_a_relaunch_then_a_park() {
    let old = Some(NOW - LIMIT - 1);
    assert_eq!(d(old, old, old, old, 0), StallDecision::Relaunch);
    assert_eq!(d(old, old, old, old, 1), StallDecision::Park);
    assert_eq!(d(old, old, old, old, 7), StallDecision::Park);
}

#[test]
fn the_limit_itself_is_inside_and_one_second_more_is_past() {
    assert_eq!(
        d(Some(NOW - LIMIT), None, None, None, 0),
        StallDecision::Active
    );
    assert_eq!(
        d(Some(NOW - LIMIT - 1), None, None, None, 0),
        StallDecision::Relaunch
    );
}

#[test]
fn a_seat_with_no_signal_at_all_is_stalled_and_a_future_time_is_active() {
    assert_eq!(d(None, None, None, None, 0), StallDecision::Relaunch);
    assert_eq!(d(None, None, None, None, 1), StallDecision::Park);
    assert_eq!(
        d(Some(NOW + 50), None, None, None, 1),
        StallDecision::Active
    );
}

proptest! {
    #[test]
    fn only_the_latest_signal_and_the_prior_count_matter(
        a in proptest::option::of(0u64..200_000),
        b in proptest::option::of(0u64..200_000),
        c in proptest::option::of(0u64..200_000),
        e in proptest::option::of(0u64..200_000),
        prior in 0u32..4,
    ) {
        let latest = [a, b, c, e].into_iter().flatten().max();
        let expected = match latest {
            Some(t) if NOW.saturating_sub(t) <= LIMIT => StallDecision::Active,
            _ if prior == 0 => StallDecision::Relaunch,
            _ => StallDecision::Park,
        };
        prop_assert_eq!(d(a, b, c, e, prior), expected);
        // The order of the four signals does not matter.
        prop_assert_eq!(d(e, c, b, a, prior), expected);
    }
}
