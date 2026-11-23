//! Protocol shared by collect mode and the CLI side.
//!
//! A session is a sequence of frames in each direction over a byte stream
//! (stdin and stdout of collect mode). Every frame is a 4-byte big-endian
//! length followed by that many bytes of payload. The payload is one message
//! in a compact binary encoding (see [`codec`]).
//!
//! The first frame in each direction is a hello carrying the protocol version.
//! Its encoding never changes, so two different versions can always detect the
//! mismatch and report it.

pub mod codec;
mod frame;
mod message;

pub use frame::{read_frame, write_frame};
pub use message::*;

// Protocol version. Increase on every incompatible change of a message.
pub const PROTOCOL_VERSION: u32 = 1;

// Marker at the start of every hello, so a peer that is not Parity (or a shell
// printing a banner) is recognised as such.
pub const MAGIC: [u8; 4] = *b"PRTY";

// Largest frame either side accepts.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

// Largest frame collect mode accepts from the CLI. Requests are small.
pub const MAX_REQUEST_FRAME: usize = 1024 * 1024;

// Size of the chunks file contents are sent in.
pub const CHUNK_SIZE: usize = 256 * 1024;

// Errors while reading, writing or decoding messages.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    // The stream ended in the middle of a frame.
    Truncated,
    FrameTooLarge(usize),
    // The payload ended before the message was complete.
    ShortMessage,
    TrailingBytes,
    UnknownTag { what: &'static str, tag: u8 },
    InvalidUtf8,
    BadMagic,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Truncated => write!(f, "stream ended in the middle of a frame"),
            Error::FrameTooLarge(n) => write!(f, "frame of {n} bytes exceeds the limit"),
            Error::ShortMessage => write!(f, "message is incomplete"),
            Error::TrailingBytes => write!(f, "message has trailing bytes"),
            Error::UnknownTag { what, tag } => write!(f, "unknown {what} tag {tag}"),
            Error::InvalidUtf8 => write!(f, "string is not valid UTF-8"),
            Error::BadMagic => write!(f, "peer does not speak the Parity protocol"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
