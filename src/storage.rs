//! Minimal file-backed storage and query engine.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use hex;
use rand::{seq::SliceRandom, thread_rng};
use secp256k1::{schnorr::Signature, Message, Secp256k1, XOnlyPublicKey};
use serde_json::{to_writer, Value};
use sha2::{Digest, Sha256};

use crate::event::{Event, Tag};
use std::os::unix::fs as unix_fs;

/// Persistent store for events and indexes rooted at `root`.
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
    verify_sig: bool,
}

impl Store {
    /// Create a new store rooted at `root`.
    pub fn new(root: PathBuf, verify_sig: bool) -> Self {
        Self { root, verify_sig }
    }

    /// Ensure the on-disk directory structure exists.
    pub fn init(&self) -> Result<()> {
        let dirs = [
            "events",
            "log",
            "latest",
            "index/by-author",
            "index/by-kind",
            "index/by-tag/d",
            "index/by-tag/t",
            "mirror/authors",
            "mirror/kinds",
            "cursor",
        ];
        for d in dirs {
            fs::create_dir_all(self.root.join(d))?;
        }
        Ok(())
    }

    /// Ingest an event if it doesn't already exist on disk.
    pub fn ingest(&self, ev: &Event) -> Result<()> {
        // Optionally verify the event's Schnorr signature before writing.
        if self.verify_sig {
            verify_event(ev)?;
        }
        // Skip ingest if the event already exists on disk.
        let path = self.event_path(&ev.id);
        if path.exists() {
            return Ok(());
        }
        // Write the event JSON atomically to its canonical path.
        let parent_dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        fs::create_dir_all(&parent_dir)?;
        let tmp = tempfile::NamedTempFile::new_in(&parent_dir)?;
        to_writer(&tmp, ev)?;
        tmp.persist(&path)?;

        // Append the event to a newline-delimited log for easy tailing.
        let log_path = self.root.join("log/events.ndjson");
        let mut log_file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        serde_json::to_writer(&mut log_file, ev)?;
        log_file.write_all(b"\n")?;

        // Update lookup indexes and create mirror symlinks.
        self.index_event(ev)?;
        self.write_mirror_links(ev)
    }

    /// Verify Schnorr signatures for a random sample of stored events.
    pub fn verify_sample(&self, sample: usize) -> Result<usize> {
        let mut paths = vec![];
        for entry in walkdir::WalkDir::new(self.root.join("events")) {
            let entry = entry?;
            if entry.file_type().is_file() {
                paths.push(entry.into_path());
            }
        }
        let mut rng = thread_rng();
        paths.shuffle(&mut rng);
        let take = sample.min(paths.len());
        for p in paths.iter().take(take) {
            let data = fs::read_to_string(p)?;
            let ev: Event = serde_json::from_str(&data)?;
            verify_event(&ev)?;
        }
        Ok(take)
    }

    /// Rebuild all indexes and latest pointers from the `events/` tree.
    pub fn reindex(&self) -> Result<()> {
        let index_dir = self.root.join("index");
        if index_dir.exists() {
            fs::remove_dir_all(&index_dir)?;
        }
        let latest_dir = self.root.join("latest");
        if latest_dir.exists() {
            fs::remove_dir_all(&latest_dir)?;
        }
        // recreate directory structure for indexes and latest
        fs::create_dir_all(self.root.join("index/by-author"))?;
        fs::create_dir_all(self.root.join("index/by-kind"))?;
        fs::create_dir_all(self.root.join("index/by-tag/d"))?;
        fs::create_dir_all(self.root.join("index/by-tag/t"))?;
        fs::create_dir_all(self.root.join("latest"))?;

        for entry in walkdir::WalkDir::new(self.root.join("events")) {
            let entry = entry?;
            if entry.file_type().is_file() {
                let data = fs::read_to_string(entry.path())?;
                let ev: Event = serde_json::from_str(&data)?;
                self.index_event(&ev)?;
            }
        }
        Ok(())
    }

