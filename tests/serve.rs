use assert_cmd::prelude::*;
use futures_util::{SinkExt, StreamExt};
use std::{fs, net::TcpListener, process::Command, time::Duration};
use tempfile::TempDir;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::protocol::Message;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn serve_cli_runs_http_and_ws() {
    let dir = TempDir::new().unwrap();
    let http_port = free_port();
    let ws_port = free_port();
    let env_path = dir.path().join("env");
    fs::write(
        &env_path,
        format!(
            "STORE_ROOT={}\nBIND_HTTP=127.0.0.1:{}\nBIND_WS=127.0.0.1:{}\nVERIFY_SIG=0\n",
            dir.path().display(),
            http_port,
            ws_port
        ),
    )
    .unwrap();

    let mut child = Command::cargo_bin("stonr")
        .unwrap()
        .args(["--env", env_path.to_str().unwrap(), "serve"])
        .spawn()
        .unwrap();

    // allow servers to start
    sleep(Duration::from_millis(300)).await;

    // HTTP health check
    let url = format!("http://127.0.0.1:{}/healthz", http_port);
    let body: serde_json::Value = reqwest::get(&url).await.unwrap().json().await.unwrap();
    assert_eq!(body["status"], "ok");

    // WebSocket EOSE
    let ws_url = format!("ws://127.0.0.1:{}/", ws_port);
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
    let req = serde_json::json!(["REQ", "s", {}]);
    ws_stream
        .send(Message::Text(req.to_string()))
        .await
        .unwrap();
    let mut got_eose = false;
    while let Some(msg) = ws_stream.next().await {
        match msg.unwrap() {
            Message::Text(t) if t.contains("EOSE") => {
                got_eose = true;
                break;
            }
            _ => {}
        }
    }
    assert!(got_eose);

    child.kill().unwrap();
    let _ = child.wait();
}
