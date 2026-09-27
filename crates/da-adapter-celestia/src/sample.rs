//! Fetch one share and check it against the header data root.
//!
//! `share.GetSamples` returns the share and a namespace Merkle proof of inclusion
//! in one row or column root. The sampling header only stores the data root, so
//! the stored proof also carries the CometBFT siblings that tie that axis root
//! to `header.commitment`.

use base64::Engine;
use da_light_core::{DaError, Header, HeaderId, Sample, SampleCoordinate, SampleProof};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::header::{data_root_from_leaf, data_root_siblings, parse_extended_header};
use crate::nmt::{inclusion_namespace, nmt_root};
use crate::rpc::CelestiaRpc;

const PROOF_VERSION: u8 = 1;

pub(crate) async fn request_sample(
    rpc: &CelestiaRpc,
    header_id: &HeaderId,
    coord: SampleCoordinate,
) -> Result<(Sample, SampleProof), DaError> {
    let raw = rpc.extended_header(header_id).await?;
    let parsed = parse_extended_header(&raw)?;
    let width = parsed.row_roots.len();
    if coord.row as usize >= width || coord.col as usize >= width {
        return Err(DaError::CoordinateOutOfRange);
    }

    let result = match rpc.samples(parsed.header.height, &[coord]).await {
        Ok(result) => result,
        Err(DaError::HeaderNotFound(_)) => {
            return Err(DaError::SampleUnavailable {
                row: coord.row,
                col: coord.col,
            });
        }
        Err(err) => return Err(err),
    };
    let fetched = match parse_rpc_sample(&result)? {
        Some(fetched) => fetched,
        None => {
            return Err(DaError::SampleUnavailable {
                row: coord.row,
                col: coord.col,
            });
        }
    };

    let mut leaves = parsed.row_roots;
    leaves.extend(parsed.column_roots);
    let axis_index = match fetched.axis {
        Axis::Row => coord.row as usize,
        Axis::Col => width + coord.col as usize,
    };
    let siblings = data_root_siblings(&leaves, axis_index)?;
    let stored = StoredProof {
        v: PROOF_VERSION,
        axis: fetched.axis,
        start: fetched.start,
        end: fetched.end,
        nodes: fetched
            .nodes
            .iter()
            .map(|node| base64::engine::general_purpose::STANDARD.encode(node))
            .collect(),
        ignore_max_namespace: fetched.ignore_max_namespace,
        siblings: siblings.iter().map(hex::encode).collect(),
    };
    let proof = serde_json::to_vec(&stored).map_err(|err| {
        DaError::Message(format!("could not encode celestia sample proof: {err}"))
    })?;
    Ok((Sample(fetched.share), SampleProof(proof)))
}

