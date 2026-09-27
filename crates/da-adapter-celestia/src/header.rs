//! Map a Celestia extended header into the sampling node's [`Header`].
//!
//! The commitment is the data root: the CometBFT Merkle root of the data
//! availability header's row roots followed by its column roots. That root is
//! the block's `data_hash`. Share count is the extended square, `width * width`.

use base64::Engine;
use da_light_core::{Commitment, DaError, Header, HeaderId};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) struct ParsedHeader {
    pub header: Header,
    pub row_roots: Vec<Vec<u8>>,
    pub column_roots: Vec<Vec<u8>>,
}

pub fn extended_header_to_header(value: &Value) -> Result<Header, DaError> {
    Ok(parse_extended_header(value)?.header)
}

pub(crate) fn parse_extended_header(value: &Value) -> Result<ParsedHeader, DaError> {
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

    let mut leaves = row_roots.clone();
    leaves.extend(column_roots.iter().cloned());
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

    Ok(ParsedHeader {
        header: Header {
            id: HeaderId(id.to_ascii_uppercase()),
            height,
            commitment: Commitment(data_root.to_vec()),
            total_shares,
        },
        row_roots,
        column_roots,
    })
}

/// CometBFT `merkle.HashFromByteSlices`: SHA-256 with `0x00` on leaves and
/// `0x01` on internal nodes, split at the largest power of two below the length.
pub(crate) fn hash_from_byte_slices(items: &[Vec<u8>]) -> [u8; 32] {
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

pub(crate) fn split_point(length: usize) -> usize {
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

/// Sibling hashes authenticating `items[index]` under [`hash_from_byte_slices`].
///
/// Hashes are ordered from the leaf toward the root. Each sibling's side is
/// determined by `index`, so a proof for a different leaf does not verify.
pub(crate) fn data_root_siblings(
    items: &[Vec<u8>],
    index: usize,
) -> Result<Vec<[u8; 32]>, DaError> {
    if items.is_empty() || index >= items.len() {
        return Err(DaError::CoordinateOutOfRange);
    }
    let mut siblings = Vec::new();
    collect_siblings(items, index, &mut siblings);
    Ok(siblings)
}

pub(crate) fn data_root_from_leaf(
    leaf: &[u8],
    index: usize,
    leaf_count: usize,
    siblings: &[[u8; 32]],
) -> Result<[u8; 32], DaError> {
    let mut sides = Vec::new();
    record_sides(leaf_count, index, &mut sides)?;
    if sides.len() != siblings.len() {
        return Err(DaError::InvalidProof);
    }
    let mut acc = leaf_hash(leaf);
    for (sibling_on_left, sibling) in sides.into_iter().zip(siblings) {
        acc = if sibling_on_left {
            inner_hash(sibling, &acc)
        } else {
            inner_hash(&acc, sibling)
        };
    }
    Ok(acc)
}

fn collect_siblings(items: &[Vec<u8>], index: usize, out: &mut Vec<[u8; 32]>) {
    if items.len() <= 1 {
        return;
    }
    let split = split_point(items.len());
    if index < split {
        collect_siblings(&items[..split], index, out);
        out.push(hash_from_byte_slices(&items[split..]));
    } else {
        let left = hash_from_byte_slices(&items[..split]);
        collect_siblings(&items[split..], index - split, out);
        out.push(left);
    }
}

fn record_sides(len: usize, index: usize, out: &mut Vec<bool>) -> Result<(), DaError> {
    if len == 0 || index >= len {
        return Err(DaError::InvalidProof);
    }
    if len <= 1 {
        return Ok(());
    }
    let split = split_point(len);
    if index < split {
        record_sides(split, index, out)?;
        out.push(false);
    } else {
        record_sides(len - split, index - split, out)?;
        out.push(true);
    }
    Ok(())
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
    fn data_root_proof_opens_every_axis_root() {
        let value: Value = serde_json::from_str(MIN_HEADER).unwrap();
        let parsed = parse_extended_header(&value).unwrap();
        let mut leaves = parsed.row_roots.clone();
        leaves.extend(parsed.column_roots.iter().cloned());
        for (index, leaf) in leaves.iter().enumerate() {
            let siblings = data_root_siblings(&leaves, index).unwrap();
            let root = data_root_from_leaf(leaf, index, leaves.len(), &siblings).unwrap();
            assert_eq!(root.as_slice(), parsed.header.commitment.0.as_slice());
        }
    }

    #[test]
    fn rejects_a_data_hash_that_does_not_match_the_roots() {
        let mut value: Value = serde_json::from_str(MIN_HEADER).unwrap();
        value["header"]["data_hash"] = Value::String("00".repeat(32));
        let err = extended_header_to_header(&value).unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }
}
