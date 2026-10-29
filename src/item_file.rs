//! Merges collections that the app stores as one JSON file per replica with an array of items keyed by `id`.
//!
//! The backlog and the scheduled tasks use this module. Items are merged one by one. Fields outside the item array
//! belong to the replica, so a rewrite keeps the target's own values for them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::file_ops::{read_regular, write_atomic};
use crate::merge::{Action, ContentHash, EntryVersion, OrderKey, ReplicaInput, hash_bytes};
use crate::pass::{Collection, PassContext};
use crate::replica::Replica;

/// The file format of one item collection.
pub trait ItemFormat: Sync {
    /// The collection's key in the state file.
    const NAME: &'static str;
    /// The file's path relative to the replica folder.
    const RELATIVE_PATH: &'static str;
    /// The top-level field that holds the item array.
    const ITEMS_FIELD: &'static str;
    /// The app refuses to read a file larger than this many bytes.
    const MAX_LEN: u64;

    /// Checks the fields outside the item array. An error skips the replica for this collection.
    fn check_document(document: &Map<String, Value>) -> Result<(), String>;

    /// Validates one item against the app's schema and returns its key and order. `None` marks an item that the
    /// app would drop. Such items stay in their replica and are not copied.
    fn validate_item(item: &Value) -> Option<(String, OrderKey)>;

    /// Returns the top-level fields of a file that the app has not created yet.
    fn new_document() -> Map<String, Value>;

    /// Serializes a document in the same layout that the app writes.
    fn render(document: &Map<String, Value>) -> Vec<u8>;
}

/// An item collection in the format `F`.
pub struct ItemFile<F>(pub std::marker::PhantomData<F>);

/// The item file of one replica.
pub struct LoadedItems {
    path: PathBuf,
    /// The file content as read. `None` when the file does not exist.
    bytes: Option<Vec<u8>>,
    /// The fields outside the item array.
    document: Map<String, Value>,
    /// All items in file order. Valid items have a key.
    items: Vec<(Option<String>, Value)>,
    current: BTreeMap<String, EntryVersion>,
}

impl<F: ItemFormat> Collection for ItemFile<F> {
    const NAME: &'static str = F::NAME;
    type Loaded = LoadedItems;

    fn load(&self, replica: &Replica) -> Result<LoadedItems, String> {
        let path = replica.dir.join(F::RELATIVE_PATH);
        let bytes =
            read_regular(&path, F::MAX_LEN).map_err(|e| format!("{}: {e}", F::RELATIVE_PATH))?;
        let Some(bytes) = bytes else {
            return Ok(LoadedItems {
                path,
                bytes: None,
                document: F::new_document(),
                items: Vec::new(),
                current: BTreeMap::new(),
            });
        };
        let Value::Object(mut document) =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", F::RELATIVE_PATH))?
        else {
            return Err(format!("{} is not a JSON object", F::RELATIVE_PATH));
        };
        F::check_document(&document)?;
        // `take` leaves `null` in place, so a rewrite keeps the field at its original position.
        let Some(Value::Array(raw_items)) = document.get_mut(F::ITEMS_FIELD).map(Value::take)
        else {
            return Err(format!(
                "{} has no `{}` array",
                F::RELATIVE_PATH,
                F::ITEMS_FIELD
            ));
        };

        let mut items = Vec::with_capacity(raw_items.len());
        let mut current = BTreeMap::new();
        for item in raw_items {
            match F::validate_item(&item) {
                Some((key, order)) if !current.contains_key(&key) => {
                    current.insert(
                        key.clone(),
                        EntryVersion {
                            hash: item_hash(&item),
                            order,
                        },
                    );
                    items.push((Some(key), item));
                }
                _ => items.push((None, item)),
            }
        }
        Ok(LoadedItems {
            path,
            bytes: Some(bytes),
            document,
            items,
            current,
        })
    }

    fn input(&self, loaded: &LoadedItems) -> ReplicaInput {
        ReplicaInput {
            current: loaded.current.clone(),
            ..Default::default()
        }
    }

    fn apply(
        &self,
        context: &mut PassContext<'_>,
        replicas: &[(&Replica, &LoadedItems)],
        _inputs: &[ReplicaInput],
        target: usize,
        actions: &[Action],
    ) -> BTreeMap<String, ContentHash> {
        let (replica, loaded) = replicas[target];
        let unchanged = loaded
            .current
            .iter()
            .map(|(k, v)| (k.clone(), v.hash))
            .collect();

        let mut items = loaded.items.clone();
        for action in actions {
            match action {
                Action::Copy { key, source, .. } => {
                    let source_items = &replicas[*source].1.items;
                    let value = source_items
                        .iter()
                        .find(|(k, _)| k.as_ref() == Some(key))
                        .map(|(_, v)| v.clone())
                        .expect("the merge copies only keys that the source holds");
                    match items.iter_mut().find(|(k, _)| k.as_ref() == Some(key)) {
                        Some(slot) => slot.1 = value,
                        None => items.push((Some(key.clone()), value)),
                    }
                }
                Action::Remove { key, .. } => items.retain(|(k, _)| k.as_ref() != Some(key)),
            }
        }

        // The app may have rewritten the file after `load`. Its newer content wins until the next pass.
        let on_disk = read_regular(&loaded.path, F::MAX_LEN).ok().flatten();
        if on_disk != loaded.bytes {
            context.report.problems.push(format!(
                "{}: {} changed during the pass",
                F::NAME,
                replica.id
            ));
            return unchanged;
        }
        if let Some(old) = &loaded.bytes
            && let Err(error) = context.back_up(&replica.id, Path::new(F::RELATIVE_PATH), old)
        {
            context.report.problems.push(format!(
                "{}: cannot back up {}: {error}",
                F::NAME,
                replica.id
            ));
            return unchanged;
        }

        let mut document = loaded.document.clone();
        document.insert(
            F::ITEMS_FIELD.to_owned(),
            Value::Array(items.iter().map(|(_, v)| v.clone()).collect()),
        );
        if let Err(error) = write_atomic(&loaded.path, &F::render(&document), None) {
            context.report.problems.push(format!(
                "{}: cannot write {}: {error}",
                F::NAME,
                replica.id
            ));
            return unchanged;
        }
        items
            .into_iter()
            .filter_map(|(key, value)| key.map(|key| (key, item_hash(&value))))
            .collect()
    }
}

/// Hashes the compact serialization of an item. `serde_json` keeps the key order of the file, so an item that the
/// app did not change keeps its hash.
fn item_hash(item: &Value) -> ContentHash {
    hash_bytes(&serde_json::to_vec(item).expect("JSON value serializes"))
}

/// Returns the number at `field` as an integer, or `None` when the field is missing or not a finite number.
pub fn integer_field(item: &Value, field: &str) -> Option<i64> {
    item.get(field)?
        .as_f64()
        .filter(|n| n.is_finite())
        .map(|n| n as i64)
}
