//! Finds the Claude Code sessions of the desktop app that quitting the app would interrupt.
//!
//! Each running Claude Code process registers itself in `sessions/<pid>.json` in the CLI's config folder
//! (`~/.claude`, or `$CLAUDE_CONFIG_DIR`). The file names the session, says whether the process serves the desktop
//! app (`entrypoint` is `claude-desktop`), and says whether a turn runs now (`status` is `busy` or `idle`). The
//! status stays `idle` while background tasks run after a turn, so for idle sessions this module also reads the
//! transcript: each background command, monitor, or agent records its task ID in the tool result that started it,
//! and the CLI records a `<task-notification>` with the same ID when the task ends. A task that started after the
//! process started and has no notification yet still runs.
//!
//! Neither file format is documented. When the registry folder does not exist, which happens with older CLI
//! versions, this module falls back to transcripts that changed in the last [`ACTIVE_SESSION_WINDOW`]. That
//! errs toward asking: an idle session can count as running.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app::claude_config_dir;
use crate::file_ops::unix_millis;

/// How recently a transcript must have changed for the fallback to count its session as running.
pub const ACTIVE_SESSION_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);

/// The `entrypoint` of the CLI processes that the desktop app starts.
const DESKTOP_ENTRYPOINT: &str = "claude-desktop";

/// The largest transcript that is read to find background tasks.
const MAX_TRANSCRIPT_LEN: u64 = 512 << 20;

/// A Claude Code session of the desktop app that quitting the app would interrupt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningSession {
    pub cli_session_id: String,
    /// The session's title from its sidebar record, or the name that the CLI registered.
    pub title: Option<String>,
    /// `true` while a turn runs. Without the registry, `true` for every session found.
    pub busy: bool,
    /// The background commands, monitors, and agents that the session started and that have not ended.
    pub background_tasks: usize,
    /// When the session last changed its state or wrote its transcript, in Unix milliseconds.
    pub last_activity_at: i64,
}

/// One `sessions/<pid>.json` file.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registration {
    pid: u32,
    session_id: String,
    #[serde(default)]
    entrypoint: Option<String>,
    #[serde(default)]
    status: Option<String>,
    /// When the process registered, in Unix milliseconds.
    started_at: i64,
    #[serde(default)]
    status_updated_at: Option<i64>,
    #[serde(default)]
    name: Option<String>,
}

/// Lists the desktop app's Claude Code sessions that run a turn or background tasks now.
pub fn running_sessions(data_dir: &Path) -> Vec<RunningSession> {
    let Some(config_dir) = claude_config_dir() else {
        return Vec::new();
    };
    let registry = config_dir.join("sessions");
    let titles =
        || crate::sessions::titles_by_cli_session(&data_dir.join(crate::app::SESSIONS_DIR));
    let mut sessions = if registry.is_dir() {
        let registrations = live_registrations(&registry);
        if registrations.is_empty() {
            return Vec::new();
        }
        let titles = titles();
        let projects = config_dir.join("projects");
        registrations
            .into_iter()
            .filter_map(|registration| {
                let busy = registration.status.as_deref() != Some("idle");
                let background_tasks = if busy {
                    // Background tasks do not change the answer for a busy session.
                    0
                } else {
                    find_transcript(&projects, &registration.session_id)
                        .map_or(0, |path| unfinished_tasks(&path, registration.started_at))
                };
                (busy || background_tasks > 0).then(|| RunningSession {
                    title: titles
                        .get(&registration.session_id)
                        .cloned()
                        .or(registration.name),
                    cli_session_id: registration.session_id,
                    busy,
                    background_tasks,
                    last_activity_at: registration
                        .status_updated_at
                        .unwrap_or(registration.started_at),
                })
            })
            .collect()
    } else {
        recently_written(&config_dir.join("projects"), &titles())
    };
    sessions.sort_by_key(|session| std::cmp::Reverse(session.last_activity_at));
    sessions.dedup_by(|a, b| a.cli_session_id == b.cli_session_id);
    sessions
}

