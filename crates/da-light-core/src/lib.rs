#![forbid(unsafe_code)]

//! Core types, sampling, and commitment verification for a data availability light node.
//!
//! This crate performs no network I/O. A [`DANetwork`] adapter fetches shares, and the
//! node runtime turns verified samples into an availability confidence score.

pub mod commitment;
pub mod error;
pub mod sampling;
pub mod traits;
pub mod types;

pub use error::DaError;
pub use sampling::confidence::{ConfidenceEngine, ConfidenceReport, SampleFailure};
pub use sampling::planner::SamplePlanner;
pub use sampling::worker::{
    run_sampling, run_sampling_across, SampleOutcome, SampleResult, SamplingPeer,
};
pub use traits::DANetwork;
pub use types::{
    coordinate_from_index, share_index, square_width, Commitment, Header, HeaderId, Sample,
    SampleCoordinate, SampleProof,
};
