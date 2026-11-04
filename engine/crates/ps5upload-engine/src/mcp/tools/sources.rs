//! Saved servers (SMB, SFTP, FTP, WebDAV) a file can come from: `sources_list`, `sources_test`,
//! `sources_browse`, `sources_fetch`. Adding one takes a password, which belongs in the app.

use serde_json::json;

use super::route::{route, text};
use super::{object, Kind, ToolDef};

const REMOTE_PATH: &str =
    "The path on a server, as remote://<server id>/<path>; remote://<server id>/ is its start.";

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "sources_list",
            title: "Saved servers",
            description: "The servers saved in this engine, with their ids. Never their \
                          passwords: `has_secret` says whether one is saved. The user adds a \
                          server in the app.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: route!(path: "/api/remote/connections", done: "The saved servers."),
        },
        ToolDef {
            name: "sources_test",
            title: "Test a saved server",
            description: "Tries to connect to a saved server, and says how that went.",
            kind: Kind::Read,
            schema: object(
                json!({ "id": text("The server's id, from sources_list.") }),
                &["id"],
            ),
            run: route!(
                method: "POST",
                path: "/api/remote/connections/{id}/test",
                done: "Tried the server.",
            ),
        },
        ToolDef {
            name: "sources_browse",
            title: "List a server's folder",
            description: "One page of a folder on a saved server. `cursor` from an answer asks \
                          for the next page.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "path": text(REMOTE_PATH),
                    "cursor": text("Where to go on from, from the page before."),
                }),
                &["path"],
            ),
            run: route!(method: "POST", path: "/api/remote/list", done: "The folder's entries."),
        },
        ToolDef {
            name: "sources_fetch",
            title: "Fetch from a server",
            description: "Copies a file from a saved server to the computer the engine runs on. \
                          Answers with a job_id: jobs_wait follows it.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "path": text(REMOTE_PATH),
                    "dest_dir": text(
                        "The folder on the engine's computer. Left out, a temporary one.",
                    ),
                }),
                &["path"],
            ),
            run: route!(method: "POST", path: "/api/remote/fetch", done: "The fetch started."),
        },
    ]
}
