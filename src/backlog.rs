//! The schema of the task backlog: `backlog/tasks.json` in each replica.
//!
//! The backlog holds follow-up tasks that the app suggested and the user set aside. The app drops items that fail
//! its validation, keeps at most 20 items per folder and 100 in total, and drops items that it suggested more than
//! 30 days ago. The app applies these limits when it changes the backlog, so this module does not apply them.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::item_file::{ItemFile, ItemFormat};
use crate::merge::OrderKey;

/// The backlog collection.
pub const BACKLOG: ItemFile<Backlog> = ItemFile(std::marker::PhantomData);

/// The backlog file format.
pub struct Backlog;

/// The file version that this module supports. The app leaves files with a larger version untouched, and so does
/// this module.
const VERSION: u64 = 1;

/// The fields of a backlog item that the app validates. Other fields are copied unchanged.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BacklogItem {
    /// `todo_` followed by 12 lowercase hexadecimal digits.
    id: String,
    #[serde(rename = "source")]
    _source: BacklogSource,
    /// Milliseconds since the Unix epoch when the task entered the backlog.
    filed_at: f64,
    /// At most 128 characters, and not empty.
    origin_session_id: String,
    /// At most 4096 characters.
    cwd: String,
}

/// Why a task entered the backlog. Maps to the lowercase string in the file.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum BacklogSource {
    Dismissed,
    Evicted,
    Orphaned,
}

fn is_todo_id(id: &str) -> bool {
    id.strip_prefix("todo_").is_some_and(|hex| {
        hex.len() == 12
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

impl ItemFormat for Backlog {
    const NAME: &'static str = "backlog";
    const RELATIVE_PATH: &'static str = "backlog/tasks.json";
    const ITEMS_FIELD: &'static str = "items";
    const MAX_LEN: u64 = 32 << 20;

    fn check_document(document: &Map<String, Value>) -> Result<(), String> {
        match document.get("version").and_then(Value::as_u64) {
            Some(version) if version <= VERSION => Ok(()),
            Some(version) => Err(format!("backlog version {version} is newer than {VERSION}")),
            None => Err("backlog has no numeric `version`".into()),
        }
    }

    fn validate_item(item: &Value) -> Option<(String, OrderKey)> {
        let parsed = BacklogItem::deserialize(item).ok()?;
        let valid = is_todo_id(&parsed.id)
            && parsed.filed_at.is_finite()
            && !parsed.origin_session_id.is_empty()
            && parsed.origin_session_id.chars().count() <= 128
            && parsed.cwd.chars().count() <= 4096;
        valid.then_some((
            parsed.id,
            OrderKey {
                primary: parsed.filed_at as i64,
                secondary: 0,
            },
        ))
    }

    fn new_document() -> Map<String, Value> {
        let mut document = Map::new();
        document.insert("version".into(), VERSION.into());
        document
    }

    /// The app writes the backlog as compact JSON.
    fn render(document: &Map<String, Value>) -> Vec<u8> {
        serde_json::to_vec(document).expect("JSON object serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(id: &str) -> Value {
        json!({"id": id, "cwd": "F:\\p", "source": "evicted", "title": "t", "prompt": "p",
               "suggestedAt": 1, "filedAt": 2, "originSessionId": "local_a", "originTaskId": "x"})
    }

    #[test]
    fn accepts_valid_item() {
        let (key, order) = Backlog::validate_item(&item("todo_9be07924bb1b")).unwrap();
        assert_eq!(key, "todo_9be07924bb1b");
        assert_eq!(order.primary, 2);
    }

    #[test]
    fn rejects_items_that_the_app_drops() {
        assert!(Backlog::validate_item(&item("todo_XYZ")).is_none());
        let mut bad_source = item("todo_9be07924bb1b");
        bad_source["source"] = json!("other");
        assert!(Backlog::validate_item(&bad_source).is_none());
    }

    #[test]
    fn refuses_newer_file_version() {
        let document = json!({"version": 2, "items": []});
        assert!(Backlog::check_document(document.as_object().unwrap()).is_err());
    }
}
