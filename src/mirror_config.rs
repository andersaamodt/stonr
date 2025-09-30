//! Persistent configuration for upstream relay mirroring.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

/// Configuration persisted for a single upstream relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayConfig {
    /// Relay WebSocket URL.
    pub url: String,
    /// Named subscriptions issued on the relay connection.
    #[serde(default)]
    pub requests: Vec<RelayRequest>,
}

/// Named `REQ` subscription issued on an upstream relay connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayRequest {
    /// Human readable identifier used for CLI management and subscription IDs.
    pub name: String,
    /// Filter applied when issuing the subscription.
    pub filter: FilterConfig,
}

/// Filter parameters used when building a Nostr subscription filter.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FilterConfig {
    /// Restrict to specific authors.
    pub authors: Option<Vec<String>>,
    /// Restrict to event kinds.
    pub kinds: Option<Vec<u32>>,
    /// Arbitrary tag filters keyed by their `#` prefix (e.g. `#t`).
    #[serde(default)]
    pub tags: BTreeMap<String, Vec<String>>,
    /// Lower bound for `created_at`.
    pub since: Option<u64>,
    /// Upper bound for `created_at`.
    pub until: Option<u64>,
    /// Maximum number of events requested from the upstream.
    pub limit: Option<u32>,
    /// Persist and reuse the latest seen timestamp between runs.
    #[serde(default = "default_use_cursor")]
    pub cursor: bool,
}

fn default_use_cursor() -> bool {
    true
}

/// Compute the directory containing persisted relay configuration.
fn relays_root(root: &Path) -> PathBuf {
    root.join("mirror").join("relays")
}

/// Determine a filesystem-safe directory for a relay URL.
fn relay_dir(root: &Path, url: &str) -> PathBuf {
    let mut hasher = Sha1::new();
    hasher.update(url.as_bytes());
    let hash = hex::encode(hasher.finalize());
    relays_root(root).join(hash)
}

fn relay_file(root: &Path, url: &str) -> PathBuf {
    relay_dir(root, url).join("relay.json")
}

/// Ensure a request name is safe for filesystem operations.
pub fn validate_request_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(anyhow!("request name cannot be empty"));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(anyhow!("request name cannot contain path separators"));
    }
    if name == "." || name == ".." {
        return Err(anyhow!("request name cannot be '.' or '..'"));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(anyhow!("request name cannot contain control characters"));
    }
    Ok(())
}

/// List all configured relays.
pub fn list_relays(root: &Path) -> Result<Vec<RelayConfig>> {
    let mut relays = vec![];
    let dir = relays_root(root);
    if !dir.exists() {
        return Ok(relays);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join("relay.json");
        if !path.exists() {
            continue;
        }
        let data = fs::read_to_string(path)?;
        let cfg: RelayConfig = serde_json::from_str(&data)?;
        relays.push(cfg);
    }
    Ok(relays)
}

/// Load a specific relay by URL.
pub fn load_relay(root: &Path, url: &str) -> Result<Option<RelayConfig>> {
    let path = relay_file(root, url);
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&data)?))
}

/// Persist a relay configuration to disk.
fn save_relay(root: &Path, relay: &RelayConfig) -> Result<()> {
    fs::create_dir_all(relay_dir(root, &relay.url))?;
    let path = relay_file(root, &relay.url);
    let data = serde_json::to_string_pretty(relay)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("missing parent directory for relay config"))?;
    fs::create_dir_all(parent)?;
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    fs::write(tmp.path(), data)?;
    tmp.persist(&path)?;
    Ok(())
}

/// Add a relay or ensure it exists.
pub fn add_relay(root: &Path, url: &str) -> Result<()> {
    if load_relay(root, url)?.is_some() {
        return Ok(());
    }
    let cfg = RelayConfig {
        url: url.to_string(),
        requests: vec![],
    };
    save_relay(root, &cfg)
}

/// Remove a relay configuration entirely.
pub fn remove_relay(root: &Path, url: &str) -> Result<bool> {
    let dir = relay_dir(root, url);
    if !dir.exists() {
        return Ok(false);
    }
    fs::remove_dir_all(dir)?;
    Ok(true)
}

/// Upsert a request for the given relay.
pub fn upsert_request(root: &Path, url: &str, request: RelayRequest) -> Result<()> {
    validate_request_name(&request.name)?;
    let mut relay = load_relay(root, url)?.unwrap_or(RelayConfig {
        url: url.to_string(),
        requests: vec![],
    });
    if let Some(existing) = relay.requests.iter_mut().find(|r| r.name == request.name) {
        *existing = request;
    } else {
        relay.requests.push(request);
    }
    save_relay(root, &relay)
}

