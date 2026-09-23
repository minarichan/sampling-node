#![forbid(unsafe_code)]

//! Celestia adapter.
//!
//! `latest_header` and `get_header` call a celestia-node JSON-RPC endpoint
//! (`header.LocalHead`, `header.GetByHeight`, `header.GetByHash`) and check
//! that the data availability header's row and column roots hash to the
//! block's `data_hash`. Share sampling is the next step.

mod header;
mod rpc;

use async_trait::async_trait;
use da_light_core::{DANetwork, DaError, Header, HeaderId, Sample, SampleCoordinate, SampleProof};

use rpc::CelestiaRpc;

const SAMPLING_NOT_IMPLEMENTED: &str = "celestia share sampling is not implemented yet";

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
        _header_id: &HeaderId,
        _coord: SampleCoordinate,
    ) -> Result<(Sample, SampleProof), DaError> {
        Err(DaError::Unsupported(SAMPLING_NOT_IMPLEMENTED))
    }

    fn verify_sample(
        &self,
        _header: &Header,
        _coord: SampleCoordinate,
        _sample: &Sample,
        _proof: &SampleProof,
    ) -> Result<(), DaError> {
        Err(DaError::Unsupported(SAMPLING_NOT_IMPLEMENTED))
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
    async fn share_sampling_is_still_unimplemented() {
        let network = CelestiaNetwork::new("http://127.0.0.1:9");
        let err = network
            .request_sample(&HeaderId("1".into()), SampleCoordinate { row: 0, col: 0 })
            .await
            .unwrap_err();
        assert_eq!(err, DaError::Unsupported(SAMPLING_NOT_IMPLEMENTED));
    }
}