    /// Update indexes and latest pointers for an event.
    fn index_event(&self, ev: &Event) -> Result<()> {
        self.append_index("index/by-author", &ev.pubkey, &ev.id)?;
        self.append_index("index/by-kind", &ev.kind.to_string(), &ev.id)?;
        for Tag(fields) in &ev.tags {
            if fields.len() >= 2 {
                match fields[0].as_str() {
                    "d" => {
                        self.append_index("index/by-tag/d", &fields[1], &ev.id)?;
                        let latest = self
                            .root
                            .join("latest")
                            .join(format!("{}.{}.{}", ev.pubkey, ev.kind, fields[1]));
                        if let Some(parent) = latest.parent() {
                            fs::create_dir_all(parent)?;
                        }
                        fs::write(latest, &ev.id)?;
                    }
                    "t" => {
                        self.append_index("index/by-tag/t", &fields[1], &ev.id)?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Create symlinks for author and kind mirrors.
    fn write_mirror_links(&self, ev: &Event) -> Result<()> {
        let rel_target = format!(
            "../../../events/{}/{}/{}.json",
            &ev.id[0..2],
            &ev.id[2..4],
            ev.id
        );
        // by author
        let author_dir = self.root.join("mirror/authors").join(&ev.pubkey);
        fs::create_dir_all(&author_dir)?;
        let author_link = author_dir.join(format!("{}-{}.json", ev.created_at, ev.id));
        if !author_link.exists() {
            unix_fs::symlink(&rel_target, &author_link)?;
        }

        // by kind
        let kind_dir = self.root.join("mirror/kinds").join(ev.kind.to_string());
        fs::create_dir_all(&kind_dir)?;
        let kind_link = kind_dir.join(format!("{}-{}.json", ev.created_at, ev.id));
        if !kind_link.exists() {
            unix_fs::symlink(rel_target, kind_link)?;
        }
        Ok(())
    }

    /// Append an event ID to the index file under `prefix/name.txt`.
    fn append_index(&self, prefix: &str, name: &str, id: &str) -> Result<()> {
        let path = self.root.join(prefix).join(format!("{}.txt", name));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(f, "{}", id)?;
        Ok(())
    }

    /// Compute the canonical path for an event ID.
    fn event_path(&self, id: &str) -> PathBuf {
        let sub1 = &id[0..2];
        let sub2 = &id[2..4];
        self.root
            .join("events")
            .join(sub1)
            .join(sub2)
            .join(format!("{}.json", id))
    }

    /// Helper to load ID sets for a list of keys under `prefix`.
    fn load_ids(&self, prefix: &str, keys: &[String]) -> Result<std::collections::HashSet<String>> {
        let mut ids = std::collections::HashSet::new();
        for key in keys {
            let path = self.root.join(prefix).join(format!("{}.txt", key));
            ids.extend(read_ids(&path)?);
        }
        Ok(ids)
    }

    /// Execute a simple intersection-based query over indexes.
    pub fn query(&self, q: Query) -> Result<Vec<Event>> {
        // Collect ID sets for each filter category and intersect them below.
        let mut sets: Vec<std::collections::HashSet<String>> = vec![];
        if let Some(authors) = q.authors {
            sets.push(self.load_ids("index/by-author", &authors)?);
        }
        if let Some(kinds) = q.kinds {
            let keys: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
            sets.push(self.load_ids("index/by-kind", &keys)?);
        }
        if let Some(d) = q.d {
            let path = self.root.join("index/by-tag/d").join(format!("{}.txt", d));
            sets.push(read_ids(&path)?);
        }
        if let Some(t) = q.t {
            let path = self.root.join("index/by-tag/t").join(format!("{}.txt", t));
            sets.push(read_ids(&path)?);
        }
        if sets.is_empty() {
            return Ok(vec![]);
        }
        let mut iter = sets.into_iter();
        // Start with the first ID set and intersect each subsequent one.
        let mut ids = iter.next().unwrap();
        for s in iter {
            ids = ids.intersection(&s).cloned().collect();
        }

        // Load matching events and apply time-based filters.
        let mut events: Vec<Event> = ids
            .into_iter()
            .filter_map(|id| {
                let path = self.event_path(&id);
                let data = fs::read_to_string(path).ok()?;
                serde_json::from_str(&data).ok()
            })
            .filter(|ev: &Event| {
                (q.since.map_or(true, |s| ev.created_at >= s))
                    && (q.until.map_or(true, |u| ev.created_at <= u))
            })
            .collect();
        // Sort newest-first so replaceable events keep the most recent version.
        events.sort_by_key(|e| std::cmp::Reverse(e.created_at));
        // Drop older replaceable events sharing the same author, kind, and `#d` tag.
        let mut seen = std::collections::HashSet::new();
        events.retain(|ev| {
            let d_tag = ev
                .tags
                .iter()
                .find_map(|Tag(fields)| match fields.as_slice() {
                    [t, val, ..] if t == "d" => Some(val.clone()),
                    _ => None,
                });
            if let Some(d) = d_tag {
                let key = format!("{}:{}:{}", ev.pubkey, ev.kind, d);
                seen.insert(key)
            } else {
                true
            }
        });
        if let Some(limit) = q.limit {
            events.truncate(limit);
        }
        Ok(events)
    }
}

/// Read newline-separated IDs from a text file.
fn read_ids(path: &Path) -> Result<std::collections::HashSet<String>> {
    if !path.exists() {
        return Ok(Default::default());
    }
    let data = fs::read_to_string(path)?;
    Ok(data.lines().map(|s| s.to_string()).collect())
}

/// Query parameters accepted by both HTTP and WebSocket interfaces.
pub struct Query {
    pub authors: Option<Vec<String>>,
    pub kinds: Option<Vec<u32>>,
    pub d: Option<String>,
    pub t: Option<String>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub limit: Option<usize>,
}

impl Query {
    /// Build a `Query` from a Nostr filter JSON object used by HTTP and WS APIs.
    pub fn from_value(val: &Value) -> Self {
        // Parse optional arrays of authors and kinds.
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
        // Tag-based queries use a one-element array for `#d`/`#t`.
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
}

/// Recompute the Nostr event hash from its fields.
pub(crate) fn event_hash(ev: &Event) -> Result<[u8; 32]> {
    let arr = serde_json::json!([0, ev.pubkey, ev.created_at, ev.kind, ev.tags, ev.content]);
    let data = serde_json::to_vec(&arr)?;
    let hash = Sha256::digest(&data);
    Ok(hash.into())
}

/// Verify an event's ID and Schnorr signature.
fn verify_event(ev: &Event) -> Result<()> {
    let hash = event_hash(ev)?;
    let calc_id = hex::encode(hash);
    if calc_id != ev.id {
        return Err(anyhow!("id mismatch"));
    }
    let sig = Signature::from_slice(&hex::decode(&ev.sig)?)?;
    let pk = XOnlyPublicKey::from_slice(&hex::decode(&ev.pubkey)?)?;
    let secp = Secp256k1::verification_only();
    let msg = Message::from_digest_slice(&hash)?;
    secp.verify_schnorr(&sig, &msg, &pk)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::{Keypair, Message, Secp256k1};
    use std::fs;
    use tempfile::TempDir;

    fn sample_event(id: &str, pubkey: &str, kind: u32, dtag: Option<&str>, created: u64) -> Event {
        let mut tags = vec![];
        if let Some(d) = dtag {
            tags.push(Tag(vec!["d".into(), d.into()]));
        }
        Event {
            id: id.into(),
            pubkey: pubkey.into(),
            kind,
            created_at: created,
            tags,
            content: String::new(),
            sig: String::new(),
        }
    }

    fn signed_event(kind: u32) -> Event {
        let secp = Secp256k1::new();
        let sk = [1u8; 32];
        let kp = Keypair::from_seckey_slice(&secp, &sk).unwrap();
        let pubkey = kp.x_only_public_key().0;
        let mut ev = Event {
            id: String::new(),
            pubkey: hex::encode(pubkey.serialize()),
            kind,
            created_at: 1,
            tags: vec![],
            content: String::new(),
            sig: String::new(),
        };
        let hash = event_hash(&ev).unwrap();
        ev.id = hex::encode(hash);
        let msg = Message::from_digest_slice(&hash).unwrap();
        let sig = secp.sign_schnorr_no_aux_rand(&msg, &kp);
        ev.sig = hex::encode(sig.as_ref());
        ev
    }

    #[test]
    fn init_and_ingest() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let ev = sample_event("abcd", "pub", 1, Some("slug"), 1);
        store.ingest(&ev).unwrap();
        // ingest again should be idempotent
        store.ingest(&ev).unwrap();
        let id_path = store.root.join("index/by-author/pub.txt");
        let ids = fs::read_to_string(id_path).unwrap();
        assert_eq!(ids.lines().count(), 1);
        assert!(store.root.join("cursor").exists());
    }

    #[test]
    fn creates_mirror_symlinks() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let ev = sample_event("abcd", "pub", 30023, Some("slug"), 42);
        store.ingest(&ev).unwrap();
        let author_link = dir.path().join(format!("mirror/authors/pub/42-abcd.json"));
        assert!(author_link.exists());
        let target = fs::read_link(&author_link).unwrap();
        assert!(target.to_str().unwrap().ends_with("events/ab/cd/abcd.json"));
        let kind_link = dir.path().join(format!("mirror/kinds/30023/42-abcd.json"));
        assert!(kind_link.exists());
        let target2 = fs::read_link(kind_link).unwrap();
        assert!(target2
            .to_str()
            .unwrap()
            .ends_with("events/ab/cd/abcd.json"));
    }

    #[test]
    fn query_intersection() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let e1 = sample_event("aa11", "p1", 1, Some("s1"), 10);
        let e2 = sample_event("bb22", "p1", 30023, Some("s2"), 20);
        store.ingest(&e1).unwrap();
        store.ingest(&e2).unwrap();
        let res = store
            .query(Query {
                authors: Some(vec!["p1".into()]),
                kinds: Some(vec![30023]),
                d: Some("s2".into()),
                t: None,
                since: None,
                until: None,
                limit: Some(10),
            })
            .unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].id, "bb22");
    }

    #[test]
    fn rebuild_indexes() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let ev = sample_event("abcd", "pub", 1, Some("slug"), 1);
        store.ingest(&ev).unwrap();
        // remove indexes and latest
        fs::remove_dir_all(dir.path().join("index")).unwrap();
        fs::remove_dir_all(dir.path().join("latest")).unwrap();
        // rebuild
        store.reindex().unwrap();
        let author_idx = fs::read_to_string(dir.path().join("index/by-author/pub.txt")).unwrap();
        assert_eq!(author_idx.trim(), "abcd");
        let latest = fs::read_to_string(dir.path().join("latest/pub.1.slug")).unwrap();
        assert_eq!(latest, "abcd");
    }

