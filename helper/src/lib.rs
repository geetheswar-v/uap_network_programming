//! Helper library for UAP protocol and Lamport logical clock.
//! Provides:
//! - UapHeader and UapMessage with binary encoding/decoding (big-endian)
//! - LamportClock implementation
//! - Common utilities for timing and IDs

pub mod uap;
pub mod clock;
pub mod util;

pub use clock::LamportClock;
pub use uap::{Command, UapHeader, UapMessage, HEADER_LEN, MAGIC, VERSION};
