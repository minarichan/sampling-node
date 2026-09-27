#![forbid(unsafe_code)]

//! Celestia adapter.
//!
//! `latest_header` and `get_header` call a celestia-node JSON-RPC endpoint
//! (`header.LocalHead`, `header.GetByHeight`, `header.GetByHash`) and check
//! that the data availability header's row and column roots hash to the
//! block's `data_hash`. `request_sample` calls `share.GetSamples`. Verification
//! checks the namespace Merkle proof against that row or column root, then
//! checks the root against the header data commitment.

mod header;
mod nmt;
mod rpc;
mod sample;

use async_trait::async_trait;
use da_light_core::{DANetwork, DaError, Header, HeaderId, Sample, SampleCoordinate, SampleProof};

use rpc::CelestiaRpc;

/// JSON-RPC client for one celestia-node.
///
/// `rpc_url` is the node's RPC root, usually `http://127.0.0.1:26658`.
/// `token` is sent as `Authorization: Bearer` when the node requires auth.
#[derive(Clone)]
pub struct CelestiaNetwork {
    rpc: CelestiaRpc,
}

impl CelestiaNetwork {
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self {
            rpc: CelestiaRpc::new(rpc_url),
        }
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.rpc = self.rpc.with_token(token);
        self
    }

    pub fn rpc_url(&self) -> &str {
        self.rpc.rpc_url()
    }
}

impl std::fmt::Debug for CelestiaNetwork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CelestiaNetwork")
            .field("rpc_url", &self.rpc_url())
            .finish()
    }
}

#[async_trait]
impl DANetwork for CelestiaNetwork {
    async fn latest_header(&self) -> Result<Header, DaError> {
        self.rpc.local_head().await
    }

    async fn get_header(&self, id: &HeaderId) -> Result<Header, DaError> {
        self.rpc.header(id).await
    }

    async fn request_sample(
        &self,
        header_id: &HeaderId,
        coord: SampleCoordinate,
    ) -> Result<(Sample, SampleProof), DaError> {
        sample::request_sample(&self.rpc, header_id, coord).await
    }

