use std::net::SocketAddr;
use std::path::PathBuf;

use da_light_core::DaError;

/// Runtime settings for one sampling node.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub listen_addr: SocketAddr,
    pub samples_per_header: u32,
    pub concurrency: usize,
    /// Times to request a share the peer does not return. Invalid proofs are not retried.
    pub sample_attempts: u32,
    /// Smallest missing-share fraction that makes a block unreconstructable.
    pub unavailable_fraction: f64,
    pub upstream_id: String,
    pub upstream_endpoint: String,
    /// SQLite file for sampling progress. Absent means the process keeps state in memory only.
    pub data_path: Option<PathBuf>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            samples_per_header: 16,
            concurrency: 8,
            sample_attempts: 3,
            unavailable_fraction: 0.25,
            upstream_id: "mock-local".into(),
            upstream_endpoint: "local".into(),
            data_path: None,
        }
    }
}

impl NodeConfig {
    pub fn validate(&self) -> Result<(), DaError> {
        if self.samples_per_header == 0 {
            return Err(DaError::Message(
                "samples per header must be at least 1".into(),
            ));
        }
        if self.concurrency == 0 {
            return Err(DaError::Message("concurrency must be at least 1".into()));
        }
        if self.sample_attempts == 0 {
            return Err(DaError::Message(
                "sample attempts must be at least 1".into(),
            ));
        }
        if !(self.unavailable_fraction > 0.0 && self.unavailable_fraction <= 1.0) {
            return Err(DaError::Message(
                "unavailable fraction must be in (0, 1]".into(),
            ));
        }
        Ok(())
    }
}