/// Reads the registrations of the desktop app's CLI processes that still run.
fn live_registrations(registry: &Path) -> Vec<Registration> {
    let registrations: Vec<Registration> = std::fs::read_dir(registry)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            let bytes = crate::file_ops::read_regular(&entry.path(), 1 << 20).ok()??;
            serde_json::from_slice::<Registration>(&bytes).ok()
        })
        .filter(|registration| registration.entrypoint.as_deref() == Some(DESKTOP_ENTRYPOINT))
        .collect();
    if registrations.is_empty() {
        return registrations;
    }
    // Without the process list, keep every registration: asking once too often is better than stopping a session.
    let Some(processes) = process_starts() else {
        return registrations;
    };
    registrations
        .into_iter()
        .filter(|registration| is_live(registration, &processes))
        .collect()
}

/// Returns the start time, in Unix seconds, of each process whose name contains `claude`.
fn process_starts() -> Option<BTreeMap<u32, u64>> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    )
    .ok()?;
    let starts = system
        .processes()
        .iter()
        .filter(|(_, process)| {
            process
                .name()
                .to_string_lossy()
                .to_lowercase()
                .contains("claude")
        })
        .map(|(pid, process)| (pid.as_u32(), process.start_time()))
        .collect();
    Some(starts)
}

/// A registration is live when its process runs and started before it registered. A process that started later
/// reuses the PID of a process that ended without removing its file.
fn is_live(registration: &Registration, processes: &BTreeMap<u32, u64>) -> bool {
    let Some(&started) = processes.get(&registration.pid) else {
        return false;
    };
    let registered = u64::try_from(registration.started_at / 1000).unwrap_or(0);
    // Allow for the rounding of process start times.
    started <= registered + 2
}

/// Finds the transcript `<session>.jsonl` in one of the project folders.
fn find_transcript(projects: &Path, session_id: &str) -> Option<PathBuf> {
    let name = format!("{session_id}.jsonl");
    std::fs::read_dir(projects)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(&name))
        .find(|path| path.is_file())
}

/// Counts the background tasks in the transcript at `path` that started at or after `since` (Unix milliseconds)
/// and have no completion notification.
fn unfinished_tasks(path: &Path, since: i64) -> usize {
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    if file
        .metadata()
        .is_ok_and(|meta| meta.len() > MAX_TRANSCRIPT_LEN)
    {
        return 0;
    }
    let mut started = BTreeSet::new();
    let mut ended = BTreeSet::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            break;
        };
        scan_line(&line, since, &mut started, &mut ended);
    }
    started.difference(&ended).count()
}

/// Records the task IDs that one transcript line starts or ends. Only lines that contain a marker are parsed.
fn scan_line(line: &str, since: i64, started: &mut BTreeSet<String>, ended: &mut BTreeSet<String>) {
    if line.contains("<task-id>") {
        ended.extend(notified_task_ids(line));
    }
    let may_start = line.contains("\"backgroundTaskId\"")
        || line.contains("\"taskId\"")
        || line.contains("\"isAsync\":true");
    if !may_start {
        return;
    }
    let Ok(entry) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let Some(result) = entry
        .get("toolUseResult")
        .filter(|result| result.is_object())
    else {
        return;
    };
    let at = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_millis);
    if at.is_none_or(|at| at < since) {
        return;
    }
    let text = |key: &str| result.get(key).and_then(Value::as_str).map(str::to_owned);
    let id = text("backgroundTaskId")
        .or_else(|| text("taskId"))
        .or_else(|| {
            result
                .get("isAsync")
                .and_then(Value::as_bool)
                .filter(|async_| *async_)
                .and_then(|_| text("agentId"))
        });
    if let Some(id) = id {
        started.insert(id);
    }
}

/// Returns the IDs in the `<task-id>` elements of `text`. A notification can sit in a JSON string, where nothing
/// in an ID needs escaping.
fn notified_task_ids(text: &str) -> Vec<String> {
    text.split("<task-id>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</task-id>"))
        .map(|(id, _)| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .collect()
}

