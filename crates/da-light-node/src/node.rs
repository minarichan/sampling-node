use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use da_light_core::{
    run_sampling, ConfidenceEngine, ConfidenceReport, DANetwork, DaError, Header, HeaderId,
    SampleOutcome, SamplePlanner, SampleResult,
};

use crate::config::NodeConfig;
use crate::peer_manager::{Peer, PeerManager};
use crate::state::MemoryStore;

/// Point-in-time view of the node for the status API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub listen_addr: String,
    pub samples_per_header: u32,
    pub unavailable_fraction: f64,
    pub headers_tracked: usize,
    pub latest: Option<ConfidenceReport>,
    pub peers: Vec<Peer>,
}

/// Sampling node bound to one DA adapter.
pub struct Node {
    network: Arc<dyn DANetwork>,
    store: Mutex<MemoryStore>,
    peers: Mutex<PeerManager>,
    planner: SamplePlanner,
    confidence: ConfidenceEngine,
    config: NodeConfig,
}

impl Node {
    pub fn new(network: Arc<dyn DANetwork>, config: NodeConfig) -> Result<Self, DaError> {
        config.validate()?;
        let peers = PeerManager::single(&config.upstream_id, &config.upstream_endpoint);
        Ok(Self {
            network,
            store: Mutex::new(MemoryStore::default()),
            peers: Mutex::new(peers),
            planner: SamplePlanner::new(config.samples_per_header),
            confidence: ConfidenceEngine::new(config.unavailable_fraction),
            config,
        })
    }

    pub async fn sample_latest(&self) -> Result<ConfidenceReport, DaError> {
        let header = self.network.latest_header().await?;
        self.sample_header(header).await
    }

    pub async fn sample_header(&self, header: Header) -> Result<ConfidenceReport, DaError> {
        let exclude = self.store.lock().await.successful_coords(&header.id);
        let coordinates = self.planner.plan_samples_excluding(&header, &exclude);
        let results = run_sampling(
            Arc::clone(&self.network),
            &header,
            coordinates,
            self.config.concurrency,
            self.config.sample_attempts,
        )
        .await;
        self.apply_peer_scores(&results).await;
        let report = self
            .store
            .lock()
            .await
            .record(&header, &results, &self.confidence);
        tracing::info!(
            header = %report.header_id,
            height = report.height,
            successful = report.successful_samples,
            failed = report.failed_samples,
            confidence = report.confidence,
            "sampling round complete"
        );
        Ok(report)
    }

    pub async fn status(&self) -> StatusSnapshot {
        let (headers_tracked, latest) = {
            let store = self.store.lock().await;
            (store.len(), store.latest_report(&self.confidence))
        };
        let peers = self.peers.lock().await.peers().to_vec();
        StatusSnapshot {
            listen_addr: self.config.listen_addr.to_string(),
            samples_per_header: self.config.samples_per_header,
            unavailable_fraction: self.config.unavailable_fraction,
            headers_tracked,
            latest,
            peers,
        }
    }

    pub async fn confidence_report(&self, header_id: Option<&str>) -> Option<ConfidenceReport> {
        let store = self.store.lock().await;
        match header_id {
            Some(id) => store.report(&HeaderId(id.to_string()), &self.confidence),
            None => store.latest_report(&self.confidence),
        }
    }

    pub async fn tracked_headers(&self) -> Vec<Header> {
        self.store.lock().await.headers()
    }

    async fn apply_peer_scores(&self, results: &[SampleResult]) {
        let mut peers = self.peers.lock().await;
        for result in results {
            let delta = match result.outcome {
                SampleOutcome::Verified => 1,
                SampleOutcome::InvalidProof => -10,
                SampleOutcome::Unavailable | SampleOutcome::Failed => -1,
            };
            peers.adjust_score(&self.config.upstream_id, delta);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use da_light_core::{
        Commitment, DANetwork, DaError, Header, HeaderId, Sample, SampleCoordinate, SampleProof,
    };

    use super::*;

    struct Flaky {
        calls: Mutex<u32>,
    }

    fn header() -> Header {
        Header {
            id: HeaderId("one-share".into()),
            height: 1,
            commitment: Commitment(vec![0; 32]),
            total_shares: 1,
        }
    }

    #[async_trait]
    impl DANetwork for Flaky {
        async fn latest_header(&self) -> Result<Header, DaError> {
            Ok(header())
        }

        async fn get_header(&self, _id: &HeaderId) -> Result<Header, DaError> {
            Ok(header())
        }

        async fn request_sample(
            &self,
            _header_id: &HeaderId,
            coord: SampleCoordinate,
        ) -> Result<(Sample, SampleProof), DaError> {
            let mut calls = self.calls.lock().expect("call counter");
            *calls += 1;
            if *calls == 1 {
                Err(DaError::SampleUnavailable {
                    row: coord.row,
                    col: coord.col,
                })
            } else {
                Ok((Sample(b"share".to_vec()), SampleProof(Vec::new())))
            }
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
    async fn a_share_missing_on_the_first_try_counts_as_verified() {
        let network = Arc::new(Flaky {
            calls: Mutex::new(0),
        });
        let node = Node::new(
            Arc::clone(&network) as Arc<dyn DANetwork>,
            NodeConfig {
                samples_per_header: 1,
                sample_attempts: 2,
                upstream_id: "celestia".into(),
                ..NodeConfig::default()
            },
        )
        .unwrap();

        let report = node.sample_latest().await.unwrap();
        assert_eq!(report.successful_samples, 1);
        assert_eq!(report.failed_samples, 0);
        assert_eq!(*network.calls.lock().expect("call counter"), 2);
        assert_eq!(node.status().await.peers[0].score, 1);
    }
}
