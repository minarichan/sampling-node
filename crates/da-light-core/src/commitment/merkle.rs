//! Binary Merkle tree with domain-separated BLAKE3 hashes.
//!
//! Leaf nodes are hashed as `0x00 || data`. Internal nodes are hashed as
//! `0x01 || left || right`. Trees are padded to the next power of two with a
//! fixed zero hash so every real share has a uniform authentication path.
//! Padding nodes are not samples and are never requested.

use crate::error::DaError;
use crate::types::{share_index, SampleCoordinate};

const LEAF_TAG: u8 = 0x00;
const NODE_TAG: u8 = 0x01;
const PADDING: [u8; 32] = [0u8; 32];
const PROOF_MAGIC: &[u8; 4] = b"MRK1";
const MAX_PROOF_DEPTH: usize = 32;

/// Merkle authentication path for one leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerkleProof {
    pub index: u32,
    pub siblings: Vec<[u8; 32]>,
}

impl MerkleProof {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + self.siblings.len() * 32);
        out.extend_from_slice(PROOF_MAGIC);
        out.extend_from_slice(&self.index.to_le_bytes());
        out.extend_from_slice(&(self.siblings.len() as u32).to_le_bytes());
        for sibling in &self.siblings {
            out.extend_from_slice(sibling);
        }
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DaError> {
        if bytes.len() < 12 || &bytes[..4] != PROOF_MAGIC {
            return Err(DaError::InvalidProof);
        }
        let index = read_u32(&bytes[4..8]);
        let count = read_u32(&bytes[8..12]) as usize;
        if count > MAX_PROOF_DEPTH {
            return Err(DaError::InvalidProof);
        }
        let needed = 12 + count * 32;
        if bytes.len() != needed {
            return Err(DaError::InvalidProof);
        }
        let mut siblings = Vec::with_capacity(count);
        for offset in (12..needed).step_by(32) {
            let mut sibling = [0u8; 32];
            sibling.copy_from_slice(&bytes[offset..offset + 32]);
            siblings.push(sibling);
        }
        Ok(Self { index, siblings })
    }
}

/// Complete tree over a list of share payloads.
#[derive(Debug, Clone)]
pub struct MerkleTree {
    leaf_count: usize,
    levels: Vec<Vec<[u8; 32]>>,
}

impl MerkleTree {
    pub fn from_leaves<T: AsRef<[u8]>>(leaves: &[T]) -> Result<Self, DaError> {
        if leaves.is_empty() {
            return Err(DaError::Message(
                "merkle tree needs at least one leaf".into(),
            ));
        }
        let padded = leaves.len().next_power_of_two();
        let mut level = Vec::with_capacity(padded);
        for leaf in leaves {
            level.push(leaf_hash(leaf.as_ref()));
        }
        level.resize(padded, PADDING);

        let mut levels = vec![level];
        while levels.last().map(Vec::len).unwrap_or(0) > 1 {
            let previous = levels.last().expect("level was just pushed");
            let mut next = Vec::with_capacity(previous.len() / 2);
            for pair in previous.chunks_exact(2) {
                next.push(node_hash(&pair[0], &pair[1]));
            }
            levels.push(next);
        }

        Ok(Self {
            leaf_count: leaves.len(),
            levels,
        })
    }

    pub fn root(&self) -> [u8; 32] {
        self.levels
            .last()
            .and_then(|level| level.first())
            .copied()
            .expect("merkle tree always has a root")
    }

    pub fn prove(&self, index: usize) -> Result<MerkleProof, DaError> {
        if index >= self.leaf_count {
            return Err(DaError::CoordinateOutOfRange);
        }
        let mut siblings = Vec::new();
        let mut cursor = index;
        for level in &self.levels[..self.levels.len() - 1] {
            let sibling = cursor ^ 1;
            siblings.push(level[sibling]);
            cursor >>= 1;
        }
        Ok(MerkleProof {
            index: u32::try_from(index).unwrap_or(u32::MAX),
            siblings,
        })
    }
}