fn parse_millis(text: &str) -> Option<i64> {
    let at =
        time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()?;
    i64::try_from(at.unix_timestamp_nanos() / 1_000_000).ok()
}

/// The fallback without the registry: sessions with a sidebar record whose transcript changed in the last
/// [`ACTIVE_SESSION_WINDOW`].
fn recently_written(projects: &Path, titles: &BTreeMap<String, String>) -> Vec<RunningSession> {
    let now = SystemTime::now();
    let mut sessions = Vec::new();
    for project in std::fs::read_dir(projects).into_iter().flatten().flatten() {
        for entry in std::fs::read_dir(project.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".jsonl") else {
                continue;
            };
            let Some(title) = titles.get(id) else {
                continue;
            };
            let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            let recent = now
                .duration_since(modified)
                .is_ok_and(|age| age <= ACTIVE_SESSION_WINDOW)
                || modified > now;
            if recent {
                sessions.push(RunningSession {
                    cli_session_id: id.to_owned(),
                    title: Some(title.clone()),
                    busy: true,
                    background_tasks: 0,
                    last_activity_at: unix_millis(modified),
                });
            }
        }
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_result(timestamp: &str, result: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{timestamp}","message":{{"role":"user","content":[]}},"toolUseResult":{result}}}"#
        )
    }

    #[test]
    fn counts_tasks_that_started_after_the_process_and_did_not_end() {
        let since = parse_millis("2026-11-19T01:00:00Z").unwrap();
        let lines = [
            // Before the process started: ignored, it died with the previous process.
            tool_result("2026-11-19T00:59:00.000Z", r#"{"backgroundTaskId":"old"}"#),
            tool_result("2026-11-19T01:01:00.000Z", r#"{"stdout":"","backgroundTaskId":"bash1"}"#),
            tool_result("2026-11-19T01:02:00.000Z", r#"{"taskId":"monitor1","timeoutMs":1000,"persistent":false}"#),
            tool_result("2026-11-19T01:03:00.000Z", r#"{"isAsync":true,"status":"async_launched","agentId":"agent1"}"#),
            tool_result("2026-11-19T01:04:00.000Z", r#"{"isAsync":false,"agentId":"sync1"}"#),
            tool_result("2026-11-19T01:05:00.000Z", r#"{"backgroundTaskId":"bash2"}"#),
            // A notification, as the queue records it.
            r#"{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>bash1</task-id>\n<status>completed</status>"}"#.to_owned(),
            r#"{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>agent1</task-id>\n<status>failed</status>"}}"#.to_owned(),
        ];
        let mut started = BTreeSet::new();
        let mut ended = BTreeSet::new();
        for line in &lines {
            scan_line(line, since, &mut started, &mut ended);
        }
        let running: Vec<_> = started.difference(&ended).cloned().collect();
        assert_eq!(running, ["bash2", "monitor1"]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        assert_eq!(unfinished_tasks(&path, since), 2);
    }

    #[test]
    fn reads_registrations_and_checks_the_process() {
        let registration: Registration = serde_json::from_str(
            r#"{"pid":406648,"sessionId":"390909fe-d4c0-4d08-b825-f2be255d29df","entrypoint":"claude-desktop",
                "startedAt":1795053341110,"status":"idle","statusUpdatedAt":1795053576008,"name":"x","kind":"interactive"}"#,
        )
        .unwrap();
        assert_eq!(registration.status.as_deref(), Some("idle"));
        let mut processes = BTreeMap::new();
        assert!(!is_live(&registration, &processes), "no process");
        processes.insert(406648, 1_795_053_340);
        assert!(is_live(&registration, &processes));
        processes.insert(406648, 1_795_060_000);
        assert!(!is_live(&registration, &processes), "PID reused later");
    }

    #[test]
    fn finds_task_ids_in_notifications() {
        assert_eq!(
            notified_task_ids("a<task-id>x1</task-id>b<task-id> y2 </task-id><task-id></task-id>"),
            ["x1", "y2"]
        );
    }
}
