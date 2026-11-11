//! The engine's queue for a console: `queue_list`, `queue_add`, `queue_pause`, `queue_resume`,
//! `queue_remove`, `queue_move`, `queue_wait`. Every client of this engine sees the same queue.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::time::Instant;

use super::route::{flag, route, text, whole};
use super::{console_required, object, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult};
use crate::mcp::encode_component;

/// How often `queue_wait` looks.
const POLL_EVERY: Duration = Duration::from_secs(2);
const MAX_WAIT_S: u64 = 600;
/// A client with a 60 s limit on a tool call must get an answer from a call with no `timeout_s`.
const DEFAULT_WAIT_S: u64 = 30;

fn item_id() -> Value {
    text("The item's id, from queue_add or queue_list.")
}

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "queue_list",
            title: "See the queue",
            description: "The engine's queue: `items` in order with each one's status (queued, \
                          running, paused, done, failed, cancelled, interrupted), and for each \
                          console whether it runs or what it waits for (`waiting_for`: hold, \
                          other_work, console, settle). Leave `console` out for all of them.",
            kind: Kind::Read,
            schema: object(
                json!({ "console": text("Only this console's queue.") }),
                &[],
            ),
            run: route!(path: "/api/queue", done: "The queue."),
        },
        ToolDef {
            name: "queue_add",
            title: "Add to the queue",
            description: "Adds an upload, download or install to a console's queue. It runs in \
                          its turn, one at a time, after anything else using that console ends. \
                          `kind` picks the route and `request` is that route's body without the \
                          console: upload_file {src, dest}, upload_dir {src_dir, dest_root}, \
                          upload_zip {zip_path, dest_root}, upload_7z, upload_rar and upload_tar \
                          {archive_path, dest_root}, upload_file_list {files, dest_root}, \
                          link_download {url, dest_dir}, download {src_path, dest_dir, kind}, \
                          install {source: {console_path | host_file | url | remote}}. An upload \
                          replaces files at its destination, so confirm first if some may be \
                          there. A failed item is not retried by itself.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "kind": {
                        "type": "string",
                        "enum": [
                            "upload_file", "upload_dir", "upload_zip", "upload_7z", "upload_rar",
                            "upload_tar", "upload_file_list", "link_download", "download", "install",
                            "image_build", "package_build", "folder_build",
                        ],
                        "description": "What to run.",
                    },
                    "request": {
                        "type": "object",
                        "description": "The route's fields, without the console.",
                    },
                    "label": text("A name to show for it. Left out, the file's name."),
                    "position": whole(
                        "Its place among the console's waiting items, from 0. Left out, the end.",
                    ),
                    "stop_on_failure": flag(
                        "If it fails, pause the items after it, for an install that needs this \
                         upload, say.",
                    ),
                }),
                &["console", "kind", "request"],
            ),
            run: route!(method: "POST", path: "/api/queue", done: "Added to the queue."),
        },
        ToolDef {
            name: "queue_pause",
            title: "Pause a queue item",
            description: "A waiting item is skipped until resumed. A running upload stops, keeps \
                          what landed and says paused a moment later. A running install or \
                          download cannot pause.",
            kind: Kind::Write { idempotent: true },
            schema: object(json!({ "id": item_id() }), &["id"]),
            run: route!(
                method: "POST",
                path: "/api/queue/{id}/pause",
                done: "Paused.",
            ),
        },
        ToolDef {
            name: "queue_resume",
            title: "Resume a queue item",
            description: "Puts a paused, failed, cancelled or interrupted item back in line: an \
                          upload picks up what already landed, an install runs again from the \
                          start. After `interrupted` on an install, check the console first: it \
                          may have gone on by itself.",
            kind: Kind::Write { idempotent: false },
            schema: object(json!({ "id": item_id() }), &["id"]),
            run: route!(
                method: "POST",
                path: "/api/queue/{id}/resume",
                done: "Back in line.",
            ),
        },
        ToolDef {
            name: "queue_remove",
            title: "Remove a queue item",
            description: "Removes an item that is not running. A running upload or download \
                          needs `cancel: true`, which stops it; what landed stays on the console. \
                          A running install cannot be removed.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "id": item_id(),
                    "cancel": flag("Also stop it if it is running."),
                }),
                &["id"],
            ),
            run: route!(
                method: "POST",
                path: "/api/queue/{id}/remove",
                done: "Removed.",
            ),
        },
        ToolDef {
            name: "queue_move",
            title: "Move a queue item",
            description: "Gives a waiting item a new place among its console's waiting items.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "id": item_id(),
                    "position": whole("The new place, from 0. Past the end means the end."),
                }),
                &["id", "position"],
            ),
            run: route!(
                method: "POST",
                path: "/api/queue/{id}/move",
                done: "Moved.",
            ),
        },
        ToolDef {
            name: "queue_wait",
            title: "Wait for a queue item",
            description: "Waits until an item has ended or paused, or for `timeout_s`, and \
                          answers with how it stands. `finished: false` means it still waits or \
                          runs: call again.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "id": item_id(),
                    "timeout_s": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_WAIT_S,
                        "description": "Longest to wait, in seconds. Default 30. Keep it under \
                                        your client's limit on a tool call.",
                    },
                }),
                &["id"],
            ),
            run: |ctx, args| Box::pin(wait(ctx, args)),
        },
    ]
}

/// The item will not move without someone asking.
fn is_settled(item: &Value) -> bool {
    !matches!(
        item.get("status").and_then(Value::as_str),
        Some("queued" | "running")
    )
}

fn describe(id: &str, item: &Value) -> String {
    let field = |name: &str| item.get(name).and_then(Value::as_str);
    let mut line = format!("Item {id}: {}.", field("status").unwrap_or("unknown"));
    if let Some(error) = field("error") {
        line.push(' ');
        line.push_str(error);
    }
    line
}

async fn wait(ctx: ToolCtx, args: Args) -> ToolResult {
    let id = args.required("id")?;
    let path = format!("/api/queue/{}", encode_component(id));
    let limit = Duration::from_secs(
        args.number("timeout_s")
            .unwrap_or(DEFAULT_WAIT_S)
            .min(MAX_WAIT_S),
    );
    let started = Instant::now();
    loop {
        let item = ctx.get(&path, &[]).await?;
        let waited = started.elapsed();
        let finished = is_settled(&item);
        if finished || waited >= limit {
            return Ok(ToolOutput::ok(
                describe(id, &item),
                json!({
                    "id": id,
                    "finished": finished,
                    "waited_s": waited.as_secs(),
                    "item": item,
                }),
            ));
        }
        tokio::time::sleep(POLL_EVERY.min(limit - waited)).await;
    }
}
