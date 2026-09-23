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
