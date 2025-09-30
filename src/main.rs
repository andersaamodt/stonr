//! Command line interface for operating the relay. Supports initialization,
//! ingesting events, serving HTTP/WebSocket endpoints, mirroring from upstream
//! relays, and signature verification.

mod config;
mod event;
mod mirror;
mod server;
mod storage;
mod ws;

use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::bail;
use clap::{Parser, Subcommand};
use config::Settings;
use storage::Store;

/// Command line interface entry point.
#[derive(Parser)]
#[command(
    name = "stonr",
    author,
    version,
    about = "File-backed Nostr relay",
    short_flag = 'v',
    long_flag = "version"
)]
struct Cli {
    /// Path to the `.env` configuration file.
    #[arg(long, default_value = ".env")]
    env: String,
    /// Subcommand to execute.
    #[command(subcommand)]
    command: Commands,
}

/// Supported CLI subcommands.
#[derive(Subcommand)]
enum Commands {
    /// Initialize the directory tree at `STORE_ROOT`.
    Init,
    /// Ingest one or more event files.
    Ingest {
        /// Paths to JSON event files to ingest.
        #[arg(required = true)]
        files: Vec<String>,
    },
    /// Rebuild indexes and latest pointers from existing events.
    Reindex,
    /// Launch HTTP and WebSocket services (and mirror if configured).
    Serve,
    /// Verify a random sample of stored events.
    Verify {
        #[arg(long, default_value_t = 1000)]
        sample: usize,
    },
    /// Manage upstream mirror configuration.
    Mirror {
        #[command(subcommand)]
        action: MirrorAction,
    },
}

/// Operations available under `stonr mirror`.
#[derive(Subcommand)]
enum MirrorAction {
    /// Add an upstream relay after verifying connectivity.
    Add { url: String },
    /// Remove an upstream relay from the configuration.
    Remove { url: String },
}

/// Execute the selected CLI subcommand.
async fn run(cli: Cli) -> anyhow::Result<()> {
    ensure_env_file(&cli.env)?;
    let cfg = Settings::from_env(&cli.env)?;
    match cli.command {
        Commands::Mirror { action } => {
            handle_mirror(action, &cli.env, &cfg).await?;
        }
        command => {
            let store = Store::new(cfg.store_root.clone(), cfg.verify_sig);
            match command {
                Commands::Init => {
                    // Create the on-disk directory structure.
                    store.init()?;
                }
                Commands::Ingest { files } => {
                    // Load each JSON file and store it if not already present.
                    for f in files {
                        let data = std::fs::read_to_string(&f)?;
                        let ev: event::Event = serde_json::from_str(&data)?;
                        store.ingest(&ev)?;
                    }
                }
                Commands::Reindex => {
                    // Rebuild indexes and latest pointers from existing events.
                    store.reindex()?;
                }
                Commands::Serve => {
                    // Initialize storage then start HTTP and WS servers.
                    store.init()?;
                    let http_addr: SocketAddr = cfg.bind_http.as_str().parse()?;
                    let ws_addr: SocketAddr = cfg.bind_ws.as_str().parse()?;
                    // If upstream relays are configured, start mirroring in the background.
                    if !cfg.relays_upstream.is_empty() {
                        let store_clone = store.clone();
                        let cfg_clone = cfg.clone();
                        tokio::spawn(async move { mirror::run(cfg_clone, store_clone).await });
                    }
                    let store_http = store.clone();
                    let store_ws = store.clone();
                    tokio::try_join!(
                        server::serve_http(http_addr, store_http, std::future::pending()),
                        ws::serve_ws(ws_addr, store_ws, std::future::pending())
                    )?;
                }
                Commands::Verify { sample } => {
                    // Randomly verify Schnorr signatures for `sample` events.
                    store.verify_sample(sample)?;
                }
                Commands::Mirror { .. } => unreachable!(),
            }
        }
    }
    Ok(())
}

