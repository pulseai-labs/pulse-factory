//! Notices (`r2.s11.w3`; ADR-0006's amendment of 2026-11-21, `docs/contracts/requests.md`,
//! "Notices").
//!
//! The controller derives one notice from each request event: a request that opened, was revised
//! while it was open, or closed. Each notice is one admitted command and holds one delivery per
//! channel, `app` and `orchestrator`, with the channel's own text. The store keeps the notices, the
//! deliveries, the targets' positions and the sends, and holds each rule in a trigger. This module
//! keeps nothing the store does not: after a restart the next tick reads the notices again and
//! catches up.
//!
//! One lock, `Chats.notices`, makes "read the sends, commit one, send the frame" a single step, so
//! a tick and a new stream never count the same send twice.
//!
//! The channels are texts: the `app` channel's targets are `device:<D-n>`, the `orchestrator`
//! channel's are `agent:<P-n>:orchestrator`. A target acknowledges a position, and the position
//! acknowledges every notice of its channel at or below it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use pf_api::types::{
    ChatItemBody, NoticeAckArgs, NoticeAcked, NoticeFrame, NoticeOutcome, NoticeRecord,
    RequestSummary,
};
use pf_engine::port::store::{
    DeliveryState, Effect, NEW_ID, NewDelivery, NewNotice, NoticeRow, NoticeSendRow,
    NoticeTargetRow, ORCHESTRATOR_AGENT, Store,
};
use pf_engine::status::redact;
use pf_ipc::Response;
use serde_json::{Value, json};

use super::chats::{LiveChat, time_of, utc};
use super::commands::{Plan, now};
use super::settings::{
    NOTIFY_APP_MAX_WORDS, NOTIFY_APP_ON, NOTIFY_ORCHESTRATOR_MAX_WORDS, NOTIFY_ORCHESTRATOR_ON,
    NOTIFY_RESEND_SECONDS,
};
use super::{Controller, lock};

/// The channel of the paired devices.
pub const APP: &str = "app";
/// The channel of the project's Orchestrator chat.
pub const ORCHESTRATOR: &str = "orchestrator";

/// The longest wait between two sends of one notice to one device, in seconds.
pub const RESEND_CAP_SECONDS: u64 = 900;

/// The notices the controller reads from the store in one page.
const PAGE: u32 = 500;

/// The most requests one carry lists. The rest are counted.
const CARRY_LINES: usize = 10;

/// The first and the last line of a carry: the mark that tells the Orchestrator that the controller
/// wrote the block and the operator did not.
pub const CARRY_OPEN: &str = "[Pulse Factory notice. The controller wrote this, not the operator.]";
pub const CARRY_CLOSE: &str = "[End of notice]";

/// The block an operator message carries into an Orchestrator chat.
pub(super) struct Carry {
    pub block: String,
    /// The notices that have a line in the block.
    listed: Vec<NoticeRow>,
    /// The highest position any waiting notice of the agent holds.
    max: u64,
}

/// What the controller knows of the notices of one request.
#[derive(Default)]
struct Seen {
    opened: bool,
    closed: bool,
    /// The highest revision any notice of the request holds.
    top: u32,
    positions: Vec<u64>,
    /// True once every delivery of a closed request is superseded.
    settled: bool,
}

/// The state of the notice step, behind `Chats.notices`.
#[derive(Default)]
pub(super) struct NoticeWatch {
    /// The newest position read from the store.
    loaded_to: u64,
    seen: BTreeMap<String, Seen>,
    /// A test seam: the notice clock runs this far ahead of the real one.
    skew: Duration,
    /// Closed notices whose chat item is not written yet. A closed notice never waits, so the
    /// store holds no waiting delivery for it: this set does, in memory only.
    closed_pending: BTreeSet<u64>,
    /// Failures printed already, so a write that fails at each tick prints once.
    failed: BTreeSet<String>,
}

impl NoticeWatch {
    fn clock(&self) -> SystemTime {
        SystemTime::now() + self.skew
    }
}

/// `device:<D-n>`.
pub fn device_target(device: &str) -> String {
    format!("device:{device}")
}

/// `agent:<P-n>:orchestrator`.
pub fn agent_target(project: &str) -> String {
    format!("agent:{project}:{ORCHESTRATOR_AGENT}")
}

/// The ID of the acknowledgement command of a target: `<device>` or `<project>.<agent>`.
fn ack_id(target: &str, position: u64) -> String {
    let name = match target.strip_prefix("device:") {
        Some(device) => device.to_string(),
        None => target
            .strip_prefix("agent:")
            .map(|rest| rest.replacen(':', ".", 1))
            .unwrap_or_else(|| target.to_string()),
    };
    format!("notice-ack:{name}:{position}")
}

