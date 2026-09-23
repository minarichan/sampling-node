#![forbid(unsafe_code)]

//! Local data-availability network for development and tests.
//!
//! Each generated header is a square of deterministic shares committed with the
//! core Merkle tree. Individual shares can be withheld so sampling can observe
//! unavailable data without leaving the process.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use da_light_core::commitment::{verify_commitment, MerkleTree};
use da_light_core::{
    share_index, Commitment, DANetwork, DaError, Header, HeaderId, Sample, SampleCoordinate,
    SampleProof,
};

/// Parameters for [`MockNetwork::generate`].
#[derive(Debug, Clone)]
pub struct MockConfig {
    pub share_count: u32,
    pub header_count: u32,
    /// Highest share indexes the mock will refuse to serve, per header.
    pub withheld_per_header: u32,
    pub share_size: usize,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            share_count: 256,
            header_count: 1,
            withheld_per_header: 0,
            share_size: 64,
        }
    }
}

impl MockConfig {
    pub fn validate(&self) -> Result<(), DaError> {
        if self.share_count == 0 {
            return Err(DaError::Message("share count must be at least 1".into()));
        }
        if self.header_count == 0 {
            return Err(DaError::Message("header count must be at least 1".into()));
        }
        if self.withheld_per_header > self.share_count {
            return Err(DaError::Message(
                "withheld shares cannot exceed the share count".into(),
            ));
        }
        if self.share_size == 0 {
            return Err(DaError::Message("share size must be at least 1".into()));
        }
        Ok(())
    }
}

struct Block {
    header: Header,
    shares: Vec<Vec<u8>>,
    tree: MerkleTree,
    withheld: HashSet<u32>,
}

struct MockInner {
    by_id: HashMap<String, Block>,
    by_height: BTreeMap<u64, String>,
}

/// Process-local DA layer. Cloning is cheap; blocks live behind an [`Arc`].
#[derive(Clone)]
pub struct MockNetwork {
    inner: Arc<MockInner>,
}

impl MockNetwork {
    pub fn generate(config: MockConfig) -> Result<Self, DaError> {
        config.validate()?;
        let mut by_id = HashMap::new();
        let mut by_height = BTreeMap::new();

        for height in 1..=config.header_count {
            let shares: Vec<Vec<u8>> = (0..config.share_count)
                .map(|index| share_bytes(u64::from(height), index, config.share_size))
                .collect();
            let tree = MerkleTree::from_leaves(&shares)?;
            let root = tree.root();
            let header = Header {
                id: HeaderId(hex::encode(root)),
                height: u64::from(height),
                commitment: Commitment(root.to_vec()),
                total_shares: config.share_count,
            };
            let withheld =
                (config.share_count - config.withheld_per_header..config.share_count).collect();
            by_height.insert(header.height, header.id.0.clone());
            by_id.insert(
                header.id.0.clone(),
                Block {
                    header,
                    shares,
                    tree,
                    withheld,
                },
            );
        }

        Ok(Self {
            inner: Arc::new(MockInner { by_id, by_height }),
        })
    }

    fn block(&self, id: &HeaderId) -> Result<&Block, DaError> {
        self.inner
            .by_id
            .get(&id.0)
            .ok_or_else(|| DaError::HeaderNotFound(id.0.clone()))
    }
}

#[async_trait]
impl DANetwork for MockNetwork {
    async fn latest_header(&self) -> Result<Header, DaError> {
        let id = self
            .inner
            .by_height
            .values()
            .next_back()
            .ok_or_else(|| DaError::Message("mock network has no headers".into()))?;
        Ok(self.block(&HeaderId(id.clone()))?.header.clone())
    }

    async fn get_header(&self, id: &HeaderId) -> Result<Header, DaError> {
        Ok(self.block(id)?.header.clone())
    }

    async fn request_sample(
        &self,
        header_id: &HeaderId,
        coord: SampleCoordinate,
    ) -> Result<(Sample, SampleProof), DaError> {
        let block = self.block(header_id)?;
        let index = share_index(coord, block.header.total_shares)?;
        if block.withheld.contains(&index) {
            return Err(DaError::SampleUnavailable {
                row: coord.row,
                col: coord.col,
            });
        }
        let share = block
            .shares
            .get(index as usize)
            .ok_or(DaError::CoordinateOutOfRange)?
            .clone();
        let proof = block.tree.prove(index as usize)?;
        Ok((Sample(share), SampleProof(proof.to_bytes())))
    }

    fn verify_sample(
        &self,
        header: &Header,
        coord: SampleCoordinate,
        sample: &Sample,
        proof: &SampleProof,
    ) -> Result<(), DaError> {
        self.block(&header.id)?;
        verify_commitment(
            &header.commitment.0,
            header.total_shares,
            coord,
            &sample.0,
            &proof.0,
        )
    }
}

fn share_bytes(height: u64, index: u32, size: usize) -> Vec<u8> {
    let mut data = format!("sampling-node|h={height}|i={index}|").into_bytes();
    if data.len() < size {
        data.resize(size, 0x5a);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use da_light_core::coordinate_from_index;

    fn network(config: MockConfig) -> MockNetwork {
        MockNetwork::generate(config).unwrap()
    }

    #[tokio::test]
    async fn every_share_verifies_when_the_count_is_not_a_power_of_two() {
        let network = network(MockConfig {
            share_count: 10,
            header_count: 2,
            withheld_per_header: 0,
            share_size: 8,
        });
        let header = network.latest_header().await.unwrap();
        assert_eq!(header.height, 2);
        for index in 0..10 {
            let coord = coordinate_from_index(index, 10);
            let (sample, proof) = network.request_sample(&header.id, coord).await.unwrap();
            network
                .verify_sample(&header, coord, &sample, &proof)
                .unwrap();
        }
    }

    #[tokio::test]
    async fn withheld_shares_are_refused_and_tampering_fails_verification() {
        let network = network(MockConfig {
            share_count: 8,
            header_count: 1,
            withheld_per_header: 1,
            share_size: 16,
        });
        let header = network.latest_header().await.unwrap();
        let withheld = coordinate_from_index(7, 8);
        let err = network
            .request_sample(&header.id, withheld)
            .await
            .unwrap_err();
        assert!(matches!(err, DaError::SampleUnavailable { .. }));

        let coord = coordinate_from_index(0, 8);
        let (mut sample, proof) = network.request_sample(&header.id, coord).await.unwrap();
        network
            .verify_sample(&header, coord, &sample, &proof)
            .unwrap();
        sample.0[0] ^= 0xff;
        assert_eq!(
            network
                .verify_sample(&header, coord, &sample, &proof)
                .unwrap_err(),
            DaError::InvalidProof
        );
    }

    #[tokio::test]
    async fn unknown_headers_and_invalidity_proofs() {
        let network = network(MockConfig {
            share_count: 4,
            ..MockConfig::default()
        });
        let missing = HeaderId("missing".into());
        assert!(matches!(
            network.get_header(&missing).await.unwrap_err(),
            DaError::HeaderNotFound(_)
        ));
        let header = network.latest_header().await.unwrap();
        assert!(!network
            .verify_invalidity_proof(&header, b"proof")
            .await
            .unwrap());
    }
}
