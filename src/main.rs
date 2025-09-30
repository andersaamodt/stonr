//! Command line interface for operating the relay. Supports initialization,
//! ingesting events, serving HTTP/WebSocket endpoints, mirroring from upstream
//! relays, and signature verification.

mod config;
mod event;
mod mirror;
mod mirror_config;
mod server;
mod storage;
mod ws;

use std::net::SocketAddr;

use anyhow::{anyhow, Result};
use clap::{Args, Parser, Subcommand};
use config::Settings;
use storage::Store;

use crate::mirror_config::{FilterConfig, RelayRequest};

/// Command line interface entry point.
#[derive(Parser)]
#[command(
    name = "stonr",
    author,
    version,
    about = "File-backed Nostr relay",
    after_help = MIRROR_SUBCOMMANDS_HELP,
    after_long_help = MIRROR_SUBCOMMANDS_HELP
)]
struct Cli {
    /// Path to the `.env` configuration file.
    #[arg(long, default_value = ".env")]
    env: String,
    /// Subcommand to execute.
    #[command(subcommand)]
    command: Commands,
}

const MIRROR_SUBCOMMANDS_HELP: &str = "\
Mirror subcommands:\n    list                         List configured upstream relays and their requests\n    add-relay <URL>               Add a relay entry without any requests\n    remove-relay <URL>            Remove a relay and all of its requests\n    list-requests <URL>           List requests configured for a relay\n    add-request <URL> <NAME>      Create a new request on a relay\n    update-request <URL> <NAME>   Update an existing request\n    remove-request <URL> <NAME>   Remove a request from a relay\n\nFilter flags (for add-request/update-request):\n    --author <PUBKEY>             Repeatable author filters\n    --kind <KIND>                 Repeatable numeric kind filters\n    --tag <NAME=VALUE>            Repeatable #tag filters\n    --since <TIMESTAMP>           Lower UNIX timestamp bound\n    --until <TIMESTAMP>           Upper UNIX timestamp bound\n    --limit <COUNT>               Hint to limit the subscription size\n    --no-cursor                   Disable resume cursors for the request\n";

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
    /// Manage upstream relay mirroring subscriptions.
    #[command(
        after_help = MIRROR_SUBCOMMANDS_HELP,
        after_long_help = MIRROR_SUBCOMMANDS_HELP
    )]
    Mirror {
        #[command(subcommand)]
        command: MirrorCommands,
    },
}