/// The text cut to `max` words at white space. A text under the cap is kept as it is. A cut text
/// keeps whole words and ends `…`.
pub fn cut_words(text: &str, max: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= max {
        return text.trim().to_string();
    }
    format!("{}…", words[..max].join(" "))
}

/// The text of a notice before the cap: the block's summary, or a sentence when the request has
/// none. A seat's words, the reason and the stop text never come here.
fn base_text(r: &RequestSummary, event: &str) -> String {
    match event {
        "closed" => format!("Request {} closed: {}.", r.id, r.status),
        _ => {
            let summary = r
                .summary
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("Request {} of run {} is open.", r.id, r.run));
            if event == "revised" {
                format!("Changed: {summary}")
            } else {
                summary
            }
        }
    }
}

/// How a delivery stands on the ledger line: `waiting` or `superseded:<why>`.
fn state_text(why: &Option<String>) -> String {
    match why {
        Some(why) => format!("superseded:{why}"),
        None => "waiting".to_string(),
    }
}

impl Controller {
    /// Move the clock of the notices forward. A test seam, as [`Controller::advance_confirm_clock`]
    /// is.
    pub fn advance_notice_clock(&self, by: Duration) {
        lock(&self.chats.notices).skew += by;
    }

    /// Forget what the controller read of the notices, as a restart does. A test seam: the store
    /// keeps every notice, so the next tick reads them again and catches up.
    pub fn forget_notices(&self) {
        let mut watch = lock(&self.chats.notices);
        let skew = watch.skew;
        *watch = NoticeWatch {
            skew,
            ..NoticeWatch::default()
        };
    }

    /// Print a failure once, until the same step works again.
    fn note_failure(&self, watch: &mut NoticeWatch, key: &str, why: &str) {
        if watch.failed.insert(key.to_string()) {
            eprintln!("pulse-factory: notices: {key}: {why}; the next tick tries again");
        }
    }

    fn note_success(watch: &mut NoticeWatch, key: &str) {
        watch.failed.remove(key);
    }

    /// The step of a tick: derive the notices that any request event lacks, supersede what a close
    /// ends, write the chat items and send the frames. `all` is every request of every run.
    pub(super) fn run_notices(&self, all: &[RequestSummary]) {
        let mut watch = lock(&self.chats.notices);
        // A stopped controller writes no notice: `status` says so.
        if self.commands_stopped().is_some() {
            return;
        }
        if !self.load_notices(&mut watch) {
            return;
        }
        let mut closed_now = Vec::new();
        for r in all {
            self.derive_one(&mut watch, r, &mut closed_now);
        }
        self.settle_closed(&mut watch, all);
        self.write_orchestrator_items(&mut watch);
        self.send_app_notices(&mut watch, &closed_now);
    }

    /// Read the notices above `loaded_to` into the watch.
    fn load_notices(&self, watch: &mut NoticeWatch) -> bool {
        loop {
            let rows = match self.store.notices_after(watch.loaded_to, PAGE) {
                Ok(rows) => rows,
                Err(e) => {
                    self.note_failure(watch, "read", &e.to_string());
                    return false;
                }
            };
            Self::note_success(watch, "read");
            let n = rows.len();
            for row in rows {
                let seen = watch.seen.entry(row.request_id.clone()).or_default();
                seen.top = seen.top.max(row.revision);
                seen.positions.push(row.position);
                match row.event.as_str() {
                    "opened" => seen.opened = true,
                    "closed" => seen.closed = true,
                    _ => {}
                }
                watch.loaded_to = row.position;
            }
            if n < PAGE as usize {
                return true;
            }
        }
    }

    /// The notice that this request's state lacks, if any, added as one command.
    fn derive_one(&self, watch: &mut NoticeWatch, r: &RequestSummary, closed_now: &mut Vec<u64>) {
        let (opened, closed, top) = watch
            .seen
            .get(&r.id)
            .map_or((false, false, 0), |s| (s.opened, s.closed, s.top));
        let event = if r.status == "open" {
            if !opened {
                "opened"
            } else if r.revision > top {
                "revised"
            } else {
                return;
            }
        } else if opened && !closed {
            // A closed request nobody was told about gets nothing.
            "closed"
        } else {
            return;
        };
        let key = format!("notice {}:{}:{event}", r.id, r.revision);
        match self.add_notice(r, event) {
            Ok(position) => {
                Self::note_success(watch, &key);
                if event == "closed" {
                    closed_now.push(position);
                    watch.closed_pending.insert(position);
                }
                self.load_notices(watch);
            }
            Err(why) => self.note_failure(watch, &key, &why),
        }
    }

