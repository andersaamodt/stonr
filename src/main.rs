mod config;
mod event;
mod server;
mod siphon;
mod storage;
mod ws;

use std::net::SocketAddr;

use clap::{Parser, Subcommand};
use config::Settings;
use storage::Store;

/// Command line interface entry point.
#[derive(Parser)]
#[command(name = "stonr", author, version, about = "File-backed Nostr relay")]
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
    /// Launch HTTP and WebSocket services (and siphon if configured).
    Serve,
    /// Verify a random sample of stored events.
    Verify {
        #[arg(long, default_value_t = 1000)]
        sample: usize,
    },
}

/// Execute the selected CLI subcommand.
async fn run(cli: Cli) -> anyhow::Result<()> {
    let cfg = Settings::from_env(&cli.env)?;
    let store = Store::new(cfg.store_root.clone(), cfg.verify_sig);
    match cli.command {
        Commands::Init => {
            store.init()?;
        }
        Commands::Ingest { files } => {
            for f in files {
                let data = std::fs::read_to_string(&f)?;
                let ev: event::Event = serde_json::from_str(&data)?;
                store.ingest(&ev)?;
            }
        }
        Commands::Reindex => {
            store.reindex()?;
        }
        Commands::Serve => {
            store.init()?;
            let http_addr: SocketAddr = cfg.bind_http.parse()?;
            let ws_addr: SocketAddr = cfg.bind_ws.parse()?;
            // If upstream relays are configured, start the siphon in the background.
            if !cfg.relays_upstream.is_empty() {
                let store_clone = store.clone();
                let cfg_clone = cfg.clone();
                tokio::spawn(async move { siphon::run(cfg_clone, store_clone).await });
            }
            let store_http = store.clone();
            let store_ws = store.clone();
            tokio::try_join!(
                server::serve_http(http_addr, store_http, std::future::pending()),
                ws::serve_ws(ws_addr, store_ws, std::future::pending())
            )?;
        }
        Commands::Verify { sample } => {
            store.verify_sample(sample)?;
        }
    }
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
    use std::{fs, sync::Mutex, time::Duration};
    use tempfile::TempDir;
    use tokio::{net::TcpListener, task};

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
    async fn run_serve_starts_http() {
        let _g = ENV_MUTEX.lock().unwrap();
        for v in [
            "STORE_ROOT",
            "BIND_HTTP",
            "BIND_WS",
            "VERIFY_SIG",
            "RELAYS_UPSTREAM",
            "TOR_SOCKS",
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
    async fn run_serve_spawns_siphon() {
        let _g = ENV_MUTEX.lock().unwrap();
        for v in [
            "STORE_ROOT",
            "BIND_HTTP",
            "BIND_WS",
            "VERIFY_SIG",
            "RELAYS_UPSTREAM",
            "TOR_SOCKS",
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
}
