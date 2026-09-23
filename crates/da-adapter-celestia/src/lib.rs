#![forbid(unsafe_code)]

//! Celestia adapter placeholder.
//!
//! Phase 2 should talk to a Celestia light node or consensus node: fetch the
//! data-availability header, sample namespaced shares, and verify namespace
//! Merkle proofs against the data root. This crate implements [`DANetwork`] so
//! the node can already be wired to that backend, and every call reports that
//! the adapter is not implemented yet.

use async_trait::async_trait;
use da_light_core::{DANetwork, DaError, Header, HeaderId, Sample, SampleCoordinate, SampleProof};

const NOT_IMPLEMENTED: &str = "celestia adapter is not implemented yet";

/// Future Celestia RPC client. `rpc_url` is retained for the phase 2 client.
#[derive(Debug, Clone)]
pub struct CelestiaNetwork {
    rpc_url: String,
}

impl CelestiaNetwork {
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into(),
        }
    }

    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }
}

#[async_trait]
impl DANetwork for CelestiaNetwork {
    async fn latest_header(&self) -> Result<Header, DaError> {
        Err(DaError::Unsupported(NOT_IMPLEMENTED))
    }

    async fn get_header(&self, _id: &HeaderId) -> Result<Header, DaError> {
        Err(DaError::Unsupported(NOT_IMPLEMENTED))
    }

    async fn request_sample(
        &self,
        _header_id: &HeaderId,
        _coord: SampleCoordinate,
    ) -> Result<(Sample, SampleProof), DaError> {
        Err(DaError::Unsupported(NOT_IMPLEMENTED))
    }

    fn verify_sample(
        &self,
        _header: &Header,
        _coord: SampleCoordinate,
        _sample: &Sample,
        _proof: &SampleProof,
    ) -> Result<(), DaError> {
        Err(DaError::Unsupported(NOT_IMPLEMENTED))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_that_the_adapter_is_not_implemented() {
        let network = CelestiaNetwork::new("http://127.0.0.1:26658");
        let err = network.latest_header().await.unwrap_err();
        assert_eq!(err, DaError::Unsupported(NOT_IMPLEMENTED));
        assert_eq!(network.rpc_url(), "http://127.0.0.1:26658");
    }
}
