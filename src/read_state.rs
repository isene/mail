//! Which messages have been read, agreed between devices.
//!
//! The rule is deliberately asymmetric, because the laptop is the
//! authoritative device:
//!
//!   * read on the laptop        → read everywhere
//!   * merely opened on a phone  → nothing at all
//!   * explicitly marked on a phone → read everywhere
//!
//! That falls out of what each side *writes* rather than from any
//! special case here. The laptop publishes every read; a phone
//! publishes only the explicit ones. Both then merge the same way.
//!
//! Storage is one file per device in a shared folder — the same shape
//! the watchit ratings use, for the same reason: two writers on one
//! file is what makes Syncthing leave `.sync-conflict-` copies nobody
//! reads. Each device writes only its own file and reads them all.
//!
//! The key is the RFC822 `Message-ID`, the one identity a mail keeps
//! across a maildir, an IMAP server and a phone. Note what is NOT used:
//! the server's `\Seen` flag. Mail is delivered here by a fetcher that
//! never writes flags back, so the server has no opinion to consult.
//!
//! Unread is a state, not an absence. Marking something unread again
//! stores `read: false` with a fresh timestamp — dropping the entry
//! would let the other device's older "read" win the next merge and the
//! message would quietly go read again.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub read: bool,
    /// Unix seconds. Newest wins when devices disagree.
    pub ts: i64,
}

/// Message-ID → mark.
pub type Marks = HashMap<String, Mark>;

/// Parse one device's file. Unreadable JSON yields nothing: a
/// half-synced file must never take the reader down.
pub fn parse(json: &str) -> Marks {
    serde_json::from_str(json).unwrap_or_default()
}

pub fn serialize(marks: &Marks) -> String {
    serde_json::to_string_pretty(marks).unwrap_or_else(|_| "{}".to_string())
}

/// Merge `src` into `dst`, newest timestamp per message wins.
pub fn merge_into(dst: &mut Marks, src: Marks) {
    for (id, m) in src {
        match dst.get(&id) {
            Some(old) if old.ts >= m.ts => {}
            _ => { dst.insert(id, m); }
        }
    }
}

/// Merge every device's file into one view.
pub fn merge_all<'a, I: IntoIterator<Item = &'a str>>(files: I) -> Marks {
    let mut out = Marks::new();
    for json in files {
        merge_into(&mut out, parse(json));
    }
    out
}

/// Has this message been read? Unknown means unread — a message nobody
/// has said anything about has not been read.
pub fn is_read(marks: &Marks, message_id: &str) -> bool {
    marks.get(message_id).map(|m| m.read).unwrap_or(false)
}

/// Record a state change.
pub fn set(marks: &mut Marks, message_id: &str, read: bool, now: i64) {
    marks.insert(message_id.to_string(), Mark { read, ts: now });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(pairs: &[(&str, bool, i64)]) -> Marks {
        pairs.iter().map(|(id, read, ts)| {
            (id.to_string(), Mark { read: *read, ts: *ts })
        }).collect()
    }

    #[test]
    fn the_laptop_reaches_the_phone() {
        let laptop = marks(&[("a@x", true, 100)]);
        let phone = Marks::new();
        let mut all = phone;
        merge_into(&mut all, laptop);
        assert!(is_read(&all, "a@x"));
    }

    #[test]
    fn opening_on_the_phone_says_nothing() {
        // The phone's file simply has no entry for a message it merely
        // displayed, so the laptop's unread state stands.
        let all = merge_all(vec![
            r#"{"a@x": {"read": false, "ts": 100}}"#,   // laptop: unread
            r#"{}"#,                                     // phone: opened, silent
        ]);
        assert!(!is_read(&all, "a@x"));
    }

    #[test]
    fn an_explicit_mark_on_the_phone_reaches_the_laptop() {
        let all = merge_all(vec![
            r#"{"a@x": {"read": false, "ts": 100}}"#,
            r#"{"a@x": {"read": true,  "ts": 200}}"#,
        ]);
        assert!(is_read(&all, "a@x"));
    }

    #[test]
    fn unread_again_is_an_event_not_an_absence() {
        let all = merge_all(vec![
            r#"{"a@x": {"read": true,  "ts": 100}}"#,
            r#"{"a@x": {"read": false, "ts": 200}}"#,
        ]);
        assert!(!is_read(&all, "a@x"), "the newer state wins, in both directions");
    }

    #[test]
    fn garbage_is_not_fatal() {
        let all = merge_all(vec!["not json", r#"{"a@x": {"read": true, "ts": 1}}"#]);
        assert!(is_read(&all, "a@x"));
    }

    #[test]
    fn an_unknown_message_is_unread() {
        assert!(!is_read(&Marks::new(), "never@seen"));
    }
}
