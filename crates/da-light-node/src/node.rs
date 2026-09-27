use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use da_light_core::{
    run_sampling_across, ConfidenceEngine, ConfidenceReport, DANetwork, DaError, Header, HeaderId,
    SampleOutcome, SamplePlanner, SampleResult, SamplingPeer,
};

use crate::config::NodeConfig;
use crate::peer_manager::{Peer, PeerManager};
use crate::persist::StateDb;
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

/// One upstream the node can sample.
pub struct Upstream {
    pub id: String,
    pub endpoint: String,
    pub network: Arc<dyn DANetwork>,
}

/// Sampling node bound to one or more upstream peers.
pub struct Node {
    upstreams: Vec<Upstream>,
    store: Mutex<MemoryStore>,
    peers: Mutex<PeerManager>,
    db: Option<StateDb>,
    planner: SamplePlanner,
    confidence: ConfidenceEngine,
    config: NodeConfig,
}

impl Node {
    pub fn new(network: Arc<dyn DANetwork>, config: NodeConfig) -> Result<Self, DaError> {
        let upstream = Upstream {
            id: config.upstream_id.clone(),
            endpoint: config.upstream_endpoint.clone(),
            network,
        };
        Self::from_upstreams(vec![upstream], config)
    }

    pub fn from_upstreams(upstreams: Vec<Upstream>, config: NodeConfig) -> Result<Self, DaError> {
        config.validate()?;
        if upstreams.is_empty() {
            return Err(DaError::Message("at least one peer is required".into()));
        }
        let mut seen = std::collections::HashSet::new();
        for upstream in &upstreams {
            if upstream.id.is_empty() || !seen.insert(upstream.id.clone()) {
                return Err(DaError::Message(format!(
                    "peer ids must be unique and non-empty (got {})",
                    upstream.id
                )));
            }
        }
        let configured: Vec<(String, String)> = upstreams
            .iter()
            .map(|upstream| (upstream.id.clone(), upstream.endpoint.clone()))
            .collect();
        let (db, store, peers) = if let Some(path) = &config.data_path {
            let (db, store, peers) = StateDb::open(path, &configured)?;
            tracing::info!(
                path = %path.display(),
                headers = store.len(),
                peers = configured.len(),
                "loaded sampling state"
            );
            (Some(db), store, peers)
        } else {
            (
                None,
                MemoryStore::default(),
                PeerManager::from_peers(
                    configured
                        .into_iter()
                        .map(|(id, endpoint)| Peer {
                            id,
                            endpoint,
                            score: 0,
                        })
                        .collect(),
                ),
            )
        };
        Ok(Self {
            upstreams,
            store: Mutex::new(store),
            peers: Mutex::new(peers),
            db,
            planner: SamplePlanner::new(config.samples_per_header),
            confidence: ConfidenceEngine::new(config.unavailable_fraction),
            config,
        })
    }

    pub async fn sample_latest(&self) -> Result<ConfidenceReport, DaError> {
        let header = self.latest_header().await?;
        self.sample_header(header).await
    }

    async fn latest_header(&self) -> Result<Header, DaError> {
        let order = self.peers.lock().await.ranked_ids();
        let mut last = None;
        for id in order {
            let Some(network) = self.network_for(&id) else {
                continue;
            };
            match network.latest_header().await {
                Ok(header) => return Ok(header),
                Err(err) => last = Some(err),
            }
        }
        Err(last.unwrap_or_else(|| DaError::Message("no peers configured".into())))
    }

    fn network_for(&self, id: &str) -> Option<Arc<dyn DANetwork>> {
        self.upstreams
            .iter()
            .find(|upstream| upstream.id == id)
            .map(|upstream| Arc::clone(&upstream.network))
    }

    pub async fn sample_header(&self, header: Header) -> Result<ConfidenceReport, DaError> {
        let exclude = self.store.lock().await.successful_coords(&header.id);
        let coordinates = self.planner.plan_samples_excluding(&header, &exclude);
        let (assignments, sampling_peers) = {
            let peers = self.peers.lock().await;
            let assignments = peers.assign(&coordinates);
            let sampling_peers = peers
                .ranked_ids()
                .into_iter()
                .filter_map(|id| {
                    self.network_for(&id)
                        .map(|network| SamplingPeer { id, network })
                })
                .collect::<Vec<_>>();
            (assignments, sampling_peers)
        };
        let mut results = run_sampling_across(
            &header,
            assignments,
            &sampling_peers,
            self.config.concurrency,
            self.config.sample_attempts,
        )
        .await;
        self.apply_peer_scores(&results).await;
        if sampling_peers.len() > 1 {
            refuse_single_source(&mut results);
        }
        let report = self
            .store
            .lock()
            .await
            .record(&header, &results, &self.confidence);
        self.save_state().await?;
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
            peers.adjust_score(&result.peer_id, delta);
        }
    }

    async fn save_state(&self) -> Result<(), DaError> {
        let Some(db) = &self.db else {
            return Ok(());
        };
        let store = self.store.lock().await;
        let peers = self.peers.lock().await;
        db.save(&store, &peers)
    }
}

/// A multi-peer round is useful only when more than one peer contributed a
/// verified share. Otherwise those successes are recorded as failures so the
/// next round can try them again.
fn refuse_single_source(results: &mut [SampleResult]) {
    let mut source = None;
    let mut verified = 0usize;
    for result in results.iter() {
        if result.outcome != SampleOutcome::Verified {
            continue;
        }
        verified += 1;
        match &source {
            None => source = Some(result.peer_id.clone()),
            Some(id) if id != &result.peer_id => return,
            Some(_) => {}
        }
    }
    if verified == 0 {
        return;
    }
    tracing::warn!(
        peer = source.as_deref().unwrap_or(""),
        verified,
        "refusing a sample set that all came from one peer"
    );
    for result in results.iter_mut() {
        if result.outcome == SampleOutcome::Verified {
            result.outcome = SampleOutcome::Failed;
            result.detail = Some("all verified samples came from one peer".into());
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
