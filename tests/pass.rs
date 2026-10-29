//! Runs whole sync passes on temporary copies of the app's folder layout.

use std::fs;
use std::path::{Path, PathBuf};

use claude_account_sync::app::AppStatus;
use claude_account_sync::config::{Config, ConfigKey};
use claude_account_sync::pass::{self, PassOptions, Report};
use claude_account_sync::state::StateDir;
use serde_json::{Value, json};

const OLD: &str = "9b741bb0-edc4-4cd2-a930-ae810e4829e9";
const NEW: &str = "246e370f-67ef-4b6a-9f78-841ee7bfe5a1";
const OLD_ORG: &str = "54724713-a025-458a-8bd1-d65e3fe2679d";
const NEW_ORG: &str = "9ea9a502-7bc3-41bb-a95d-146ce4d1e579";

struct Fixture {
    _root: tempfile::TempDir,
    data: PathBuf,
    state: StateDir,
    config: Config,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("Claude");
        fs::create_dir_all(replica_path(&data, OLD, OLD_ORG)).unwrap();
        fs::create_dir_all(replica_path(&data, NEW, NEW_ORG)).unwrap();
        let state = StateDir {
            root: root.path().join("state"),
        };
        Self {
            _root: root,
            data,
            state,
            config: Config::default(),
        }
    }

    fn old(&self) -> PathBuf {
        replica_path(&self.data, OLD, OLD_ORG)
    }

    fn new_replica(&self) -> PathBuf {
        replica_path(&self.data, NEW, NEW_ORG)
    }

    fn sync(&self, status: AppStatus) -> Report {
        let report = pass::run(
            &self.data,
            &self.state,
            &self.config,
            status,
            PassOptions::default(),
        )
        .unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        report
    }

    fn sync_stopped(&self) -> Report {
        self.sync(AppStatus {
            running: false,
            active_account: Some(NEW.into()),
        })
    }
}

fn replica_path(data: &Path, account: &str, org: &str) -> PathBuf {
    data.join("claude-code-sessions").join(account).join(org)
}

fn record(id: &str, last_activity: i64, archived: bool) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "sessionId": id, "cliSessionId": "cli", "cwd": "F:\\p", "createdAt": 1,
        "lastActivityAt": last_activity, "isArchived": archived, "title": id, "enabledMcpTools": {"": true}
    }))
    .unwrap()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn first_pass_copies_records_and_backs_up_the_original_tree() {
    let fixture = Fixture::new();
    fs::write(
        fixture.old().join("local_a.json"),
        record("local_a", 10, false),
    )
    .unwrap();
    fs::write(
        fixture.old().join("local_b.json"),
        record("local_b", 20, true),
    )
    .unwrap();

    let report = fixture.sync_stopped();

    assert_eq!(report.changes.len(), 2);
    for id in ["local_a", "local_b"] {
        let copied = fs::read(fixture.new_replica().join(format!("{id}.json"))).unwrap();
        assert_eq!(
            copied,
            fs::read(fixture.old().join(format!("{id}.json"))).unwrap()
        );
    }
    assert_eq!(
        read_json(&fixture.new_replica().join("archived-sessions.idx")),
        json!({"v": 1, "archived": ["local_b"]})
    );
    let backup = report.backup_dir.unwrap();
    assert!(
        backup
            .join("initial")
            .join(OLD)
            .join(OLD_ORG)
            .join("local_a.json")
            .is_file()
    );

    assert!(fixture.sync_stopped().changes.is_empty());
}

#[test]
fn deletion_in_one_account_moves_other_copies_to_backup() {
    let fixture = Fixture::new();
    fs::write(
        fixture.old().join("local_a.json"),
        record("local_a", 10, false),
    )
    .unwrap();
    fixture.sync_stopped();

    fs::remove_file(fixture.new_replica().join("local_a.json")).unwrap();
    fs::write(fixture.new_replica().join("deleted_local_a"), "1").unwrap();
    let report = fixture.sync_stopped();

    assert!(!fixture.old().join("local_a.json").exists());
    let backup = report.backup_dir.unwrap();
    assert!(
        backup
            .join(OLD)
            .join(OLD_ORG)
            .join("local_a.json")
            .is_file()
    );
    assert!(!fixture.old().join("deleted_local_a").exists());
}

#[test]
fn edit_propagates_and_running_app_blocks_writes_to_its_account() {
    let fixture = Fixture::new();
    fs::write(
        fixture.old().join("local_a.json"),
        record("local_a", 10, false),
    )
    .unwrap();
    fixture.sync_stopped();

    // The app runs with the new account and archives the session there.
    let running = AppStatus {
        running: true,
        active_account: Some(NEW.into()),
    };
    fs::write(
        fixture.new_replica().join("local_a.json"),
        record("local_a", 30, true),
    )
    .unwrap();
    fixture.sync(running.clone());
    assert_eq!(
        fs::read(fixture.old().join("local_a.json")).unwrap(),
        record("local_a", 30, true)
    );
    assert_eq!(
        read_json(&fixture.old().join("archived-sessions.idx"))["archived"],
        json!(["local_a"])
    );

    // A session that exists only in the inactive account waits until the app stops.
    fs::write(
        fixture.old().join("local_c.json"),
        record("local_c", 5, false),
    )
    .unwrap();
    fixture.sync(running);
    assert!(!fixture.new_replica().join("local_c.json").exists());
    fixture.sync_stopped();
    assert!(fixture.new_replica().join("local_c.json").exists());
}

