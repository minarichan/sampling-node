//! Namespace Merkle proofs for one Celestia share.
//!
//! A row or column root is an NMT root. The erasure-coded tree prepends a
//! namespace to each share before hashing: the share's own namespace in the
//! original quadrant, and the parity namespace (`0xFF` repeated) everywhere
//! else. Inner nodes ignore that parity namespace when computing their range
//! (`IgnoreMaxNamespace`).

use std::collections::VecDeque;

use da_light_core::DaError;
use sha2::{Digest, Sha256};

use crate::header::split_point;

pub(crate) const NAMESPACE_SIZE: usize = 29;
pub(crate) const SHARE_SIZE: usize = 512;
const NODE_SIZE: usize = NAMESPACE_SIZE * 2 + 32;

pub(crate) fn inclusion_namespace(
    share: &[u8],
    row: usize,
    col: usize,
    width: usize,
) -> Result<[u8; NAMESPACE_SIZE], DaError> {
    if share.len() != SHARE_SIZE || width < 2 || width % 2 != 0 {
        return Err(DaError::InvalidProof);
    }
    let original = width / 2;
    if row >= original || col >= original {
        return Ok([0xff; NAMESPACE_SIZE]);
    }
    let mut namespace = [0u8; NAMESPACE_SIZE];
    namespace.copy_from_slice(&share[..NAMESPACE_SIZE]);
    Ok(namespace)
}

/// Recompute the row or column root that commits `share`.
pub(crate) fn nmt_root(
    share: &[u8],
    namespace: &[u8],
    start: usize,
    end: usize,
    nodes: &[Vec<u8>],
    ignore_max_namespace: bool,
) -> Result<Vec<u8>, DaError> {
    if end != start + 1 {
        return Err(DaError::InvalidProof);
    }
    let leaf = hash_leaf(namespace, share)?;
    let estimate = subtree_estimate(end)?;
    let mut leaves = VecDeque::from([leaf]);
    let mut proof_nodes: VecDeque<Vec<u8>> = nodes.iter().cloned().collect();
    let walked = walk(
        0,
        estimate,
        start,
        end,
        &mut leaves,
        &mut proof_nodes,
        ignore_max_namespace,
    )?;
    let mut root = walked.ok_or(DaError::InvalidProof)?;
    for node in proof_nodes {
        root = hash_node(&root, &node, ignore_max_namespace)?;
    }
    if !leaves.is_empty() {
        return Err(DaError::InvalidProof);
    }
    Ok(root)
}

fn hash_leaf(namespace: &[u8], share: &[u8]) -> Result<Vec<u8>, DaError> {
    if namespace.len() != NAMESPACE_SIZE || share.len() != SHARE_SIZE {
        return Err(DaError::InvalidProof);
    }
    let mut prefixed = Vec::with_capacity(NAMESPACE_SIZE + SHARE_SIZE);
    prefixed.extend_from_slice(namespace);
    prefixed.extend_from_slice(share);

    let mut hasher = Sha256::new();
    hasher.update([0x00]);
    hasher.update(&prefixed);
    let digest: [u8; 32] = hasher.finalize().into();

    let mut out = Vec::with_capacity(NODE_SIZE);
    out.extend_from_slice(namespace);
    out.extend_from_slice(namespace);
    out.extend_from_slice(&digest);
    Ok(out)
}

fn hash_node(left: &[u8], right: &[u8], ignore_max_namespace: bool) -> Result<Vec<u8>, DaError> {
    let (left_min, left_max) = namespace_range(left)?;
    let (right_min, right_max) = namespace_range(right)?;
    if right_min < left_max {
        return Err(DaError::InvalidProof);
    }
    let max = if ignore_max_namespace && right_min.iter().all(|byte| *byte == 0xff) {
        left_max
    } else {
        right_max
    };

    let mut hasher = Sha256::new();
    hasher.update([0x01]);
    hasher.update(left);
    hasher.update(right);
    let digest: [u8; 32] = hasher.finalize().into();

    let mut out = Vec::with_capacity(NODE_SIZE);
    out.extend_from_slice(left_min);
    out.extend_from_slice(max);
    out.extend_from_slice(&digest);
    Ok(out)
}

