//! Per-peer conversation-summary metadata for the F1 conversation-list UI.
//!
//! The chat-lib stores a full `Dialogue` (rkyv-serialized) per peer under
//! the `sigchat.dialogue` PDDB dict. Reading the full Dialogue just to know
//! the recency, unread count, and last-line preview of every conversation
//! would scale poorly. Instead we maintain a tiny per-peer summary record
//! under `sigchat.peers` (one PDDB key per peer UUID) holding only the
//! fields the conversation-list view needs:
//!
//!   { display_name, last_ts, last_snippet, unread }
//!
//! Updated by the receive path (`main_ws::deliver_*`) on inbound messages
//! and by `lib.rs` on outbound and on focus changes (`open_peer` clears
//! `unread`).
//!
//! See ADR 0012 for the architectural rationale.

#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::panic)]

use std::io::{Read, Write};

pub const PEERS_DICT: &str = "sigchat.peers";

const SNIPPET_MAX_BYTES: usize = 80;

/// In-memory summary for a single peer. Persisted as a small JSON blob
/// under `sigchat.peers/<uuid>`. New fields can be added at the end with
/// `Option<...>` reads to stay forward-compatible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSummary {
    pub uuid: String,
    pub display_name: String,
    pub last_ts: u64,
    pub last_snippet: String,
    pub unread: u32,
}

impl PeerSummary {
    fn to_json(&self) -> String {
        format!(
            "{{\"display_name\":\"{}\",\"last_ts\":{},\"last_snippet\":\"{}\",\"unread\":{}}}",
            json_escape(&self.display_name),
            self.last_ts,
            json_escape(&self.last_snippet),
            self.unread,
        )
    }

    fn from_json(uuid: &str, s: &str) -> Option<Self> {
        let display_name = extract_string_field(s, "display_name").unwrap_or_default();
        let last_ts: u64 = extract_number_field(s, "last_ts").and_then(|v| v.parse().ok()).unwrap_or(0);
        let last_snippet = extract_string_field(s, "last_snippet").unwrap_or_default();
        let unread: u32 = extract_number_field(s, "unread").and_then(|v| v.parse().ok()).unwrap_or(0);
        Some(PeerSummary {
            uuid: uuid.to_string(),
            display_name,
            last_ts,
            last_snippet,
            unread,
        })
    }
}

/// Read a single peer's summary from PDDB. Returns `None` if no record
/// exists yet (e.g., this is the first message we're tracking from them).
pub fn read(uuid: &str) -> Option<PeerSummary> {
    let pddb = pddb::Pddb::new();
    pddb.try_mount();
    let raw = pddb_get_string(&pddb, PEERS_DICT, uuid)?;
    PeerSummary::from_json(uuid, &raw)
}

/// Persist a peer summary, overwriting any existing record.
pub fn write_summary(summary: &PeerSummary) -> std::io::Result<()> {
    let pddb = pddb::Pddb::new();
    pddb.try_mount();
    let payload = summary.to_json();
    pddb.delete_key(PEERS_DICT, &summary.uuid, None).ok();
    let mut h = pddb.get(PEERS_DICT, &summary.uuid, None, true, true, None, None::<fn()>)?;
    h.write_all(payload.as_bytes())?;
    pddb.sync().ok();
    Ok(())
}

/// Apply an inbound DataMessage update to a peer's summary: bump
/// `last_ts`, replace `last_snippet`, and increment `unread` iff
/// `is_focused == false`. Creates the record if absent. The display
/// name is left untouched if a record already exists; otherwise it
/// defaults to the UUID itself (caller can override via
/// [`set_display_name`] later).
pub fn record_inbound(uuid: &str, ts: u64, body: &str, is_focused: bool) -> std::io::Result<()> {
    let mut summary = read(uuid).unwrap_or_else(|| PeerSummary {
        uuid: uuid.to_string(),
        display_name: uuid.to_string(),
        last_ts: 0,
        last_snippet: String::new(),
        unread: 0,
    });
    summary.last_ts = ts;
    summary.last_snippet = truncate_snippet(body);
    if !is_focused {
        summary.unread = summary.unread.saturating_add(1);
    }
    write_summary(&summary)
}

/// Apply an outbound message update. Bumps `last_ts` and replaces
/// `last_snippet`. Does not touch `unread` (we only count inbound).
pub fn record_outbound(uuid: &str, ts: u64, body: &str) -> std::io::Result<()> {
    let mut summary = read(uuid).unwrap_or_else(|| PeerSummary {
        uuid: uuid.to_string(),
        display_name: uuid.to_string(),
        last_ts: 0,
        last_snippet: String::new(),
        unread: 0,
    });
    summary.last_ts = ts;
    summary.last_snippet = truncate_snippet(body);
    write_summary(&summary)
}