#[derive(Subcommand)]
enum MirrorCommands {
    /// List configured upstream relays and their requests.
    List,
    /// Add a relay entry without any requests.
    AddRelay { url: String },
    /// Remove a relay and all of its requests.
    RemoveRelay { url: String },
    /// List requests configured for a relay.
    ListRequests { url: String },
    /// Create a new request on a relay.
    AddRequest {
        url: String,
        name: String,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Update an existing request.
    UpdateRequest {
        url: String,
        name: String,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Remove a request from a relay.
    RemoveRequest { url: String, name: String },
}

#[derive(Args, Clone, Default)]
struct FilterArgs {
    #[arg(long = "author")]
    authors: Vec<String>,
    #[arg(long = "kind")]
    kinds: Vec<u32>,
    #[arg(long = "tag")]
    tags: Vec<String>,
    #[arg(long)]
    since: Option<u64>,
    #[arg(long)]
    until: Option<u64>,
    #[arg(long)]
    limit: Option<u32>,
    #[arg(long)]
    no_cursor: bool,
}

fn filter_from_args(args: &FilterArgs) -> Result<FilterConfig> {
    let mut filter = FilterConfig::default();
    if !args.authors.is_empty() {
        filter.authors = Some(args.authors.clone());
    }
    if !args.kinds.is_empty() {
        filter.kinds = Some(args.kinds.clone());
    }
    if !args.tags.is_empty() {
        let mut map = std::collections::BTreeMap::new();
        for entry in &args.tags {
            let (key, value) = entry
                .split_once('=')
                .ok_or_else(|| anyhow!("tag filters must use name=value"))?;
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                return Err(anyhow!("tag filters must use name=value"));
            }
            map.entry(key.to_string())
                .or_insert_with(Vec::new)
                .push(value.to_string());
        }
        filter.tags = map;
    }
    filter.since = args.since;
    filter.until = args.until;
    filter.limit = args.limit;
    filter.cursor = !args.no_cursor;
    Ok(filter)
}

fn handle_mirror_command(cfg: &Settings, command: MirrorCommands) -> Result<()> {
    match command {
        MirrorCommands::List => {
            let relays = mirror_config::list_relays(&cfg.store_root)?;
            if relays.is_empty() {
                println!("(no relays configured)");
            } else {
                for relay in relays {
                    println!("{}", relay.url);
                    for req in relay.requests {
                        let filter_json = mirror_config::filter_to_json(&req.filter, None);
                        println!(
                            "  - {} {}",
                            req.name,
                            serde_json::Value::Object(filter_json)
                        );
                    }
                }
            }
        }
        MirrorCommands::AddRelay { url } => {
            mirror_config::add_relay(&cfg.store_root, &url)?;
            println!("added relay {url}");
        }
        MirrorCommands::RemoveRelay { url } => {
            if mirror_config::remove_relay(&cfg.store_root, &url)? {
                println!("removed relay {url}");
            } else {
                return Err(anyhow!("relay not found"));
            }
        }
        MirrorCommands::ListRequests { url } => {
            let relay = mirror_config::load_relay(&cfg.store_root, &url)?
                .ok_or_else(|| anyhow!("relay not found"))?;
            if relay.requests.is_empty() {
                println!("(no requests)");
            } else {
                for req in relay.requests {
                    let filter_json = mirror_config::filter_to_json(&req.filter, None);
                    println!("{} {}", req.name, serde_json::Value::Object(filter_json));
                }
            }
        }
        MirrorCommands::AddRequest { url, name, filter } => {
            let relay = mirror_config::load_relay(&cfg.store_root, &url)?;
            if let Some(ref relay_cfg) = relay {
                if relay_cfg.requests.iter().any(|r| r.name == name) {
                    return Err(anyhow!("request '{name}' already exists"));
                }
            }
            let filter_cfg = filter_from_args(&filter)?;
            let request = RelayRequest {
                name,
                filter: filter_cfg,
            };
            mirror_config::upsert_request(&cfg.store_root, &url, request)?;
            println!("added request on {url}");
        }
        MirrorCommands::UpdateRequest { url, name, filter } => {
            let relay = mirror_config::load_relay(&cfg.store_root, &url)?
                .ok_or_else(|| anyhow!("relay not found"))?;
            if !relay.requests.iter().any(|r| r.name == name) {
                return Err(anyhow!("request '{name}' not found"));
            }
            let filter_cfg = filter_from_args(&filter)?;
            let request = RelayRequest {
                name,
                filter: filter_cfg,
            };
            mirror_config::upsert_request(&cfg.store_root, &url, request)?;
            println!("updated request on {url}");
        }
        MirrorCommands::RemoveRequest { url, name } => {
            if mirror_config::remove_request(&cfg.store_root, &url, &name)? {
                println!("removed request {name} from {url}");
            } else {
                return Err(anyhow!("request '{name}' not found"));
            }
        }
    }
    Ok(())
}

/// Execute the selected CLI subcommand.
async fn run(cli: Cli) -> Result<()> {
    let cfg = Settings::from_env(&cli.env)?;
    let store = Store::new(cfg.store_root.clone(), cfg.verify_sig);
    match cli.command {
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
            let http_addr: SocketAddr = cfg.bind_http.parse()?;
            let ws_addr: SocketAddr = cfg.bind_ws.parse()?;
            let store_clone = store.clone();
            let cfg_clone = cfg.clone();
            tokio::spawn(async move { mirror::run(cfg_clone, store_clone).await });
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
        Commands::Mirror { command } => {
            handle_mirror_command(&cfg, command)?;
        }
    }
    Ok(())
}

#[cfg_attr(test, allow(dead_code))]
fn rewrite_help_args<I>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();
    if let Some(idx) = args.iter().position(|arg| arg == "--help" || arg == "-h") {
        if idx + 1 < args.len() && !args[idx + 1].starts_with('-') {
            let mut rewritten = Vec::with_capacity(args.len());
            rewritten.extend(args[..idx].iter().cloned());
            rewritten.push("help".to_string());
            rewritten.extend(args[idx + 1..].iter().cloned());
            return rewritten;
        }
    }
    args
}

#[cfg(not(test))]
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse_from(rewrite_help_args(std::env::args()));
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
    async fn run_serve_spawns_mirror() {
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

    #[tokio::test]
    async fn mirror_cli_updates_config() {
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
        let url = "wss://relay.example".to_string();

        run(Cli {
            env: env_file.clone(),
            command: Commands::Mirror {
                command: MirrorCommands::AddRelay { url: url.clone() },
            },
        })
        .await
        .unwrap();

        run(Cli {
            env: env_file.clone(),
            command: Commands::Mirror {
                command: MirrorCommands::AddRequest {
                    url: url.clone(),
                    name: "default".into(),
                    filter: FilterArgs {
                        authors: vec!["npub1".into()],
                        kinds: vec![1],
                        tags: vec!["t=topic".into()],
                        since: Some(1),
                        until: None,
                        limit: Some(10),
                        no_cursor: true,
                    },
                },
            },
        })
        .await
        .unwrap();

        let relay = mirror_config::load_relay(dir.path(), &url)
            .unwrap()
            .unwrap();
        assert_eq!(relay.requests.len(), 1);
        let req = &relay.requests[0];
        assert_eq!(req.name, "default");
        assert_eq!(
            req.filter.authors.as_ref().unwrap(),
            &vec![String::from("npub1")]
        );
        assert_eq!(req.filter.kinds.as_ref().unwrap(), &vec![1]);
        assert_eq!(
            req.filter.tags.get("t").unwrap(),
            &vec![String::from("topic")]
        );
        assert_eq!(req.filter.since, Some(1));
        assert_eq!(req.filter.limit, Some(10));
        assert!(!req.filter.cursor);

        run(Cli {
            env: env_file.clone(),
            command: Commands::Mirror {
                command: MirrorCommands::RemoveRequest {
                    url: url.clone(),
                    name: "default".into(),
                },
            },
        })
        .await
        .unwrap();
        let relay = mirror_config::load_relay(dir.path(), &url)
            .unwrap()
            .unwrap();
        assert!(relay.requests.is_empty());

        run(Cli {
            env: env_file,
            command: Commands::Mirror {
                command: MirrorCommands::RemoveRelay { url: url.clone() },
            },
        })
        .await
        .unwrap();
        assert!(mirror_config::load_relay(dir.path(), &url)
            .unwrap()
            .is_none());
    }
}