    /// One notice: its deliveries at birth, and the admission. Returns its position.
    fn add_notice(&self, r: &RequestSummary, event: &str) -> Result<u64, String> {
        let project = self.run_project_of(&r.run);
        let project_ref = project.as_deref();
        // The orchestrator's keys also resolve at the agent; the app's do not.
        let agent_of = |key: &str| {
            (key != NOTIFY_APP_ON && key != NOTIFY_APP_MAX_WORDS).then_some(ORCHESTRATOR_AGENT)
        };
        let on = |key: &str| self.notify_setting(key, project_ref, agent_of(key)) != 0;
        let words = |key: &str| {
            usize::try_from(self.notify_setting(key, project_ref, agent_of(key)))
                .unwrap_or(usize::MAX)
        };
        let base = redact(&base_text(r, event));
        let closed = event == "closed";
        let app_why = if !on(NOTIFY_APP_ON) {
            Some("channel_off")
        } else if closed {
            Some("request_closed")
        } else {
            None
        };
        let orchestrator_why = if !on(NOTIFY_ORCHESTRATOR_ON) {
            Some("channel_off")
        } else if project.is_none() {
            Some("no_project")
        } else if closed {
            Some("request_closed")
        } else {
            None
        };
        let delivery = |channel: &str, key: &str, why: Option<&str>| NewDelivery {
            channel: channel.to_string(),
            text: cut_words(&base, words(key)),
            why: why.map(str::to_string),
        };
        let deliveries = vec![
            delivery(APP, NOTIFY_APP_MAX_WORDS, app_why),
            delivery(
                ORCHESTRATOR,
                NOTIFY_ORCHESTRATOR_MAX_WORDS,
                orchestrator_why,
            ),
        ];
        let command = format!("notice:{}:{}:{event}", r.id, r.revision);
        let line = format!(
            "notice {NEW_ID} request={}@{} event={event} run={} project={} app={} \
             orchestrator={} command={command}",
            r.id,
            r.revision,
            r.run,
            project.as_deref().unwrap_or("-"),
            state_text(&deliveries[0].why),
            state_text(&deliveries[1].why),
        );
        let content =
            json!({ "request": r.id, "revision": r.revision, "event": event }).to_string();
        let effect = Effect::NoticeAdd(NewNotice {
            request_id: r.id.clone(),
            revision: r.revision,
            event: event.to_string(),
            run_id: r.run.clone(),
            project_id: project,
            created_at: now(),
            deliveries,
        });
        let (admission, _) = self.run_command(
            "notice",
            &command,
            content,
            Plan::Admit {
                effects: vec![effect],
                subject: NEW_ID.to_string(),
                line,
            },
        )?;
        admission
            .unit_id
            .and_then(|p| p.parse().ok())
            .ok_or_else(|| format!("the admission of {command} names no position"))
    }

    /// Supersede the deliveries that still wait for a request that closed. One command each.
    fn settle_closed(&self, watch: &mut NoticeWatch, all: &[RequestSummary]) {
        for r in all.iter().filter(|r| r.status != "open") {
            let Some(seen) = watch.seen.get(&r.id) else {
                continue;
            };
            if !seen.closed || seen.settled {
                continue;
            }
            let positions = seen.positions.clone();
            let mut clean = true;
            for position in positions {
                let rows = match self.store.notice_deliveries(position) {
                    Ok(rows) => rows,
                    Err(e) => {
                        self.note_failure(watch, "read deliveries", &e.to_string());
                        clean = false;
                        continue;
                    }
                };
                for d in rows.iter().filter(|d| d.state == DeliveryState::Waiting) {
                    if let Err(why) = self.supersede(position, &d.channel, "request_closed") {
                        self.note_failure(
                            watch,
                            &format!("notice-supersede:{position}:{}", d.channel),
                            &why,
                        );
                        clean = false;
                    }
                }
            }
            if clean && let Some(seen) = watch.seen.get_mut(&r.id) {
                seen.settled = true;
            }
        }
    }

    fn supersede(&self, position: u64, channel: &str, why: &str) -> Result<(), String> {
        let command = format!("notice-supersede:{position}:{channel}");
        self.run_command(
            "notice_supersede",
            &command,
            json!({ "position": position, "channel": channel }).to_string(),
            Plan::Admit {
                effects: vec![Effect::NoticeSupersede {
                    position,
                    channel: channel.to_string(),
                    why: why.to_string(),
                    updated_at: now(),
                }],
                subject: position.to_string(),
                line: format!("notice_superseded {position} channel={channel} why={why}"),
            },
        )
        .map(|_| ())
    }

    /// One acknowledgement command. `Ok(true)` when it was admitted, `Ok(false)` for a replay.
    pub(super) fn commit_ack(
        &self,
        target: &str,
        channel: &str,
        position: u64,
        outcome: &str,
    ) -> Result<bool, String> {
        let command = ack_id(target, position);
        self.run_command(
            "notice_ack",
            &command,
            json!({ "target": target, "position": position }).to_string(),
            Plan::Admit {
                effects: vec![Effect::NoticeAck {
                    target: target.to_string(),
                    channel: channel.to_string(),
                    position,
                    outcome: outcome.to_string(),
                    updated_at: now(),
                }],
                subject: position.to_string(),
                line: format!("notice_ack {target} position={position} outcome={outcome}"),
            },
        )
        .map(|(_, replayed)| !replayed)
    }

