//! The schema of `archived-sessions.idx`, which lists archived session ids in each replica.
//!
//! The app reads this file as a hint while it loads the sidebar, and it loads without the hint when the file is
//! missing or invalid. Each record also stores `isArchived`. After this tool changes records in a replica, it
//! rewrites the index from those records so that the hint and the records agree.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::record::is_session_id;

pub const FILE_NAME: &str = "archived-sessions.idx";

/// The app refuses to read an index larger than this many bytes.
pub const MAX_LEN: u64 = 8 << 20;

const VERSION: u32 = 1;

/// The contents of `archived-sessions.idx`: `{"v":1,"archived":[<sorted session ids>]}`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ArchiveIndex {
    v: u32,
    archived: Vec<String>,
}

/// Parses an index. Returns `None` when the version is not 1 or an id is invalid, which matches the cases in which
/// the app ignores the file.
pub fn parse(bytes: &[u8]) -> Option<BTreeSet<String>> {
    let index: ArchiveIndex = serde_json::from_slice(bytes).ok()?;
    if index.v != VERSION || !index.archived.iter().all(|id| is_session_id(id)) {
        return None;
    }
    Some(index.archived.into_iter().collect())
}

/// Renders an index in the app's format: compact JSON with ids in ascending order.
pub fn render(archived: &BTreeSet<String>) -> Vec<u8> {
    let index = ArchiveIndex {
        v: VERSION,
        archived: archived.iter().cloned().collect(),
    };
    serde_json::to_vec(&index).expect("index serializes to JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_in_app_format() {
        let ids: BTreeSet<String> = ["local_b".to_string(), "local_a".to_string()].into();
        let bytes = render(&ids);
        assert_eq!(bytes, br#"{"v":1,"archived":["local_a","local_b"]}"#);
        assert_eq!(parse(&bytes), Some(ids));
    }

    #[test]
    fn rejects_unknown_version_and_invalid_ids() {
        assert_eq!(parse(br#"{"v":2,"archived":[]}"#), None);
        assert_eq!(parse(br#"{"v":1,"archived":["x"]}"#), None);
    }
}
