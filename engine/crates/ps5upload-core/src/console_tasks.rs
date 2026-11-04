//! Tasks started on the console page (PS5Upload Minion), for the apps connected to that console
//! (#658). They run in the helper and in its SMB daemon, so every app can read them; the helper
//! keeps their names and how they ended (`payload/src/console_tasks.c`). A job an app started
//! itself is never in this list, so it cannot show twice.
//!
//! An older helper answers `ERR_UNKNOWN_METHOD`: that is "no such list", never an error.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::console_files::is_unsupported;
use crate::mgmt::{self, m};

/// This read runs with the status poll: it must give up well before the next one.
const LIST_TIMEOUT: Duration = Duration::from_secs(8);
/// A finished network-share job is listed this long, as the helper does with its own
/// (`CTASKS_KEEP_MS`).
const SMB_KEEP_S: i64 = 5 * 60;
/// What an id of the SMB daemon's job starts with, here and in cancel.
const SMB_PREFIX: &str = "smb:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleTask {
    /// Hex job id, or `smb:` and the daemon's id: what cancel takes.
    pub id: String,
    /// `copy`, `move`, `delete`, `chmod`, `verify`, `unpack`, `extract`, `pkg_install`, `other`.
    pub kind: String,
    /// `running`, `done`, `failed`, `cancelled`, or `gone` (it ended and nobody saw how).
    pub state: String,
    /// The file or folder it works on; may be empty.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub files_done: u64,
    #[serde(default)]
    pub files_total: u64,
    #[serde(default)]
    pub bytes_done: u64,
    #[serde(default)]
    pub bytes_total: u64,
    /// Seconds since it ended; 0 while it runs.
    #[serde(default)]
    pub age_s: u64,
    /// Whether cancel can stop it.
    #[serde(default)]
    pub cancellable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConsoleTasks {
    /// False for a helper older than this feature.
    pub supported: bool,
    pub tasks: Vec<ConsoleTask>,
}

