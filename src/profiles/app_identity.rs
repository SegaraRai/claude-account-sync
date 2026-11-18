//! Reads account names and email addresses from the desktop app's own cache.
//!
//! The app's claude.ai page keeps its query cache in IndexedDB, and that cache holds the signed-in account: its
//! `uuid`, `email_address`, `full_name`, and `display_name`. IndexedDB is a LevelDB database whose newest records
//! sit uncompressed in its `*.log` files, as values in V8's serialization format. This module scans those files
//! for account objects without a full parser: it finds each `uuid` key and reads the string fields that follow it
//! before the next object starts. A record that does not parse is skipped, so a format change only stops updates.
//!
//! Older records move into compressed `*.ldb` tables, which this module does not read. The watcher reads the log
//! after every pass, so it sees each account while that account is signed in.

use std::collections::BTreeMap;
use std::path::Path;

use super::Identity;
use crate::file_ops::read_regular;
use crate::replica::is_uuid;

/// The IndexedDB database of the claude.ai page, relative to the app's data folder.
const DATABASE: &str = "IndexedDB/https_claude.ai_0.indexeddb.leveldb";

/// How far after a `uuid` the fields of the same account object may appear.
const OBJECT_WINDOW: usize = 4096;

/// The key that starts an account object: a one-byte string `uuid`.
const UUID_KEY: &[u8] = b"\x22\x04uuid";

/// Returns the identity of each account that the app's cache holds, keyed by account UUID. When a log holds an
/// account several times, the last record, which is the newest, wins.
pub fn read_app_identities(data_dir: &Path) -> BTreeMap<String, Identity> {
    let mut found = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(data_dir.join(DATABASE)) else {
        return found;
    };
    // LevelDB numbers its logs in order, so sorting the names reads older logs first.
    let mut logs: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "log"))
        .collect();
    logs.sort();
    for path in logs {
        if let Ok(Some(bytes)) = read_regular(&path, 64 << 20) {
            found.extend(scan(&bytes));
        }
    }
    found
}

/// Finds the account objects in `bytes`, in order.
fn scan(bytes: &[u8]) -> Vec<(String, Identity)> {
    let starts: Vec<usize> = find_all(bytes, UUID_KEY).collect();
    let mut accounts = Vec::new();
    for (index, &start) in starts.iter().enumerate() {
        let Some((uuid, after)) = read_string(bytes, start + UUID_KEY.len()) else {
            continue;
        };
        if !is_uuid(&uuid) {
            continue;
        }
        // The object's fields end where the next object with a `uuid` starts.
        let end = starts
            .get(index + 1)
            .copied()
            .unwrap_or(bytes.len())
            .min(after + OBJECT_WINDOW);
        let object = &bytes[after..end];
        let email = field(object, "email_address").filter(|email| email.contains('@'));
        // An organization also has a `uuid`, but no email address.
        let Some(email) = email else {
            continue;
        };
        let name = field(object, "display_name").or_else(|| field(object, "full_name"));
        accounts.push((
            uuid.to_lowercase(),
            Identity {
                display_name: name,
                email: Some(email),
                organization_name: None,
            },
        ));
    }
    accounts
}

/// Reads the string value of the one-byte string key `key` in `object`.
fn field(object: &[u8], key: &str) -> Option<String> {
    let mut needle = vec![0x22, u8::try_from(key.len()).ok()?];
    needle.extend_from_slice(key.as_bytes());
    let at = find_all(object, &needle).next()?;
    let (value, _) = read_string(object, at + needle.len())?;
    let value = value.trim();
    (!value.is_empty() && !value.contains('\0')).then(|| value.to_owned())
}