    /// One send of a notice to a target: `sends` is the stored count plus one.
    pub(super) fn commit_send(
        &self,
        watch: &NoticeWatch,
        position: u64,
        target: &str,
        sends: u64,
    ) -> Result<(), String> {
        let command = format!("notice-send:{position}:{target}:{sends}");
        self.run_command(
            "notice_send",
            &command,
            json!({ "position": position, "target": target, "sends": sends }).to_string(),
            Plan::Admit {
                effects: vec![Effect::NoticeSend {
                    position,
                    target: target.to_string(),
                    sends,
                    sent_at: utc(watch.clock()),
                }],
                subject: position.to_string(),
                line: format!("notice_send {position} target={target} attempt={sends}"),
            },
        )
        .map(|_| ())
    }

    // ------------------------------------------------------------------ the frames

    /// The summary of a request, read from the store. `None` for a request or a run it cannot find.
    fn summary_of_request(&self, id: &str) -> Option<RequestSummary> {
        let row = self.store.request(id).ok().flatten()?;
        let handle = self.find(Some(&row.run_id)).ok()?;
        Some(self.request_summary_of(&handle, &row))
    }

    /// The frame of a notice for a device: the record, with the text of the app delivery.
    fn notice_frame(&self, n: &NoticeRow, text: &str, sends: u64) -> NoticeFrame {
        let summary = self.summary_of_request(&n.request_id);
        NoticeFrame::Notice {
            notice: NoticeRecord {
                position: n.position,
                event: n.event.clone(),
                request: n.request_id.clone(),
                revision: n.revision,
                run: n.run_id.clone(),
                project: n.project_id.clone(),
                unit: summary.as_ref().and_then(|s| s.unit.clone()),
                kind: summary.as_ref().map(|s| s.kind.clone()).unwrap_or_default(),
                cause: summary.as_ref().and_then(|s| s.cause.clone()),
                text: text.to_string(),
                created_at: n.created_at.clone(),
                sends,
            },
        }
    }

    /// The devices with a stream open, once each, in order.
    fn connected_devices(&self) -> Vec<String> {
        let mut streams = lock(&self.chats.streams);
        streams.retain(|(_, tx)| !tx.is_closed());
        let ids: BTreeSet<String> = streams.iter().map(|(id, _)| id.clone()).collect();
        ids.into_iter().collect()
    }

    /// Send one notice to one device: one admitted send, then the frame on each of its streams.
    fn send_to_device(
        &self,
        watch: &mut NoticeWatch,
        device: &str,
        n: &NoticeRow,
        text: &str,
        sends: u64,
    ) -> Option<NoticeFrame> {
        let target = device_target(device);
        let key = format!("notice-send:{}:{target}:{sends}", n.position);
        match self.commit_send(watch, n.position, &target, sends) {
            Ok(()) => {
                Self::note_success(watch, &key);
                let frame = self.notice_frame(n, text, sends);
                let _ = self
                    .chats
                    .notice_frames
                    .send((device.to_string(), frame.clone()));
                Some(frame)
            }
            Err(why) => {
                self.note_failure(watch, &key, &why);
                None
            }
        }
    }

    /// Is a notice sent `sends` times, last at `last_sent_at`, due again at `now`?
    fn resend_due(&self, watch: &NoticeWatch, n: &NoticeRow, send: &NoticeSendRow) -> bool {
        let resend = self.notify_setting(NOTIFY_RESEND_SECONDS, n.project_id.as_deref(), None);
        let doubled = 1u64
            .checked_shl(
                u32::try_from(send.sends.saturating_sub(1))
                    .unwrap_or(u32::MAX)
                    .min(20),
            )
            .unwrap_or(1);
        let wait = resend.saturating_mul(doubled).min(RESEND_CAP_SECONDS);
        match time_of(&send.last_sent_at) {
            Some(last) => watch.clock() >= last + Duration::from_secs(wait),
            None => true,
        }
    }