fn last_name(path: &str) -> String {
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

fn u64_of(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// One SMB daemon job (`payload/smb/smbd_jobs.c`): a copy or a removal the page started. Library
/// scans are the page's Library, not tasks. `None` also for a finished one past [`SMB_KEEP_S`].
fn smb_task(j: &Value, now: i64) -> Option<ConsoleTask> {
    let id = str_of(j, "job");
    if id.is_empty() {
        return None;
    }
    let daemon_kind = str_of(j, "kind");
    let kind = match daemon_kind {
        "download" | "upload" => "copy",
        "remove" => "delete",
        _ => return None,
    };
    let state = match str_of(j, "state") {
        "running" => "running",
        "done" => "done",
        "cancelled" => "cancelled",
        _ => "failed",
    };
    let running = state == "running";
    // "ended" is newer than the daemon's "started": without it a long copy reads as old the
    // moment it ends.
    let ended = j
        .get("ended")
        .and_then(Value::as_i64)
        .filter(|e| *e > 0)
        .or_else(|| j.get("started").and_then(Value::as_i64))
        .unwrap_or(now);
    let age = (now - ended).max(0);
    if !running && age > SMB_KEEP_S {
        return None;
    }
    Some(ConsoleTask {
        id: format!("{SMB_PREFIX}{id}"),
        kind: kind.to_string(),
        state: state.to_string(),
        name: last_name(str_of(
            j,
            if daemon_kind == "download" {
                "dest"
            } else {
                "src"
            },
        )),
        files_done: u64_of(j, "files_done"),
        files_total: u64_of(j, "files_total"),
        bytes_done: u64_of(j, "bytes_done"),
        bytes_total: u64_of(j, "bytes_total"),
        age_s: if running { 0 } else { age as u64 },
        cancellable: running,
    })
}

/// The helper's reply as one list. Anything it does not recognise is left out, not an error: this
/// runs on every poll, and one odd entry must not hide the others.
pub fn parse_reply(reply: &Value) -> Vec<ConsoleTask> {
    let mut out = Vec::new();
    if let Some(tasks) = reply.get("tasks").and_then(Value::as_array) {
        for t in tasks {
            if let Ok(mut task) = serde_json::from_value::<ConsoleTask>(t.clone()) {
                // A Sony install cannot be stopped once it started; the page's own Cancel covers
                // the copy in front of it.
                task.cancellable = task.state == "running" && task.kind != "pkg_install";
                out.push(task);
            }
        }
    }
    let now = reply
        .get("now")
        .and_then(Value::as_i64)
        .unwrap_or_else(unix_now);
    if let Some(jobs) = reply.get("smb_jobs").and_then(Value::as_array) {
        out.extend(jobs.iter().filter_map(|j| smb_task(j, now)));
    }
    out
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// What the console page has running and what recently ended there.
pub fn list(addr: &str) -> Result<ConsoleTasks> {
    let bytes = match mgmt::call_with(
        addr,
        m::CONSOLE_TASKS,
        m::CONSOLE_TASKS.label,
        b"{}",
        Some(LIST_TIMEOUT),
    ) {
        Ok(b) => b,
        Err(e) if is_unsupported(&e) => {
            return Ok(ConsoleTasks {
                supported: false,
                tasks: Vec::new(),
            })
        }
        Err(e) => return Err(e),
    };
    let reply: Value = serde_json::from_slice(&bytes).context("CONSOLE_TASKS reply")?;
    Ok(ConsoleTasks {
        supported: true,
        tasks: parse_reply(&reply),
    })
}

/// Stops one task of the list. A refusal is an error with the helper's token (`bad_job`,
/// `unknown_job`, `failed`); an older helper answers `ERR_UNKNOWN_METHOD`.
pub fn cancel(addr: &str, id: &str) -> Result<()> {
    mgmt::call(
        addr,
        m::CONSOLE_TASK_CANCEL,
        &serde_json::to_vec(&serde_json::json!({ "id": id }))?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn method_numbers_are_the_schemas() {
        assert_eq!(m::CONSOLE_TASKS.id, 195);
        assert_eq!(m::CONSOLE_TASK_CANCEL.id, 196);
    }

    /// The exact keys payload/src/console_tasks.c prints (selftest there); a spelling drift here
    /// would read as zeros, silently.
    #[test]
    fn reads_the_helpers_keys() {
        let v: Value = serde_json::from_str(
            r#"{"ok":true,"now":1000,"tasks":[{"id":"aa","kind":"copy","state":"running","name":"a.iso",
            "files_done":1,"files_total":2,"bytes_done":50,"bytes_total":100,"age_s":0}],"smb_jobs":[]}"#,
        )
        .unwrap();
        let t = parse_reply(&v);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].id, "aa");
        assert_eq!(t[0].name, "a.iso");
        assert_eq!(t[0].files_done, 1);
        assert_eq!(t[0].files_total, 2);
        assert_eq!(t[0].bytes_done, 50);
        assert_eq!(t[0].bytes_total, 100);
        assert!(t[0].cancellable);
    }

    #[test]
    fn only_a_running_task_can_be_cancelled_and_not_an_install() {
        let v = json!({"now":1,"tasks":[
            {"id":"a","kind":"copy","state":"done","age_s":3},
            {"id":"b","kind":"pkg_install","state":"running"},
            {"id":"c","kind":"delete","state":"running"},
        ]});
        let t = parse_reply(&v);
        assert!(!t[0].cancellable);
        assert_eq!(t[0].age_s, 3);
        assert!(!t[1].cancellable);
        assert!(t[2].cancellable);
    }

    #[test]
    fn share_jobs_are_named_and_scans_left_out() {
        let v = json!({"now":1000,"tasks":[],"smb_jobs":[
            {"job":"d1","kind":"download","state":"running","src":"Share/Games/x.iso","dest":"/data/y/x.iso",
             "bytes_done":5,"bytes_total":10,"files_done":0,"files_total":1,"started":900,"ended":0},
            {"job":"u1","kind":"upload","state":"cancelled","src":"/data/up/","dest":"S/up","started":500,"ended":990},
            {"job":"r1","kind":"remove","state":"failed","src":"S/old","dest":"","started":990,"ended":995},
            {"job":"s1","kind":"scan","state":"running","src":"S","dest":""},
        ]});
        let t = parse_reply(&v);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].id, "smb:d1");
        assert_eq!((t[0].kind.as_str(), t[0].name.as_str()), ("copy", "x.iso"));
        assert!(t[0].cancellable);
        assert_eq!(t[0].age_s, 0);
        assert_eq!(t[1].state, "cancelled");
        assert_eq!(t[1].name, "up");
        assert_eq!(t[1].age_s, 10);
        assert!(!t[1].cancellable);
        assert_eq!(
            (t[2].kind.as_str(), t[2].state.as_str()),
            ("delete", "failed")
        );
    }

    #[test]
    fn a_long_share_copy_that_just_ended_is_not_old_but_an_old_one_is_dropped() {
        let v = json!({"now":100_000,"smb_jobs":[
            {"job":"long","kind":"download","state":"done","src":"s","dest":"/d","started":1000,"ended":99_990},
            {"job":"old","kind":"download","state":"done","src":"s","dest":"/d","started":1000,"ended":1200},
            // a daemon from before "ended": judged by when it started
            {"job":"nostamp","kind":"download","state":"done","src":"s","dest":"/d","started":99_900},
        ]});
        let t = parse_reply(&v);
        let ids: Vec<&str> = t.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["smb:long", "smb:nostamp"]);
    }

    #[test]
    fn odd_entries_and_missing_lists_are_left_out() {
        assert!(parse_reply(&json!({"ok":true})).is_empty());
        let t = parse_reply(
            &json!({"tasks":[{"nope":1},{"id":"a","kind":"copy","state":"running"}],"smb_jobs":[{"kind":"download"}]}),
        );
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn the_reply_to_the_app_is_snake_case() {
        let s = serde_json::to_value(ConsoleTasks {
            supported: false,
            tasks: vec![],
        })
        .unwrap();
        assert_eq!(s, json!({"supported":false,"tasks":[]}));
    }
}
