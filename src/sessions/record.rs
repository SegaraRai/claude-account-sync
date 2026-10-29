//! The schema of session records: the `local_<id>.json` files that list Code tab sessions in the sidebar.
//!
//! The app stores many more fields than this schema reads. The tool copies record files byte for byte, so unknown
//! fields reach other replicas unchanged. The schema only validates the fields that the merge and the archive
//! index depend on.

use serde::Deserialize;

/// Errors that make a record file unusable as a merge source.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("invalid JSON or missing required field: {0}")]
    Schema(#[from] serde_json::Error),
    #[error("sessionId {found:?} does not match the file name {expected:?}")]
    IdMismatch { expected: String, found: String },
    #[error("cliSessionId is empty")]
    EmptyCliSessionId,
}

/// Prefix of session ids and record file names.
pub const SESSION_ID_PREFIX: &str = "local_";

/// Prefix of the files that record a deleted session. The rest of the file name is the session id.
pub const TOMBSTONE_PREFIX: &str = "deleted_";

/// Returns `true` when `id` has the form `local_<letters, digits, '_', or '-'>`, which the app requires for
/// session ids.
pub fn is_session_id(id: &str) -> bool {
    id.strip_prefix(SESSION_ID_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

/// The fields of a session record that this tool reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    /// Equals the file name without `.json`.
    pub session_id: String,
    /// Names the transcript `<cliSessionId>.jsonl` under `~/.claude/projects`. Transcripts do not depend on the
    /// account, so a copied record opens the same transcript.
    pub cli_session_id: String,
    pub cwd: String,
    /// Milliseconds since the Unix epoch.
    pub created_at: f64,
    /// Milliseconds since the Unix epoch. The merge orders conflicting versions by this field.
    pub last_activity_at: f64,
    #[serde(default)]
    pub is_archived: bool,
    #[serde(default)]
    pub title: Option<String>,
}

impl SessionRecord {
    /// Parses and validates the record file of session `expected_id`.
    pub fn parse(bytes: &[u8], expected_id: &str) -> Result<Self, RecordError> {
        let record: Self = serde_json::from_slice(bytes)?;
        if record.session_id != expected_id {
            return Err(RecordError::IdMismatch {
                expected: expected_id.into(),
                found: record.session_id,
            });
        }
        if record.cli_session_id.is_empty() {
            return Err(RecordError::EmptyCliSessionId);
        }
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_session_ids() {
        assert!(is_session_id("local_09332c6b-65b5-447c-b5dd-c571c76675ca"));
        assert!(!is_session_id("local_"));
        assert!(!is_session_id("local_a.b"));
        assert!(!is_session_id("todo_1"));
    }

    #[test]
    fn parses_record_and_ignores_unknown_fields() {
        let bytes = br#"{"sessionId":"local_a","cliSessionId":"c","cwd":"F:\\p","createdAt":1,"lastActivityAt":2,"isArchived":true,"extra":{"":true}}"#;
        let record = SessionRecord::parse(bytes, "local_a").unwrap();
        assert!(record.is_archived);
        assert_eq!(record.last_activity_at, 2.0);
    }

    #[test]
    fn rejects_record_for_other_session() {
        let bytes = br#"{"sessionId":"local_b","cliSessionId":"c","cwd":"","createdAt":1,"lastActivityAt":2}"#;
        assert!(matches!(
            SessionRecord::parse(bytes, "local_a"),
            Err(RecordError::IdMismatch { .. })
        ));
    }

    #[test]
    fn rejects_record_without_required_fields() {
        assert!(SessionRecord::parse(br#"{"sessionId":"local_a"}"#, "local_a").is_err());
    }
}
