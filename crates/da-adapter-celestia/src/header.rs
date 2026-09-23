//! Map a Celestia extended header into the sampling node's [`Header`].
//!
//! The commitment is the data root: the CometBFT Merkle root of the data
//! availability header's row roots followed by its column roots. That root is
//! the block's `data_hash`. Share count is the extended square, `width * width`.

use base64::Engine;
use da_light_core::{Commitment, DaError, Header, HeaderId};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn extended_header_to_header(value: &Value) -> Result<Header, DaError> {
    let height = json_u64(pointer(value, "/header/height")?)?;
    let block_hash = pointer(value, "/commit/block_id/hash")?
        .as_str()
        .ok_or_else(|| DaError::Message("celestia block hash is not a string".into()))?;
    let data_hash = decode_hex(
        pointer(value, "/header/data_hash")?
            .as_str()
            .ok_or_else(|| DaError::Message("celestia data_hash is not a string".into()))?,
    )?;
    let row_roots = decode_roots(pointer(value, "/dah/row_roots")?)?;
    let column_roots = decode_roots(pointer(value, "/dah/column_roots")?)?;
    if row_roots.is_empty() || row_roots.len() != column_roots.len() {
        return Err(DaError::Message(
            "celestia data availability header has unequal row and column roots".into(),
        ));
    }

    let mut leaves = row_roots;
    leaves.extend(column_roots);
    let data_root = hash_from_byte_slices(&leaves);
    if data_root != data_hash {
        return Err(DaError::Message(
            "computed data root does not match the celestia header data_hash".into(),
        ));
    }

    let width = leaves.len() / 2;
    let total_shares = u32::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(width))
        .ok_or_else(|| DaError::Message("celestia square is too large".into()))?;

    let id = block_hash.trim();
    if id.is_empty() {
        return Err(DaError::Message("celestia block hash is empty".into()));
    }

    Ok(Header {
        id: HeaderId(id.to_ascii_uppercase()),
        height,
        commitment: Commitment(data_root.to_vec()),
        total_shares,
    })
}

/// CometBFT `merkle.HashFromByteSlices`: SHA-256 with `0x00` on leaves and
/// `0x01` on internal nodes, split at the largest power of two below the length.
fn hash_from_byte_slices(items: &[Vec<u8>]) -> [u8; 32] {
    match items.len() {
        0 => sha256(&[]),
        1 => leaf_hash(&items[0]),
        n => {
            let split = split_point(n);
            let left = hash_from_byte_slices(&items[..split]);
            let right = hash_from_byte_slices(&items[split..]);
            inner_hash(&left, &right)
        }
    }
}

fn split_point(length: usize) -> usize {
    let bitlen = usize::BITS - length.leading_zeros();
    let mut split = 1usize << (bitlen - 1);
    if split == length {
        split >>= 1;
    }
    split
}

fn leaf_hash(leaf: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([0x00]);
    hasher.update(leaf);
    hasher.finalize().into()
}

fn inner_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update([0x01]);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn pointer<'a>(value: &'a Value, path: &str) -> Result<&'a Value, DaError> {
    value
        .pointer(path)
        .ok_or_else(|| DaError::Message(format!("celestia header is missing {path}")))
}

fn json_u64(value: &Value) -> Result<u64, DaError> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| DaError::Message("celestia height is out of range".into())),
        Value::String(text) => text
            .parse()
            .map_err(|_| DaError::Message(format!("celestia height is not an integer: {text}"))),
        _ => Err(DaError::Message("celestia height is not a number".into())),
    }
}

fn decode_roots(value: &Value) -> Result<Vec<Vec<u8>>, DaError> {
    let roots = value.as_array().ok_or_else(|| {
        DaError::Message("celestia data availability roots are not an array".into())
    })?;
    roots
        .iter()
        .map(|root| {
            let text = root.as_str().ok_or_else(|| {
                DaError::Message("celestia data availability root is not a string".into())
            })?;
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(|_| {
                    DaError::Message("celestia data availability root is not base64".into())
                })
        })
        .collect()
}

fn decode_hex(text: &str) -> Result<[u8; 32], DaError> {
    let bytes = hex::decode(text.trim())
        .map_err(|_| DaError::Message("celestia data_hash is not hex".into()))?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| DaError::Message("celestia data_hash must be 32 bytes".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN_HEADER: &str = r#"{
        "header": {
            "height": "1",
            "data_hash": "3D96B7D238E7E0456F6AF8E7CDF0A67BD6CF9C2089ECB559C659DCAA1F880353"
        },
        "commit": {
            "block_id": {
                "hash": "7A5FABB19713D732D967B1DA84FA0DF5E87A7B62302D783F78743E216C1A3550"
            }
        },
        "dah": {
            "row_roots": [
                "//////////////////////////////////////7//////////////////////////////////////huZWOTTDmD36N1F75A9BshxNlRasCnNpQiWqIhdVHcU",
                "/////////////////////////////////////////////////////////////////////////////5iieeroHBMfF+sER3JpvROIeEJZjbY+TRE0ntADQLL3"
            ],
            "column_roots": [
                "//////////////////////////////////////7//////////////////////////////////////huZWOTTDmD36N1F75A9BshxNlRasCnNpQiWqIhdVHcU",
                "/////////////////////////////////////////////////////////////////////////////5iieeroHBMfF+sER3JpvROIeEJZjbY+TRE0ntADQLL3"
            ]
        }
    }"#;

    #[test]
    fn maps_the_minimum_celestia_header() {
        let value: Value = serde_json::from_str(MIN_HEADER).unwrap();
        let header = extended_header_to_header(&value).unwrap();
        assert_eq!(header.height, 1);
        assert_eq!(header.total_shares, 4);
        assert_eq!(
            header.id.0,
            "7A5FABB19713D732D967B1DA84FA0DF5E87A7B62302D783F78743E216C1A3550"
        );
        assert_eq!(
            hex::encode_upper(&header.commitment.0),
            "3D96B7D238E7E0456F6AF8E7CDF0A67BD6CF9C2089ECB559C659DCAA1F880353"
        );
    }

    #[test]
    fn rejects_a_data_hash_that_does_not_match_the_roots() {
        let mut value: Value = serde_json::from_str(MIN_HEADER).unwrap();
        value["header"]["data_hash"] = Value::String("00".repeat(32));
        let err = extended_header_to_header(&value).unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }
}
