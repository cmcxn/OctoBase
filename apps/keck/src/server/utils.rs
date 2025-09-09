use std::io::Write;

use jwst_codec::{CrdtWriter, JwstCodecError, JwstCodecResult, RawEncoder};

pub use jwst_logger::{debug, error, info, warn};
pub use nanoid::nanoid;
pub use serde::{Deserialize, Serialize};

pub fn encode_update_with_guid<S: AsRef<str>>(update: &[u8], guid: S) -> JwstCodecResult<Vec<u8>> {
    let mut encoder = RawEncoder::default();
    encoder.write_var_string(guid)?;
    let mut buffer = encoder.into_inner();

    buffer
        .write_all(update)
        .map_err(|e| JwstCodecError::InvalidWriteBuffer(e.to_string()))?;

    Ok(buffer)
}