/// Reads a V8-serialized string at `pos`: `"` with a Latin-1 string, or `c` with a UTF-16LE string, each after a
/// varint byte length. V8 may put padding bytes before a two-byte string to align it. Returns the string and the
/// position after it.
fn read_string(bytes: &[u8], mut pos: usize) -> Option<(String, usize)> {
    while bytes.get(pos) == Some(&0) && pos < bytes.len() {
        pos += 1;
    }
    let tag = *bytes.get(pos)?;
    let (len, start) = read_varint(bytes, pos + 1)?;
    let end = start.checked_add(len)?;
    let data = bytes.get(start..end)?;
    let text = match tag {
        0x22 => data.iter().map(|&byte| char::from(byte)).collect(),
        b'c' if len % 2 == 0 => {
            let units: Vec<u16> = data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            String::from_utf16(&units).ok()?
        }
        b'S' => String::from_utf8(data.to_vec()).ok()?,
        _ => return None,
    };
    Some((text, end))
}

fn read_varint(bytes: &[u8], mut pos: usize) -> Option<(usize, usize)> {
    let mut value: usize = 0;
    for shift in (0..35).step_by(7) {
        let byte = *bytes.get(pos)?;
        pos += 1;
        value |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some((value, pos));
        }
    }
    None
}

fn find_all<'a>(haystack: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    haystack
        .windows(needle.len())
        .enumerate()
        .filter(move |(_, window)| *window == needle)
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_byte(text: &str) -> Vec<u8> {
        let mut out = vec![0x22, u8::try_from(text.len()).unwrap()];
        out.extend_from_slice(text.as_bytes());
        out
    }

    fn two_byte(text: &str) -> Vec<u8> {
        let units: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut out = vec![0x00, b'c', u8::try_from(units.len()).unwrap()];
        out.extend(units);
        out
    }

    fn account(uuid: &str, email: &str, name: Vec<u8>) -> Vec<u8> {
        let mut out = b"o".to_vec();
        out.extend(one_byte("tagged_id"));
        out.extend(one_byte("user_01"));
        out.extend(one_byte("uuid"));
        out.extend(one_byte(uuid));
        out.extend(one_byte("email_address"));
        out.extend(one_byte(email));
        out.extend(one_byte("display_name"));
        out.extend(name);
        out.extend(one_byte("memberships"));
        out.extend(b"Ao");
        out.extend(one_byte("uuid"));
        out.extend(one_byte("9ea9a502-7bc3-41bb-a9a0-000000000000"));
        out.extend(one_byte("name"));
        out.extend(one_byte("Org"));
        out
    }

    const A: &str = "246e370f-67ef-4b6a-9f78-841ee7bfe5a1";
    const B: &str = "9b741bb0-edc4-4cd2-a930-ae810e4829e9";

    #[test]
    fn reads_accounts_and_skips_organizations() {
        let mut bytes = b"junk".to_vec();
        bytes.extend(account(A, "a@example.com", one_byte("Abelia 2")));
        bytes.extend(account(B, "b@example.com", two_byte("アベリア")));
        bytes.extend(account(A, "a2@example.com", one_byte("Abelia Two")));
        let found = scan(&bytes);
        let names: Vec<_> = found
            .iter()
            .map(|(uuid, identity)| {
                (
                    uuid.as_str(),
                    identity.display_name.as_deref().unwrap(),
                    identity.email.as_deref().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                (A, "Abelia 2", "a@example.com"),
                (B, "アベリア", "b@example.com"),
                (A, "Abelia Two", "a2@example.com"),
            ]
        );
    }

    #[test]
    fn reads_the_newest_record_from_the_logs() {
        let data = tempfile::tempdir().unwrap();
        let db = data.path().join(DATABASE);
        std::fs::create_dir_all(&db).unwrap();
        std::fs::write(
            db.join("000003.log"),
            account(A, "old@example.com", one_byte("Old")),
        )
        .unwrap();
        std::fs::write(
            db.join("000005.log"),
            account(A, "new@example.com", one_byte("New")),
        )
        .unwrap();
        let found = read_app_identities(data.path());
        assert_eq!(found[A].display_name.as_deref(), Some("New"));
        assert_eq!(found[A].email.as_deref(), Some("new@example.com"));
    }

    #[test]
    fn truncated_record_is_skipped() {
        let bytes = account(A, "a@example.com", one_byte("Abelia"));
        let cut = &bytes[..bytes.len() - 70];
        assert!(scan(&bytes[..20]).is_empty());
        assert_eq!(scan(cut).len(), 1);
    }
}
