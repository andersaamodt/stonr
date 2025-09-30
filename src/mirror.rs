//! Upstream relay mirroring for importing events into the local store.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::sleep;
use tokio_socks::tcp::Socks5Stream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{client_async, tungstenite::Message, WebSocketStream};
use url::Url;

use crate::{
    config::Settings,
    event::Event,
    mirror_config::{self, RelayConfig},
    storage::Store,
};

/// Spawn a mirroring task for each configured upstream relay, reloading the list
/// periodically so CLI changes are picked up while the service is running.
pub async fn run(cfg: Settings, store: Store) {
    let mut tasks: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    loop {
        match mirror_config::list_relays(&cfg.store_root) {
            Ok(relays) => {
                let mut seen = HashSet::new();
                for relay in relays {
                    seen.insert(relay.url.clone());
                    if tasks.contains_key(&relay.url) {
                        continue;
                    }
                    let cfg_clone = cfg.clone();
                    let store_clone = store.clone();
                    let url = relay.url.clone();
                    let handle = tokio::spawn(async move {
                        mirror_loop(cfg_clone, store_clone, url).await;
                    });
                    tasks.insert(relay.url, handle);
                }
                // Abort tasks for relays that were removed from disk.
                let removed: Vec<String> = tasks
                    .keys()
                    .filter(|url| !seen.contains(*url))
                    .cloned()
                    .collect();
                for url in removed {
                    if let Some(handle) = tasks.remove(&url) {
                        handle.abort();
                    }
                }
            }
            Err(e) => eprintln!("mirror config error: {e}"),
        }
        sleep(Duration::from_secs(5)).await;
    }
}

