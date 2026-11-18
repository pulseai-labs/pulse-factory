//! The replay buffer: the output the controller has not acknowledged, as a ring of a fixed size.
//!
//! Every output byte has a cursor: its offset since the child started. The buffer holds the bytes
//! from `base` to `end`. A push that would exceed the size drops the oldest bytes and records a
//! gap, so the child is never blocked by a slow reader.

use std::collections::VecDeque;

/// What a read from a cursor finds.
#[derive(Debug, PartialEq, Eq)]
pub enum Read {
    /// The cursor is below the oldest kept byte: these bytes were lost. Read again from `to`.
    Gap { from: u64, to: u64 },
    /// Up to the asked number of bytes from the cursor. Empty at the end.
    Bytes(Vec<u8>),
}

#[derive(Debug)]
pub struct ReplayBuffer {
    cap: usize,
    data: VecDeque<u8>,
    base: u64,
    gaps: Vec<(u64, u64)>,
}

impl ReplayBuffer {
    /// A buffer that keeps at most `cap` bytes (at least one).
    pub fn new(cap: usize) -> ReplayBuffer {
        ReplayBuffer {
            cap: cap.max(1),
            data: VecDeque::new(),
            base: 0,
            gaps: Vec::new(),
        }
    }

    /// The cursor of the oldest kept byte.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// The cursor after the newest byte: the number of bytes the child has written.
    pub fn end(&self) -> u64 {
        self.base + self.data.len() as u64
    }

    /// The bytes kept.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Every stretch dropped by an overflow, in order, as (from, to) cursors.
    pub fn gaps(&self) -> &[(u64, u64)] {
        &self.gaps
    }

    /// Add output. An overflow drops the oldest bytes and records one gap for them.
    pub fn push(&mut self, bytes: &[u8]) {
        self.data.extend(bytes);
        if self.data.len() > self.cap {
            let over = self.data.len() - self.cap;
            self.data.drain(..over);
            let from = self.base;
            self.base += over as u64;
            match self.gaps.last_mut() {
                Some(last) if last.1 == from => last.1 = self.base,
                _ => self.gaps.push((from, self.base)),
            }
        }
    }

    /// Acknowledge output below `to`: the buffer drops it. A cursor past the end counts as the end,
    /// and one below the base changes nothing.
    pub fn ack(&mut self, to: u64) {
        let to = to.min(self.end());
        if to > self.base {
            self.data.drain(..(to - self.base) as usize);
            self.base = to;
        }
    }

    /// Up to `max` bytes from `from`, or the gap in front of them. `from` must not exceed the end.
    pub fn read(&self, from: u64, max: usize) -> Read {
        if from < self.base {
            return Read::Gap {
                from,
                to: self.base,
            };
        }
        let start = (from - self.base) as usize;
        let n = max.min(self.data.len().saturating_sub(start));
        let (a, b) = self.data.as_slices();
        let mut out = Vec::with_capacity(n);
        if start < a.len() {
            let take = n.min(a.len() - start);
            out.extend_from_slice(&a[start..start + take]);
            if take < n {
                out.extend_from_slice(&b[..n - take]);
            }
        } else if n > 0 {
            out.extend_from_slice(&b[start - a.len()..start - a.len() + n]);
        }
        Read::Bytes(out)
    }
}
