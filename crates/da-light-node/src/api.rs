use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use da_light_core::{square_width, ConfidenceReport, Header};

use crate::node::Node;

#[derive(Clone)]
struct AppState {
    node: Arc<Node>,
}

#[derive(Debug, Deserialize)]
struct ConfidenceQuery {
    header: Option<String>,
}

#[derive(Debug, Serialize)]
struct HeaderView {
    id: String,
    height: u64,
    total_shares: u32,
    square_width: u32,
    commitment: String,
}

#[derive(Debug, Serialize)]
struct HeadersResponse {
    headers: Vec<HeaderView>,
}

struct ApiFailure {
    status: StatusCode,
    message: String,
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        let body = Json(json!({ "error": self.message }));
        (self.status, body).into_response()
    }
}

pub fn router(node: Arc<Node>) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/confidence", get(confidence))
        .route("/headers", get(headers))
        .route("/sample", post(sample))
        .with_state(AppState { node })
}

async fn status(State(state): State<AppState>) -> Json<crate::StatusSnapshot> {
    Json(state.node.status().await)
}

async fn confidence(
    State(state): State<AppState>,
    Query(query): Query<ConfidenceQuery>,
) -> Result<Json<ConfidenceReport>, ApiFailure> {
    state
        .node
        .confidence_report(query.header.as_deref())
        .await
        .map(Json)
        .ok_or_else(|| ApiFailure {
            status: StatusCode::NOT_FOUND,
            message: "no sampling results for that header".into(),
        })
}

async fn headers(State(state): State<AppState>) -> Json<HeadersResponse> {
    let headers = state
        .node
        .tracked_headers()
        .await
        .iter()
        .map(header_view)
        .collect();
    Json(HeadersResponse { headers })
}

async fn sample(State(state): State<AppState>) -> Result<Json<ConfidenceReport>, ApiFailure> {
    state
        .node
        .sample_latest()
        .await
        .map(Json)
        .map_err(|err| ApiFailure {
            status: StatusCode::BAD_GATEWAY,
            message: err.to_string(),
        })
}

fn header_view(header: &Header) -> HeaderView {
    HeaderView {
        id: header.id.0.clone(),
        height: header.height,
        total_shares: header.total_shares,
        square_width: square_width(header.total_shares),
        commitment: hex::encode(&header.commitment.0),
    }
}
