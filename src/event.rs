//! Nostr event model.

use serde::{Deserialize, Serialize};

/// Wrapper for a Nostr tag expressed as an array of strings.
///
/// Tags appear as small arrays where the first element denotes the type and the
/// following elements hold data. Common examples include:
///
/// - `p` – references another author's public key
/// - `e` – links to another event ID
/// - `d` – unique identifier for replaceable events
/// - `t` – free-form topic or hashtag
///
/// Each tag is stored verbatim so uncommon or custom tags are preserved. For
/// example, a `["t", "news"]` tag from the protocol is represented as
/// `Tag(vec!["t".into(), "news".into()])`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tag(pub Vec<String>);

/// Core Nostr event persisted on disk and served to clients.
///
/// ```json
/// {
///   "id": "aa11",
///   "pubkey": "npub...",
///   "kind": 1,
///   "created_at": 1700000000,
///   "tags": [["t", "news"], ["d", "slug"]],
///   "content": "hello",
///   "sig": "deadbeef"
/// }
/// ```
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
    /// Arbitrary tags such as `d` (identifier) or `t` (topic).
    pub tags: Vec<Tag>,
    /// Event content body.
    pub content: String,
    /// Schnorr signature over the event hash.
    pub sig: String,
}