/// Reset a peer's `unread` counter to zero. Called when the user
/// focuses the peer's conversation.
pub fn clear_unread(uuid: &str) -> std::io::Result<()> {
    if let Some(mut summary) = read(uuid) {
        if summary.unread != 0 {
            summary.unread = 0;
            return write_summary(&summary);
        }
    }
    Ok(())
}

/// Enumerate every peer summary, sorted by `last_ts` descending
/// (most recent first). Pinning is not yet supported — see ADR 0012
/// for the deferred-features list.
pub fn list_sorted() -> Vec<PeerSummary> {
    let pddb = pddb::Pddb::new();
    pddb.try_mount();
    let keys = match pddb.list_keys(PEERS_DICT, None) {
        Ok(k) => k,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<PeerSummary> = keys
        .into_iter()
        .filter_map(|uuid| {
            pddb_get_string(&pddb, PEERS_DICT, &uuid)
                .and_then(|raw| PeerSummary::from_json(&uuid, &raw))
        })
        .collect();
    out.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    out
}

fn truncate_snippet(s: &str) -> String {
    if s.len() <= SNIPPET_MAX_BYTES {
        return s.to_string();
    }
    // Avoid splitting a UTF-8 codepoint mid-byte.
    let mut end = SNIPPET_MAX_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + 1);
    out.push_str(&s[..end]);
    out.push('…');
    out
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn extract_string_field(s: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = s.find(&needle)? + needle.len();
    let rest = &s[start..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            },
            c => out.push(c),
        }
    }
    None
}

fn extract_number_field(s: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let start = s.find(&needle)? + needle.len();
    let rest = &s[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(rest[..end].to_string())
}

fn pddb_get_string(pddb: &pddb::Pddb, dict: &str, key: &str) -> Option<String> {
    match pddb.get(dict, key, None, true, false, None, None::<fn()>) {
        Ok(mut handle) => {
            let mut buf = Vec::new();
            handle.read_to_end(&mut buf).ok()?;
            String::from_utf8(buf).ok()
        }
        Err(_) => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_preserves_fields() {
        let s = PeerSummary {
            uuid: "uuid-x".into(),
            display_name: "Alice".into(),
            last_ts: 1_700_000_000_000,
            last_snippet: "hello".into(),
            unread: 3,
        };
        let json = s.to_json();
        let back = PeerSummary::from_json("uuid-x", &json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn json_round_trip_handles_quotes_and_backslashes() {
        let s = PeerSummary {
            uuid: "u".into(),
            display_name: r#"Carol "C." \ Whitfield"#.into(),
            last_ts: 0,
            last_snippet: "line\nbreak".into(),
            unread: 0,
        };
        let json = s.to_json();
        let back = PeerSummary::from_json("u", &json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn from_json_tolerates_missing_fields() {
        // Forward-compat: a record written by an older build with fewer
        // fields should still produce a usable summary.
        let s = r#"{"last_ts":42}"#;
        let back = PeerSummary::from_json("u", s).unwrap();
        assert_eq!(back.last_ts, 42);
        assert_eq!(back.unread, 0);
        assert!(back.display_name.is_empty());
    }

    #[test]
    fn truncate_snippet_keeps_short_strings() {
        assert_eq!(truncate_snippet("hi"), "hi");
    }

    #[test]
    fn truncate_snippet_clips_at_byte_budget() {
        let long = "a".repeat(200);
        let out = truncate_snippet(&long);
        // Result must end with the ellipsis and contain at most
        // SNIPPET_MAX_BYTES + 3 (UTF-8 for `…`) bytes.
        assert!(out.ends_with('…'));
        assert!(out.len() <= SNIPPET_MAX_BYTES + 3);
    }

    #[test]
    fn truncate_snippet_does_not_split_utf8() {
        // 4-byte codepoint at the boundary must not be split mid-byte.
        let mut s = "a".repeat(SNIPPET_MAX_BYTES - 1);
        s.push('🦀'); // 4 bytes
        s.push('!');
        let out = truncate_snippet(&s);
        // Must be valid UTF-8 (any String already is, but the slice
        // must not panic — covered by the function's char_boundary loop).
        assert!(out.ends_with('…'));
    }
}
