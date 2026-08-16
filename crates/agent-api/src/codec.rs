use serde::{Serialize, de::DeserializeOwned};

use crate::{MAX_FRAME_BYTES, WireContractError};

/// Encodes one bounded little-endian-length-prefixed JSON frame.
///
/// # Errors
///
/// Returns a contract error when serialization fails or the payload exceeds the v0.1 bound.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, WireContractError> {
    let payload = serde_json::to_vec(value).map_err(WireContractError::InvalidJson)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(WireContractError::FrameTooLarge);
    }
    let length = u32::try_from(payload.len()).map_err(|_| WireContractError::FrameTooLarge)?;
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decodes exactly one bounded little-endian-length-prefixed JSON frame.
///
/// # Errors
///
/// Returns a contract error before allocating the declared payload when its bound is exceeded.
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, WireContractError> {
    let prefix: [u8; 4] = frame
        .get(..4)
        .ok_or(WireContractError::IncompleteFrame)?
        .try_into()
        .map_err(|_| WireContractError::IncompleteFrame)?;
    let length = usize::try_from(u32::from_le_bytes(prefix))
        .map_err(|_| WireContractError::FrameTooLarge)?;
    if length > MAX_FRAME_BYTES {
        return Err(WireContractError::FrameTooLarge);
    }
    let payload = frame.get(4..).ok_or(WireContractError::IncompleteFrame)?;
    if payload.len() != length {
        return Err(WireContractError::IncompleteFrame);
    }
    serde_json::from_slice(payload).map_err(WireContractError::InvalidJson)
}