pub(crate) fn verify_sample(
    header: &Header,
    coord: SampleCoordinate,
    sample: &Sample,
    proof: &SampleProof,
) -> Result<(), DaError> {
    let stored: StoredProof =
        serde_json::from_slice(&proof.0).map_err(|_| DaError::InvalidProof)?;
    if stored.v != PROOF_VERSION {
        return Err(DaError::InvalidProof);
    }
    let width =
        usize::try_from(square_side(header.total_shares)?).map_err(|_| DaError::InvalidProof)?;
    if coord.row as usize >= width || coord.col as usize >= width {
        return Err(DaError::CoordinateOutOfRange);
    }
    let (expected_start, axis_index) = match stored.axis {
        Axis::Row => (coord.col, coord.row as usize),
        Axis::Col => (coord.row, width + coord.col as usize),
    };
    if stored.start != expected_start || stored.end != expected_start + 1 {
        return Err(DaError::InvalidProof);
    }

    let nodes = stored
        .nodes
        .iter()
        .map(|node| {
            base64::engine::general_purpose::STANDARD
                .decode(node)
                .map_err(|_| DaError::InvalidProof)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let siblings = stored
        .siblings
        .iter()
        .map(|sibling| {
            let bytes = hex::decode(sibling).map_err(|_| DaError::InvalidProof)?;
            <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| DaError::InvalidProof)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let namespace = inclusion_namespace(&sample.0, coord.row as usize, coord.col as usize, width)?;
    let axis_root = nmt_root(
        &sample.0,
        &namespace,
        stored.start as usize,
        stored.end as usize,
        &nodes,
        stored.ignore_max_namespace,
    )?;
    let data_root = data_root_from_leaf(&axis_root, axis_index, width * 2, &siblings)?;
    if data_root.as_slice() != header.commitment.0.as_slice() {
        return Err(DaError::InvalidProof);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Axis {
    Row,
    Col,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredProof {
    v: u8,
    axis: Axis,
    start: u32,
    end: u32,
    nodes: Vec<String>,
    ignore_max_namespace: bool,
    siblings: Vec<String>,
}

struct FetchedSample {
    share: Vec<u8>,
    axis: Axis,
    start: u32,
    end: u32,
    nodes: Vec<Vec<u8>>,
    ignore_max_namespace: bool,
}

fn parse_rpc_sample(result: &Value) -> Result<Option<FetchedSample>, DaError> {
    let samples = result.as_array().ok_or_else(|| {
        DaError::Message("celestia share.GetSamples result is not an array".into())
    })?;
    let Some(sample) = samples.first() else {
        return Ok(None);
    };
    match sample.get("proof") {
        None | Some(Value::Null) => return Ok(None),
        Some(_) => {}
    }
    let share = sample
        .get("share")
        .and_then(Value::as_str)
        .ok_or(DaError::InvalidProof)?;
    let share = base64::engine::general_purpose::STANDARD
        .decode(share)
        .map_err(|_| DaError::InvalidProof)?;
    let proof = &sample["proof"];
    let start = proof_index(proof.get("start"))?;
    let end = proof_index(proof.get("end"))?;
    let nodes = proof
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or(DaError::InvalidProof)?;
    let nodes = nodes
        .iter()
        .map(|node| {
            let text = node.as_str().ok_or(DaError::InvalidProof)?;
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(|_| DaError::InvalidProof)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ignore_max_namespace = proof
        .get("is_max_namespace_ignored")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(Some(FetchedSample {
        share,
        axis: axis_from_value(&sample["proof_type"])?,
        start,
        end,
        nodes,
        ignore_max_namespace,
    }))
}

fn proof_index(value: Option<&Value>) -> Result<u32, DaError> {
    match value {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(DaError::InvalidProof),
        _ => Err(DaError::InvalidProof),
    }
}

fn axis_from_value(value: &Value) -> Result<Axis, DaError> {
    match value {
        Value::Number(number) if number.as_u64() == Some(0) => Ok(Axis::Row),
        Value::Number(number) if number.as_u64() == Some(1) => Ok(Axis::Col),
        Value::String(text) if text.eq_ignore_ascii_case("row") => Ok(Axis::Row),
        Value::String(text) if text.eq_ignore_ascii_case("col") => Ok(Axis::Col),
        _ => Err(DaError::InvalidProof),
    }
}

fn square_side(total_shares: u32) -> Result<u32, DaError> {
    let side = da_light_core::square_width(total_shares);
    if side < 2 || side % 2 != 0 || side.saturating_mul(side) != total_shares {
        return Err(DaError::InvalidProof);
    }
    Ok(side)
}

#[cfg(test)]
pub(crate) fn rpc_sample_value(share: &[u8], axis: &str, index: usize, nodes: &[Vec<u8>]) -> Value {
    serde_json::json!({
        "share": base64::engine::general_purpose::STANDARD.encode(share),
        "proof": {
            "start": index,
            "end": index + 1,
            "nodes": nodes
                .iter()
                .map(|node| base64::engine::general_purpose::STANDARD.encode(node))
                .collect::<Vec<_>>(),
            "is_max_namespace_ignored": true,
        },
        "proof_type": if axis == "row" { 0 } else { 1 },
    })
}

/// Header and one row-proof sample for every cell of an extended square.
///
/// `width` is the extended square side. It must be even and at least 2.
/// Original-quadrant namespaces increase in both row and column order so each
/// axis tree is sorted.
#[cfg(test)]
pub(crate) struct ShareFixture {
    pub header: Value,
    pub samples: std::collections::HashMap<(u32, u32), Value>,
    pub block_hash: String,
}

#[cfg(test)]
pub(crate) fn share_fixture(width: usize) -> ShareFixture {
    use crate::header::hash_from_byte_slices;
    use crate::nmt::{build_axis, prove_share, NAMESPACE_SIZE, SHARE_SIZE};
    use base64::Engine;

    assert!(width >= 2 && width % 2 == 0, "extended width must be even");
    let original = width / 2;
    let mut shares = vec![vec![Vec::new(); width]; width];
    for row in 0..width {
        for col in 0..width {
            let mut share = vec![(row * width + col) as u8; SHARE_SIZE];
            if row < original && col < original {
                let namespace = u8::try_from(row * original + col + 1).expect("namespace fits");
                share[..NAMESPACE_SIZE].fill(namespace);
            }
            shares[row][col] = share;
        }
    }

    let row_roots = (0..width)
        .map(|row| {
            build_axis(&shares[row], original, row)
                .expect("row root")
                .root
        })
        .collect::<Vec<_>>();
    let column_roots = (0..width)
        .map(|col| {
            let column = (0..width)
                .map(|row| shares[row][col].clone())
                .collect::<Vec<_>>();
            build_axis(&column, original, col)
                .expect("column root")
                .root
        })
        .collect::<Vec<_>>();

    let mut samples = std::collections::HashMap::new();
    for row in 0..width {
        let built = build_axis(&shares[row], original, row).expect("row proof tree");
        for col in 0..width {
            let nodes = prove_share(&built.leaf_hashes, col).expect("row proof");
            samples.insert(
                (row as u32, col as u32),
                rpc_sample_value(&shares[row][col], "row", col, &nodes),
            );
        }
    }

    let row_b64 = row_roots
        .iter()
        .map(|root| base64::engine::general_purpose::STANDARD.encode(root))
        .collect::<Vec<_>>();
    let col_b64 = column_roots
        .iter()
        .map(|root| base64::engine::general_purpose::STANDARD.encode(root))
        .collect::<Vec<_>>();
    let mut leaves = row_roots;
    leaves.extend(column_roots);
    let block_hash = "CD".repeat(32);
    ShareFixture {
        header: serde_json::json!({
            "header": {
                "height": "4",
                "data_hash": hex::encode_upper(hash_from_byte_slices(&leaves)),
            },
            "commit": {"block_id": {"hash": block_hash}},
            "dah": {"row_roots": row_b64, "column_roots": col_b64},
        }),
        samples,
        block_hash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::hash_from_byte_slices;
    use crate::nmt::{build_axis, prove_share, NAMESPACE_SIZE, SHARE_SIZE};
    use da_light_core::Commitment;

    struct Square {
        shares: Vec<Vec<Vec<u8>>>,
        row_roots: Vec<Vec<u8>>,
        column_roots: Vec<Vec<u8>>,
        header: Header,
    }

    impl Square {
        fn new() -> Self {
            let width = 2;
            let original = 1;
            let mut shares = vec![vec![Vec::new(); width]; width];
            for row in 0..width {
                for col in 0..width {
                    let mut share = vec![(row * 16 + col) as u8; SHARE_SIZE];
                    if row < original && col < original {
                        share[..NAMESPACE_SIZE].fill(0x01);
                    }
                    shares[row][col] = share;
                }
            }
            let row_roots = (0..width)
                .map(|row| build_axis(&shares[row], original, row).unwrap().root)
                .collect::<Vec<_>>();
            let column_roots = (0..width)
                .map(|col| {
                    let column = (0..width)
                        .map(|row| shares[row][col].clone())
                        .collect::<Vec<_>>();
                    build_axis(&column, original, col).unwrap().root
                })
                .collect::<Vec<_>>();
            let mut leaves = row_roots.clone();
            leaves.extend(column_roots.iter().cloned());
            let data_root = hash_from_byte_slices(&leaves);
            Self {
                shares,
                row_roots,
                column_roots,
                header: Header {
                    id: HeaderId("AA".repeat(32)),
                    height: 7,
                    commitment: Commitment(data_root.to_vec()),
                    total_shares: (width * width) as u32,
                },
            }
        }

        fn sample(&self, row: usize, col: usize, axis: Axis) -> (Sample, SampleProof) {
            let share = self.shares[row][col].clone();
            let (leaf_hashes, index) = match axis {
                Axis::Row => {
                    let built = build_axis(&self.shares[row], 1, row).unwrap();
                    (built.leaf_hashes, col)
                }
                Axis::Col => {
                    let column = (0..self.shares.len())
                        .map(|r| self.shares[r][col].clone())
                        .collect::<Vec<_>>();
                    let built = build_axis(&column, 1, col).unwrap();
                    (built.leaf_hashes, row)
                }
            };
            let nodes = prove_share(&leaf_hashes, index).unwrap();
            let axis_name = match axis {
                Axis::Row => "row",
                Axis::Col => "col",
            };
            let body = serde_json::json!([rpc_sample_value(&share, axis_name, index, &nodes)]);
            let fetched = parse_rpc_sample(&body).unwrap().unwrap();
            let mut leaves = self.row_roots.clone();
            leaves.extend(self.column_roots.iter().cloned());
            let axis_index = match axis {
                Axis::Row => row,
                Axis::Col => self.shares.len() + col,
            };
            let siblings = data_root_siblings(&leaves, axis_index).unwrap();
            let stored = StoredProof {
                v: PROOF_VERSION,
                axis,
                start: fetched.start,
                end: fetched.end,
                nodes: fetched
                    .nodes
                    .iter()
                    .map(|node| base64::engine::general_purpose::STANDARD.encode(node))
                    .collect(),
                ignore_max_namespace: fetched.ignore_max_namespace,
                siblings: siblings.iter().map(hex::encode).collect(),
            };
            (
                Sample(fetched.share),
                SampleProof(serde_json::to_vec(&stored).unwrap()),
            )
        }
    }

    #[test]
    fn row_and_column_proofs_match_the_data_root() {
        let square = Square::new();
        for row in 0..2 {
            for col in 0..2 {
                for axis in [Axis::Row, Axis::Col] {
                    let (sample, proof) = square.sample(row, col, axis);
                    let coord = SampleCoordinate {
                        row: row as u32,
                        col: col as u32,
                    };
                    verify_sample(&square.header, coord, &sample, &proof).unwrap();
                }
            }
        }
    }

    #[test]
    fn a_changed_share_or_coordinate_fails_verification() {
        let square = Square::new();
        let coord = SampleCoordinate { row: 0, col: 1 };
        let (mut sample, proof) = square.sample(0, 1, Axis::Row);
        sample.0[40] ^= 0x01;
        assert_eq!(
            verify_sample(&square.header, coord, &sample, &proof).unwrap_err(),
            DaError::InvalidProof
        );

        let (sample, proof) = square.sample(0, 1, Axis::Row);
        let wrong = SampleCoordinate { row: 0, col: 0 };
        assert_eq!(
            verify_sample(&square.header, wrong, &sample, &proof).unwrap_err(),
            DaError::InvalidProof
        );
    }
}
