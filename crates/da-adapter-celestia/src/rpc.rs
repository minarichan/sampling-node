use std::time::Duration;

use da_light_core::{DaError, Header, HeaderId};
use serde_json::{json, Value};

use crate::header::extended_header_to_header;

#[derive(Clone)]
pub struct CelestiaRpc {
    rpc_url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl CelestiaRpc {
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into().trim_end_matches('/').to_string(),
            token: None,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("reqwest client builds with the default settings"),
        }
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        let token = token.into();
        self.token = (!token.is_empty()).then_some(token);
        self
    }

    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    pub async fn local_head(&self) -> Result<Header, DaError> {
        let result = self.call("header.LocalHead", json!([])).await?;
        extended_header_to_header(&result)
    }

    pub async fn header(&self, id: &HeaderId) -> Result<Header, DaError> {
        match lookup(id)? {
            Lookup::Height(height) => {
                let result = self.call("header.GetByHeight", json!([height])).await?;
                extended_header_to_header(&result)
            }
            Lookup::Hash(hash) => {
                let result = self.call("header.GetByHash", json!([hash])).await?;
                extended_header_to_header(&result)
            }
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, DaError> {
        let mut request = self.http.post(&self.rpc_url).json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }

        let response = request.send().await.map_err(|err| {
            DaError::Message(format!(
                "celestia rpc request to {} failed: {err}",
                self.rpc_url
            ))
        })?;
        let status = response.status();
        let body: Value = response.json().await.map_err(|err| {
            DaError::Message(format!("celestia rpc response was not json: {err}"))
        })?;
        if !status.is_success() {
            return Err(DaError::Message(format!(
                "celestia rpc {method} returned {status}: {body}"
            )));
        }
        if let Some(message) = body.pointer("/error/message").and_then(Value::as_str) {
            return Err(rpc_error(message));
        }
        body.get("result")
            .cloned()
            .ok_or_else(|| DaError::Message(format!("celestia rpc {method} returned no result")))
    }
}

enum Lookup {
    Height(u64),
    Hash(String),
}

fn lookup(id: &HeaderId) -> Result<Lookup, DaError> {
    let id = id.0.trim();
    if id.len() == 64 && id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(Lookup::Hash(id.to_ascii_uppercase()));
    }
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
        let height: u64 = id
            .parse()
            .map_err(|_| DaError::HeaderNotFound(id.to_string()))?;
        if height == 0 {
            return Err(DaError::HeaderNotFound(id.to_string()));
        }
        return Ok(Lookup::Height(height));
    }
    Err(DaError::HeaderNotFound(format!(
        "{id} is not a celestia block hash or height"
    )))
}

fn rpc_error(message: &str) -> DaError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("not found") {
        DaError::HeaderNotFound(message.to_string())
    } else {
        DaError::Message(message.to_string())
    }
}
