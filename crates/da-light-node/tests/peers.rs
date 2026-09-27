use std::sync::Arc;

use async_trait::async_trait;
use da_adapter_mock::{MockConfig, MockNetwork};
use da_light_core::{DANetwork, DaError, Header, HeaderId, Sample, SampleCoordinate, SampleProof};
use da_light_node::{Node, NodeConfig, Upstream};

fn square() -> MockNetwork {
    MockNetwork::generate(MockConfig {
        share_count: 4,
        header_count: 1,
        withheld_per_header: 0,
        share_size: 32,
    })
    .unwrap()
}

fn config(attempts: u32) -> NodeConfig {
    NodeConfig {
        samples_per_header: 4,
        sample_attempts: attempts,
        ..NodeConfig::default()
    }
}

struct Silent;

#[async_trait]
impl DANetwork for Silent {
    async fn latest_header(&self) -> Result<Header, DaError> {
        Err(DaError::Message("silent peer has no header".into()))
    }

    async fn get_header(&self, id: &HeaderId) -> Result<Header, DaError> {
        Err(DaError::Message(format!(
            "silent peer has no header {}",
            id.0
        )))
    }

    async fn request_sample(
        &self,
        _header_id: &HeaderId,
        coord: SampleCoordinate,
    ) -> Result<(Sample, SampleProof), DaError> {
        Err(DaError::SampleUnavailable {
            row: coord.row,
            col: coord.col,
        })
    }

    fn verify_sample(
        &self,
        _header: &Header,
        _coord: SampleCoordinate,
        _sample: &Sample,
        _proof: &SampleProof,
    ) -> Result<(), DaError> {
        Ok(())
    }
}

#[tokio::test]
async fn two_peers_both_serve_and_both_are_scored() {
    let mock = square();
    let node = Node::from_upstreams(
        vec![
            Upstream {
                id: "a".into(),
                endpoint: "a".into(),
                network: Arc::new(mock.clone()) as Arc<dyn DANetwork>,
            },
            Upstream {
                id: "b".into(),
                endpoint: "b".into(),
                network: Arc::new(mock) as Arc<dyn DANetwork>,
            },
        ],
        config(1),
    )
    .unwrap();

    let report = node.sample_latest().await.unwrap();
    assert_eq!(report.successful_samples, 4);
    let peers = node.status().await.peers;
    assert!(peers.iter().all(|peer| peer.score > 0));
    assert_eq!(peers.iter().map(|peer| peer.score).sum::<i32>(), 4);
}

#[tokio::test]
async fn verified_shares_from_only_one_peer_do_not_count() {
    let node = Node::from_upstreams(
        vec![
            Upstream {
                id: "good".into(),
                endpoint: "good".into(),
                network: Arc::new(square()) as Arc<dyn DANetwork>,
            },
            Upstream {
                id: "silent".into(),
                endpoint: "silent".into(),
                network: Arc::new(Silent) as Arc<dyn DANetwork>,
            },
        ],
        config(1),
    )
    .unwrap();

    let report = node.sample_latest().await.unwrap();
    assert_eq!(report.successful_samples, 0);
    assert_eq!(report.failed_samples, 4);
    let peers = node.status().await.peers;
    assert_eq!(peer_score(&peers, "good"), 2);
    assert_eq!(peer_score(&peers, "silent"), -2);
}

fn peer_score(peers: &[da_light_node::Peer], id: &str) -> i32 {
    peers.iter().find(|peer| peer.id == id).unwrap().score
}