/// Verify `sample` against a 32-byte Merkle root at the share coordinate.
pub fn verify_commitment(
    commitment: &[u8],
    total_shares: u32,
    coord: SampleCoordinate,
    sample: &[u8],
    proof_bytes: &[u8],
) -> Result<(), DaError> {
    if commitment.len() != 32 {
        return Err(DaError::InvalidProof);
    }
    let mut root = [0u8; 32];
    root.copy_from_slice(commitment);
    let index = share_index(coord, total_shares)?;
    let proof = MerkleProof::from_bytes(proof_bytes)?;
    verify_path(&root, index, sample, &proof)
}

fn verify_path(
    root: &[u8; 32],
    index: u32,
    leaf: &[u8],
    proof: &MerkleProof,
) -> Result<(), DaError> {
    if proof.index != index {
        return Err(DaError::InvalidProof);
    }
    let mut hash = leaf_hash(leaf);
    let mut cursor = index;
    for sibling in &proof.siblings {
        hash = if cursor & 1 == 0 {
            node_hash(&hash, sibling)
        } else {
            node_hash(sibling, &hash)
        };
        cursor >>= 1;
    }
    if hash != *root {
        Err(DaError::InvalidProof)
    } else {
        Ok(())
    }
}

fn leaf_hash(data: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[LEAF_TAG]);
    hasher.update(data);
    *hasher.finalize().as_bytes()
}

fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[NODE_TAG]);
    hasher.update(left);
    hasher.update(right);
    *hasher.finalize().as_bytes()
}

fn read_u32(bytes: &[u8]) -> u32 {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(bytes);
    u32::from_le_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::coordinate_from_index;

    #[test]
    fn proves_every_leaf_including_non_power_of_two() {
        let leaves: Vec<Vec<u8>> = (0..10).map(|i| format!("leaf-{i}").into_bytes()).collect();
        let tree = MerkleTree::from_leaves(&leaves).unwrap();
        let root = tree.root();
        for (index, leaf) in leaves.iter().enumerate() {
            let proof = tree.prove(index).unwrap();
            verify_path(&root, index as u32, leaf, &proof).unwrap();
            let decoded = MerkleProof::from_bytes(&proof.to_bytes()).unwrap();
            assert_eq!(decoded, proof);
            verify_path(&root, index as u32, leaf, &decoded).unwrap();
        }
    }

    #[test]
    fn single_leaf_has_an_empty_proof() {
        let tree = MerkleTree::from_leaves(&[b"only".as_slice()]).unwrap();
        let proof = tree.prove(0).unwrap();
        assert!(proof.siblings.is_empty());
        verify_path(&tree.root(), 0, b"only", &proof).unwrap();
    }

    #[test]
    fn rejects_tampered_leaves_and_garbage_proofs() {
        let tree = MerkleTree::from_leaves(&[b"alpha".as_slice(), b"beta".as_slice()]).unwrap();
        let proof = tree.prove(0).unwrap();
        assert_eq!(
            verify_path(&tree.root(), 0, b"alphb", &proof).unwrap_err(),
            DaError::InvalidProof
        );
        assert_eq!(
            MerkleProof::from_bytes(b"nope").unwrap_err(),
            DaError::InvalidProof
        );
        assert_eq!(tree.prove(2).unwrap_err(), DaError::CoordinateOutOfRange);
        assert_eq!(
            MerkleTree::from_leaves(&[] as &[Vec<u8>]).unwrap_err(),
            DaError::Message("merkle tree needs at least one leaf".into())
        );
    }

    #[test]
    fn commitment_verifier_binds_the_coordinate() {
        let leaves: Vec<Vec<u8>> = (0..5).map(|i| vec![i]).collect();
        let tree = MerkleTree::from_leaves(&leaves).unwrap();
        let coord = coordinate_from_index(3, 5);
        let proof = tree.prove(3).unwrap().to_bytes();
        verify_commitment(&tree.root(), 5, coord, &leaves[3], &proof).unwrap();

        let other = coordinate_from_index(1, 5);
        assert_eq!(
            verify_commitment(&tree.root(), 5, other, &leaves[3], &proof).unwrap_err(),
            DaError::InvalidProof
        );
    }
}
