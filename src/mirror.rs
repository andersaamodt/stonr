//! Upstream relay mirroring for importing events into the local store.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_socks::tcp::Socks5Stream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{client_async, tungstenite::Message, WebSocketStream};
use url::Url;

use crate::{
    config::{Settings, SinceMode},
    event::Event,
    storage::Store,
};

/// Spawn a mirroring task for each configured upstream relay.
pub async fn run(cfg: Settings, store: Store) {
    for relay in cfg.relays_upstream.clone() {
        let cfg_clone = cfg.clone();
        let store_clone = store.clone();
        tokio::spawn(async move {
            if let Err(e) = mirror_relay(relay, cfg_clone, store_clone).await {
                eprintln!("mirror error: {e}");
            }
        });
    }
}

/// Connect to a relay, subscribe, and persist received events.
///
/// The mirroring workflow is:
/// 1. Determine the starting timestamp (`since`) from a stored cursor or fixed
///    configuration.
/// 2. Build a Nostr filter and open a WebSocket connection to the upstream
///    relay (optionally via Tor).
/// 3. Send a `REQ` subscription and process incoming `EVENT` messages,
///    updating the latest timestamp seen.
/// 4. After receiving `EOSE`, write the cursor so the next run resumes from the
///    newest event.
async fn mirror_relay(relay: String, cfg: Settings, store: Store) -> Result<()> {
    // Determine the starting timestamp either from a stored cursor or a fixed
    // configuration value.
    let since = match cfg.filter_since_mode {
        SinceMode::Cursor => read_cursor(&cfg.store_root, &relay).unwrap_or(0),
        SinceMode::Fixed(ts) => ts,
    };
    // Assemble the filter sent in the REQ message based on config options.
    let mut filter = serde_json::Map::new();
    if let Some(a) = cfg.filter_authors.clone() {
        filter.insert(
            "authors".into(),
            Value::Array(a.into_iter().map(Value::String).collect()),
        );
    }
    if let Some(k) = cfg.filter_kinds.clone() {
        filter.insert(
            "kinds".into(),
            Value::Array(k.into_iter().map(|v| Value::Number(v.into())).collect()),
        );
    }
    if let Some(t) = cfg.filter_tag_t.clone() {
        filter.insert(
            "#t".into(),
            Value::Array(t.into_iter().map(Value::String).collect()),
        );
    }
    if since > 0 {
        filter.insert("since".into(), Value::Number(since.into()));
    }
    let req = json!(["REQ", "mirror", Value::Object(filter)]);
    // Open the WebSocket (optionally through Tor) and send the subscription.
    let mut ws = connect_ws(&relay, cfg.tor_socks.as_deref()).await?;
    ws.send(Message::Text(req.to_string())).await?;
    let mut latest = since;
    // Consume messages until EOSE, persisting each event and tracking the
    // newest timestamp seen.
    while let Some(msg) = ws.next().await {
        match msg? {
            Message::Text(txt) => {
                if let Ok(val) = serde_json::from_str::<Value>(&txt) {
                    if let Some(arr) = val.as_array() {
                        match arr.get(0).and_then(|v| v.as_str()) {
                            Some("EVENT") if arr.len() >= 3 => {
                                if let Ok(ev) = serde_json::from_value::<Event>(arr[2].clone()) {
                                    latest = latest.max(ev.created_at);
                                    if let Err(e) = store.ingest(&ev) {
                                        eprintln!("ingest error: {e}");
                                    }
                                }
                            }
                            Some("EOSE") => break,
                            _ => {}
                        }
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    // Persist the cursor so the next run resumes from where we left off.
    write_cursor(&cfg.store_root, &relay, latest)?;
    Ok(())
}

/// Establish a WebSocket connection, optionally via a SOCKS5 proxy.
///
/// The underlying TCP stream may either be a direct `TcpStream` or a
/// `Socks5Stream` when routing through Tor. To hide this difference the stream
/// is boxed as a `dyn AsyncReadWrite`, allowing the caller to treat both cases
/// uniformly. Any network or handshake errors bubble up to the caller.
async fn connect_ws(
    relay: &str,
    tor_socks: Option<&str>,
) -> Result<WebSocketStream<Box<dyn AsyncReadWrite + Unpin + Send>>> {
    let url = Url::parse(relay)?;
    let host = url.host_str().ok_or_else(|| anyhow!("missing host"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("missing port"))?;
    let req = relay.into_client_request()?;
    let stream: Box<dyn AsyncReadWrite + Unpin + Send> = if let Some(proxy) = tor_socks {
        Box::new(Socks5Stream::connect(proxy, (host, port)).await?)
    } else {
        Box::new(TcpStream::connect((host, port)).await?)
    };
    let (ws, _) = client_async(req, stream).await?;
    Ok(ws)
}

/// Blanket trait for boxed async read/write streams.
///
/// `TcpStream` and `Socks5Stream` implement the standard `AsyncRead` and
/// `AsyncWrite` traits but have different concrete types. Boxing them behind a
/// trait object lets `connect_ws` return a single stream type regardless of how
/// the connection was established.
trait AsyncReadWrite: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite> AsyncReadWrite for T {}

/// Compute the cursor file path for a relay URL.
///
/// Each upstream relay gets a SHA1-hashed filename under `cursor/` so that
/// timestamps persist across runs without leaking the relay URL itself.
fn cursor_path(root: &PathBuf, relay: &str) -> PathBuf {
    let mut hasher = Sha1::new();
    hasher.update(relay.as_bytes());
    let hash = hex::encode(hasher.finalize());
    root.join("cursor").join(format!("{}.since", hash))
}

/// Read the last seen timestamp for a relay.
///
/// Returns `None` if no cursor file exists or if the contents fail to parse.
fn read_cursor(root: &PathBuf, relay: &str) -> Option<u64> {
    let path = cursor_path(root, relay);
    std::fs::read_to_string(path).ok()?.parse().ok()
}

/// Persist the last seen timestamp for a relay.
///
/// Any I/O error while creating directories or writing the file is returned
/// to the caller.
fn write_cursor(root: &PathBuf, relay: &str, ts: u64) -> Result<()> {
    let path = cursor_path(root, relay);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, ts.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{Settings, SinceMode},
        event::{Event, Tag},
    };
    use tempfile::TempDir;
    use tokio_tungstenite::{accept_async, tungstenite::Message as TMsg};

    #[tokio::test]
    async fn mirror_ingests_and_updates_cursor() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();

        // prepare events
        let ev1 = Event {
            id: "aa11".into(),
            pubkey: "p".into(),
            kind: 1,
            created_at: 1,
            tags: vec![Tag(vec!["d".into(), "s".into()])],
            content: String::new(),
            sig: String::new(),
        };
        let ev2 = Event {
            id: "bb22".into(),
            pubkey: "p".into(),
            kind: 1,
            created_at: 2,
            tags: vec![Tag(vec!["d".into(), "s".into()])],
            content: String::new(),
            sig: String::new(),
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            // read req
            let _ = ws.next().await;
            ws.send(TMsg::Text(json!(["EVENT", "s", ev1]).to_string()))
                .await
                .unwrap();
            ws.send(TMsg::Text(json!(["EVENT", "s", ev2]).to_string()))
                .await
                .unwrap();
            ws.send(TMsg::Text(serde_json::json!(["EOSE", "s"]).to_string()))
                .await
                .unwrap();
        });

        let relay_url = format!("ws://{}", addr);
        let cfg = Settings {
            store_root: dir.path().to_path_buf(),
            bind_http: String::new(),
            bind_ws: String::new(),
            verify_sig: false,
            relays_upstream: vec![relay_url.clone()],
            tor_socks: None,
            filter_authors: None,
            filter_kinds: None,
            filter_tag_t: None,
            filter_since_mode: SinceMode::Fixed(0),
        };
        mirror_relay(relay_url, cfg.clone(), store.clone())
            .await
            .unwrap();
        server.abort();

        assert!(dir.path().join("events/aa/11/aa11.json").exists());
        assert!(dir.path().join("events/bb/22/bb22.json").exists());
        let mut hasher = Sha1::new();
        hasher.update(cfg.relays_upstream[0].as_bytes());
        let hash = hex::encode(hasher.finalize());
        let cursor = dir.path().join(format!("cursor/{}.since", hash));
        let ts = std::fs::read_to_string(cursor).unwrap();
        assert_eq!(ts.trim(), "2");
    }
    #[tokio::test]
    async fn mirror_resumes_from_cursor() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let relay_url = format!("ws://{}", addr);
        super::write_cursor(&dir.path().to_path_buf(), &relay_url, 5).unwrap();

        let ev = Event {
            id: "aa11".into(),
            pubkey: "p".into(),
            kind: 1,
            created_at: 6,
            tags: vec![Tag(vec!["d".into(), "s".into()])],
            content: String::new(),
            sig: String::new(),
        };
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            if let Some(Ok(TMsg::Text(txt))) = ws.next().await {
                assert!(txt.contains("\"since\":5"));
            }
            ws.send(TMsg::Text(json!(["EVENT", "s", ev]).to_string()))
                .await
                .unwrap();
            ws.send(TMsg::Text(json!(["EOSE", "s"]).to_string()))
                .await
                .unwrap();
        });

        let cfg = Settings {
            store_root: dir.path().to_path_buf(),
            bind_http: String::new(),
            bind_ws: String::new(),
            verify_sig: false,
            relays_upstream: vec![relay_url.clone()],
            tor_socks: None,
            filter_authors: None,
            filter_kinds: None,
            filter_tag_t: None,
            filter_since_mode: SinceMode::Cursor,
        };
        mirror_relay(relay_url.clone(), cfg, store.clone())
            .await
            .unwrap();
        server.abort();
        assert!(dir.path().join("events/aa/11/aa11.json").exists());
        assert_eq!(
            super::read_cursor(&dir.path().to_path_buf(), &relay_url),
            Some(6)
        );
    }

    async fn spawn_socks_proxy(target: std::net::SocketAddr) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut inbound, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2];
            inbound.read_exact(&mut buf).await.unwrap();
            let nmethods = buf[1] as usize;
            let mut methods = vec![0u8; nmethods];
            inbound.read_exact(&mut methods).await.unwrap();
            inbound.write_all(&[0x05, 0x00]).await.unwrap();

            let mut req = [0u8; 4];
            inbound.read_exact(&mut req).await.unwrap();
            match req[3] {
                0x01 => {
                    let mut _addr = [0u8; 4];
                    inbound.read_exact(&mut _addr).await.unwrap();
                }
                0x03 => {
                    let mut len = [0u8; 1];
                    inbound.read_exact(&mut len).await.unwrap();
                    let mut name = vec![0u8; len[0] as usize];
                    inbound.read_exact(&mut name).await.unwrap();
                }
                0x04 => {
                    let mut _addr = [0u8; 16];
                    inbound.read_exact(&mut _addr).await.unwrap();
                }
                _ => {}
            }
            let mut _port = [0u8; 2];
            inbound.read_exact(&mut _port).await.unwrap();
            let mut outbound = tokio::net::TcpStream::connect(target).await.unwrap();
            inbound
                .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            tokio::io::copy_bidirectional(&mut inbound, &mut outbound)
                .await
                .ok();
        });
        addr
    }

    #[tokio::test]
    async fn mirror_via_socks_proxy() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let ev = Event {
            id: "aa11".into(),
            pubkey: "p".into(),
            kind: 1,
            created_at: 1,
            tags: vec![Tag(vec!["d".into(), "s".into()])],
            content: String::new(),
            sig: String::new(),
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            let _ = ws.next().await;
            ws.send(TMsg::Text(json!(["EVENT", "s", ev]).to_string()))
                .await
                .unwrap();
            ws.send(TMsg::Text(json!(["EOSE", "s"]).to_string()))
                .await
                .unwrap();
        });

        let proxy = spawn_socks_proxy(addr).await;
        let relay_url = format!("ws://{}", addr);
        let cfg = Settings {
            store_root: dir.path().to_path_buf(),
            bind_http: String::new(),
            bind_ws: String::new(),
            verify_sig: false,
            relays_upstream: vec![relay_url.clone()],
            tor_socks: Some(proxy.to_string()),
            filter_authors: None,
            filter_kinds: None,
            filter_tag_t: None,
            filter_since_mode: SinceMode::Fixed(0),
        };
        mirror_relay(relay_url, cfg, store.clone()).await.unwrap();
        server.abort();
        assert!(dir.path().join("events/aa/11/aa11.json").exists());
    }

    #[tokio::test]
    async fn mirror_sends_filters_in_req() {
        use serde_json::Value;
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            if let Some(Ok(TMsg::Text(txt))) = ws.next().await {
                let val: Value = serde_json::from_str(&txt).unwrap();
                let filt = &val[2];
                assert_eq!(filt["authors"][0], "a1");
                assert_eq!(filt["kinds"][0], 1);
                assert_eq!(filt["#t"][0], "tag1");
                assert_eq!(filt["since"], 5);
            }
            ws.send(TMsg::Text(json!(["EOSE", "s"]).to_string()))
                .await
                .unwrap();
        });
        let relay_url = format!("ws://{}", addr);
        let cfg = Settings {
            store_root: dir.path().to_path_buf(),
            bind_http: String::new(),
            bind_ws: String::new(),
            verify_sig: false,
            relays_upstream: vec![relay_url.clone()],
            tor_socks: None,
            filter_authors: Some(vec!["a1".into()]),
            filter_kinds: Some(vec![1]),
            filter_tag_t: Some(vec!["tag1".into()]),
            filter_since_mode: SinceMode::Fixed(5),
        };
        mirror_relay(relay_url, cfg, store.clone()).await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn mirror_cursor_mode_without_file_starts_at_zero() {
        use serde_json::Value;
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            if let Some(Ok(TMsg::Text(txt))) = ws.next().await {
                let v: Value = serde_json::from_str(&txt).unwrap();
                assert!(v[2]["since"].is_null());
            }
            ws.send(TMsg::Text(
                json!(["EVENT", "s", {
                    "id": "aa11",
                    "pubkey": "p",
                    "kind": 1,
                    "created_at": 1,
                    "tags": [],
                    "content": "",
                    "sig": ""
                }])
                .to_string(),
            ))
            .await
            .unwrap();
            ws.send(TMsg::Text(json!(["EOSE", "s"]).to_string()))
                .await
                .unwrap();
        });
        let relay_url = format!("ws://{}", addr);
        let cfg = Settings {
            store_root: dir.path().to_path_buf(),
            bind_http: String::new(),
            bind_ws: String::new(),
            verify_sig: false,
            relays_upstream: vec![relay_url.clone()],
            tor_socks: None,
            filter_authors: None,
            filter_kinds: None,
            filter_tag_t: None,
            filter_since_mode: SinceMode::Cursor,
        };
        mirror_relay(relay_url.clone(), cfg, store.clone())
            .await
            .unwrap();
        server.abort();
        let mut hasher = Sha1::new();
        hasher.update(relay_url.as_bytes());
        let hash = hex::encode(hasher.finalize());
        let cursor_path = dir.path().join(format!("cursor/{}.since", hash));
        assert_eq!(std::fs::read_to_string(cursor_path).unwrap().trim(), "1");
    }

    #[tokio::test]
    async fn mirror_ignores_non_text_messages() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let ev = Event {
            id: "aa11".into(),
            pubkey: "p".into(),
            kind: 1,
            created_at: 1,
            tags: vec![Tag(vec!["d".into(), "s".into()])],
            content: String::new(),
            sig: String::new(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            let _ = ws.next().await;
            ws.send(TMsg::Binary(vec![1, 2, 3])).await.unwrap();
            ws.send(TMsg::Text(json!(["EVENT", "s", ev]).to_string()))
                .await
                .unwrap();
            ws.send(TMsg::Text(json!(["EOSE", "s"]).to_string()))
                .await
                .unwrap();
        });
        let relay_url = format!("ws://{}", addr);
        let cfg = Settings {
            store_root: dir.path().to_path_buf(),
            bind_http: String::new(),
            bind_ws: String::new(),
            verify_sig: false,
            relays_upstream: vec![relay_url.clone()],
            tor_socks: None,
            filter_authors: None,
            filter_kinds: None,
            filter_tag_t: None,
            filter_since_mode: SinceMode::Fixed(0),
        };
        mirror_relay(relay_url, cfg, store.clone()).await.unwrap();
        server.abort();
        assert!(dir.path().join("events/aa/11/aa11.json").exists());
    }

    #[test]
    fn cursor_round_trip() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        write_cursor(&root, "ws://example", 42).unwrap();
        assert_eq!(read_cursor(&root, "ws://example"), Some(42));
    }

    #[tokio::test]
    async fn connect_ws_invalid_url_errors() {
        assert!(super::connect_ws("not a url", None).await.is_err());
    }

    #[tokio::test]
    async fn connect_ws_unreachable_host_errors() {
        assert!(super::connect_ws("ws://127.0.0.1:1", None).await.is_err());
    }

    #[tokio::test]
    async fn run_spawns_tasks() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let cfg = Settings {
            store_root: dir.path().to_path_buf(),
            bind_http: String::new(),
            bind_ws: String::new(),
            verify_sig: false,
            relays_upstream: vec!["ws://127.0.0.1:1".into()],
            tor_socks: None,
            filter_authors: None,
            filter_kinds: None,
            filter_tag_t: None,
            filter_since_mode: SinceMode::Fixed(0),
        };
        super::run(cfg, store).await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    #[tokio::test]
    async fn mirror_logs_ingest_errors() {
        use tokio_tungstenite::tungstenite::protocol::Message as TMsg;
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), true);
        store.init().unwrap();

        let bad_ev = serde_json::json!({
            "id": "bad", "pubkey": "p", "kind": 1,
            "created_at": 1, "tags": [], "content": "", "sig": ""
        });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            let _ = ws.next().await;
            ws.send(TMsg::Text(json!(["EVENT", "s", bad_ev]).to_string()))
                .await
                .unwrap();
            ws.send(TMsg::Text(json!(["EOSE", "s"]).to_string()))
                .await
                .unwrap();
        });
        let relay_url = format!("ws://{}", addr);
        let cfg = Settings {
            store_root: dir.path().to_path_buf(),
            bind_http: String::new(),
            bind_ws: String::new(),
            verify_sig: true,
            relays_upstream: vec![relay_url.clone()],
            tor_socks: None,
            filter_authors: None,
            filter_kinds: None,
            filter_tag_t: None,
            filter_since_mode: SinceMode::Fixed(0),
        };
        mirror_relay(relay_url, cfg, store.clone()).await.unwrap();
        server.abort();
        assert!(!dir.path().join("events/ba/d0/bad.json").exists());
    }
}
