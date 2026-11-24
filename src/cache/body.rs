//! The canonical fact-body codec: a one-byte codec header followed by the
//! postcard-encoded payload, content-addressed by the BLAKE3-256 digest of the
//! **encoded** bytes.
//!
//! Every value the shared fact store persists goes through [`encode`] and is
//! identified by its [`BodyId`]. The header names the codec so a future format
//! can sit beside postcard without re-keying the store; taking the id over the
//! encoded bytes (header included) means two byte-identical bodies share one
//! id, so the store deduplicates on content alone.

use anyhow::{Context, Result, bail};

/// The codec header byte for a postcard-encoded payload.
pub const CODEC_POSTCARD: u8 = 1;

/// The content address of one encoded body: BLAKE3-256 over its bytes.
pub type BodyId = [u8; 32];

/// One encoded body: its content address and the exact bytes it addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedBody {
    pub id: BodyId,
    pub bytes: Vec<u8>,
}

/// Encodes `value` as `CODEC_POSTCARD` + its postcard payload and addresses the
/// result by the BLAKE3-256 digest of those bytes.
pub fn encode<T: serde::Serialize>(value: &T) -> Result<EncodedBody> {
    let payload =
        postcard::to_allocvec(value).context("postcard-encoding the fact body payload")?;
    let mut bytes = Vec::with_capacity(1 + payload.len());
    bytes.push(CODEC_POSTCARD);
    bytes.extend_from_slice(&payload);
    let id = body_id(&bytes);
    Ok(EncodedBody { id, bytes })
}

/// Decodes one encoded body, rejecting a missing or unknown codec header and
/// then postcard-decoding the payload.
pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let Some((&header, payload)) = bytes.split_first() else {
        bail!("empty fact body: no codec header byte");
    };
    if header != CODEC_POSTCARD {
        bail!("unknown fact-body codec header {header} (expected CODEC_POSTCARD={CODEC_POSTCARD})");
    }
    postcard::from_bytes(payload).context("postcard-decoding the fact body payload")
}

/// The content address of an encoded body: BLAKE3-256 over the exact bytes.
pub fn body_id(bytes: &[u8]) -> BodyId {
    *blake3::hash(bytes).as_bytes()
}

#[cfg(test)]
mod tests;
