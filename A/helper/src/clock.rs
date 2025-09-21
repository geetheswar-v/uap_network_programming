//! Lamport logical clock for distributed events.

use core::sync::atomic::{AtomicU64, Ordering};

/// A threadsafe Lamport clock.
/// Use `tick()` for local events, `send_event()` before sending, and `recv_event()` on receive.
#[derive(Debug)]
pub struct LamportClock(AtomicU64);

impl Default for LamportClock {
    fn default() -> Self {
        Self(AtomicU64::new(0))
    }
}

impl LamportClock {
    /// Create a clock starting at 0.
    pub fn new() -> Self { Self::default() }

    /// Read the current logical time.
    #[inline]
    pub fn now(&self) -> u64 { self.0.load(Ordering::Relaxed) }

    /// Increment for a local event and return the new value.
    #[inline]
    pub fn tick(&self) -> u64 { self.0.fetch_add(1, Ordering::AcqRel) + 1 }

    /// Called immediately before sending a message. Increments and returns timestamp to embed.
    #[inline]
    pub fn send_event(&self) -> u64 { self.tick() }

    /// Called on receive with the sender's logical time; applies max rule and increments.
    pub fn recv_event(&self, remote_time: u64) -> u64 {
        // Update to max(local, remote)
        let mut current = self.0.load(Ordering::Acquire);
        loop {
            let target = current.max(remote_time);
            match self.0.compare_exchange(current, target, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        // Then increment for the receive event itself
        self.tick()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lamport_basic() {
        let c = LamportClock::new();
        assert_eq!(c.now(), 0);
        assert_eq!(c.tick(), 1);
        assert_eq!(c.send_event(), 2);
        // receive from remote 5 -> set to 5 then increment -> 6
        assert_eq!(c.recv_event(5), 6);
        assert_eq!(c.now(), 6);
    }
}