async fn mirror_loop(cfg: Settings, store: Store, url: String) {
    loop {
        match mirror_config::load_relay(&cfg.store_root, &url) {
            Ok(Some(relay_cfg)) => {
                if relay_cfg.requests.is_empty() {
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
                if let Err(e) = mirror_relay(&cfg, &store, relay_cfg.clone()).await {
                    eprintln!("mirror error ({}): {e}", url);
                    sleep(Duration::from_secs(5)).await;
                }
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("mirror load error ({}): {e}", url);
                sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// Connect to a relay, issue all configured subscriptions, and persist incoming
/// events. Cursors are tracked per-request so each subscription can resume from
/// its own last seen timestamp.
async fn mirror_relay(cfg: &Settings, store: &Store, relay: RelayConfig) -> Result<()> {
    let mut ws = connect_ws(&relay.url, cfg.tor_socks.as_deref()).await?;
    let mut pending = HashSet::new();
    let mut latest_by_sub: HashMap<String, u64> = HashMap::new();

    for request in relay.requests.clone() {
        let cursor = mirror_config::read_cursor(&cfg.store_root, &relay, &request);
        let filter_map = mirror_config::filter_to_json(&request.filter, cursor);
        let sub_id = mirror_config::subscription_id(&relay, &request);
        let msg = serde_json::Value::Array(vec![
            serde_json::Value::String("REQ".into()),
            serde_json::Value::String(sub_id.clone()),
            serde_json::Value::Object(filter_map),
        ]);
        ws.send(Message::Text(msg.to_string())).await?;
        pending.insert(sub_id.clone());
        latest_by_sub.insert(sub_id, cursor.unwrap_or(0));
    }

    while let Some(msg) = ws.next().await {
        match msg? {
            Message::Text(txt) => {
                if let Ok(val) = serde_json::from_str::<Value>(&txt) {
                    if let Some(arr) = val.as_array() {
                        match arr.get(0).and_then(|v| v.as_str()) {
                            Some("EVENT") if arr.len() >= 3 => {
                                if let (Some(sub), Some(ev_val)) =
                                    (arr.get(1).and_then(|v| v.as_str()), arr.get(2))
                                {
                                    if let Ok(ev) = serde_json::from_value::<Event>(ev_val.clone())
                                    {
                                        if let Err(e) = store.ingest(&ev) {
                                            eprintln!("ingest error: {e}");
                                        }
                                        latest_by_sub
                                            .entry(sub.to_string())
                                            .and_modify(|ts| *ts = (*ts).max(ev.created_at));
                                    }
                                }
                            }
                            Some("EOSE") => {
                                if let Some(sub) = arr.get(1).and_then(|v| v.as_str()) {
                                    pending.remove(sub);
                                }
                                if pending.is_empty() {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    for request in &relay.requests {
        let sub_id = mirror_config::subscription_id(&relay, request);
        if let Some(latest) = latest_by_sub.get(&sub_id) {
            if *latest > 0 {
                if let Err(e) =
                    mirror_config::write_cursor(&cfg.store_root, &relay, request, *latest)
                {
                    eprintln!("cursor write error ({}:{}): {e}", relay.url, request.name);
                }
            }
        }
    }

    Ok(())
}

/// Establish a WebSocket connection, optionally via a SOCKS5 proxy.
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
trait AsyncReadWrite: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite> AsyncReadWrite for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        event::{Event, Tag},
        mirror_config::{FilterConfig, RelayRequest},
    };
    use tempfile::TempDir;
    use tokio_tungstenite::{accept_async, tungstenite::Message as TMsg};

    fn sample_event(id: &str, created_at: u64) -> Event {
        Event {
            id: id.into(),
            pubkey: "p".into(),
            kind: 1,
            created_at,
            tags: vec![Tag(vec!["d".into(), "s".into()])],
            content: String::new(),
            sig: String::new(),
        }
    }

    #[tokio::test]
    async fn mirror_ingests_and_updates_cursor() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let relay_url = format!("ws://{}", addr);
        let mut filter = FilterConfig::default();
        filter.cursor = true;
        let request = RelayRequest {
            name: "default".into(),
            filter,
        };
        let relay_cfg = RelayConfig {
            url: relay_url.clone(),
            requests: vec![request.clone()],
        };
        let sub_id = mirror_config::subscription_id(&relay_cfg, &request);

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            if let Some(Ok(TMsg::Text(txt))) = ws.next().await {
                assert!(txt.contains(&format!("\"{}\"", sub_id)));
            }
            ws.send(TMsg::Text(
                serde_json::json!(["EVENT", sub_id, sample_event("aa11", 1)]).to_string(),
            ))
            .await
            .unwrap();
            ws.send(TMsg::Text(
                serde_json::json!(["EVENT", sub_id, sample_event("bb22", 2)]).to_string(),
            ))
            .await
            .unwrap();
            ws.send(TMsg::Text(serde_json::json!(["EOSE", sub_id]).to_string()))
                .await
                .unwrap();
        });

        let cfg = Settings {
            store_root: dir.path().to_path_buf(),
            bind_http: String::new(),
            bind_ws: String::new(),
            verify_sig: false,
            relays_upstream: vec![],
            tor_socks: None,
            filter_authors: None,
            filter_kinds: None,
            filter_tag_t: None,
            filter_since_mode: crate::config::SinceMode::Cursor,
        };

        mirror_relay(&cfg, &store, relay_cfg).await.unwrap();
        server.abort();

        assert!(
            mirror_config::read_cursor(
                &cfg.store_root,
                &RelayConfig {
                    url: relay_url.clone(),
                    requests: vec![request.clone()],
                },
                &request
            )
            .unwrap()
                >= 2
        );
    }

    #[tokio::test]
    async fn mirror_respects_existing_cursor() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let relay_url = format!("ws://{}", addr);
        let mut filter = FilterConfig::default();
        filter.cursor = true;
        let request = RelayRequest {
            name: "cursor".into(),
            filter,
        };
        let relay_cfg = RelayConfig {
            url: relay_url.clone(),
            requests: vec![request.clone()],
        };
        mirror_config::write_cursor(&dir.path().to_path_buf(), &relay_cfg, &request, 5).unwrap();
        let sub_id = mirror_config::subscription_id(&relay_cfg, &request);

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            if let Some(Ok(TMsg::Text(txt))) = ws.next().await {
                assert!(txt.contains("\"since\":5"));
            }
            ws.send(TMsg::Text(
                serde_json::json!(["EVENT", sub_id, sample_event("cc33", 6)]).to_string(),
            ))
            .await
            .unwrap();
            ws.send(TMsg::Text(serde_json::json!(["EOSE", sub_id]).to_string()))
                .await
                .unwrap();
        });

        let cfg = Settings {
            store_root: dir.path().to_path_buf(),
            bind_http: String::new(),
            bind_ws: String::new(),
            verify_sig: false,
            relays_upstream: vec![],
            tor_socks: None,
            filter_authors: None,
            filter_kinds: None,
            filter_tag_t: None,
            filter_since_mode: crate::config::SinceMode::Cursor,
        };

        mirror_relay(&cfg, &store, relay_cfg.clone()).await.unwrap();
        server.abort();

        assert_eq!(
            mirror_config::read_cursor(&dir.path().to_path_buf(), &relay_cfg, &request),
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

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let relay_url = format!("ws://{}", addr);
        let mut filter = FilterConfig::default();
        filter.cursor = true;
        let request = RelayRequest {
            name: "proxy".into(),
            filter,
        };
        let relay_cfg = RelayConfig {
            url: relay_url.clone(),
            requests: vec![request.clone()],
        };
        let sub_id = mirror_config::subscription_id(&relay_cfg, &request);

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            let _ = ws.next().await;
            ws.send(TMsg::Text(
                serde_json::json!(["EVENT", sub_id, sample_event("dd44", 1)]).to_string(),
            ))
            .await
            .unwrap();
            ws.send(TMsg::Text(serde_json::json!(["EOSE", sub_id]).to_string()))
                .await
                .unwrap();
        });

        let proxy = spawn_socks_proxy(addr).await;
        let cfg = Settings {
            store_root: dir.path().to_path_buf(),
            bind_http: String::new(),
            bind_ws: String::new(),
            verify_sig: false,
            relays_upstream: vec![],
            tor_socks: Some(proxy.to_string()),
            filter_authors: None,
            filter_kinds: None,
            filter_tag_t: None,
            filter_since_mode: crate::config::SinceMode::Cursor,
        };

        mirror_relay(&cfg, &store, relay_cfg).await.unwrap();
        server.abort();
    }
}
