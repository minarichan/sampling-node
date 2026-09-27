use da_light_core::SampleCoordinate;
use serde::{Deserialize, Serialize};

/// One upstream the node attributes sample results to.
///
/// Scores move up on verified samples and down when a sample is missing or
/// fails its proof. Higher scores are asked more often. When more than one
/// peer is configured, a round whose verified shares all came from one peer
/// is not counted toward confidence.
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

    pub(crate) fn from_peers(peers: Vec<Peer>) -> Self {
        Self { peers }
    }

    /// Peer ids, highest score first. Equal scores keep configuration order.
    pub(crate) fn ranked_ids(&self) -> Vec<String> {
        let mut indexed: Vec<_> = self.peers.iter().enumerate().collect();
        indexed.sort_by(|left, right| right.1.score.cmp(&left.1.score).then(left.0.cmp(&right.0)));
        indexed
            .into_iter()
            .map(|(_, peer)| peer.id.clone())
            .collect()
    }

    /// Give higher-scoring peers more coordinates, and use a second peer when
    /// more than one sample is drawn from more than one peer.
    pub(crate) fn assign(
        &self,
        coordinates: &[SampleCoordinate],
    ) -> Vec<(SampleCoordinate, String)> {
        if self.peers.is_empty() {
            return Vec::new();
        }
        let min_score = self.peers.iter().map(|peer| peer.score).min().unwrap_or(0);
        let mut ring = Vec::new();
        for peer in &self.peers {
            for _ in 0..peer_weight(peer.score, min_score) {
                ring.push(peer.id.clone());
            }
        }
        let mut assigned: Vec<_> = coordinates
            .iter()
            .enumerate()
            .map(|(index, coordinate)| (*coordinate, ring[index % ring.len()].clone()))
            .collect();
        if self.peers.len() > 1 && assigned.len() > 1 {
            let only = assigned[0].1.clone();
            if assigned.iter().all(|(_, id)| id == &only) {
                if let Some(other) = self.peers.iter().find(|peer| peer.id != only) {
                    assigned.last_mut().unwrap().1 = other.id.clone();
                }
            }
        }
        assigned
    }
}

fn peer_weight(score: i32, min_score: i32) -> usize {
    let weight = i64::from(score) - i64::from(min_score) + 1;
    usize::try_from(weight).unwrap_or(usize::MAX).clamp(1, 8)
}

#[cfg(test)]
mod tests {
    use da_light_core::SampleCoordinate;

    use super::*;

    fn peers(scores: &[(&str, i32)]) -> PeerManager {
        PeerManager::from_peers(
            scores
                .iter()
                .map(|(id, score)| Peer {
                    id: (*id).to_string(),
                    endpoint: (*id).to_string(),
                    score: *score,
                })
                .collect(),
        )
    }

    fn coords(count: u32) -> Vec<SampleCoordinate> {
        (0..count)
            .map(|index| SampleCoordinate { row: 0, col: index })
            .collect()
    }

    #[test]
    fn equal_peers_split_the_round() {
        let assigned = peers(&[("a", 0), ("b", 0)]).assign(&coords(4));
        let a = assigned.iter().filter(|(_, id)| id == "a").count();
        let b = assigned.iter().filter(|(_, id)| id == "b").count();
        assert_eq!(a, 2);
        assert_eq!(b, 2);
    }

    #[test]
    fn a_higher_score_is_asked_more_often_but_not_alone() {
        let assigned = peers(&[("best", 10), ("other", 0)]).assign(&coords(4));
        let best = assigned.iter().filter(|(_, id)| id == "best").count();
        let other = assigned.iter().filter(|(_, id)| id == "other").count();
        assert!(best > other);
        assert!(other >= 1);
    }
}
