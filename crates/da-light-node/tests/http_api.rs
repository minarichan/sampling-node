use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use da_adapter_mock::{MockConfig, MockNetwork};
use da_light_core::DANetwork;
use da_light_node::api::router;
use da_light_node::{Node, NodeConfig};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn body_of(app: axum::Router, method: &str, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn http_api_reports_status_and_accepts_another_round() {
    let network = MockNetwork::generate(MockConfig {
        share_count: 64,
        header_count: 2,
        withheld_per_header: 0,
        share_size: 32,
    })
    .unwrap();
    let node = Arc::new(
        Node::new(
            Arc::new(network) as Arc<dyn DANetwork>,
            NodeConfig {
                samples_per_header: 8,
                ..NodeConfig::default()
            },
        )
        .unwrap(),
    );
    let first = node.sample_latest().await.unwrap();
    assert_eq!(first.successful_samples, 8);
    assert_eq!(first.height, 2);

    let app = router(Arc::clone(&node));
    let (status, body) = body_of(app.clone(), "GET", "/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["headers_tracked"], 1);
    assert_eq!(body["latest"]["successful_samples"], 8);

    let (status, headers) = body_of(app.clone(), "GET", "/headers").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["headers"][0]["height"], 2);
    assert_eq!(headers["headers"][0]["square_width"], 8);
    assert_eq!(
        headers["headers"][0]["commitment"].as_str().unwrap().len(),
        64
    );

    let (status, second) = body_of(app.clone(), "POST", "/sample").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["successful_samples"], 16);
    assert!(second["confidence"].as_f64().unwrap() > first.confidence);

    let (status, missing) = body_of(app, "GET", "/confidence?header=missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(missing["error"].as_str().unwrap().contains("no sampling"));
}
