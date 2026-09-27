use serde::{Deserialize, Serialize};

/// One upstream the node attributes sample results to.
///
/// Phase 1 has a single logical upstream (the adapter). Scores move up on
/// verified samples and down when a sample is missing or fails its proof.
/// Selecting among many peers, and refusing a sample set that all came from
/// one peer, belongs to phase 2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub endpoint: String,
    pub score: i32,
}

#[derive(Debug)]
pub struct PeerManager {
    peers: Vec<Peer>,
}

impl PeerManager {
    pub fn single(id: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self::with_score(id, endpoint, 0)
    }

    pub(crate) fn with_score(
        id: impl Into<String>,
        endpoint: impl Into<String>,
        score: i32,
    ) -> Self {
        Self {
            peers: vec![Peer {
                id: id.into(),
                endpoint: endpoint.into(),
                score,
            }],
        }
    }

    pub fn adjust_score(&mut self, id: &str, delta: i32) {
        if let Some(peer) = self.peers.iter_mut().find(|peer| peer.id == id) {
            peer.score = peer.score.saturating_add(delta);
        }
    }

    pub fn peers(&self) -> &[Peer] {
        &self.peers
    }
}