    /// The app channel at a tick: each connected device gets each waiting notice that it was never
    /// sent or that is due again, and each closed notice of this tick once.
    fn send_app_notices(&self, watch: &mut NoticeWatch, closed_now: &[u64]) {
        let devices = self.connected_devices();
        if devices.is_empty() {
            return;
        }
        let targets = match self.store.notice_targets() {
            Ok(t) => t,
            Err(e) => {
                self.note_failure(watch, "read targets", &e.to_string());
                return;
            }
        };
        let waiting = match self.store.waiting_deliveries(APP) {
            Ok(w) => w,
            Err(e) => {
                self.note_failure(watch, "read deliveries", &e.to_string());
                return;
            }
        };
        for device in &devices {
            let target = device_target(device);
            let Some(row) = targets.iter().find(|t| t.target == target) else {
                // Not joined yet: its first stream joins it.
                continue;
            };
            for d in waiting.iter().filter(|d| d.position > row.acked) {
                let Ok(Some(n)) = self.store.notice(d.position) else {
                    continue;
                };
                let sent = self
                    .store
                    .notice_sends(d.position)
                    .ok()
                    .and_then(|rows| rows.into_iter().find(|s| s.target == target));
                let sends = match &sent {
                    None => 1,
                    Some(s) if self.resend_due(watch, &n, s) => s.sends + 1,
                    Some(_) => continue,
                };
                self.send_to_device(watch, device, &n, &d.text, sends);
            }
            for position in closed_now {
                let Ok(Some(n)) = self.store.notice(*position) else {
                    continue;
                };
                let Some(d) = self
                    .store
                    .notice_deliveries(*position)
                    .ok()
                    .and_then(|rows| rows.into_iter().find(|d| d.channel == APP))
                else {
                    continue;
                };
                // A closed notice is sent once, and only when the app channel is on.
                if d.why.as_deref() == Some("request_closed") {
                    self.send_to_device(watch, device, &n, &d.text, 1);
                }
            }
        }
    }

    /// A device's first stream: it joins at the newest position, so it gets no old notice.
    pub(super) fn join_notices(&self, device: &str) {
        let _one = lock(&self.chats.notices);
        if self.commands_stopped().is_some() {
            return;
        }
        let target = device_target(device);
        let joined = match self.store.notice_targets() {
            Ok(rows) => rows.iter().any(|t| t.target == target),
            Err(_) => return,
        };
        if joined {
            return;
        }
        let newest = self.store.newest_notice().unwrap_or(0);
        if let Err(why) = self.commit_ack(&target, APP, newest, "joined") {
            eprintln!("pulse-factory: notices: device {device} did not join: {why}");
        }
    }

    /// The notices a new stream gets after its `requests` frame: each waiting notice above the
    /// device's position, oldest first, each as one send.
    pub(super) fn notice_backlog(&self, device: &str) -> Vec<NoticeFrame> {
        let mut watch = lock(&self.chats.notices);
        if self.commands_stopped().is_some() {
            return Vec::new();
        }
        let target = device_target(device);
        let Ok(targets) = self.store.notice_targets() else {
            return Vec::new();
        };
        let Some(row) = targets.iter().find(|t| t.target == target) else {
            return Vec::new();
        };
        let Ok(waiting) = self.store.waiting_deliveries(APP) else {
            return Vec::new();
        };
        let mut frames = Vec::new();
        for d in waiting.iter().filter(|d| d.position > row.acked) {
            let Ok(Some(n)) = self.store.notice(d.position) else {
                continue;
            };
            let sends = self
                .store
                .notice_sends(d.position)
                .ok()
                .and_then(|rows| rows.into_iter().find(|s| s.target == target))
                .map_or(0, |s| s.sends)
                + 1;
            let key = format!("notice-send:{}:{target}:{sends}", n.position);
            match self.commit_send(&watch, n.position, &target, sends) {
                Ok(()) => {
                    Self::note_success(&mut watch, &key);
                    frames.push(self.notice_frame(&n, &d.text, sends));
                }
                Err(why) => self.note_failure(&mut watch, &key, &why),
            }
        }
        frames
    }

    // ------------------------------------------------------------------ notice_ack