fn namespace_range(node: &[u8]) -> Result<(&[u8], &[u8]), DaError> {
    if node.len() != NODE_SIZE {
        return Err(DaError::InvalidProof);
    }
    let min = &node[..NAMESPACE_SIZE];
    let max = &node[NAMESPACE_SIZE..NAMESPACE_SIZE * 2];
    if max < min {
        return Err(DaError::InvalidProof);
    }
    Ok((min, max))
}

fn subtree_estimate(end: usize) -> Result<usize, DaError> {
    if end == 0 {
        return Err(DaError::InvalidProof);
    }
    if end == 1 {
        return Ok(1);
    }
    split_point(end).checked_mul(2).ok_or(DaError::InvalidProof)
}

fn walk(
    start: usize,
    end: usize,
    proof_start: usize,
    proof_end: usize,
    leaves: &mut VecDeque<Vec<u8>>,
    nodes: &mut VecDeque<Vec<u8>>,
    ignore_max_namespace: bool,
) -> Result<Option<Vec<u8>>, DaError> {
    if end <= start {
        return Err(DaError::InvalidProof);
    }
    if end - start == 1 {
        if start >= proof_start && start < proof_end {
            return Ok(Some(leaves.pop_front().ok_or(DaError::InvalidProof)?));
        }
        return Ok(nodes.pop_front());
    }
    if end <= proof_start || start >= proof_end {
        return Ok(nodes.pop_front());
    }
    let split = split_point(end - start);
    if split == 0 || split >= end - start {
        return Err(DaError::InvalidProof);
    }
    let left = walk(
        start,
        start + split,
        proof_start,
        proof_end,
        leaves,
        nodes,
        ignore_max_namespace,
    )?;
    let right = walk(
        start + split,
        end,
        proof_start,
        proof_end,
        leaves,
        nodes,
        ignore_max_namespace,
    )?;
    match (left, right) {
        (Some(left), None) => Ok(Some(left)),
        (Some(left), Some(right)) => Ok(Some(hash_node(&left, &right, ignore_max_namespace)?)),
        (None, _) => Err(DaError::InvalidProof),
    }
}

#[cfg(test)]
pub(crate) struct BuiltAxis {
    pub root: Vec<u8>,
    pub leaf_hashes: Vec<Vec<u8>>,
}

#[cfg(test)]
pub(crate) fn build_axis(
    shares: &[Vec<u8>],
    square_size: usize,
    axis_index: usize,
) -> Result<BuiltAxis, DaError> {
    let mut leaf_hashes = Vec::with_capacity(shares.len());
    let mut previous: Option<Vec<u8>> = None;
    for (position, share) in shares.iter().enumerate() {
        let in_original = position < square_size && axis_index < square_size;
        let namespace = if in_original {
            share
                .get(..NAMESPACE_SIZE)
                .ok_or(DaError::InvalidProof)?
                .to_vec()
        } else {
            vec![0xff; NAMESPACE_SIZE]
        };
        if let Some(previous) = &previous {
            if namespace.as_slice() < previous.as_slice() {
                return Err(DaError::InvalidProof);
            }
        }
        previous = Some(namespace.clone());
        leaf_hashes.push(hash_leaf(&namespace, share)?);
    }
    let root = root_of(&leaf_hashes)?;
    Ok(BuiltAxis { root, leaf_hashes })
}

#[cfg(test)]
pub(crate) fn prove_share(leaf_hashes: &[Vec<u8>], index: usize) -> Result<Vec<Vec<u8>>, DaError> {
    if leaf_hashes.is_empty() || index >= leaf_hashes.len() {
        return Err(DaError::InvalidProof);
    }
    let mut proof = Vec::new();
    let mut full = split_point(leaf_hashes.len()).saturating_mul(2);
    if full < 1 {
        full = 1;
    }
    prove_walk(leaf_hashes, 0, full, index, index + 1, true, &mut proof)?;
    Ok(proof)
}