/// Create a default `.env` file if one is not already present at `path`.
fn ensure_env_file(path: &str) -> anyhow::Result<()> {
    let env_path = Path::new(path);
    if env_path.exists() {
        return Ok(());
    }
    if let Some(parent) = env_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let base_dir = match env_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => std::env::current_dir()?,
    };
    let store_root = base_dir.join("stonr-data");
    let mut content = String::new();
    content.push_str(&format!("STORE_ROOT={}\n", display_path(&store_root)));
    content.push_str("BIND_HTTP=127.0.0.1:7777\n");
    content.push_str("BIND_WS=127.0.0.1:7778\n");
    content.push_str("VERIFY_SIG=0\n");
    content.push_str("RELAYS_UPSTREAM=\n");
    content.push_str("FILTER_AUTHORS=\n");
    content.push_str("FILTER_KINDS=\n");
    content.push_str("FILTER_TAG_T=\n");
    content.push_str("FILTER_SINCE_MODE=cursor\n");
    content.push_str("TOR_SOCKS=\n");
    fs::write(env_path, content)?;
    Ok(())
}

fn display_path(path: &PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

async fn handle_mirror(action: MirrorAction, env_path: &str, cfg: &Settings) -> anyhow::Result<()> {
    match action {
        MirrorAction::Add { url } => add_mirror(env_path, cfg, url).await?,
        MirrorAction::Remove { url } => remove_mirror(env_path, cfg, url)?,
    }
    Ok(())
}

async fn add_mirror(env_path: &str, cfg: &Settings, url: String) -> anyhow::Result<()> {
    if cfg.relays_upstream.iter().any(|existing| existing == &url) {
        bail!("mirror already configured: {url}");
    }
    mirror::test_connection(&url, cfg.tor_socks.as_deref()).await?;
    let mut relays = cfg.relays_upstream.clone();
    relays.push(url);
    write_relays_to_env(env_path, &relays)?;
    Ok(())
}

fn remove_mirror(env_path: &str, cfg: &Settings, url: String) -> anyhow::Result<()> {
    let mut relays = cfg.relays_upstream.clone();
    let before = relays.len();
    relays.retain(|existing| existing != &url);
    if relays.len() == before {
        bail!("mirror not configured: {url}");
    }
    write_relays_to_env(env_path, &relays)?;
    Ok(())
}

fn write_relays_to_env(env_path: &str, relays: &[String]) -> anyhow::Result<()> {
    let content = fs::read_to_string(env_path)?;
    let relays_joined = relays.join(",");
    let mut new_content = String::new();
    let mut replaced = false;
    for line in content.lines() {
        if line.starts_with("RELAYS_UPSTREAM=") {
            new_content.push_str(&format!("RELAYS_UPSTREAM={relays_joined}\n"));
            replaced = true;
        } else {
            new_content.push_str(line);
            new_content.push('\n');
        }
    }
    if !replaced {
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(&format!("RELAYS_UPSTREAM={relays_joined}\n"));
    }
    fs::write(env_path, new_content)?;
    std::env::set_var("RELAYS_UPSTREAM", relays_joined);
    Ok(())
}

#[cfg(not(test))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    run(cli).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use futures_util::StreamExt;
    use std::{fs, sync::Mutex, time::Duration};
    use tempfile::TempDir;
    use tokio::{net::TcpListener, task};
    use tokio_tungstenite::{accept_async, tungstenite::Message as TMsg};

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    async fn write_env(dir: &TempDir, extra: &str) -> String {
        let env_path = dir.path().join(".env");
        let content = format!(
            "STORE_ROOT={}\nBIND_HTTP=127.0.0.1:0\nBIND_WS=127.0.0.1:0\nVERIFY_SIG=0\nRELAYS_UPSTREAM=\n{}",
            dir.path().to_str().unwrap(),
            extra
        );
        fs::write(&env_path, content).unwrap();
        env_path.to_str().unwrap().into()
    }

    #[tokio::test]
    async fn run_init_ingest_reindex_verify() {
        let _g = ENV_MUTEX.lock().unwrap();
        for v in [
            "STORE_ROOT",
            "BIND_HTTP",
            "BIND_WS",
            "VERIFY_SIG",
            "RELAYS_UPSTREAM",
            "TOR_SOCKS",
            "FILTER_AUTHORS",
            "FILTER_KINDS",
            "FILTER_TAG_T",
            "FILTER_SINCE_MODE",
        ] {
            std::env::remove_var(v);
        }
        let dir = TempDir::new().unwrap();
        let env_file = write_env(&dir, "").await;

        // init
        run(Cli {
            env: env_file.clone(),
            command: Commands::Init,
        })
        .await
        .unwrap();

        // ingest
        let ev_path = dir.path().join("ev.json");
        let ev = Event {
            id: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            pubkey: "p".into(),
            kind: 1,
            created_at: 1,
            tags: vec![],
            content: String::new(),
            sig: String::new(),
        };
        fs::write(&ev_path, serde_json::to_string(&ev).unwrap()).unwrap();
        run(Cli {
            env: env_file.clone(),
            command: Commands::Ingest {
                files: vec![ev_path.to_str().unwrap().into()],
            },
        })
        .await
        .unwrap();

        // reindex
        run(Cli {
            env: env_file.clone(),
            command: Commands::Reindex,
        })
        .await
        .unwrap();

        // verify with zero sample to avoid signature check
        run(Cli {
            env: env_file,
            command: Commands::Verify { sample: 0 },
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn init_creates_default_env() {
        let _g = ENV_MUTEX.lock().unwrap();
        for v in [
            "STORE_ROOT",
            "BIND_HTTP",
            "BIND_WS",
            "VERIFY_SIG",
            "RELAYS_UPSTREAM",
            "TOR_SOCKS",
            "FILTER_AUTHORS",
            "FILTER_KINDS",
            "FILTER_TAG_T",
            "FILTER_SINCE_MODE",
        ] {
            std::env::remove_var(v);
        }
        let dir = TempDir::new().unwrap();
        let env_path = dir.path().join(".env");
        run(Cli {
            env: env_path.to_string_lossy().into_owned(),
            command: Commands::Init,
        })
        .await
        .unwrap();

        let data = fs::read_to_string(&env_path).unwrap();
        let expected_root = dir.path().join("stonr-data");
        assert!(data.contains(&format!("STORE_ROOT={}", expected_root.to_string_lossy())));
        assert!(data.contains("BIND_HTTP=127.0.0.1:7777"));
        assert!(data.contains("BIND_WS=127.0.0.1:7778"));
        assert!(expected_root.join("events").exists());
    }

    #[tokio::test]
    async fn run_serve_starts_http() {
        let _g = ENV_MUTEX.lock().unwrap();
        for v in [
            "STORE_ROOT",
            "BIND_HTTP",
            "BIND_WS",
            "VERIFY_SIG",
            "RELAYS_UPSTREAM",
            "TOR_SOCKS",
            "FILTER_AUTHORS",
            "FILTER_KINDS",
            "FILTER_TAG_T",
            "FILTER_SINCE_MODE",
        ] {
            std::env::remove_var(v);
        }
        let dir = TempDir::new().unwrap();
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_port = http_listener.local_addr().unwrap().port();
        drop(http_listener);
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_port = ws_listener.local_addr().unwrap().port();
        drop(ws_listener);
        let env_path = dir.path().join(".env");
        let content = format!(
            "STORE_ROOT={}\nBIND_HTTP=127.0.0.1:{}\nBIND_WS=127.0.0.1:{}\nVERIFY_SIG=0\nRELAYS_UPSTREAM=\n",
            dir.path().to_str().unwrap(),
            http_port,
            ws_port
        );
        fs::write(&env_path, content).unwrap();
        let env_str = env_path.to_str().unwrap().to_string();

        let handle = task::spawn(run(Cli {
            env: env_str.clone(),
            command: Commands::Serve,
        }));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let url = format!("http://127.0.0.1:{}/healthz", http_port);
        let resp = reqwest::get(url).await.unwrap();
        assert!(resp.status().is_success());
        handle.abort();
    }

    #[tokio::test]
    async fn run_serve_spawns_mirror() {
        let _g = ENV_MUTEX.lock().unwrap();
        for v in [
            "STORE_ROOT",
            "BIND_HTTP",
            "BIND_WS",
            "VERIFY_SIG",
            "RELAYS_UPSTREAM",
            "TOR_SOCKS",
            "FILTER_AUTHORS",
            "FILTER_KINDS",
            "FILTER_TAG_T",
            "FILTER_SINCE_MODE",
        ] {
            std::env::remove_var(v);
        }
        let dir = TempDir::new().unwrap();
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_port = http_listener.local_addr().unwrap().port();
        drop(http_listener);
        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_port = ws_listener.local_addr().unwrap().port();
        drop(ws_listener);
        let env_path = dir.path().join(".env");
        let content = format!(
            "STORE_ROOT={}\nBIND_HTTP=127.0.0.1:{}\nBIND_WS=127.0.0.1:{}\nVERIFY_SIG=0\nRELAYS_UPSTREAM=ws://127.0.0.1:9\n",
            dir.path().to_str().unwrap(),
            http_port,
            ws_port
        );
        fs::write(&env_path, content).unwrap();
        let env_str = env_path.to_str().unwrap().to_string();

        let handle = task::spawn(run(Cli {
            env: env_str.clone(),
            command: Commands::Serve,
        }));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let url = format!("http://127.0.0.1:{}/healthz", http_port);
        let resp = reqwest::get(url).await.unwrap();
        assert!(resp.status().is_success());
        handle.abort();
    }

    #[tokio::test]
    async fn mirror_add_validates_and_updates_env() {
        let _g = ENV_MUTEX.lock().unwrap();
        for v in [
            "STORE_ROOT",
            "BIND_HTTP",
            "BIND_WS",
            "VERIFY_SIG",
            "RELAYS_UPSTREAM",
            "TOR_SOCKS",
            "FILTER_AUTHORS",
            "FILTER_KINDS",
            "FILTER_TAG_T",
            "FILTER_SINCE_MODE",
        ] {
            std::env::remove_var(v);
        }
        let dir = TempDir::new().unwrap();
        let env_file = write_env(&dir, "").await;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = task::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            while let Some(msg) = ws.next().await {
                if matches!(msg.unwrap(), TMsg::Close(_)) {
                    break;
                }
            }
        });

        let url = format!("ws://{}", addr);
        run(Cli {
            env: env_file.clone(),
            command: Commands::Mirror {
                action: MirrorAction::Add { url: url.clone() },
            },
        })
        .await
        .unwrap();
        server.await.unwrap();

        let data = fs::read_to_string(&env_file).unwrap();
        assert!(data.contains(&format!("RELAYS_UPSTREAM={url}")));
    }

    #[tokio::test]
    async fn mirror_remove_updates_env() {
        let _g = ENV_MUTEX.lock().unwrap();
        for v in [
            "STORE_ROOT",
            "BIND_HTTP",
            "BIND_WS",
            "VERIFY_SIG",
            "RELAYS_UPSTREAM",
            "TOR_SOCKS",
            "FILTER_AUTHORS",
            "FILTER_KINDS",
            "FILTER_TAG_T",
            "FILTER_SINCE_MODE",
        ] {
            std::env::remove_var(v);
        }
        let dir = TempDir::new().unwrap();
        let env_path = dir.path().join(".env");
        let content = format!(
            "STORE_ROOT={}\nBIND_HTTP=127.0.0.1:0\nBIND_WS=127.0.0.1:0\nVERIFY_SIG=0\nRELAYS_UPSTREAM=ws://one,ws://two\n",
            dir.path().to_str().unwrap(),
        );
        fs::write(&env_path, content).unwrap();
        let env_file = env_path.to_string_lossy().into_owned();

        run(Cli {
            env: env_file.clone(),
            command: Commands::Mirror {
                action: MirrorAction::Remove {
                    url: "ws://one".into(),
                },
            },
        })
        .await
        .unwrap();

        let data = fs::read_to_string(&env_file).unwrap();
        assert!(data.contains("RELAYS_UPSTREAM=ws://two"));
        assert!(!data.contains("ws://one"));
    }
}
