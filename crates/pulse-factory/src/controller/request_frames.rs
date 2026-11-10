//! The request frames of the stream (`r1.s4.w1`, G3 and R8): a second wire enum, `RequestFrame`, on the
//! same `GET /v1/stream`. `ChatFrame` does not change.
//!
//! The controller keeps the last `(revision, status)` it sent of every request and the generation of
//! the claim it last sent. After each `tick_all`, and after each request verb (the socket's or the
//! API's), it reads every run's requests and the claims, and sends one `request` frame for each request
//! that is new or changed and one `claim` frame when the generation changed. At start it fills the
//! snapshot without sending. A reader that connects gets the open requests and the claim as one
//! `requests` frame right after `hello`. A change that lands between that snapshot and the live frames
//! arrives twice, so a reader keys a request by `(id, revision, status)`.

use std::collections::BTreeMap;

use pf_api::types::{RequestFrame, RequestSummary};

use super::{Controller, lock};

/// What the controller last sent.
#[derive(Default)]
pub(super) struct RequestWatch {
    seen: BTreeMap<String, (u32, String)>,
    /// The generation of the claim last sent: `None` before any claim.
    claim: Option<u64>,
}

fn state_of(r: &RequestSummary) -> (u32, String) {
    (r.revision, r.status.clone())
}

impl Controller {
    /// Fill the snapshot with what is there now. It sends nothing.
    pub(super) fn prime_request_watch(&self) {
        let mut watch = lock(&self.chats.request_watch);
        if let Ok(all) = self.request_summaries(None, true) {
            watch.seen = all.iter().map(|r| (r.id.clone(), state_of(r))).collect();
        }
        if let Ok(claim) = self.current_claim_record() {
            watch.claim = claim.map(|c| c.generation);
        }
    }

    /// Send a frame for each request that is new or changed, and for a claim that changed.
    pub(super) fn publish_request_frames(&self) {
        let mut watch = lock(&self.chats.request_watch);
        let Ok(all) = self.request_summaries(None, true) else {
            return;
        };
        let claim = self.current_claim_record().ok();
        // A reader that is not there is not an error.
        let send = |frame: RequestFrame| {
            let _ = self.chats.request_frames.send(frame);
        };
        if let Some(claim) = claim {
            let generation = claim.as_ref().map(|c| c.generation);
            if generation != watch.claim {
                watch.claim = generation;
                send(RequestFrame::Claim { claim });
            }
        }
        for r in all {
            let now = state_of(&r);
            if watch.seen.get(&r.id) != Some(&now) {
                watch.seen.insert(r.id.clone(), now);
                send(RequestFrame::Request { request: r });
            }
        }
    }

    /// The `requests` frame of a new stream: the open requests and the claim.
    pub(super) fn requests_frame(&self) -> Result<RequestFrame, String> {
        Ok(RequestFrame::Requests {
            requests: self.request_summaries(None, false)?,
            claim: self.current_claim_record()?,
        })
    }
}
