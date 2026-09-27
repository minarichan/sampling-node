use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::error::DaError;
use crate::traits::DANetwork;
use crate::types::{Header, SampleCoordinate};

/// Outcome of one sample request after commitment verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleOutcome {
    Verified,
    Unavailable,
    InvalidProof,
    Failed,
}

impl SampleOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unavailable => "unavailable",
            Self::InvalidProof => "invalid proof",
            Self::Failed => "failed",
        }
    }
}

/// One finished sample attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleResult {
    pub coordinate: SampleCoordinate,
    pub outcome: SampleOutcome,
    pub detail: Option<String>,
}

/// Fetch and verify `coordinates` with at most `concurrency` requests in flight.
///
/// `attempts` is how many times a share the peer does not return is requested.
/// A verified share, or one whose proof is invalid, is not requested again.
pub async fn run_sampling(
    network: Arc<dyn DANetwork>,
    header: &Header,
    coordinates: Vec<SampleCoordinate>,
    concurrency: usize,
    attempts: u32,
) -> Vec<SampleResult> {
    if coordinates.is_empty() {
        return Vec::new();
    }

    let semaphore = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut tasks = JoinSet::new();
    let mut results = Vec::with_capacity(coordinates.len());

    for coordinate in coordinates {
        let network = Arc::clone(&network);
        let header = header.clone();
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("sampling semaphore stays open");
            fetch_one(network, header, coordinate, attempts).await
        });
    }

    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(result) => results.push(result),
            Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
            Err(_) => {}
        }
    }

    results
}

async fn fetch_one(
    network: Arc<dyn DANetwork>,
    header: Header,
    coordinate: SampleCoordinate,
    attempts: u32,
) -> SampleResult {
    let attempts = attempts.max(1);
    let mut latest = None;
    for _ in 0..attempts {
        let result = request_once(Arc::clone(&network), header.clone(), coordinate).await;
        if !matches!(
            result.outcome,
            SampleOutcome::Unavailable | SampleOutcome::Failed
        ) {
            return result;
        }
        latest = Some(result);
    }
    latest.expect("at least one sample attempt runs")
}

async fn request_once(
    network: Arc<dyn DANetwork>,
    header: Header,
    coordinate: SampleCoordinate,
) -> SampleResult {
    match network.request_sample(&header.id, coordinate).await {
        Ok((sample, proof)) => match network.verify_sample(&header, coordinate, &sample, &proof) {
            Ok(()) => SampleResult {
                coordinate,
                outcome: SampleOutcome::Verified,
                detail: None,
            },
            Err(err) => failure(coordinate, &err),
        },
        Err(err) => failure(coordinate, &err),
    }
}