#[cfg(test)]
fn root_of(leaf_hashes: &[Vec<u8>]) -> Result<Vec<u8>, DaError> {
    fn rec(leaf_hashes: &[Vec<u8>], start: usize, end: usize) -> Result<Vec<u8>, DaError> {
        match end - start {
            1 => Ok(leaf_hashes[start].clone()),
            span => {
                let split = split_point(span);
                let left = rec(leaf_hashes, start, start + split)?;
                let right = rec(leaf_hashes, start + split, end)?;
                hash_node(&left, &right, true)
            }
        }
    }
    if leaf_hashes.is_empty() {
        return Err(DaError::InvalidProof);
    }
    rec(leaf_hashes, 0, leaf_hashes.len())
}

#[cfg(test)]
fn prove_walk(
    leaf_hashes: &[Vec<u8>],
    start: usize,
    end: usize,
    proof_start: usize,
    proof_end: usize,
    include_node: bool,
    proof: &mut Vec<Vec<u8>>,
) -> Result<Option<Vec<u8>>, DaError> {
    if start >= leaf_hashes.len() {
        return Ok(None);
    }
    if end - start == 1 {
        let leaf = leaf_hashes[start].clone();
        if (start < proof_start || start >= proof_end) && include_node {
            proof.push(leaf.clone());
        }
        return Ok(Some(leaf));
    }
    let mut descend = include_node;
    if (end <= proof_start || start >= proof_end) && include_node {
        descend = false;
    }
    let split = split_point(end - start);
    let left = prove_walk(
        leaf_hashes,
        start,
        start + split,
        proof_start,
        proof_end,
        descend,
        proof,
    )?;
    let right = prove_walk(
        leaf_hashes,
        start + split,
        end,
        proof_start,
        proof_end,
        descend,
        proof,
    )?;
    let hash = match (left, right) {
        (Some(left), None) => left,
        (Some(left), Some(right)) => hash_node(&left, &right, true)?,
        (None, _) => return Err(DaError::InvalidProof),
    };
    if include_node && !descend {
        proof.push(hash.clone());
    }
    Ok(Some(hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn share(namespace_byte: u8, fill: u8) -> Vec<u8> {
        let mut share = vec![fill; SHARE_SIZE];
        share[..NAMESPACE_SIZE].fill(namespace_byte);
        share
    }

    #[test]
    fn single_share_proofs_recompute_the_row_root() {
        let shares = vec![
            share(0x01, 0x11),
            share(0x02, 0x22),
            share(0x03, 0x33),
            share(0x04, 0x44),
        ];
        let axis = build_axis(&shares, 4, 0).unwrap();
        for (index, share) in shares.iter().enumerate() {
            let nodes = prove_share(&axis.leaf_hashes, index).unwrap();
            let namespace = &share[..NAMESPACE_SIZE];
            let root = nmt_root(share, namespace, index, index + 1, &nodes, true).unwrap();
            assert_eq!(root, axis.root);
        }
    }

    #[test]
    fn parity_namespace_does_not_widen_the_root() {
        let original = share(0x01, 0x11);
        let parity = vec![0xab; SHARE_SIZE];
        let shares = vec![original.clone(), parity.clone()];
        let axis = build_axis(&shares, 1, 0).unwrap();
        let nodes = prove_share(&axis.leaf_hashes, 1).unwrap();
        let root = nmt_root(&parity, &[0xff; NAMESPACE_SIZE], 1, 2, &nodes, true).unwrap();
        assert_eq!(root, axis.root);
        assert_eq!(
            &root[NAMESPACE_SIZE..NAMESPACE_SIZE * 2],
            &[0x01; NAMESPACE_SIZE]
        );

        let mut tampered = parity;
        tampered[NAMESPACE_SIZE] ^= 0xff;
        let wrong = nmt_root(&tampered, &[0xff; NAMESPACE_SIZE], 1, 2, &nodes, true).unwrap();
        assert_ne!(wrong, axis.root);
    }
}
