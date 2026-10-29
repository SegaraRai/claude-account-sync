//! The schema of local scheduled tasks: `scheduled-tasks.json` in each replica.
//!
//! Each task points to a prompt file under `~/.claude/scheduled-tasks`, which does not depend on the account. This
//! module merges the task entries. The other top-level fields, such as `recordedSkips` and `runRetries`, record
//! runs in one account, so each replica keeps its own values.

use serde::Deserialize;
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::item_file::{ItemFile, ItemFormat};
use crate::merge::OrderKey;

/// The scheduled tasks collection.
pub const SCHEDULED_TASKS: ItemFile<ScheduledTasks> = ItemFile(std::marker::PhantomData);

/// The scheduled tasks file format.
pub struct ScheduledTasks;

/// The fields of a task entry that the app requires. Other fields are copied unchanged.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScheduledTask {
    /// Not empty.
    id: String,
    #[serde(rename = "enabled")]
    _enabled: bool,
    #[serde(rename = "filePath")]
    _file_path: String,
    /// Milliseconds since the Unix epoch.
    created_at: f64,
    /// RFC 3339 time of the last run, when the task has run.
    last_run_at: Option<String>,
}

impl ItemFormat for ScheduledTasks {
    const NAME: &'static str = "scheduledTasks";
    const RELATIVE_PATH: &'static str = "scheduled-tasks.json";
    const ITEMS_FIELD: &'static str = "scheduledTasks";
    const MAX_LEN: u64 = 32 << 20;

    fn check_document(_document: &Map<String, Value>) -> Result<(), String> {
        Ok(())
    }

    /// Orders versions by the last run, so that the version from the account that ran the task most recently
    /// wins a conflict.
    fn validate_item(item: &Value) -> Option<(String, OrderKey)> {
        let task = ScheduledTask::deserialize(item).ok()?;
        if task.id.is_empty() || !task.created_at.is_finite() {
            return None;
        }
        let last_run_ms = task
            .last_run_at
            .and_then(|text| OffsetDateTime::parse(&text, &Rfc3339).ok())
            .map_or(0, |time| (time.unix_timestamp_nanos() / 1_000_000) as i64);
        Some((
            task.id,
            OrderKey {
                primary: last_run_ms,
                secondary: task.created_at as i64,
            },
        ))
    }

    fn new_document() -> Map<String, Value> {
        let mut document = Map::new();
        document.insert(Self::ITEMS_FIELD.into(), Value::Array(Vec::new()));
        document
    }

    /// The app writes this file as JSON indented by two spaces.
    fn render(document: &Map<String, Value>) -> Vec<u8> {
        serde_json::to_vec_pretty(document).expect("JSON object serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn orders_tasks_by_last_run() {
        let task = json!({"id": "t", "enabled": true, "filePath": "f", "createdAt": 5,
                          "lastRunAt": "2026-10-29T00:00:00.000Z"});
        let (key, order) = ScheduledTasks::validate_item(&task).unwrap();
        assert_eq!(key, "t");
        assert_eq!(
            order,
            OrderKey {
                primary: 1_793_232_000_000,
                secondary: 5
            }
        );
    }

    #[test]
    fn rejects_task_without_required_fields() {
        assert!(ScheduledTasks::validate_item(&json!({"id": "t", "createdAt": 1})).is_none());
    }

    #[test]
    fn renders_like_the_app() {
        let document = json!({"scheduledTasks": [], "recordedSkips": {}});
        let rendered = ScheduledTasks::render(document.as_object().unwrap());
        assert_eq!(
            String::from_utf8(rendered).unwrap(),
            "{\n  \"scheduledTasks\": [],\n  \"recordedSkips\": {}\n}"
        );
    }
}
