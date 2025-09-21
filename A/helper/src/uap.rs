//! UAP protocol types and binary encoding/decoding (big-endian).

use core::fmt;

/// Fixed magic value for all UAP messages.
pub const MAGIC: u16 = 0xC461;
/// Protocol version.
pub const VERSION: u8 = 1;
/// UAP header length in bytes.
pub const HEADER_LEN: usize = 2 + 1 + 1 + 4 + 4 + 8 + 8; // 28

/// UAP message command kind.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    Hello = 0,
    Data = 1,
    Alive = 2,
    Goodbye = 3,
}

impl Command {
    #[inline]
    pub fn from_u8(v: u8) -> Result<Self, UapError> {
        match v {
            0 => Ok(Command::Hello),
            1 => Ok(Command::Data),
            2 => Ok(Command::Alive),
            3 => Ok(Command::Goodbye),
            _ => Err(UapError::UnknownCommand(v)),
        }
    }
}

/// Wire header for every UAP message.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct UapHeader {
    pub magic: u16,
    pub version: u8,
    pub command: Command,
    pub sequence_number: u32,
    pub session_id: u32,
    pub logical_clock: u64,
    pub timestamp: u64,
}

impl UapHeader {
    /// Construct a header with the fixed magic and version.
    pub fn new(command: Command, sequence_number: u32, session_id: u32, logical_clock: u64, timestamp: u64) -> Self {
        Self {
            magic: MAGIC,
            version: VERSION,
            command,
            sequence_number,
            session_id,
            logical_clock,
            timestamp,
        }
    }

    /// Serialize the header to a 28-byte big-endian buffer.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut buf = [0u8; HEADER_LEN];
        let mut i = 0usize;
        // magic (u16)
        buf[i..i + 2].copy_from_slice(&self.magic.to_be_bytes());
        i += 2;
        // version (u8)
        buf[i] = self.version;
        i += 1;
        // command (u8)
        buf[i] = self.command as u8;
        i += 1;
        // sequence number (u32)
        buf[i..i + 4].copy_from_slice(&self.sequence_number.to_be_bytes());
        i += 4;
        // session id (u32)
        buf[i..i + 4].copy_from_slice(&self.session_id.to_be_bytes());
        i += 4;
        // logical clock (u64)
        buf[i..i + 8].copy_from_slice(&self.logical_clock.to_be_bytes());
        i += 8;
        // timestamp (u64)
        buf[i..i + 8].copy_from_slice(&self.timestamp.to_be_bytes());
        // i += 8;
        buf
    }

    /// Parse a header from the first 28 bytes of `data`.
    pub fn decode(data: &[u8]) -> Result<Self, UapError> {
        if data.len() < HEADER_LEN {
            return Err(UapError::TruncatedHeader);
        }
        let mut i = 0usize;
        let magic = u16::from_be_bytes([data[i], data[i + 1]]);
        i += 2;
        if magic != MAGIC {
            return Err(UapError::InvalidMagic(magic));
        }
        let version = data[i];
        i += 1;
        if version != VERSION {
            return Err(UapError::UnsupportedVersion(version));
        }
        let cmd_raw = data[i];
        i += 1;
        let command = Command::from_u8(cmd_raw)?;
        let sequence_number = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
        i += 4;
        let session_id = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
        i += 4;
        let logical_clock = u64::from_be_bytes([
            data[i], data[i + 1], data[i + 2], data[i + 3], data[i + 4], data[i + 5], data[i + 6], data[i + 7],
        ]);
        i += 8;
        let timestamp = u64::from_be_bytes([
            data[i], data[i + 1], data[i + 2], data[i + 3], data[i + 4], data[i + 5], data[i + 6], data[i + 7],
        ]);
        Ok(Self { magic, version, command, sequence_number, session_id, logical_clock, timestamp })
    }
}

/// A full UAP message (header + optional data payload).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UapMessage {
    pub header: UapHeader,
    pub payload: Option<Vec<u8>>, // only for DATA
}

impl UapMessage {
    /// Create a HELLO message (no payload).
    pub fn hello(sequence_number: u32, session_id: u32, logical_clock: u64, timestamp: u64) -> Self {
        Self { header: UapHeader::new(Command::Hello, sequence_number, session_id, logical_clock, timestamp), payload: None }
    }
    /// Create an ALIVE message (no payload).
    pub fn alive(sequence_number: u32, session_id: u32, logical_clock: u64, timestamp: u64) -> Self {
        Self { header: UapHeader::new(Command::Alive, sequence_number, session_id, logical_clock, timestamp), payload: None }
    }
    /// Create a GOODBYE message (no payload).
    pub fn goodbye(sequence_number: u32, session_id: u32, logical_clock: u64, timestamp: u64) -> Self {
        Self { header: UapHeader::new(Command::Goodbye, sequence_number, session_id, logical_clock, timestamp), payload: None }
    }
    /// Create a DATA message with payload.
    pub fn data(sequence_number: u32, session_id: u32, logical_clock: u64, timestamp: u64, payload: Vec<u8>) -> Self {
        Self { header: UapHeader::new(Command::Data, sequence_number, session_id, logical_clock, timestamp), payload: Some(payload) }
    }

    /// Encode to a single UDP-ready buffer (one message per packet).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.as_ref().map(|p| p.len()).unwrap_or(0));
        out.extend_from_slice(&self.header.encode());
        if self.header.command == Command::Data {
            if let Some(p) = &self.payload {
                out.extend_from_slice(p);
            }
        }
        out
    }

    /// Decode a message from bytes; validates magic/version and command.
    pub fn decode(data: &[u8]) -> Result<Self, UapError> {
        let header = UapHeader::decode(data)?;
        let payload = match header.command {
            Command::Data => Some(data[HEADER_LEN..].to_vec()),
            _ => None,
        };
        Ok(Self { header, payload })
    }
}

/// Errors that can occur while decoding or validating UAP messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UapError {
    TruncatedHeader,
    InvalidMagic(u16),
    UnsupportedVersion(u8),
    UnknownCommand(u8),
}

impl fmt::Display for UapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UapError::TruncatedHeader => write!(f, "truncated header: expected {} bytes", HEADER_LEN),
            UapError::InvalidMagic(m) => write!(f, "invalid magic: 0x{m:04X}"),
            UapError::UnsupportedVersion(v) => write!(f, "unsupported version: {v}"),
            UapError::UnknownCommand(c) => write!(f, "unknown command: {c}"),
        }
    }
}

impl std::error::Error for UapError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let hdr = UapHeader::new(Command::Data, 42, 0xDEAD_BEEF, 123, 456);
        let bytes = hdr.encode();
        assert_eq!(bytes.len(), HEADER_LEN);
        let parsed = UapHeader::decode(&bytes).unwrap();
        assert_eq!(hdr, parsed);
    }

    #[test]
    fn message_roundtrip_with_payload() {
        let payload = b"hello world".to_vec();
        let msg = UapMessage::data(1, 2, 3, 4, payload.clone());
        let bytes = msg.encode();
        let parsed = UapMessage::decode(&bytes).unwrap();
        assert_eq!(parsed.header.command, Command::Data);
        assert_eq!(parsed.payload.unwrap(), payload);
    }

    #[test]
    fn invalid_magic() {
        let mut bytes = [0u8; HEADER_LEN];
        // wrong magic
        bytes[0..2].copy_from_slice(&0xABCDu16.to_be_bytes());
        bytes[2] = VERSION;
        bytes[3] = Command::Hello as u8;
        assert!(matches!(UapHeader::decode(&bytes), Err(UapError::InvalidMagic(0xABCD))));
    }
}