#[test]
fn invalid_record_is_left_alone() {
    let fixture = Fixture::new();
    fs::write(fixture.old().join("local_a.json"), b"{not json").unwrap();
    fs::write(
        fixture.new_replica().join("local_a.json"),
        record("local_a", 1, false),
    )
    .unwrap();

    let report = fixture.sync_stopped();

    assert!(report.changes.is_empty());
    assert_eq!(
        fs::read(fixture.old().join("local_a.json")).unwrap(),
        b"{not json"
    );
}

#[test]
fn backlog_items_merge_by_id() {
    let fixture = Fixture::new();
    let item = |id: &str, filed: i64| {
        json!({"id": id, "cwd": "F:\\p", "source": "evicted", "title": "t", "prompt": "p",
               "suggestedAt": filed, "filedAt": filed, "originSessionId": "local_a", "originTaskId": "x"})
    };
    let backlog =
        |items: Value| serde_json::to_vec(&json!({"version": 1, "items": items})).unwrap();
    fs::create_dir_all(fixture.old().join("backlog")).unwrap();
    fs::create_dir_all(fixture.new_replica().join("backlog")).unwrap();
    fs::write(
        fixture.old().join("backlog/tasks.json"),
        backlog(json!([item("todo_000000000001", 1)])),
    )
    .unwrap();
    fs::write(
        fixture.new_replica().join("backlog/tasks.json"),
        backlog(json!([item("todo_000000000002", 2)])),
    )
    .unwrap();

    fixture.sync_stopped();
    for replica in [fixture.old(), fixture.new_replica()] {
        let ids: Vec<Value> = read_json(&replica.join("backlog/tasks.json"))["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].clone())
            .collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
    }

    // The user removes an item in one account, and the next pass removes it from the other.
    fs::write(
        fixture.new_replica().join("backlog/tasks.json"),
        backlog(json!([item("todo_000000000002", 2)])),
    )
    .unwrap();
    fixture.sync_stopped();
    assert_eq!(
        read_json(&fixture.old().join("backlog/tasks.json"))["items"],
        json!([item("todo_000000000002", 2)])
    );
}

#[test]
fn scheduled_tasks_keep_each_replica_run_state() {
    let fixture = Fixture::new();
    let task =
        json!({"id": "daily", "enabled": true, "filePath": "C:\\t\\SKILL.md", "createdAt": 1});
    fs::write(
        fixture.old().join("scheduled-tasks.json"),
        serde_json::to_vec_pretty(
            &json!({"scheduledTasks": [task], "recordedSkips": {"daily": []}}),
        )
        .unwrap(),
    )
    .unwrap();
    fs::write(
        fixture.new_replica().join("scheduled-tasks.json"),
        serde_json::to_vec_pretty(
            &json!({"scheduledTasks": [], "sundayAliasBoundaryStamped": true}),
        )
        .unwrap(),
    )
    .unwrap();

    fixture.sync_stopped();

    assert_eq!(
        read_json(&fixture.new_replica().join("scheduled-tasks.json")),
        json!({"scheduledTasks": [task], "sundayAliasBoundaryStamped": true})
    );
}

#[test]
fn dry_run_changes_nothing() {
    let fixture = Fixture::new();
    fs::write(
        fixture.old().join("local_a.json"),
        record("local_a", 10, false),
    )
    .unwrap();
    let status = AppStatus {
        running: false,
        active_account: None,
    };
    let report = pass::run(
        &fixture.data,
        &fixture.state,
        &fixture.config,
        status,
        PassOptions { dry_run: true },
    )
    .unwrap();
    assert_eq!(report.changes.len(), 1);
    assert!(!fixture.new_replica().join("local_a.json").exists());
    assert!(!fixture.state.state_file().exists());
}

#[test]
fn config_excludes_replicas_and_disables_collections() {
    let mut fixture = Fixture::new();
    fs::write(
        fixture.old().join("local_a.json"),
        record("local_a", 10, false),
    )
    .unwrap();
    fs::write(
        fixture.old().join("scheduled-tasks.json"),
        serde_json::to_vec(&json!({"scheduledTasks": [
            {"id": "daily", "enabled": true, "filePath": "f", "createdAt": 1}
        ]}))
        .unwrap(),
    )
    .unwrap();

    fixture.config.excluded.insert(NEW.parse().unwrap());
    let report = fixture.sync_stopped();
    assert!(report.changes.is_empty());
    assert_eq!(report.excluded.len(), 1);

    fixture.config.excluded.clear();
    fixture
        .config
        .set(ConfigKey::ScheduledTasks, "false")
        .unwrap();
    let report = fixture.sync_stopped();
    assert_eq!(report.disabled_collections, ["scheduledTasks"]);
    assert!(fixture.new_replica().join("local_a.json").exists());
    assert!(!fixture.new_replica().join("scheduled-tasks.json").exists());
}
