//! HTTP endpoints for health checks, relay info, and queries.

use anyhow::Result;
use axum::{
    body::Body,
    extract::{Query as AxumQuery, State},
    http::header,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{future::Future, net::SocketAddr, sync::Arc};

use crate::storage::{Query, Store};

#[derive(Clone)]
struct HttpState {
    store: Store,
    verbose: bool,
}

/// Response body for the `/healthz` endpoint.
#[derive(Serialize, Deserialize)]
struct Health {
    /// Always "ok" when the server is running.
    status: String,
}

/// Start an HTTP server exposing `/healthz`, `/query`, and relay info.
pub async fn serve_http(
    addr: SocketAddr,
    store: Store,
    verbose: bool,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let state = Arc::new(HttpState { store, verbose });
    let app = Router::new()
        .route("/", get(relay_info))
        .route("/healthz", get(healthz))
        .route("/query", get(query))
        .with_state(state);
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

/// Health check endpoint.
async fn healthz(State(state): State<Arc<HttpState>>) -> Json<Health> {
    if state.verbose {
        println!("[http] GET /healthz");
    }
    Json(Health {
        status: "ok".to_string(),
    })
}

/// Minimal NIP-11 relay information document.
#[derive(Serialize, Deserialize)]
struct RelayInfo {
    /// Human-readable relay name.
    name: String,
    /// Software identifier (here it is always "stonr").
    software: String,
    /// Semantic version string such as "0.1.0".
    version: String,
}

/// Basic NIP-11 relay information document.
async fn relay_info(State(state): State<Arc<HttpState>>) -> impl axum::response::IntoResponse {
    if state.verbose {
        println!("[http] GET /");
    }
    (
        [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
        Json(RelayInfo {
            name: "stonr".into(),
            software: "stonr".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }),
    )
}

/// URL query parameters accepted by the `/query` endpoint.
#[derive(Deserialize)]
struct QueryParams {
    /// Comma-separated hex public keys.
    authors: Option<String>,
    /// Comma-separated kind numbers (e.g. `1,30023`).
    kinds: Option<String>,
    /// Single `#d` tag value.
    d: Option<String>,
    /// Single `#t` topic value.
    t: Option<String>,
    /// Minimum `created_at` timestamp.
    since: Option<String>,
    /// Maximum `created_at` timestamp.
    until: Option<String>,
    /// Maximum number of events to return.
    limit: Option<String>,
}

/// Convert query string parameters into a [`Query`] understood by the store.
///
/// Supported URL parameters mirror Nostr filter fields:
/// - `authors` – comma-separated list of public keys
/// - `kinds` – comma-separated list of kind numbers
/// - `d` / `t` – single `#d` or `#t` tag value
/// - `since` / `until` – Unix timestamps bounding `created_at`
/// - `limit` – maximum number of events to return
///
/// Example: `/query?authors=npub1&kinds=1,30023&since=1700000000`
fn params_to_query(params: QueryParams) -> Query {
    use serde_json::Value;
    let mut obj = serde_json::Map::new();
    if let Some(a) = params.authors {
        let arr = a.split(',').map(|s| Value::String(s.to_string())).collect();
        obj.insert("authors".into(), Value::Array(arr));
    }
    if let Some(k) = params.kinds {
        let arr = k
            .split(',')
            .filter_map(|v| v.parse::<u32>().ok())
            .map(|v| Value::Number(v.into()))
            .collect();
        obj.insert("kinds".into(), Value::Array(arr));
    }
    if let Some(d) = params.d {
        obj.insert("#d".into(), Value::Array(vec![Value::String(d)]));
    }
    if let Some(t) = params.t {
        obj.insert("#t".into(), Value::Array(vec![Value::String(t)]));
    }
    if let Some(s) = params.since.and_then(|v| v.parse::<u64>().ok()) {
        obj.insert("since".into(), Value::Number(s.into()));
    }
    if let Some(u) = params.until.and_then(|v| v.parse::<u64>().ok()) {
        obj.insert("until".into(), Value::Number(u.into()));
    }
    if let Some(l) = params.limit.and_then(|v| v.parse::<u64>().ok()) {
        obj.insert("limit".into(), Value::Number(l.into()));
    }
    Query::from_value(&Value::Object(obj))
}

/// Parse query parameters and return matching events as NDJSON.
async fn query(
    State(state): State<Arc<HttpState>>,
    AxumQuery(params): AxumQuery<QueryParams>,
) -> axum::response::Response {
    // Translate URL parameters into a `Query` structure shared with the WS API.
    let q = params_to_query(params);
    let events = state.store.query(q).unwrap_or_default();
    if state.verbose {
        println!("[http] GET /query -> {} events", events.len());
    }
    // Return newline-delimited JSON so clients can stream and parse incrementally.
    let body = events
        .into_iter()
        .map(|e| serde_json::to_string(&e).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    axum::response::Response::builder()
        .header("Content-Type", "application/x-ndjson")
        .body(Body::from(body))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use reqwest::{self, header::ACCESS_CONTROL_ALLOW_ORIGIN};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::task;

    fn state(store: &Store, verbose: bool) -> Arc<HttpState> {
        Arc::new(HttpState {
            store: store.clone(),
            verbose,
        })
    }

    #[tokio::test]
    async fn health_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        let state = state(&store, false);
        let app = Router::new()
            .route("/healthz", get(super::healthz))
            .with_state(state);
        let server = axum::serve(listener, app.into_make_service());
        let handle = task::spawn(async move {
            server.await.unwrap();
        });

        let url = format!("http://{}/healthz", addr);
        let resp = reqwest::get(&url).await.unwrap();
        let body: super::Health = resp.json().await.unwrap();
        assert_eq!(body.status, "ok");
        handle.abort();
    }

    #[tokio::test]
    async fn relay_info_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        let state = state(&store, false);
        let app = Router::new()
            .route("/", get(super::relay_info))
            .with_state(state);
        let server = axum::serve(listener, app.into_make_service());
        let handle = task::spawn(async move {
            server.await.unwrap();
        });

        let url = format!("http://{}/", addr);
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(
            resp.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            "*"
        );
        let info: super::RelayInfo = resp.json().await.unwrap();
        assert_eq!(info.name, "stonr");
        handle.abort();
    }

    #[tokio::test]
    async fn query_endpoint_filters() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let events = vec![
            Event {
                id: "aa11".into(),
                pubkey: "p1".into(),
                kind: 1,
                created_at: 1,
                tags: vec![],
                content: String::new(),
                sig: String::new(),
            },
            Event {
                id: "bb22".into(),
                pubkey: "p1".into(),
                kind: 1,
                created_at: 2,
                tags: vec![],
                content: String::new(),
                sig: String::new(),
            },
            Event {
                id: "cc33".into(),
                pubkey: "p2".into(),
                kind: 1,
                created_at: 3,
                tags: vec![],
                content: String::new(),
                sig: String::new(),
            },
            Event {
                id: "dd44".into(),
                pubkey: "p1".into(),
                kind: 2,
                created_at: 4,
                tags: vec![],
                content: String::new(),
                sig: String::new(),
            },
        ];
        for ev in &events {
            store.ingest(ev).unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/query", get(super::query))
            .with_state(state(&store, false));
        let server = axum::serve(listener, app.into_make_service());
        let handle = task::spawn(async move {
            server.await.unwrap();
        });
        let url = format!(
            "http://{}/query?authors=p1,p2&kinds=1&since=2&until=3&limit=2",
            addr
        );
        let resp = reqwest::get(&url).await.unwrap().text().await.unwrap();
        let lines: Vec<_> = resp.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("cc33"));
        assert!(lines[1].contains("bb22"));
        handle.abort();
    }

    #[tokio::test]
    async fn query_d_and_t_params() {
        use crate::event::Tag;
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let ev1 = Event {
            id: "aa11".into(),
            pubkey: "p1".into(),
            kind: 1,
            created_at: 1,
            tags: vec![
                Tag(vec!["d".into(), "slug1".into()]),
                Tag(vec!["t".into(), "tag1".into()]),
            ],
            content: String::new(),
            sig: String::new(),
        };
        let ev2 = Event {
            id: "bb22".into(),
            pubkey: "p2".into(),
            kind: 1,
            created_at: 2,
            tags: vec![
                Tag(vec!["d".into(), "slug2".into()]),
                Tag(vec!["t".into(), "tag2".into()]),
            ],
            content: String::new(),
            sig: String::new(),
        };
        store.ingest(&ev1).unwrap();
        store.ingest(&ev2).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/query", get(super::query))
            .with_state(state(&store, false));
        let server = axum::serve(listener, app.into_make_service());
        let handle = task::spawn(async move {
            server.await.unwrap();
        });
        let url = format!("http://{}/query?d=slug1&t=tag1", addr);
        let resp = reqwest::get(&url).await.unwrap().text().await.unwrap();
        let lines: Vec<_> = resp.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("aa11"));
        handle.abort();
    }

    #[tokio::test]
    async fn query_replaceable_returns_latest() {
        use crate::event::Tag;
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let e1 = Event {
            id: "aa11".into(),
            pubkey: "p1".into(),
            kind: 30023,
            created_at: 1,
            tags: vec![Tag(vec!["d".into(), "slug".into()])],
            content: String::new(),
            sig: String::new(),
        };
        let e2 = Event {
            id: "bb22".into(),
            pubkey: "p1".into(),
            kind: 30023,
            created_at: 2,
            tags: vec![Tag(vec!["d".into(), "slug".into()])],
            content: String::new(),
            sig: String::new(),
        };
        store.ingest(&e1).unwrap();
        store.ingest(&e2).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/query", get(super::query))
            .with_state(state(&store, false));
        let server = axum::serve(listener, app.into_make_service());
        let handle = task::spawn(async move {
            server.await.unwrap();
        });
        let url = format!(
            "http://{}/query?authors=p1&kinds=30023&d=slug&limit=10",
            addr
        );
        let resp = reqwest::get(&url).await.unwrap().text().await.unwrap();
        let lines: Vec<_> = resp.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("bb22"));
        handle.abort();
    }

    #[tokio::test]
    async fn query_no_matches() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let ev = Event {
            id: "aa11".into(),
            pubkey: "p1".into(),
            kind: 1,
            created_at: 1,
            tags: vec![],
            content: String::new(),
            sig: String::new(),
        };
        store.ingest(&ev).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = state(&store, true);
        let app = Router::new()
            .route("/query", get(super::query))
            .with_state(state);
        let server = axum::serve(listener, app.into_make_service());
        let handle = task::spawn(async move {
            server.await.unwrap();
        });
        let url = format!("http://{}/query?authors=p2", addr);
        let resp = reqwest::get(&url).await.unwrap().text().await.unwrap();
        assert!(resp.is_empty());
        handle.abort();
    }

    #[tokio::test]
    async fn query_no_params_returns_empty() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/query", get(super::query))
            .with_state(state(&store, false));
        let server = axum::serve(listener, app.into_make_service());
        let handle = task::spawn(async move {
            server.await.unwrap();
        });
        let url = format!("http://{}/query", addr);
        let resp = reqwest::get(&url).await.unwrap().text().await.unwrap();
        assert!(resp.is_empty());
        handle.abort();
    }

    #[tokio::test]
    async fn query_invalid_numbers_are_ignored() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/query", get(super::query))
            .with_state(state(&store, false));
        let server = axum::serve(listener, app.into_make_service());
        let handle = task::spawn(async move {
            server.await.unwrap();
        });
        let url = format!("http://{}/query?since=oops&limit=nah", addr);
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.is_empty());
        handle.abort();
    }

    #[tokio::test]
    async fn serve_http_serves_health() {
        use std::time::Duration;
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let store_clone = store.clone();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let shutdown = async move {
            let _ = shutdown_rx.await;
        };
        let handle = tokio::spawn(async move {
            super::serve_http(addr, store_clone, false, shutdown)
                .await
                .unwrap();
        });
        let url = format!("http://{}/healthz", addr);
        let resp: super::Health = {
            let mut attempts = 0;
            const MAX_ATTEMPTS: usize = 50;
            const RETRY_DELAY_MS: u64 = 50;
            loop {
                match reqwest::get(&url).await {
                    Ok(resp) => break resp,
                    Err(err) => {
                        attempts += 1;
                        if attempts >= MAX_ATTEMPTS {
                            panic!(
                                "failed to fetch health endpoint after {} retries: {:?}",
                                attempts, err
                            );
                        }
                        tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
                    }
                }
            }
        }
        .json()
        .await
        .unwrap();
        assert_eq!(resp.status, "ok");
        let _ = shutdown_tx.send(());
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn serve_http_bind_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        // binding to the same address should error because it's already taken
        assert!(
            super::serve_http(addr, store, false, std::future::pending())
                .await
                .is_err()
        );
    }
}
