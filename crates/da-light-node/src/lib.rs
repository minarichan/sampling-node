#![forbid(unsafe_code)]

//! Node runtime that samples a [`da_light_core::DANetwork`], stores progress,
//! and exposes it over HTTP.

pub mod api;
pub mod config;
pub mod node;
pub mod peer_manager;

mod persist;
mod state;

pub use config::NodeConfig;
pub use node::{Node, StatusSnapshot, Upstream};
pub use peer_manager::Peer;