    #[test]
    fn tag_index_and_reindex() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let ev = Event {
            id: "aa11".into(),
            pubkey: "p1".into(),
            kind: 1,
            created_at: 1,
            tags: vec![Tag(vec!["t".into(), "tag1".into()])],
            content: String::new(),
            sig: String::new(),
        };
        store.ingest(&ev).unwrap();
        let tag_path = dir.path().join("index/by-tag/t/tag1.txt");
        let contents = fs::read_to_string(&tag_path).unwrap();
        assert_eq!(contents.trim(), "aa11");
        fs::remove_file(tag_path).unwrap();
        store.reindex().unwrap();
        let rebuilt = fs::read_to_string(dir.path().join("index/by-tag/t/tag1.txt")).unwrap();
        assert_eq!(rebuilt.trim(), "aa11");
    }

    #[test]
    fn latest_pointer_updates_and_query_returns_latest() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let e1 = sample_event("aa11", "p1", 30023, Some("slug"), 1);
        let e2 = sample_event("bb22", "p1", 30023, Some("slug"), 2);
        store.ingest(&e1).unwrap();
        store.ingest(&e2).unwrap();
        let latest_path = dir.path().join("latest/p1.30023.slug");
        let latest = fs::read_to_string(latest_path).unwrap();
        assert_eq!(latest, "bb22");
        let res = store
            .query(Query {
                authors: Some(vec!["p1".into()]),
                kinds: Some(vec![30023]),
                d: Some("slug".into()),
                t: None,
                since: None,
                until: None,
                limit: None,
            })
            .unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].id, "bb22");
    }

    #[test]
    fn ingest_rejects_bad_sig() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), true);
        store.init().unwrap();
        let mut ev = signed_event(1);
        ev.sig.replace_range(0..2, "00");
        assert!(store.ingest(&ev).is_err());
    }

    #[test]
    fn verify_sample_checks_events() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let ev1 = signed_event(1);
        let ev2 = signed_event(2);
        store.ingest(&ev1).unwrap();
        store.ingest(&ev2).unwrap();
        assert_eq!(store.verify_sample(10).unwrap(), 2);
        // corrupt one event's signature
        let mut bad = ev1.clone();
        bad.sig = "00".repeat(64);
        let path = store.event_path(&bad.id);
        fs::write(path, serde_json::to_string(&bad).unwrap()).unwrap();
        assert!(store.verify_sample(10).is_err());
    }

    #[test]
    fn query_since_until_and_limit() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let e1 = sample_event("aa11", "p1", 1, None, 10);
        let e2 = sample_event("bb22", "p1", 1, None, 20);
        let e3 = sample_event("cc33", "p1", 1, None, 30);
        store.ingest(&e1).unwrap();
        store.ingest(&e2).unwrap();
        store.ingest(&e3).unwrap();
        let res = store
            .query(Query {
                authors: Some(vec!["p1".into()]),
                kinds: Some(vec![1]),
                d: None,
                t: None,
                since: Some(15),
                until: Some(25),
                limit: Some(1),
            })
            .unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].id, "bb22");
    }

    #[test]
    fn query_without_filters_returns_empty() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let res = store
            .query(Query {
                authors: None,
                kinds: None,
                d: None,
                t: None,
                since: None,
                until: None,
                limit: None,
            })
            .unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn ingest_rejects_id_mismatch() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), true);
        store.init().unwrap();
        let mut ev = signed_event(1);
        ev.id.replace_range(0..2, "ff");
        assert!(store.ingest(&ev).is_err());
    }

    #[test]
    fn read_ids_returns_empty_for_missing_file() {
        let path = std::path::PathBuf::from("missing.txt");
        let ids = super::read_ids(&path).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn mirror_symlinks_not_duplicated() {
        let dir = TempDir::new().unwrap();
        let store = Store::new(dir.path().to_path_buf(), false);
        store.init().unwrap();
        let ev = sample_event("abcd", "pub", 30023, Some("slug"), 42);
        store.ingest(&ev).unwrap();
        store.ingest(&ev).unwrap();
        let author_dir = dir.path().join("mirror/authors/pub");
        let author_count = std::fs::read_dir(author_dir).unwrap().count();
        assert_eq!(author_count, 1);
        let kind_dir = dir.path().join("mirror/kinds/30023");
        let kind_count = std::fs::read_dir(kind_dir).unwrap().count();
        assert_eq!(kind_count, 1);
    }

    #[test]
    fn event_hash_matches_reference() {
        use sha2::{Digest, Sha256};
        let ev = Event {
            id: String::new(),
            pubkey: "00".repeat(32),
            kind: 1,
            created_at: 1,
            tags: vec![],
            content: String::new(),
            sig: String::new(),
        };
        let expected = {
            let obj =
                serde_json::json!([0, ev.pubkey, ev.created_at, ev.kind, ev.tags, ev.content]);
            let mut hasher = Sha256::new();
            hasher.update(serde_json::to_vec(&obj).unwrap());
            let bytes = hasher.finalize();
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            arr
        };
        assert_eq!(event_hash(&ev).unwrap(), expected);
    }
}
