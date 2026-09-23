use async_trait::async_trait;

use crate::error::DaError;
use crate::types::{Header, HeaderId, Sample, SampleCoordinate, SampleProof};

/// Abstract interface for a data-availability network.
///
/// Mock, Celestia-style, and other adapters implement this trait. The sampling
/// engine depends only on it, so a new DA layer does not change the node core.
#[async_trait]
pub trait DANetwork: Send + Sync {
    /// Latest known header.
    async fn latest_header(&self) -> Result<Header, DaError>;

    /// Fetch one header by id.
    async fn get_header(&self, id: &HeaderId) -> Result<Header, DaError>;

    /// Request the share at `coord` plus a proof that it belongs to the header commitment.
    async fn request_sample(
        &self,
        header_id: &HeaderId,
        coord: SampleCoordinate,
    ) -> Result<(Sample, SampleProof), DaError>;

    /// Check that `sample` and `proof` match the commitment published in `header`.
    fn verify_sample(
        &self,
        header: &Header,
        coord: SampleCoordinate,
        sample: &Sample,
        proof: &SampleProof,
    ) -> Result<(), DaError>;

    /// Verify a fraud or invalidity proof when the DA layer supports one.
    ///
    /// The default result is `Ok(false)`: the proof does not demonstrate that the
    /// data is invalid. Adapters that understand these proofs override this method.
    async fn verify_invalidity_proof(
        &self,
        header: &Header,
        proof: &[u8],
    ) -> Result<bool, DaError> {
        let _ = (header, proof);
        Ok(false)
    }
}
