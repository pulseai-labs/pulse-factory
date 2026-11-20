//! AC-9 of `r2.s2.w3` and AC-12 of `r2.s2.w5` (audit 12, Q8): how long a controller takes to be ready
//! after a `SIGKILL`. Twenty paper seats and four chats live in detached hosts: the seats are claimed
//! by a paper run's live session rows, the chats by their sessions (`ClaudeChat` over the replay).
//! The controller is killed and started again. The test measures the time from the new process's start to
//! the first answered hook call of the 20th seat, which must be an allow and never "unknown", and the
//! time `VACUUM INTO` takes on the test's store (the work of the backup step before a migration, w2).
//! It prints both, checks that every host and every chat survived, and asserts that the built-in `session.hook_wait_seconds` is at least three times
//! their sum, so a hook that waits out a restart waits long enough.

mod common;

use std::time::{Duration, Instant};

use common::chat_restart::ChatRig;
use pulse_factory::controller::settings::SESSION_KEYS;
use serde_json::{Value, json};

const SEATS: usize = 20;
const CHATS: usize = 4;

fn allowed() -> Value {
    json!({ "role": "x", "tool": "bash", "input": { "command": "ls -la" } })
}

#[test]
fn twenty_seats_and_four_chats_are_ready_again_well_inside_the_hook_wait() {
    // `ChatRig` starts the controller and the chat session S-1; three more chats follow.
    let chats = ChatRig::new("plain_turn", None, &[], &[]);
    for i in 2..=CHATS {
        let (project, _) = chats.new_project(&format!("chat{i}"));
        chats.start_session(&project);
    }
    let mut rig = chats.r;
    rig.arm();
    let units: Vec<String> = (1..=SEATS).map(|i| format!("u{i}")).collect();
    for (i, u) in units.iter().enumerate() {
        rig.queue(u, u32::try_from(i + 1).unwrap());
    }
    for u in &units {
        rig.wait_seat(u);
        rig.unit_of_seat(u);
        rig.wait_attached(u);
    }
    assert_eq!(
        rig.live_hosts().len(),
        SEATS + CHATS,
        "twenty seat hosts and four chat hosts"
    );

    rig.kill9();
    // The 20th seat calls its hook while the controller starts: the hook waits for the socket.
    let last = units.last().unwrap();
    let out = rig.out(last);
    let tmp = out.join("hook.in.tmp");
    std::fs::write(&tmp, allowed().to_string()).unwrap();
    let start = Instant::now();
    rig.spawn_controller();
    std::fs::rename(&tmp, out.join("hook.in")).unwrap();
    rig.wait_for("the 20th seat's hook call", Duration::from_secs(60), || {
        out.join("hook.out").exists()
    });
    let ready = start.elapsed();
    let answer: Value = serde_json::from_str(
        std::fs::read_to_string(out.join("hook.out"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(answer["decision"], "allow", "{answer}");

    // Every seat is answered by its rules, none "unknown to this controller".
    for u in &units {
        let a = rig.hook(u, &allowed());
        assert_eq!(a["decision"], "allow", "{u}: {a}");
    }
    assert_eq!(rig.live_hosts().len(), SEATS + CHATS);
    let survived = rig
        .ledger_lines("host_tier1")
        .into_iter()
        .filter(|l| l.contains(" survived "))
        .count();
    assert_eq!(
        survived,
        SEATS + CHATS,
        "every host survived, seats and chats"
    );
    let rebound = std::fs::read_to_string(rig.state.join("chat.ledger"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(" chat rebind ") && l.contains("outcome=survived"))
        .count();
    assert_eq!(rebound, CHATS, "every chat was rebound");
    // The backup step's work: `VACUUM INTO` a copy of the store.
    let db = rusqlite::Connection::open(rig.state.join("store.sqlite")).unwrap();
    let copy = rig.root.join("backup.sqlite");
    let begun = Instant::now();
    db.execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
        .unwrap();
    let backup = begun.elapsed();
    assert!(copy.exists());

    println!(
        "time to ready: {} ms ({SEATS} hosts, {CHATS} chats), backup: {} ms",
        ready.as_millis(),
        backup.as_millis()
    );
    let built_in = SESSION_KEYS
        .iter()
        .find(|(k, ..)| *k == "session.hook_wait_seconds")
        .map(|(_, _, _, d)| *d)
        .unwrap();
    let needed = 3 * (ready + backup);
    assert!(
        Duration::from_secs(built_in) >= needed,
        "the built-in hook wait of {built_in} s is under three times the time to ready and the backup \
         ({needed:?}): raise it in SESSION_KEYS"
    );
}
