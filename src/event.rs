//! Nostr event model.

use serde::{Deserialize, Serialize};

/// Simple tag wrapper preserving tag fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tag(pub Vec<String>);

/// Core Nostr event persisted on disk and served to clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// Event identifier (hex of SHA-256 hash).
    pub id: String,
    /// Author public key (hex).
    pub pubkey: String,
    /// Kind number, e.g. `1` or `30023`.
    pub kind: u32,
    /// Unix timestamp of creation.
    pub created_at: u64,
    /// Arbitrary tags.
    pub tags: Vec<Tag>,
    /// Event content body.
    pub content: String,
    /// Schnorr signature over the event hash.
    pub sig: String,
}
