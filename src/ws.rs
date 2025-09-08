//! Minimal NIP-01 WebSocket server.

use std::{future::Future, net::SocketAddr, sync::Arc};

use anyhow::Result;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::StreamExt;
use serde_json::Value;

use crate::storage::{Query, Store};

/// Start a WebSocket server speaking a minimal subset of NIP-01.
pub async fn serve_ws(
    addr: SocketAddr,
    store: Store,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let app = Router::new()
        .route("/", get(handler))
        .with_state(Arc::new(store));
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

/// Handle the HTTP upgrade and spawn the connection processor.
async fn handler(ws: WebSocketUpgrade, State(store): State<Arc<Store>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| async move { process(socket, store).await })
}

/// Process incoming REQ/CLOSE messages on a WebSocket connection.
async fn process(mut socket: WebSocket, store: Arc<Store>) {
    while let Some(Ok(msg)) = socket.next().await {
        if let Message::Text(txt) = msg {
            if let Ok(val) = serde_json::from_str::<Value>(&txt) {
                if let Some(arr) = val.as_array() {
                    match arr.get(0).and_then(|v| v.as_str()) {
                        Some("REQ") if arr.len() >= 3 => {
                            let sub = arr[1].as_str().unwrap_or_default().to_string();
                            let filt = arr[2].clone();
                            let q = parse_filter(filt);
                            if let Ok(events) = store.query(q) {
                                for ev in events {
                                    let msg = serde_json::json!(["EVENT", sub, ev]);
                                    let _ = socket.send(Message::Text(msg.to_string())).await;
                                }
                            }
                            let eose = serde_json::json!(["EOSE", sub]);
                            let _ = socket.send(Message::Text(eose.to_string())).await;
                        }
                        Some("CLOSE") => {
                            // ignore for now
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Convert a Nostr filter JSON value into a `Query`.
fn parse_filter(val: Value) -> Query {
    let authors = val.get("authors").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect()
    });
    let kinds = val.get("kinds").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_u64().map(|u| u as u32))
            .collect()
    });
    let d = val
        .get("#d")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.get(0))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let t = val
        .get("#t")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.get(0))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let since = val.get("since").and_then(|v| v.as_u64());
    let until = val.get("until").and_then(|v| v.as_u64());
    let limit = val
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    Query {
        authors,
        kinds,
        d,
        t,
        since,
        until,
        limit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, Tag};
    use futures_util::{SinkExt, StreamExt};
    use tempfile::TempDir;
    use tokio_tungstenite::tungstenite::protocol::Message as TungMessage;

    #[test]
    fn parse_filter_fields() {
        let val = serde_json::json!({
            "authors": ["a1", "a2"],
            "kinds": [1, 2],
            "#d": ["slug"],
            "#t": ["tag"],
            "since": 1,
            "until": 2,
            "limit": 3
        });
        let q = super::parse_filter(val);
        assert_eq!(q.authors.unwrap(), vec!["a1".to_string(), "a2".to_string()]);
        assert_eq!(q.kinds.unwrap(), vec![1, 2]);
        assert_eq!(q.d.unwrap(), "slug");
        assert_eq!(q.t.unwrap(), "tag");
        assert_eq!(q.since, Some(1));
        assert_eq!(q.until, Some(2));
        assert_eq!(q.limit, Some(3));
    }

    #[test]
    fn parse_filter_defaults() {
        let q = super::parse_filter(serde_json::json!({}));
        assert!(q.authors.is_none());
        assert!(q.kinds.is_none());
        assert!(q.d.is_none());
        assert!(q.t.is_none());
        assert!(q.since.is_none());
        assert!(q.until.is_none());
        assert!(q.limit.is_none());
    }

    #[tokio::test]
    async fn ws_round_trip() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let ev = Event {
            id: "aa11".into(),
            pubkey: "p1".into(),
            kind: 1,
            created_at: 1,
            tags: vec![Tag(vec!["d".into(), "slug".into()])],
            content: String::new(),
            sig: String::new(),
        };
        store.ingest(&ev).unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", get(handler))
            .with_state(Arc::new(store));
        let server = axum::serve(listener, app.into_make_service());
        let handle = tokio::spawn(async move {
            server.await.unwrap();
        });

        let url = format!("ws://{}/", addr);
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let req_msg = serde_json::json!([
            "REQ",
            "sub",
            {
                "authors": ["p1"],
                "kinds": [1],
                "#d": ["slug"],
            }
        ]);
        ws_stream
            .send(TungMessage::Text(req_msg.to_string()))
            .await
            .unwrap();

        let mut got_event = false;
        while let Some(msg) = ws_stream.next().await {
            match msg.unwrap() {
                TungMessage::Text(t) => {
                    if t.contains("EVENT") {
                        got_event = true;
                    }
                    if t.contains("EOSE") {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(got_event);
        handle.abort();
    }

    #[tokio::test]
    async fn ws_limit_and_since() {
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
                pubkey: "p1".into(),
                kind: 1,
                created_at: 3,
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
            .route("/", get(handler))
            .with_state(Arc::new(store));
        let server = axum::serve(listener, app.into_make_service());
        let handle = tokio::spawn(async move {
            server.await.unwrap();
        });

        let url = format!("ws://{}/", addr);
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let req_msg = serde_json::json!([
            "REQ",
            "sub",
            {
                "authors": ["p1"],
                "kinds": [1],
                "since": 2,
                "limit": 1
            }
        ]);
        ws_stream
            .send(TungMessage::Text(req_msg.to_string()))
            .await
            .unwrap();

        let mut events = vec![];
        while let Some(msg) = ws_stream.next().await {
            match msg.unwrap() {
                TungMessage::Text(t) => {
                    if t.contains("EVENT") {
                        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                        let ev_id = v[2]["id"].as_str().unwrap().to_string();
                        events.push(ev_id);
                    }
                    if t.contains("EOSE") {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert_eq!(events, vec!["cc33".to_string()]);
        handle.abort();
    }

    #[tokio::test]
    async fn ws_tag_filter() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let ev1 = Event {
            id: "aa11".into(),
            pubkey: "p1".into(),
            kind: 1,
            created_at: 1,
            tags: vec![Tag(vec!["t".into(), "tag1".into()])],
            content: String::new(),
            sig: String::new(),
        };
        let ev2 = Event {
            id: "bb22".into(),
            pubkey: "p1".into(),
            kind: 1,
            created_at: 2,
            tags: vec![Tag(vec!["t".into(), "tag2".into()])],
            content: String::new(),
            sig: String::new(),
        };
        store.ingest(&ev1).unwrap();
        store.ingest(&ev2).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", get(handler))
            .with_state(Arc::new(store));
        let server = axum::serve(listener, app.into_make_service());
        let handle = tokio::spawn(async move {
            server.await.unwrap();
        });
        let url = format!("ws://{}/", addr);
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let req_msg = serde_json::json!([
            "REQ",
            "sub",
            {"#t": ["tag1"]}
        ]);
        ws_stream
            .send(TungMessage::Text(req_msg.to_string()))
            .await
            .unwrap();
        let mut events = vec![];
        while let Some(msg) = ws_stream.next().await {
            match msg.unwrap() {
                TungMessage::Text(t) => {
                    if t.contains("EVENT") {
                        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                        events.push(v[2]["id"].as_str().unwrap().to_string());
                    }
                    if t.contains("EOSE") {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert_eq!(events, vec!["aa11".to_string()]);
        handle.abort();
    }

    #[tokio::test]
    async fn ws_close_then_req() {
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
        let app = Router::new()
            .route("/", get(handler))
            .with_state(Arc::new(store));
        let server = axum::serve(listener, app.into_make_service());
        let handle = tokio::spawn(async move {
            server.await.unwrap();
        });
        let url = format!("ws://{}/", addr);
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        ws_stream
            .send(TungMessage::Text("[\"CLOSE\",\"s\"]".into()))
            .await
            .unwrap();
        let req_msg = serde_json::json!(["REQ", "s", {"authors": ["p1"], "kinds": [1]}]);
        ws_stream
            .send(TungMessage::Text(req_msg.to_string()))
            .await
            .unwrap();
        let mut got_event = false;
        while let Some(msg) = ws_stream.next().await {
            match msg.unwrap() {
                TungMessage::Text(t) => {
                    if t.contains("EVENT") {
                        got_event = true;
                    }
                    if t.contains("EOSE") {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(got_event);
        handle.abort();
    }

    #[tokio::test]
    async fn ws_replaceable_returns_latest() {
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
            .route("/", get(handler))
            .with_state(Arc::new(store));
        let server = axum::serve(listener, app.into_make_service());
        let handle = tokio::spawn(async move {
            server.await.unwrap();
        });
        let url = format!("ws://{}/", addr);
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let req = serde_json::json!([
            "REQ",
            "s",
            {"authors": ["p1"], "kinds": [30023], "#d": ["slug"]}
        ]);
        ws_stream
            .send(TungMessage::Text(req.to_string()))
            .await
            .unwrap();
        let mut events = vec![];
        while let Some(msg) = ws_stream.next().await {
            match msg.unwrap() {
                TungMessage::Text(t) => {
                    if t.contains("EVENT") {
                        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                        events.push(v[2]["id"].as_str().unwrap().to_string());
                    }
                    if t.contains("EOSE") {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert_eq!(events, vec!["bb22".to_string()]);
        handle.abort();
    }

    #[tokio::test]
    async fn ws_limit_zero_returns_eose() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", get(handler))
            .with_state(Arc::new(store));
        let server = axum::serve(listener, app.into_make_service());
        let handle = tokio::spawn(async move {
            server.await.unwrap();
        });
        let url = format!("ws://{}/", addr);
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let req = serde_json::json!(["REQ", "s", {"limit": 0}]);
        ws_stream
            .send(TungMessage::Text(req.to_string()))
            .await
            .unwrap();
        let mut saw_event = false;
        let mut saw_eose = false;
        while let Some(msg) = ws_stream.next().await {
            match msg.unwrap() {
                TungMessage::Text(t) => {
                    if t.contains("EVENT") {
                        saw_event = true;
                    }
                    if t.contains("EOSE") {
                        saw_eose = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(!saw_event);
        assert!(saw_eose);
        handle.abort();
    }

    #[tokio::test]
    async fn ws_malformed_messages_are_ignored() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", get(handler))
            .with_state(Arc::new(store));
        let server = axum::serve(listener, app.into_make_service());
        let handle = tokio::spawn(async move {
            server.await.unwrap();
        });
        let url = format!("ws://{}/", addr);
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        ws_stream
            .send(TungMessage::Text("not json".into()))
            .await
            .unwrap();
        ws_stream
            .send(TungMessage::Text("{}".into()))
            .await
            .unwrap();
        let req = serde_json::json!(["REQ", "s", {"authors": ["p1"], "kinds": [1]}]);
        ws_stream
            .send(TungMessage::Text(req.to_string()))
            .await
            .unwrap();
        let mut saw_eose = false;
        while let Some(msg) = ws_stream.next().await {
            match msg.unwrap() {
                TungMessage::Text(t) => {
                    if t.contains("EOSE") {
                        saw_eose = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(saw_eose);
        ws_stream
            .send(TungMessage::Text("[\"CLOSE\",\"s\"]".into()))
            .await
            .unwrap();
        handle.abort();
    }

    #[tokio::test]
    async fn ws_req_no_matches_returns_only_eose() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/", get(handler))
            .with_state(Arc::new(store));
        let server = axum::serve(listener, app.into_make_service());
        let handle = tokio::spawn(async move {
            server.await.unwrap();
        });
        let url = format!("ws://{}/", addr);
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let req = serde_json::json!(["REQ", "s", {"authors": ["p"], "kinds": [1]}]);
        ws_stream
            .send(TungMessage::Text(req.to_string()))
            .await
            .unwrap();
        let mut saw_event = false;
        let mut saw_eose = false;
        while let Some(msg) = ws_stream.next().await {
            match msg.unwrap() {
                TungMessage::Text(t) => {
                    if t.contains("EVENT") {
                        saw_event = true;
                    }
                    if t.contains("EOSE") {
                        saw_eose = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(!saw_event);
        assert!(saw_eose);
        handle.abort();
    }

    #[tokio::test]
    async fn serve_ws_serves_connections() {
        use tokio_tungstenite::tungstenite::Message as TungMessage;
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let store_clone = store.clone();
        let shutdown = tokio::time::sleep(std::time::Duration::from_millis(100));
        let handle = tokio::spawn(async move {
            super::serve_ws(addr, store_clone, shutdown).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let url = format!("ws://{}/", addr);
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let req = serde_json::json!(["REQ", "s", {"limit": 0}]);
        ws_stream
            .send(TungMessage::Text(req.to_string()))
            .await
            .unwrap();
        let mut saw_eose = false;
        while let Some(msg) = ws_stream.next().await {
            if let TungMessage::Text(t) = msg.unwrap() {
                if t.contains("EOSE") {
                    saw_eose = true;
                    break;
                }
            }
        }
        assert!(saw_eose);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn serve_ws_bind_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        assert!(super::serve_ws(addr, store, std::future::pending())
            .await
            .is_err());
    }
}
