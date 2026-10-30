//! Incremental reader of the shared sample ring: every consumer gets each new sample exactly once.

use crate::shared::*;

pub struct RingTap {
    last: u64,
    l: Vec<f32>,
    r: Vec<f32>,
}

const MAX_CHUNK: usize = 1 << 18;

impl RingTap {
    pub fn new() -> Self {
        Self { last: 0, l: Vec::new(), r: Vec::new() }
    }

    /// Forget everything pending (call when a consumer starts).
    pub fn sync(&mut self, sh: &Shared) {
        if let Some(r) = sh.ring.try_lock() {
            self.last = r.total;
        }
    }

    /// New samples since the previous call. Never blocks: returns nothing if the ring is busy.
    pub fn poll(&mut self, sh: &Shared) -> (&[f32], &[f32]) {
        let Some(ring) = sh.ring.try_lock() else { return (&[], &[]) };
        if ring.total < self.last {
            // the capture was restarted
            self.last = 0;
        }
        let mut n = (ring.total - self.last) as usize;
        if n > MAX_CHUNK {
            self.last = ring.total - MAX_CHUNK as u64;
            n = MAX_CHUNK;
        }
        if n == 0 {
            return (&[], &[]);
        }
        self.l.resize(n, 0.0);
        self.r.resize(n, 0.0);
        ring.copy_abs(self.last as usize, &mut self.l, &mut self.r);
        self.last = ring.total;
        (&self.l, &self.r)
    }
}