fn failure(coordinate: SampleCoordinate, err: &DaError) -> SampleResult {
    let outcome = match err {
        DaError::SampleUnavailable { .. } => SampleOutcome::Unavailable,
        DaError::InvalidProof => SampleOutcome::InvalidProof,
        _ => SampleOutcome::Failed,
    };
    SampleResult {
        coordinate,
        outcome,
        detail: Some(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Commitment, HeaderId, Sample, SampleProof};
    use async_trait::async_trait;

    struct FakeNetwork {
        unavailable: SampleCoordinate,
    }

    #[async_trait]
    impl DANetwork for FakeNetwork {
        async fn latest_header(&self) -> Result<Header, DaError> {
            Ok(sample_header())
        }

        async fn get_header(&self, id: &HeaderId) -> Result<Header, DaError> {
            let header = sample_header();
            if header.id == *id {
                Ok(header)
            } else {
                Err(DaError::HeaderNotFound(id.0.clone()))
            }
        }

        async fn request_sample(
            &self,
            _header_id: &HeaderId,
            coord: SampleCoordinate,
        ) -> Result<(Sample, SampleProof), DaError> {
            if coord == self.unavailable {
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

    fn sample_header() -> Header {
        Header {
            id: HeaderId("fake".into()),
            height: 1,
            commitment: Commitment(vec![0; 32]),
            total_shares: 4,
        }
    }

    #[tokio::test]
    async fn worker_counts_verified_and_unavailable_samples() {
        let network = Arc::new(FakeNetwork {
            unavailable: SampleCoordinate { row: 0, col: 1 },
        });
        let header = sample_header();
        let coordinates = vec![
            SampleCoordinate { row: 0, col: 0 },
            SampleCoordinate { row: 0, col: 1 },
            SampleCoordinate { row: 1, col: 0 },
            SampleCoordinate { row: 1, col: 1 },
        ];
        let results = run_sampling(network, &header, coordinates, 2, 1).await;
        assert_eq!(results.len(), 4);
        let verified = results
            .iter()
            .filter(|result| result.outcome == SampleOutcome::Verified)
            .count();
        let unavailable = results
            .iter()
            .filter(|result| result.outcome == SampleOutcome::Unavailable)
            .count();
        assert_eq!(verified, 3);
        assert_eq!(unavailable, 1);
    }

    struct FlakyNetwork {
        calls: std::sync::Mutex<std::collections::HashMap<SampleCoordinate, usize>>,
    }

    impl FlakyNetwork {
        fn record(&self, coord: SampleCoordinate) -> usize {
            let mut calls = self.calls.lock().expect("call counter");
            let count = calls.entry(coord).or_insert(0);
            *count += 1;
            *count
        }
    }

    #[async_trait]
    impl DANetwork for FlakyNetwork {
        async fn latest_header(&self) -> Result<Header, DaError> {
            Ok(sample_header())
        }

        async fn get_header(&self, _id: &HeaderId) -> Result<Header, DaError> {
            Ok(sample_header())
        }

        async fn request_sample(
            &self,
            _header_id: &HeaderId,
            coord: SampleCoordinate,
        ) -> Result<(Sample, SampleProof), DaError> {
            let call = self.record(coord);
            if coord == (SampleCoordinate { row: 0, col: 0 }) && call == 1 {
                Err(DaError::SampleUnavailable {
                    row: coord.row,
                    col: coord.col,
                })
            } else if coord == (SampleCoordinate { row: 1, col: 0 }) {
                Err(DaError::Message("connection reset".into()))
            } else {
                Ok((Sample(b"share".to_vec()), SampleProof(Vec::new())))
            }
        }

        fn verify_sample(
            &self,
            _header: &Header,
            coord: SampleCoordinate,
            _sample: &Sample,
            _proof: &SampleProof,
        ) -> Result<(), DaError> {
            if coord == (SampleCoordinate { row: 1, col: 1 }) {
                Err(DaError::InvalidProof)
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn a_missing_share_is_retried_and_an_invalid_proof_is_not() {
        let network = Arc::new(FlakyNetwork {
            calls: std::sync::Mutex::new(std::collections::HashMap::new()),
        });
        let header = sample_header();
        let coordinates = vec![
            SampleCoordinate { row: 0, col: 0 },
            SampleCoordinate { row: 1, col: 0 },
            SampleCoordinate { row: 1, col: 1 },
        ];
        let results = run_sampling(
            Arc::clone(&network) as Arc<dyn DANetwork>,
            &header,
            coordinates,
            1,
            3,
        )
        .await;
        let by_coord = |row, col| {
            results
                .iter()
                .find(|result| result.coordinate == SampleCoordinate { row, col })
                .unwrap()
                .outcome
        };
        assert_eq!(by_coord(0, 0), SampleOutcome::Verified);
        assert_eq!(by_coord(1, 0), SampleOutcome::Failed);
        assert_eq!(by_coord(1, 1), SampleOutcome::InvalidProof);
        let calls = network.calls.lock().expect("call counter");
        assert_eq!(calls[&SampleCoordinate { row: 0, col: 0 }], 2);
        assert_eq!(calls[&SampleCoordinate { row: 1, col: 0 }], 3);
        assert_eq!(calls[&SampleCoordinate { row: 1, col: 1 }], 1);
    }
}
