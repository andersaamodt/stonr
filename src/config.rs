//! Configuration loading from `.env` files.

use std::{env, path::PathBuf};

use anyhow::{Context, Result};

/// Runtime settings derived from environment variables.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Root directory for all storage.
    pub store_root: PathBuf,
    /// HTTP bind address, e.g. `127.0.0.1:7777`.
    pub bind_http: String,
    /// WebSocket bind address, e.g. `127.0.0.1:7778`.
    pub bind_ws: String,
    /// Enable Schnorr signature verification on ingest.
    pub verify_sig: bool,
    /// Upstream relays to siphon from.
    pub relays_upstream: Vec<String>,
    /// Optional Tor SOCKS proxy (host:port).
    pub tor_socks: Option<String>,
    /// Optional author filters for siphoning.
    pub filter_authors: Option<Vec<String>>,
    /// Optional kind filters for siphoning.
    pub filter_kinds: Option<Vec<u32>>,
    /// Optional `#t` tag filters for siphoning.
    pub filter_tag_t: Option<Vec<String>>,
    /// Strategy for determining the starting timestamp when siphoning.
    pub filter_since_mode: SinceMode,
}

/// Determines how the siphon derives the `since` value for subscriptions.
#[derive(Debug, Clone, PartialEq)]
pub enum SinceMode {
    /// Resume from the last cursor stored per relay.
    Cursor,
    /// Start from a fixed Unix timestamp.
    Fixed(u64),
}

impl Settings {
    /// Load settings from the specified `.env` file.
    pub fn from_env(path: &str) -> Result<Self> {
        dotenvy::from_filename(path).context("reading env file")?;
        let store_root = PathBuf::from(env::var("STORE_ROOT")?);
        let bind_http = env::var("BIND_HTTP")?;
        let bind_ws = env::var("BIND_WS")?;
        let verify_sig = env::var("VERIFY_SIG").unwrap_or_else(|_| "0".into()) == "1";
        let relays_upstream = csv_strings(env::var("RELAYS_UPSTREAM").unwrap_or_default());
        let tor_socks = env::var("TOR_SOCKS").ok().filter(|s| !s.is_empty());
        let filter_authors = env::var("FILTER_AUTHORS").ok().and_then(|s| {
            let v = csv_strings(s);
            if v.is_empty() {
                None
            } else {
                Some(v)
            }
        });
        let filter_kinds = env::var("FILTER_KINDS").ok().and_then(|s| {
            let v = csv_u32(s);
            if v.is_empty() {
                None
            } else {
                Some(v)
            }
        });
        let filter_tag_t = env::var("FILTER_TAG_T").ok().and_then(|s| {
            let v = csv_strings(s);
            if v.is_empty() {
                None
            } else {
                Some(v)
            }
        });
        let since_str = env::var("FILTER_SINCE_MODE").unwrap_or_else(|_| "cursor".into());
        let filter_since_mode = if let Some(rest) = since_str.strip_prefix("fixed:") {
            SinceMode::Fixed(rest.parse().unwrap_or(0))
        } else {
            SinceMode::Cursor
        };
        Ok(Self {
            store_root,
            bind_http,
            bind_ws,
            verify_sig,
            relays_upstream,
            tor_socks,
            filter_authors,
            filter_kinds,
            filter_tag_t,
            filter_since_mode,
        })
    }
}

/// Split a comma-separated string into trimmed string values.
pub fn csv_strings(input: impl AsRef<str>) -> Vec<String> {
    let s = input.as_ref();
    s.split(',')
        .filter_map(|s| {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        })
        .collect()
}