/// Remove a request from a relay.
pub fn remove_request(root: &Path, url: &str, name: &str) -> Result<bool> {
    validate_request_name(name)?;
    let Some(mut relay) = load_relay(root, url)? else {
        return Ok(false);
    };
    let before = relay.requests.len();
    relay.requests.retain(|r| r.name != name);
    if relay.requests.len() == before {
        return Ok(false);
    }
    save_relay(root, &relay)?;
    Ok(true)
}

/// Convert the stored filter into a JSON object used for REQ messages.
pub fn filter_to_json(
    filter: &FilterConfig,
    cursor_since: Option<u64>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    if let Some(authors) = &filter.authors {
        if !authors.is_empty() {
            map.insert(
                "authors".into(),
                serde_json::Value::Array(
                    authors
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
        }
    }
    if let Some(kinds) = &filter.kinds {
        if !kinds.is_empty() {
            map.insert(
                "kinds".into(),
                serde_json::Value::Array(
                    kinds
                        .iter()
                        .cloned()
                        .map(|k| serde_json::Value::Number(k.into()))
                        .collect(),
                ),
            );
        }
    }
    for (tag, values) in &filter.tags {
        if values.is_empty() {
            continue;
        }
        let key = if tag.starts_with('#') {
            tag.clone()
        } else {
            format!("#{tag}")
        };
        map.insert(
            key,
            serde_json::Value::Array(
                values
                    .iter()
                    .cloned()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    let final_since = match (cursor_since, filter.since) {
        (Some(cur), Some(base)) => Some(cur.max(base)),
        (Some(cur), None) => Some(cur),
        (None, Some(base)) => Some(base),
        (None, None) => None,
    };
    if let Some(since) = final_since {
        map.insert("since".into(), serde_json::Value::Number(since.into()));
    }
    if let Some(until) = filter.until {
        map.insert("until".into(), serde_json::Value::Number(until.into()));
    }
    if let Some(limit) = filter.limit {
        map.insert("limit".into(), serde_json::Value::Number(limit.into()));
    }
    map
}

/// Sanitize a request name for inclusion in subscription IDs.
pub fn subscription_id(relay: &RelayConfig, request: &RelayRequest) -> String {
    let mut id = String::from("mirror-");
    let mut hasher = Sha1::new();
    hasher.update(relay.url.as_bytes());
    let relay_hash = hex::encode(hasher.finalize());
    id.push_str(&relay_hash[..8]);
    id.push('-');
    for c in request.name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            id.push(c);
        } else {
            id.push('_');
        }
    }
    if id.len() > 48 {
        id.truncate(48);
    }
    id
}

/// Persist the latest seen timestamp for a relay/request pair.
pub fn write_cursor(
    root: &Path,
    relay: &RelayConfig,
    request: &RelayRequest,
    ts: u64,
) -> Result<()> {
    if !request.filter.cursor {
        return Ok(());
    }
    let dir = root.join("cursor");
    fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.{}.since", cursor_key(&relay.url), request.name));
    let tmp = tempfile::NamedTempFile::new_in(&dir)?;
    fs::write(tmp.path(), ts.to_string())?;
    tmp.persist(file)?;
    Ok(())
}

/// Read the last seen timestamp for a relay/request pair if available.
pub fn read_cursor(root: &Path, relay: &RelayConfig, request: &RelayRequest) -> Option<u64> {
    if !request.filter.cursor {
        return None;
    }
    let dir = root.join("cursor");
    let path = dir.join(format!("{}.{}.since", cursor_key(&relay.url), request.name));
    fs::read_to_string(path).ok()?.parse().ok()
}

fn cursor_key(url: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(url.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn relay_roundtrip() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        add_relay(root, "wss://example.com").unwrap();
        let mut filter = FilterConfig::default();
        filter.authors = Some(vec!["npub1".into()]);
        let req = RelayRequest {
            name: "default".into(),
            filter,
        };
        upsert_request(root, "wss://example.com", req.clone()).unwrap();
        let relays = list_relays(root).unwrap();
        assert_eq!(relays.len(), 1);
        assert_eq!(relays[0].requests.len(), 1);
        assert_eq!(relays[0].requests[0].name, "default");
        remove_request(root, "wss://example.com", "default").unwrap();
        let relay = load_relay(root, "wss://example.com").unwrap().unwrap();
        assert!(relay.requests.is_empty());
        assert!(remove_relay(root, "wss://example.com").unwrap());
    }
}
