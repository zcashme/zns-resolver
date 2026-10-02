//! A typed, read-only client for the ZNS resolver JSON-RPC API.

use std::sync::atomic::{AtomicU64, Ordering};

use reqwest::Client as HttpClient;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

/// An active name binding returned by the resolver.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct NameRecord {
    pub name: String,
    pub address: String,
    pub txid: String,
    pub height: u64,
    pub last_action: Action,
    /// `"none"` for no expiry, or a decimal Unix timestamp.
    pub expires_at: String,
}

/// A lifecycle action recorded by the resolver.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Claim,
    Update,
    Release,
}

/// Trims whitespace and lowercases a lookup name, like the TypeScript client.
pub fn normalize_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

/// Checks the canonical ZNS name grammar: 1–63 lowercase ASCII letters or digits.
pub fn is_valid_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

/// The result of the resolver's `resolve` method.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ResolveResult {
    /// Exact name query: a record, or `None` when no active name exists.
    Exact(Option<NameRecord>),
    /// Empty query or address query.
    Many(Vec<NameRecord>),
}

/// A page returned by `list_names` or `reverse_lookup`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub limit: u64,
    pub offset: u64,
}

/// One entry in the resolver's lifecycle event history.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct NameEvent {
    pub id: i64,
    pub name: String,
    pub action: Action,
    pub txid: String,
    pub height: u64,
    pub action_index: u64,
    pub address: String,
    /// `"none"` for no expiry, or a decimal Unix timestamp.
    pub expires_at: String,
}

/// A page returned by `events`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct EventsPage {
    pub events: Vec<NameEvent>,
    pub total: u64,
    pub limit: u64,
    pub offset: u64,
}

/// Optional filters for `events`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct EventsFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_height: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
}

/// Current resolver sync state and metadata.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub synced_height: u64,
    pub synced: bool,
    pub viewing_key: String,
    pub registered: u64,
}

/// Errors returned by the resolver client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("resolver HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("resolver response JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("resolver JSON-RPC error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("resolver JSON-RPC response ID mismatch: expected {expected}, got {actual:?}")]
    ResponseIdMismatch { expected: u64, actual: Option<u64> },
}

/// An async client for the resolver's read-only JSON-RPC API.
pub struct ResolverClient {
    endpoint: String,
    http: HttpClient,
    next_id: AtomicU64,
}

impl ResolverClient {
    /// Creates a client for a resolver JSON-RPC endpoint.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            http: HttpClient::new(),
            next_id: AtomicU64::new(1),
        }
    }

    /// Queries a name, address, or the empty string for all names.
    pub async fn resolve(
        &self,
        query: impl Into<String>,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> Result<ResolveResult, Error> {
        self.call(
            "resolve",
            ResolveParams {
                query: query.into(),
                limit,
                offset,
            },
        )
        .await
    }

    /// Resolves a name to its active binding, if any.
    pub async fn resolve_name(&self, name: &str) -> Result<Option<NameRecord>, Error> {
        let name = normalize_name(name);
        if !is_valid_name(&name) {
            return Ok(None);
        }
        self.call(
            "resolve",
            ResolveParams {
                query: name,
                limit: None,
                offset: None,
            },
        )
        .await
    }

    /// Checks whether a canonical name is available.
    pub async fn is_available(&self, name: &str) -> Result<bool, Error> {
        Ok(self.resolve_name(name).await?.is_none())
    }

    /// Lists active names with the resolver's pagination envelope.
    pub async fn list_names(
        &self,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> Result<Page<NameRecord>, Error> {
        self.call("list_names", Pagination { limit, offset }).await
    }

    /// Finds active names bound to a unified address.
    pub async fn reverse_lookup(
        &self,
        address: &str,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> Result<Page<NameRecord>, Error> {
        self.call(
            "reverse_lookup",
            ReverseLookupParams {
                address,
                limit,
                offset,
            },
        )
        .await
    }

    /// Reads the resolver's current sync state.
    pub async fn status(&self) -> Result<Status, Error> {
        self.call("status", EmptyParams {}).await
    }

    /// Reads a filtered page of lifecycle events.
    pub async fn events(&self, filter: EventsFilter) -> Result<EventsPage, Error> {
        self.call("events", filter).await
    }

    async fn call<P, R>(&self, method: &str, params: P) -> Result<R, Error>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = RpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        let response: RpcResponse = self
            .http
            .post(&self.endpoint)
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if response.id != Some(id) {
            return Err(Error::ResponseIdMismatch {
                expected: id,
                actual: response.id,
            });
        }
        if let Some(error) = response.error {
            return Err(Error::Rpc {
                code: error.code,
                message: error.message,
            });
        }

        Ok(serde_json::from_value(response.result)?)
    }
}

#[derive(Serialize)]
struct RpcRequest<'a, P> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: P,
}

#[derive(Deserialize)]
struct RpcResponse {
    id: Option<u64>,
    #[serde(default)]
    result: Value,
    error: Option<RpcFault>,
}

#[derive(Deserialize)]
struct RpcFault {
    code: i64,
    message: String,
}

#[derive(Serialize)]
struct ResolveParams {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<u64>,
}

#[derive(Serialize)]
struct Pagination {
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<u64>,
}

#[derive(Serialize)]
struct ReverseLookupParams<'a> {
    address: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<u64>,
}

#[derive(Serialize)]
struct EmptyParams {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_exact_record_and_null_shapes() {
        let record: ResolveResult = serde_json::from_str(
            r#"{"name":"alice","address":"utest1abc","txid":"00","height":42,"last_action":"claim","expires_at":"none"}"#,
        )
        .unwrap();
        assert!(matches!(record, ResolveResult::Exact(Some(_))));

        let missing: ResolveResult = serde_json::from_str("null").unwrap();
        assert_eq!(missing, ResolveResult::Exact(None));
    }

    #[test]
    fn resolves_array_shape_and_serializes_event_filters_as_rpc_params() {
        let many: ResolveResult = serde_json::from_str("[]").unwrap();
        assert_eq!(many, ResolveResult::Many(vec![]));

        let params = serde_json::to_value(EventsFilter {
            name: Some("alice".to_string()),
            action: Some(Action::Update),
            since_height: Some(42),
            limit: Some(10),
            offset: Some(5),
        })
        .unwrap();
        assert_eq!(params["action"], "update");
        assert_eq!(params["since_height"], 42);
        assert_eq!(params["offset"], 5);
    }

    #[test]
    fn name_helpers_follow_the_canonical_grammar() {
        assert_eq!(normalize_name(" Alice "), "alice");
        assert!(is_valid_name("alice42"));
        assert!(!is_valid_name("Alice"));
        assert!(!is_valid_name("a-name"));
        assert!(is_valid_name(&"a".repeat(63)));
        assert!(!is_valid_name(&"a".repeat(64)));
    }
}