/// Split a comma-separated string into `u32` values, skipping invalid entries.
pub fn csv_u32(input: impl AsRef<str>) -> Vec<u32> {
    let s = input.as_ref();
    s.split(',').filter_map(|s| s.trim().parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs, sync::Mutex};
    use tempfile::tempdir;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn loads_env() {
        let _g = ENV_MUTEX.lock().unwrap();
        let vars = [
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
        ];
        for v in vars.iter() {
            env::remove_var(v);
        }
        let dir = tempdir().unwrap();
        let env_path = dir.path().join(".env");
        fs::write(
            &env_path,
            concat!(
                "STORE_ROOT=/tmp\n",
                "BIND_HTTP=127.0.0.1:8080\n",
                "BIND_WS=127.0.0.1:8081\n",
                "VERIFY_SIG=1\n",
                "RELAYS_UPSTREAM=ws://r1,ws://r2\n",
                "TOR_SOCKS=\n",
                "FILTER_AUTHORS=npub1\n",
                "FILTER_KINDS=1,30023\n",
                "FILTER_TAG_T=essay\n",
                "FILTER_SINCE_MODE=fixed:1700000000\n"
            ),
        )
        .unwrap();
        let cfg = Settings::from_env(env_path.to_str().unwrap()).unwrap();
        assert_eq!(cfg.bind_http, "127.0.0.1:8080");
        assert_eq!(cfg.bind_ws, "127.0.0.1:8081");
        assert_eq!(cfg.store_root, PathBuf::from("/tmp"));
        assert!(cfg.verify_sig);
        assert_eq!(cfg.relays_upstream.len(), 2);
        assert_eq!(
            cfg.filter_authors.as_ref().unwrap(),
            &vec![String::from("npub1")]
        );
        assert_eq!(cfg.filter_kinds.as_ref().unwrap(), &vec![1, 30023]);
        assert_eq!(
            cfg.filter_tag_t.as_ref().unwrap(),
            &vec![String::from("essay")]
        );
        assert_eq!(cfg.filter_since_mode, SinceMode::Fixed(1700000000));
    }

    #[test]
    fn csv_helpers() {
        assert_eq!(csv_strings("a, b , ,c"), vec!["a", "b", "c"]);
        assert!(csv_strings("").is_empty());
        assert_eq!(csv_u32("1, 2, x,3"), vec![1, 2, 3]);
        assert!(csv_u32("").is_empty());
    }

    #[test]
    fn tor_socks_parsed() {
        let _g = ENV_MUTEX.lock().unwrap();
        let vars = ["STORE_ROOT", "BIND_HTTP", "BIND_WS", "TOR_SOCKS"];
        for v in vars.iter() {
            env::remove_var(v);
        }
        let dir = tempdir().unwrap();
        let env_path = dir.path().join(".env");
        fs::write(
            &env_path,
            concat!(
                "STORE_ROOT=/tmp\n",
                "BIND_HTTP=127.0.0.1:8080\n",
                "BIND_WS=127.0.0.1:8081\n",
                "TOR_SOCKS=127.0.0.1:9050\n",
            ),
        )
        .unwrap();
        let cfg = Settings::from_env(env_path.to_str().unwrap()).unwrap();
        assert_eq!(cfg.tor_socks, Some("127.0.0.1:9050".into()));
    }

    #[test]
    fn defaults_when_optional_absent() {
        let _g = ENV_MUTEX.lock().unwrap();
        let vars = [
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
        ];
        for v in vars.iter() {
            env::remove_var(v);
        }
        let dir = tempdir().unwrap();
        let env_path = dir.path().join(".env");
        fs::write(
            &env_path,
            concat!(
                "STORE_ROOT=/tmp\n",
                "BIND_HTTP=127.0.0.1:8080\n",
                "BIND_WS=127.0.0.1:8081\n"
            ),
        )
        .unwrap();
        let cfg = Settings::from_env(env_path.to_str().unwrap()).unwrap();
        assert!(cfg.relays_upstream.is_empty());
        assert!(cfg.tor_socks.is_none());
        assert!(cfg.filter_authors.is_none());
        assert!(cfg.filter_kinds.is_none());
        assert!(cfg.filter_tag_t.is_none());
        assert_eq!(cfg.filter_since_mode, SinceMode::Cursor);
    }

    #[test]
    fn empty_filters_are_none() {
        let _g = ENV_MUTEX.lock().unwrap();
        let vars = [
            "STORE_ROOT",
            "BIND_HTTP",
            "BIND_WS",
            "FILTER_AUTHORS",
            "FILTER_KINDS",
            "FILTER_TAG_T",
        ];
        for v in vars.iter() {
            env::remove_var(v);
        }
        let dir = tempdir().unwrap();
        let env_path = dir.path().join(".env");
        fs::write(
            &env_path,
            concat!(
                "STORE_ROOT=/tmp\n",
                "BIND_HTTP=127.0.0.1:8080\n",
                "BIND_WS=127.0.0.1:8081\n",
                "FILTER_AUTHORS=\n",
                "FILTER_KINDS=\n",
                "FILTER_TAG_T=\n",
            ),
        )
        .unwrap();
        let cfg = Settings::from_env(env_path.to_str().unwrap()).unwrap();
        assert!(cfg.filter_authors.is_none());
        assert!(cfg.filter_kinds.is_none());
        assert!(cfg.filter_tag_t.is_none());
    }

    #[test]
    fn missing_required_fields_error() {
        let _g = ENV_MUTEX.lock().unwrap();
        let vars = ["STORE_ROOT", "BIND_HTTP", "BIND_WS"];
        for v in vars.iter() {
            env::remove_var(v);
        }
        let dir = tempdir().unwrap();
        let env_path = dir.path().join(".env");
        fs::write(
            &env_path,
            concat!("BIND_HTTP=127.0.0.1:8080\n", "BIND_WS=127.0.0.1:8081\n"),
        )
        .unwrap();
        assert!(Settings::from_env(env_path.to_str().unwrap()).is_err());
    }

    #[test]
    fn invalid_fixed_since_mode_defaults_to_zero() {
        let _g = ENV_MUTEX.lock().unwrap();
        let vars = ["STORE_ROOT", "BIND_HTTP", "BIND_WS", "FILTER_SINCE_MODE"];
        for v in vars.iter() {
            env::remove_var(v);
        }
        let dir = tempdir().unwrap();
        let env_path = dir.path().join(".env");
        fs::write(
            &env_path,
            concat!(
                "STORE_ROOT=/tmp\n",
                "BIND_HTTP=127.0.0.1:8080\n",
                "BIND_WS=127.0.0.1:8081\n",
                "FILTER_SINCE_MODE=fixed:notanumber\n",
            ),
        )
        .unwrap();
        let cfg = Settings::from_env(env_path.to_str().unwrap()).unwrap();
        assert_eq!(cfg.filter_since_mode, SinceMode::Fixed(0));
    }
}