    fn verify_sample(
        &self,
        header: &Header,
        coord: SampleCoordinate,
        sample: &Sample,
        proof: &SampleProof,
    ) -> Result<(), DaError> {
        sample::verify_sample(header, coord, sample, proof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Json;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use serde_json::{json, Value};
    use tokio::net::TcpListener;

    const FIXTURE: &str = r#"{
        "header": {
            "height": "1",
            "data_hash": "3D96B7D238E7E0456F6AF8E7CDF0A67BD6CF9C2089ECB559C659DCAA1F880353"
        },
        "commit": {
            "block_id": {
                "hash": "7a5fabb19713d732d967b1da84fa0df5e87a7b62302d783f78743e216c1a3550"
            }
        },
        "dah": {
            "row_roots": [
                "//////////////////////////////////////7//////////////////////////////////////huZWOTTDmD36N1F75A9BshxNlRasCnNpQiWqIhdVHcU",
                "/////////////////////////////////////////////////////////////////////////////5iieeroHBMfF+sER3JpvROIeEJZjbY+TRE0ntADQLL3"
            ],
            "column_roots": [
                "//////////////////////////////////////7//////////////////////////////////////huZWOTTDmD36N1F75A9BshxNlRasCnNpQiWqIhdVHcU",
                "/////////////////////////////////////////////////////////////////////////////5iieeroHBMfF+sER3JpvROIeEJZjbY+TRE0ntADQLL3"
            ]
        }
    }"#;

    async fn serve(token: Option<&'static str>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let app = Router::new().route(
            "/",
            post(
                move |headers: HeaderMap, Json(body): Json<Value>| async move {
                    if let Some(expected) = token {
                        let presented = headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok());
                        if presented != Some(&format!("Bearer {expected}")) {
                            return (
                                StatusCode::UNAUTHORIZED,
                                Json(json!({"error": {"message": "missing token"}})),
                            );
                        }
                    }
                    let method = body["method"].as_str().unwrap_or("");
                    let response = match method {
                        "header.LocalHead" | "header.GetByHeight" | "header.GetByHash" => {
                            json!({"jsonrpc": "2.0", "id": 1, "result": fixture})
                        }
                        _ => json!({"jsonrpc": "2.0", "id": 1, "error": {"message": "not found"}}),
                    };
                    (StatusCode::OK, Json(response))
                },
            ),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn fetches_the_local_head_and_a_header_by_height_or_hash() {
        let url = serve(Some("secret")).await;
        let network = CelestiaNetwork::new(&url).with_token("secret");
        let latest = network.latest_header().await.unwrap();
        assert_eq!(latest.height, 1);
        assert_eq!(latest.total_shares, 4);
        assert_eq!(
            latest.id.0,
            "7A5FABB19713D732D967B1DA84FA0DF5E87A7B62302D783F78743E216C1A3550"
        );

        let by_hash = network.get_header(&latest.id).await.unwrap();
        assert_eq!(by_hash, latest);
        let by_height = network
            .get_header(&HeaderId(latest.height.to_string()))
            .await
            .unwrap();
        assert_eq!(by_height, latest);
    }

    #[tokio::test]
    async fn requests_one_share_and_verifies_it_against_the_data_root() {
        use crate::nmt::{build_axis, prove_share, NAMESPACE_SIZE, SHARE_SIZE};
        use base64::Engine;

        let mut q0 = vec![0x11u8; SHARE_SIZE];
        q0[..NAMESPACE_SIZE].fill(0x01);
        let parity_col = vec![0x22u8; SHARE_SIZE];
        let parity_row = vec![0x33u8; SHARE_SIZE];
        let parity_corner = vec![0x44u8; SHARE_SIZE];
        let shares = vec![
            vec![q0.clone(), parity_col],
            vec![parity_row, parity_corner],
        ];
        let row_roots = (0..2)
            .map(|row| build_axis(&shares[row], 1, row).unwrap().root)
            .collect::<Vec<_>>();
        let column_roots = (0..2)
            .map(|col| {
                let column = vec![shares[0][col].clone(), shares[1][col].clone()];
                build_axis(&column, 1, col).unwrap().root
            })
            .collect::<Vec<_>>();
        let row0 = build_axis(&shares[0], 1, 0).unwrap();
        let nodes = prove_share(&row0.leaf_hashes, 0).unwrap();
        let sample = sample::rpc_sample_value(&q0, "row", 0, &nodes);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let row_b64: Vec<_> = row_roots
            .iter()
            .map(|root| base64::engine::general_purpose::STANDARD.encode(root))
            .collect();
        let col_b64: Vec<_> = column_roots
            .iter()
            .map(|root| base64::engine::general_purpose::STANDARD.encode(root))
            .collect();
        let mut leaves = row_roots;
        leaves.extend(column_roots);
        let data_root = hex::encode_upper(crate::header::hash_from_byte_slices(&leaves));
        let header = json!({
            "header": {"height": "9", "data_hash": data_root},
            "commit": {"block_id": {"hash": "ab".repeat(32)}},
            "dah": {"row_roots": row_b64, "column_roots": col_b64}
        });
        let app = Router::new().route(
            "/",
            post(move |Json(body): Json<Value>| {
                let header = header.clone();
                let sample = sample.clone();
                async move {
                    let method = body["method"].as_str().unwrap_or("");
                    let response = match method {
                        "header.GetByHash" => {
                            json!({"jsonrpc": "2.0", "id": 1, "result": header})
                        }
                        "share.GetSamples" => {
                            let row = body["params"][1][0]["row"].as_u64();
                            let col = body["params"][1][0]["col"].as_u64();
                            if row == Some(0) && col == Some(0) {
                                json!({"jsonrpc": "2.0", "id": 1, "result": [sample]})
                            } else {
                                json!({"jsonrpc": "2.0", "id": 1, "error": {"message": "share not found"}})
                            }
                        }
                        _ => json!({"jsonrpc": "2.0", "id": 1, "error": {"message": "not found"}}),
                    };
                    (StatusCode::OK, Json(response))
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let network = CelestiaNetwork::new(format!("http://{addr}"));
        let header_id = HeaderId("ab".repeat(32));
        let header = network.get_header(&header_id).await.unwrap();
        assert_eq!(header.total_shares, 4);
        let coord = SampleCoordinate { row: 0, col: 0 };
        let (share, proof) = network.request_sample(&header_id, coord).await.unwrap();
        assert_eq!(share.0, q0);
        network
            .verify_sample(&header, coord, &share, &proof)
            .unwrap();

        let missing = network
            .request_sample(&header_id, SampleCoordinate { row: 1, col: 1 })
            .await
            .unwrap_err();
        assert_eq!(missing, DaError::SampleUnavailable { row: 1, col: 1 });
        let outside = network
            .request_sample(&header_id, SampleCoordinate { row: 4, col: 0 })
            .await
            .unwrap_err();
        assert_eq!(outside, DaError::CoordinateOutOfRange);
    }

    #[tokio::test]
    async fn the_node_samples_a_whole_fixture_square() {
        use std::sync::Arc;

        use da_light_node::{Node, NodeConfig};

        let fixture = sample::share_fixture(4);
        let samples = Arc::new(fixture.samples);
        let header = Arc::new(fixture.header);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route(
            "/",
            post({
                let samples = Arc::clone(&samples);
                let header = Arc::clone(&header);
                move |Json(body): Json<Value>| {
                    let samples = Arc::clone(&samples);
                    let header = Arc::clone(&header);
                    async move {
                        let method = body["method"].as_str().unwrap_or("");
                        let response = match method {
                            "header.LocalHead" | "header.GetByHeight" | "header.GetByHash" => {
                                json!({"jsonrpc": "2.0", "id": 1, "result": header.as_ref()})
                            }
                            "share.GetSamples" => {
                                let row = body["params"][1][0]["row"].as_u64().unwrap_or(u64::MAX);
                                let col = body["params"][1][0]["col"].as_u64().unwrap_or(u64::MAX);
                                match samples.get(&(row as u32, col as u32)) {
                                    Some(sample) => {
                                        json!({"jsonrpc": "2.0", "id": 1, "result": [sample]})
                                    }
                                    None => json!({"jsonrpc": "2.0", "id": 1, "error": {"message": "share not found"}}),
                                }
                            }
                            _ => json!({"jsonrpc": "2.0", "id": 1, "error": {"message": "not found"}}),
                        };
                        (StatusCode::OK, Json(response))
                    }
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let network = CelestiaNetwork::new(format!("http://{addr}"));
        let node = Node::new(
            Arc::new(network),
            NodeConfig {
                samples_per_header: 16,
                upstream_id: "celestia".into(),
                upstream_endpoint: format!("http://{addr}"),
                ..NodeConfig::default()
            },
        )
        .unwrap();

        let report = node.sample_latest().await.unwrap();
        let expected = 1.0 - 0.75_f64.powf(16.0);
        assert_eq!(report.header_id, fixture.block_hash);
        assert_eq!(report.height, 4);
        assert_eq!(report.total_shares, 16);
        assert_eq!(report.successful_samples, 16);
        assert_eq!(report.failed_samples, 0);
        assert!((report.confidence - expected).abs() < 1e-12);
        assert_eq!(report.level, "Very High");

        let status = node.status().await;
        assert_eq!(status.peers[0].id, "celestia");
        assert_eq!(status.peers[0].score, 16);
    }
}
