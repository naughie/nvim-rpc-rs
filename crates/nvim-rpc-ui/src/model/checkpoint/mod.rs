mod validate;
mod value;
pub(in crate::model) mod wire;

use super::{Limits, State};

use std::fmt;
use std::sync::Arc;

const MAGIC: &[u8] = b"NRUI";
// Version 2 uses Postcard. The field order and enum tags of serializable
// model types are part of the format.
const VERSION: u8 = 2;
const MAX_VALUE_DEPTH: usize = 96;
const MAX_WIRE_LENGTH: usize = u32::MAX as usize;
const WIRE_LIMITS: Limits = Limits {
    max_grids: MAX_WIRE_LENGTH,
    max_cells: usize::MAX,
    max_string_bytes: MAX_WIRE_LENGTH,
    max_collection_len: MAX_WIRE_LENGTH,
    max_depth: MAX_VALUE_DEPTH,
    max_pending_events: 0,
    max_messages: MAX_WIRE_LENGTH,
};

/// A committed display state that can initialize another UI attachment.
///
/// A checkpoint retains hidden grids and the rest of the display state. Frame
/// sequences, damage, and one-shot events are outside the checkpoint. A
/// checkpoint exported by a reference model can also retain grids and
/// placements that were absent from its most recent frame. Use
/// [`Model::checkpoint()`](crate::Model::checkpoint) or
/// [`Frames::checkpoint()`](crate::Frames::checkpoint) to capture one.
///
/// Persist the bytes from [`Self::encode()`] as an opaque record. An `NRUI`
/// header and one-byte format version precede the [Postcard] payload. The
/// decoder accepts format version 2 and rejects other versions.
/// The application chooses where to store the bytes and must match the
/// checkpoint to the same running Neovim process before restoring it.
///
/// [Postcard]: https://postcard.jamesmunns.com/wire-format
#[derive(Debug, Clone)]
pub struct Checkpoint(Arc<State>);

impl Checkpoint {
    pub(crate) fn from_committed(state: Arc<State>) -> Self {
        Self(state)
    }

    pub(crate) fn into_state(self, limits: Limits) -> Result<Arc<State>, CheckpointError> {
        validate::state(&self.0, limits)?;
        Ok(self.0)
    }

    /// Borrows the checkpoint's retained display state.
    ///
    /// When captured in reference mode, this state can contain retained data
    /// that does not appear in the latest frame.
    pub fn state(&self) -> &State {
        &self.0
    }

    /// Encodes the complete checkpoint for caller-managed storage.
    ///
    /// Returns [`CheckpointError::Limit`] when retained values exceed the
    /// format's maximum nesting depth of 96, or the encoded record or a field
    /// exceeds the format's 32-bit byte or element count.
    pub fn encode(&self) -> Result<Vec<u8>, CheckpointError> {
        validate::state(&self.0, WIRE_LIMITS)?;

        let mut bytes = MAGIC.to_vec();
        bytes.push(VERSION);
        let bytes = postcard::to_extend(self.state(), bytes).map_err(CheckpointError::Encode)?;
        if bytes.len() > MAX_WIRE_LENGTH {
            return Err(CheckpointError::Limit("encoded bytes"));
        }
        Ok(bytes)
    }

    /// Decodes a checkpoint within a byte budget and the destination model's limits.
    ///
    /// `max_bytes` bounds the complete encoded record, including its header.
    /// The decoder reads one complete payload and checks the format's nesting
    /// bound while decoding. The resulting state must satisfy `limits`. Save
    /// the output of [`Self::encode()`] directly to preserve its representation.
    pub fn decode(bytes: &[u8], limits: Limits, max_bytes: usize) -> Result<Self, CheckpointError> {
        if bytes.len() > max_bytes.min(MAX_WIRE_LENGTH) {
            return Err(CheckpointError::Limit("encoded bytes"));
        }
        let Some((&version, payload)) = bytes.strip_prefix(MAGIC).and_then(<[u8]>::split_first)
        else {
            return Err(CheckpointError::Invalid("header"));
        };
        if version != VERSION {
            return Err(CheckpointError::IncompatibleVersion(version));
        }

        let (state, remainder) =
            postcard::take_from_bytes(payload).map_err(CheckpointError::Decode)?;
        if !remainder.is_empty() {
            return Err(CheckpointError::Invalid("trailing bytes"));
        }
        validate::state(&state, limits)?;
        wire::verify_payload(&state, payload)?;
        Ok(Self(Arc::new(state)))
    }
}

/// A display checkpoint could not be encoded, decoded, or restored.
#[derive(Debug)]
pub enum CheckpointError {
    /// The record uses an unsupported format version.
    IncompatibleVersion(u8),
    /// The record or the state it contains is invalid.
    Invalid(&'static str),
    /// A resource limit was exceeded.
    Limit(&'static str),
    /// The Postcard payload could not be written.
    Encode(postcard::Error),
    /// The Postcard payload could not be parsed as a checkpoint.
    Decode(postcard::Error),
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompatibleVersion(version) => {
                write!(f, "unsupported UI checkpoint version {version}")
            }
            Self::Invalid(reason) => write!(f, "invalid UI checkpoint: {reason}"),
            Self::Limit(name) => write!(f, "UI checkpoint resource limit exceeded: {name}"),
            Self::Encode(e) => write!(f, "could not encode UI checkpoint: {e}"),
            Self::Decode(e) => write!(f, "could not decode UI checkpoint: {e}"),
        }
    }
}

impl std::error::Error for CheckpointError {}
