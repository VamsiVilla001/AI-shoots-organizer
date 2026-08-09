//! Embeddings are stored as raw little-endian `f32` blobs.
//!
//! SQLite has no native vector type and the libraries that add one are an
//! optional upgrade (see the architecture plan, §15). Until the player library
//! outgrows a linear scan, a plain blob keeps the schema portable and lets any
//! SQLite build open the file.

/// Packs an embedding into a blob for storage.
pub fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for value in v {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Unpacks a stored blob. Returns `None` if the blob is not a whole number of
/// `f32`s, which would mean the row was written by something else.
pub fn blob_to_vec(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips() {
        let v = vec![0.012_f32, -0.311, 0.882, 0.0, f32::MIN_POSITIVE];
        assert_eq!(blob_to_vec(&vec_to_blob(&v)).unwrap(), v);
    }

    #[test]
    fn rejects_ragged_blob() {
        assert!(blob_to_vec(&[1, 2, 3]).is_none());
    }
}