    /// `notice_ack` of a paired device: it deals with every notice at or below `position`.
    pub(super) fn device_notice_ack(&self, device: &str, args: &Value) -> Response {
        let args: NoticeAckArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            Err(e) => {
                return Response::err(format!(
                    "notice_ack refused: the arguments are not readable: {e}"
                ));
            }
        };
        if let Err(response) = self.live_device(device) {
            return response;
        }
        let newest = match self.store.newest_notice() {
            Ok(n) => n,
            Err(e) => return Response::err(e.to_string()),
        };
        if args.position > newest {
            return Response::err(format!(
                "notice_ack refused: position {} is above the newest notice, {newest}",
                args.position
            ));
        }
        let outcome = match args.outcome {
            NoticeOutcome::Shown => "shown",
            NoticeOutcome::Blocked => "blocked",
        };
        let target = device_target(device);
        let admitted = {
            let _one = lock(&self.chats.notices);
            let stored = match self.store.notice_targets() {
                Ok(rows) => rows.into_iter().find(|t| t.target == target),
                Err(e) => return Response::err(e.to_string()),
            };
            // A position that does not rise admits nothing: the store's refusal is the backstop.
            if stored.is_some_and(|t| args.position <= t.acked) {
                false
            } else {
                match self.commit_ack(&target, APP, args.position, outcome) {
                    Ok(admitted) => admitted,
                    Err(why) => return Response::err(format!("notice_ack refused: {why}")),
                }
            }
        };
        let acked = NoticeAcked {
            schema_version: super::requests::SCHEMA_VERSION,
            device: device.to_string(),
            position: args.position,
            outcome: args.outcome,
            admitted,
        };
        Response::ok(serde_json::to_value(acked).unwrap_or(Value::Null))
    }

    /// The refusal of `notice_ack` on the Unix socket.
    pub(super) fn socket_notice_ack() -> Response {
        Response::err("notice_ack is for paired devices")
    }

    // ------------------------------------------------------------------ the Orchestrator's chat

    /// The newest session of the project's Orchestrator that has not ended.
    fn orchestrator_session(&self, project: &str) -> Option<String> {
        self.store
            .chat_sessions(project)
            .ok()?
            .into_iter()
            .filter(|s| s.agent_id == ORCHESTRATOR_AGENT && s.ended_at.is_none())
            .max_by(|a, b| {
                (&a.created_at, session_number(&a.id)).cmp(&(&b.created_at, session_number(&b.id)))
            })
            .map(|s| s.id)
    }

    /// The IDs and the command that follow a notice's text in the chat: `(request R-n, run …, unit
    /// …, stage …, PR …) Read it: pulse-factory show R-n`, with each ID that exists.
    pub(super) fn notice_ids(&self, n: &NoticeRow) -> String {
        let summary = self.summary_of_request(&n.request_id);
        let mut ids = vec![
            format!("request {}", n.request_id),
            format!("run {}", n.run_id),
        ];
        if let Some(s) = &summary {
            if let Some(unit) = &s.unit {
                ids.push(format!("unit {unit}"));
            }
            if !s.stage.is_empty() {
                ids.push(format!("stage {}", s.stage));
            }
            if let Some(pr) = &s.pr {
                ids.push(format!("PR {pr}"));
            }
        }
        format!(
            "({}) Read it: pulse-factory show {}",
            ids.join(", "),
            n.request_id
        )
    }

    /// The text of a notice's chat line: the channel's text, then the IDs and the command.
    pub(super) fn notice_line(&self, n: &NoticeRow, text: &str) -> String {
        format!("{text} {}", self.notice_ids(n))
    }

    /// Write the notice into the project's newest Orchestrator session as one `notice` item.
    fn write_item(&self, n: &NoticeRow, session: &str, text: &str) -> Result<(), String> {
        let code = match n.event.as_str() {
            "opened" => "request_opened",
            "revised" => "request_revised",
            _ => "request_closed",
        };
        let body = ChatItemBody::Notice {
            code: code.to_string(),
            text: self.notice_line(n, text),
        };
        let item = super::chat_view::new_item(session, &body);
        self.append_item(item).map(|_| ())
    }

    /// The orchestrator channel at a tick: a target row for each project with a waiting notice, and
    /// each notice written once as an item. The item is written first and the send recorded after,
    /// so a send row means the item is stored. A notice of a project with no session waits.
    fn write_orchestrator_items(&self, watch: &mut NoticeWatch) {
        let waiting = match self.store.waiting_deliveries(ORCHESTRATOR) {
            Ok(w) => w,
            Err(e) => {
                self.note_failure(watch, "read deliveries", &e.to_string());
                return;
            }
        };
        let mut acked: BTreeMap<String, u64> = match self.store.notice_targets() {
            Ok(rows) => rows
                .into_iter()
                .filter(|t| t.channel == ORCHESTRATOR)
                .map(|t| (t.target, t.acked))
                .collect(),
            Err(e) => {
                self.note_failure(watch, "read targets", &e.to_string());
                return;
            }
        };
        for d in &waiting {
            let Ok(Some(n)) = self.store.notice(d.position) else {
                continue;
            };
            let Some(project) = n.project_id.clone() else {
                continue;
            };
            let target = agent_target(&project);
            // An agent's row is made with the first notice of its project, just below that
            // notice, so that notice waits for it.
            if !acked.contains_key(&target) {
                let below = d.position.saturating_sub(1);
                match self.commit_ack(&target, ORCHESTRATOR, below, "joined") {
                    Ok(_) => {
                        acked.insert(target.clone(), below);
                    }
                    Err(why) => {
                        self.note_failure(watch, &format!("join {target}"), &why);
                        continue;
                    }
                }
            }
            if d.position <= acked[&target] {
                continue;
            }
            let sent = self
                .store
                .notice_sends(d.position)
                .ok()
                .is_some_and(|rows| rows.iter().any(|s| s.target == target));
            if sent {
                continue;
            }
            self.deliver_item(watch, &n, &target, &d.text);
        }
        // A closed notice is shown once. Its delivery never waits, so it is kept here.
        let pending: Vec<u64> = watch.closed_pending.iter().copied().collect();
        for position in pending {
            let Ok(Some(n)) = self.store.notice(position) else {
                continue;
            };
            let Some(project) = n.project_id.clone() else {
                watch.closed_pending.remove(&position);
                continue;
            };
            let d = self
                .store
                .notice_deliveries(position)
                .ok()
                .and_then(|rows| rows.into_iter().find(|d| d.channel == ORCHESTRATOR));
            match d {
                Some(d) if d.why.as_deref() == Some("request_closed") => {
                    if self.deliver_item(watch, &n, &agent_target(&project), &d.text) {
                        watch.closed_pending.remove(&position);
                    }
                }
                _ => {
                    watch.closed_pending.remove(&position);
                }
            }
        }
    }

    /// Write the item into the project's newest session and record the send. False when there is
    /// no session or a write failed.
    fn deliver_item(
        &self,
        watch: &mut NoticeWatch,
        n: &NoticeRow,
        target: &str,
        text: &str,
    ) -> bool {
        let Some(project) = n.project_id.as_deref() else {
            return false;
        };
        let Some(session) = self.orchestrator_session(project) else {
            return false;
        };
        self.deliver_item_to(watch, n, target, text, &session)
    }

    /// [`Controller::deliver_item`] into the session given.
    pub(super) fn deliver_item_to(
        &self,
        watch: &mut NoticeWatch,
        n: &NoticeRow,
        target: &str,
        text: &str,
        session: &str,
    ) -> bool {
        let session = session.to_string();
        let key = format!("notice item {}", n.position);
        if let Err(why) = self.write_item(n, &session, text) {
            self.note_failure(watch, &key, &why);
            return false;
        }
        Self::note_success(watch, &key);
        let sends = self
            .store
            .notice_sends(n.position)
            .ok()
            .and_then(|rows| rows.into_iter().find(|s| s.target == target))
            .map_or(0, |s| s.sends)
            + 1;
        let send_key = format!("notice-send:{}:{target}:{sends}", n.position);
        match self.commit_send(watch, n.position, target, sends) {
            Ok(()) => {
                Self::note_success(watch, &send_key);
                true
            }
            Err(why) => {
                self.note_failure(watch, &send_key, &why);
                false
            }
        }
    }

    // ------------------------------------------------------------------ the carry

    /// What the next operator message carries into an Orchestrator chat: the notices that wait for
    /// the agent, as a marked block. `None` when the session is no Orchestrator's, a turn is open,
    /// the channel is off, or nothing waits.
    pub(super) fn notice_carry(&self, live: &LiveChat) -> Option<Carry> {
        if live.agent != ORCHESTRATOR_AGENT || self.commands_stopped().is_some() {
            return None;
        }
        if lock(&live.inner).turn_since.is_some() {
            return None;
        }
        let project = live.project.as_str();
        if self.notify_setting(
            NOTIFY_ORCHESTRATOR_ON,
            Some(project),
            Some(ORCHESTRATOR_AGENT),
        ) == 0
        {
            return None;
        }
        let mut watch = lock(&self.chats.notices);
        let target = agent_target(project);
        let acked = self
            .store
            .notice_targets()
            .ok()?
            .into_iter()
            .find(|t| t.target == target)?
            .acked;
        // The newest notice of each request that waits, with the channel's text.
        let mut newest: BTreeMap<String, (NoticeRow, String)> = BTreeMap::new();
        let mut max = 0;
        for d in self.store.waiting_deliveries(ORCHESTRATOR).ok()? {
            if d.position <= acked {
                continue;
            }
            let Ok(Some(n)) = self.store.notice(d.position) else {
                continue;
            };
            if n.project_id.as_deref() != Some(project) {
                continue;
            }
            // A notice that is no item yet is written into this session first.
            let sent = self
                .store
                .notice_sends(n.position)
                .ok()
                .is_some_and(|rows| rows.iter().any(|s| s.target == target));
            if !sent {
                self.deliver_item_to(&mut watch, &n, &target, &d.text, &live.session);
            }
            max = max.max(n.position);
            let keep = newest
                .get(&n.request_id)
                .is_none_or(|(old, _)| old.position < n.position);
            if keep {
                newest.insert(n.request_id.clone(), (n, d.text));
            }
        }
        if newest.is_empty() {
            return None;
        }
        let mut listed: Vec<(NoticeRow, String)> = newest.into_values().collect();
        listed.sort_by_key(|(n, _)| std::cmp::Reverse(n.position));
        let more = listed.len().saturating_sub(CARRY_LINES);
        listed.truncate(CARRY_LINES);
        let mut block = format!("{CARRY_OPEN}\n");
        for (n, text) in &listed {
            block.push_str(&self.notice_line(n, text));
            block.push('\n');
        }
        if more > 0 {
            block.push_str(&format!("and {more} more: pulse-factory requests\n"));
        }
        block.push_str(CARRY_CLOSE);
        Some(Carry {
            block,
            listed: listed.into_iter().map(|(n, _)| n).collect(),
            max,
        })
    }

    /// After a send that carried notices: one `notice-send` for each, and the position the live
    /// chat remembers until the turn result is stored.
    pub(super) fn notices_carried(&self, live: &LiveChat, carry: Carry) {
        let mut watch = lock(&self.chats.notices);
        let target = agent_target(&live.project);
        for n in &carry.listed {
            let sends = self
                .store
                .notice_sends(n.position)
                .ok()
                .and_then(|rows| rows.into_iter().find(|s| s.target == target))
                .map_or(0, |s| s.sends)
                + 1;
            let key = format!("notice-send:{}:{target}:{sends}", n.position);
            match self.commit_send(&watch, n.position, &target, sends) {
                Ok(()) => Self::note_success(&mut watch, &key),
                Err(why) => self.note_failure(&mut watch, &key, &why),
            }
        }
        lock(&live.inner).carried = Some(carry.max);
    }

    /// The turn result of an operator message was stored: the agent acknowledges what the message
    /// carried, with the outcome `carried`.
    pub(super) fn ack_carried(&self, live: &LiveChat) {
        let Some(position) = lock(&live.inner).carried.take() else {
            return;
        };
        let _one = lock(&self.chats.notices);
        let target = agent_target(&live.project);
        let stored = self
            .store
            .notice_targets()
            .ok()
            .and_then(|rows| rows.into_iter().find(|t| t.target == target))
            .map(|t| t.acked);
        if stored.is_some_and(|acked| position <= acked) {
            return;
        }
        if let Err(why) = self.commit_ack(&target, ORCHESTRATOR, position, "carried") {
            eprintln!("pulse-factory: notices: {target} did not acknowledge {position}: {why}");
        }
    }

    // ------------------------------------------------------------------ status

    /// The `notices` field of a run's status: each waiting delivery of the run's notices with the
    /// target that has not acknowledged it, and the paired devices that cannot show a notice.
    pub(super) fn notices_status(&self, run_id: &str) -> Result<Value, String> {
        let watch = lock(&self.chats.notices);
        let now = watch.clock();
        let targets = self.store.notice_targets().map_err(|e| e.to_string())?;
        let devices = self.store.devices().map_err(|e| e.to_string())?;
        let live = |target: &NoticeTargetRow| match target.target.strip_prefix("device:") {
            Some(id) => devices.iter().any(|d| d.id == id && d.revoked_at.is_none()),
            None => true,
        };
        let mut waiting: Vec<Value> = Vec::new();
        for channel in [APP, ORCHESTRATOR] {
            for d in self
                .store
                .waiting_deliveries(channel)
                .map_err(|e| e.to_string())?
            {
                let Some(n) = self.store.notice(d.position).map_err(|e| e.to_string())? else {
                    continue;
                };
                if n.run_id != run_id {
                    continue;
                }
                let sends = self
                    .store
                    .notice_sends(d.position)
                    .map_err(|e| e.to_string())?;
                let age = time_of(&n.created_at)
                    .and_then(|t| now.duration_since(t).ok())
                    .map_or(0, |d| d.as_secs());
                let mut waiting_on: Vec<String> = targets
                    .iter()
                    .filter(|t| t.channel == channel && live(t) && t.acked < d.position)
                    .filter(|t| {
                        channel == APP
                            || n.project_id
                                .as_deref()
                                .is_some_and(|p| t.target == agent_target(p))
                    })
                    .map(|t| t.target.clone())
                    .collect();
                if channel == ORCHESTRATOR
                    && let Some(p) = &n.project_id
                    && !targets.iter().any(|t| t.target == agent_target(p))
                {
                    waiting_on.push(agent_target(p));
                }
                for target in waiting_on {
                    waiting.push(json!({
                        "position": d.position,
                        "request": n.request_id,
                        "revision": n.revision,
                        "event": n.event,
                        "target": target,
                        "channel": channel,
                        "sends": sends.iter().find(|s| s.target == target).map_or(0, |s| s.sends),
                        "waiting_seconds": age,
                    }));
                }
            }
        }
        let blocked: Vec<String> = targets
            .iter()
            .filter(|t| t.channel == APP && t.outcome == "blocked" && live(t))
            .filter_map(|t| t.target.strip_prefix("device:").map(str::to_string))
            .collect();
        Ok(json!({ "waiting": waiting, "blocked": blocked }))
    }
}

/// The number of an `S-<n>` ID, for ordering sessions made in the same millisecond.
fn session_number(id: &str) -> u64 {
    id.strip_prefix("S-")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}
