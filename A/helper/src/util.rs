//! Utility helpers shared by client and server.

use core::time::Duration;
use rand::RngCore;

/// Unix time in nanoseconds since epoch as u64.
/// Uses standard library's time facilities; intended for timestamping messages.
pub fn now_unix_nanos() -> u64 {
    use std::time::SystemTime;
    let now = SystemTime::now();
    let since = now.duration_since(SystemTime::UNIX_EPOCH).unwrap_or(Duration::from_secs(0));
    since.as_secs() * 1_000_000_000 + since.subsec_nanos() as u64
}

/// Generate a random 32-bit session ID.
pub fn random_session_id() -> u32 {
    let mut rng = rand::rng();
    rng.next_u32()
}

use core::sync::atomic::{AtomicU32, Ordering};

/// A simple atomic counter for sequence numbers.
#[derive(Debug, Default)]
pub struct SeqCounter(AtomicU32);

impl SeqCounter {
    pub fn new() -> Self { Self(AtomicU32::new(0)) }
    /// Get current value.
    pub fn get(&self) -> u32 { self.0.load(Ordering::Relaxed) }
    /// Post-increment: returns previous value, then increments.
    pub fn next(&self) -> u32 { self.0.fetch_add(1, Ordering::AcqRel) }
    /// Reset to 0.
    pub fn reset(&self) { self.0.store(0, Ordering::Release) }
}
