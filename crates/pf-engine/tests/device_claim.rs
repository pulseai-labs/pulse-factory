//! AC-5 of `r1.s4.w1`: the claim helper that a CLI claim and a device's claim share, and the holder
//! label of a paired device. `claim_effects` is pure: it holds the checks of the CLI's claim (the next
//! generation, the holder label, the hash shape) and returns the fence of the current claim and the
//! new claim row, as effects the caller admits.

use pf_engine::claim::{claim_effects, device_holder, holder_device};
use pf_engine::port::store::{ClaimRow, Effect};

const HASH: &str = "ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12";
const NOW: &str = "2026-11-09T10:00:00.000Z";

fn row(generation: u64, holder: &str, fenced: Option<&str>) -> ClaimRow {
    ClaimRow {
        generation,
        token_hash: HASH.to_string(),
        holder: holder.to_string(),
        claimed_at: "2026-11-09T09:00:00.000Z".to_string(),
        fenced_at: fenced.map(str::to_string),
    }
}

#[test]
fn the_first_claim_is_generation_one_with_one_effect() {
    let (generation, effects) =
        claim_effects(&[], "  ann@host  ", &HASH.to_uppercase(), NOW).unwrap();
    assert_eq!(generation, 1);
    assert_eq!(
        effects,
        vec![Effect::PutClaim(ClaimRow {
            generation: 1,
            token_hash: HASH.to_string(),
            holder: "ann@host".to_string(),
            claimed_at: NOW.to_string(),
            fenced_at: None,
        })],
        "the holder is trimmed and the hash is lower case"
    );
}

#[test]
fn a_new_claim_is_the_next_generation_and_fences_the_current_one() {
    let claims = [
        row(1, "ann@host", Some("2026-11-09T09:30:00.000Z")),
        row(2, "bob@host", None),
    ];
    let (generation, effects) = claim_effects(&claims, "device D-1 (MacBook)", HASH, NOW).unwrap();
    assert_eq!(generation, 3);
    assert_eq!(effects.len(), 2);
    assert_eq!(
        effects[0],
        Effect::PutClaim(row(2, "bob@host", Some(NOW))),
        "the current claim is fenced at the time of the new one"
    );
    let Effect::PutClaim(new) = &effects[1] else {
        panic!("the second effect is the new claim: {effects:?}");
    };
    assert_eq!(
        (new.generation, new.holder.as_str()),
        (3, "device D-1 (MacBook)")
    );
    assert_eq!(new.fenced_at, None);
}

#[test]
fn the_next_generation_counts_fenced_claims_too() {
    // Both claims are fenced: no claim is current, and the next is still 3.
    let claims = [
        row(1, "ann@host", Some("2026-11-09T09:10:00.000Z")),
        row(2, "bob@host", Some("2026-11-09T09:20:00.000Z")),
    ];
    let (generation, effects) = claim_effects(&claims, "carl@host", HASH, NOW).unwrap();
    assert_eq!(generation, 3);
    assert_eq!(effects.len(), 1, "nothing to fence: {effects:?}");
}

#[test]
fn a_bad_hash_an_empty_holder_and_a_multi_line_holder_are_refused() {
    for hash in ["", "abc", &"g".repeat(64), &HASH[..63], &format!("{HASH}0")] {
        let why = claim_effects(&[], "ann@host", hash, NOW).unwrap_err();
        assert!(why.contains("64 hex digits"), "{hash:?}: {why}");
    }
    for holder in ["", "   ", "\n"] {
        let why = claim_effects(&[], holder, HASH, NOW).unwrap_err();
        assert!(why.contains("holder"), "{holder:?}: {why}");
    }
    let why = claim_effects(&[], "ann\nbob", HASH, NOW).unwrap_err();
    assert!(why.contains("holder"), "{why}");
}

#[test]
fn a_device_holder_label_round_trips_to_the_device_id() {
    let label = device_holder("D-1", "MacBook");
    assert_eq!(label, "device D-1 (MacBook)");
    assert_eq!(holder_device(&label), Some("D-1"));
    // A name of more than one line is cut to its first line.
    assert_eq!(device_holder("D-2", "Mac\nBook Pro"), "device D-2 (Mac)");
    assert_eq!(holder_device("device D-2 (Mac)"), Some("D-2"));
    // A name with parentheses still reads back the ID.
    assert_eq!(
        holder_device(&device_holder("D-3", "Ann's (work) Mac")),
        Some("D-3")
    );
}

#[test]
fn a_cli_shaped_label_reads_as_no_device() {
    for label in [
        "ann@host",
        "ann@host (claude)",
        "device",
        "device ",
        "device D-1",
        "xdevice D-1 (a)",
        "",
    ] {
        assert_eq!(holder_device(label), None, "{label:?}");
    }
}
