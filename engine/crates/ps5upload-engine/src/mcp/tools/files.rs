//! Files on a console: `files_list`, `files_stat`, `files_mkdir`, `files_delete`, `files_copy`,
//! `files_move`, `files_read`, `files_op_status`, `files_op_cancel`.
//!
//! Copy, move and delete block their route until the work is done, up to an hour on the console.
//! Many clients give up on a tool call after a minute, and the work goes on regardless: an agent
//! would read a failure, try again, and run the copy twice. So the call runs on its own task, the
//! tool waits for it for `wait_s`, and past that answers "still running" with an `op_id` that
//! `files_op_status` follows to the end. [`Ops`] keeps the outcome until it is read.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use axum::http::Method;
use base64::Engine as _;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use super::{
    console_optional, console_required, object, success_json, without_nulls, Args, Kind, ToolCtx,
    ToolDef, ToolOutput, ToolResult,
};

/// Longest `wait_s`.
const MAX_WAIT_S: u64 = 600;
/// Under the 60 s many clients allow a tool call.
const DEFAULT_WAIT_S: u64 = 30;
/// Finished operations remembered for `files_op_status`.
const KEEP_FINISHED: usize = 64;
/// The largest operation id: above this, JavaScript clients cannot hold it exactly.
const MAX_OP_ID: u64 = (1 << 53) - 1;
/// Where the ids this server picks start: far from the engine's and the app's, within `MAX_OP_ID`.
const FIRST_OP_ID: u64 = 1 << 52;

fn path_argument(what: &'static str) -> Value {
    json!({ "type": "string", "description": what })
}

pub(super) fn wait_argument() -> Value {
    json!({
        "type": "integer",
        "minimum": 0,
        "maximum": MAX_WAIT_S,
        "description": "How long to wait for it to finish, in seconds, before answering with its \
                        op_id instead (it keeps running either way). Default 30. Keep it under \
                        your client's limit on a tool call.",
    })
}

pub(super) fn op_id_argument(description: &'static str) -> Value {
    json!({ "type": "integer", "minimum": 1, "maximum": MAX_OP_ID, "description": description })
}

pub(super) const OWN_OP_ID: &str =
    "Your own id for this operation, to follow it with files_op_status. Left \
                         out, one is picked and given in the answer if it runs long.";

const OVERWRITE: &str = "Merge into an existing destination, replacing files that collide. Only \
                         when the user asked for it: left out, an existing destination is refused.";

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "files_list",
            title: "List a console folder",
            description: "A folder's entries on a console, up to 256 at a time: name, kind, size, \
                          modified time. `offset` pages on. `truncated` means the answer was cut \
                          short; otherwise a page shorter than `limit` is the last.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "console": console_optional(),
                    "path": path_argument("The folder, like /data or /mnt/ext0."),
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "How many entries to skip.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 256,
                        "description": "How many entries to return. Default and maximum 256.",
                    },
                }),
                &["path"],
            ),
            run: |ctx, args| Box::pin(list(ctx, args)),
        },
        ToolDef {
            name: "files_stat",
            title: "File details",
            description: "One file or folder's details on a console: kind, size, times, mode.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "console": console_optional(),
                    "path": path_argument("The file or folder."),
                }),
                &["path"],
            ),
            run: |ctx, args| Box::pin(stat(ctx, args)),
        },
        ToolDef {
            name: "files_read",
            title: "Read a console file",
            description: "Reads up to 256 KiB of a console file: as text when it is text, as \
                          base64 otherwise. `offset` reads further on, to page through a longer \
                          file or read a log's tail.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "console": console_optional(),
                    "path": path_argument("The file."),
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Where to start, in bytes. Default 0.",
                    },
                    "max_bytes": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 262144,
                        "description": "How much to read. Default and maximum 262144 (256 KiB).",
                    },
                }),
                &["path"],
            ),
            run: |ctx, args| Box::pin(read(ctx, args)),
        },
        ToolDef {
            name: "files_mkdir",
            title: "Make a console folder",
            description: "Makes a folder on a console.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "path": path_argument("The new folder's full path."),
                }),
                &["console", "path"],
            ),
            run: |ctx, args| Box::pin(mkdir(ctx, args)),
        },
        ToolDef {
            name: "files_delete",
            title: "Delete from a console",
            description: "Deletes a file or a whole folder on a console, for good. Confirm the \
                          exact path and console with the user first. A big folder can take \
                          minutes: past `wait_s` this answers with an op_id.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "path": path_argument("What to delete."),
                    "op_id": op_id_argument(OWN_OP_ID),
                    "wait_s": wait_argument(),
                }),
                &["console", "path"],
            ),
            run: |ctx, args| Box::pin(delete(ctx, args)),
        },
        ToolDef {
            name: "files_copy",
            title: "Copy on a console",
            description: "Copies a file or folder to another place on the same console, across \
                          drives too. It can take long: past `wait_s` this answers with an op_id.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "from": path_argument("What to copy."),
                    "to": path_argument("Where to: the destination's full path."),
                    "overwrite": { "type": "boolean", "description": OVERWRITE },
                    "op_id": op_id_argument(OWN_OP_ID),
                    "wait_s": wait_argument(),
                }),
                &["console", "from", "to"],
            ),
            run: |ctx, args| Box::pin(copy(ctx, args)),
        },
        ToolDef {
            name: "files_move",
            title: "Move on a console",
            description: "Moves or renames a file or folder on a console. Within a drive it is \
                          instant; across drives it copies, then deletes the source after a \
                          verified finish, which can take long: past `wait_s` this answers with \
                          an op_id.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "from": path_argument("What to move."),
                    "to": path_argument("Where to: the destination's full path."),
                    "overwrite": { "type": "boolean", "description": OVERWRITE },
                    "op_id": op_id_argument(OWN_OP_ID),
                    "wait_s": wait_argument(),
                }),
                &["console", "from", "to"],
            ),
            run: |ctx, args| Box::pin(move_to(ctx, args)),
        },
        ToolDef {
            name: "files_op_status",
            title: "Follow a file operation",
            description:
                "How an operation that answered with an op_id stands (a copy, move, delete, \
                          extract, save backup or restore): its progress while it runs, and how \
                          it ended, if it was started through this server.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "console": console_optional(),
                    "op_id": op_id_argument("The operation's id."),
                }),
                &["op_id"],
            ),
            run: |ctx, args| Box::pin(op_status(ctx, args)),
        },
        ToolDef {
            name: "files_op_cancel",
            title: "Cancel a file operation",
            description: "Asks a running copy, move or delete to stop. It stops within moments; \
                          what was already done stays done. `cancelled: false` means it was not \
                          running.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "op_id": op_id_argument("The operation's id."),
                }),
                &["console", "op_id"],
            ),
            run: |ctx, args| Box::pin(op_cancel(ctx, args)),
        },
    ]
}

async fn list(ctx: ToolCtx, args: Args) -> ToolResult {
    let mut query = args.console_query();
    query.push(("path", args.required("path")?.to_string()));
    for name in ["offset", "limit"] {
        if let Some(number) = args.number(name) {
            query.push((name, number.to_string()));
        }
    }
    let answer = ctx.get("/api/ps5/list-dir", &query).await?;
    let count = answer
        .get("entries")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let more = if answer.get("truncated").and_then(Value::as_bool) == Some(true) {
        " The answer was cut short: page on with `offset`."
    } else {
        ""
    };
    Ok(ToolOutput::ok(format!("{count} entries.{more}"), answer))
}

async fn stat(ctx: ToolCtx, args: Args) -> ToolResult {
    let path = args.required("path")?;
    let body = without_nulls(json!({ "addr": args.text("console"), "path": path }));
    let answer = ctx.post("/api/ps5/fs/stat", &body).await?;
    Ok(ToolOutput::ok(format!("Details of {path}."), answer))
}

async fn read(ctx: ToolCtx, args: Args) -> ToolResult {
    let path = args.required("path")?;
    let offset = args.number("offset").unwrap_or(0);
    let body = without_nulls(json!({
        "addr": args.text("console"),
        "path": path,
        "offset": offset,
        "max_bytes": args.number("max_bytes"),
    }));
    let answer = ctx.post("/api/ps5/fs/read-preview", &body).await?;
    let encoded = answer
        .get("base64")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return Err(ToolOutput::error(
            "The engine's answer for that file could not be read.",
        ));
    };
    let read = bytes.len();
    Ok(match text_of(bytes) {
        // `size` is what the text covers, so `offset + size` is where the next read goes, even
        // when this read stopped inside a character.
        Some(text) => ToolOutput::ok(
            format!(
                "{} bytes of {path} from offset {offset}, as text.",
                text.len()
            ),
            json!({
                "path": path,
                "offset": offset,
                "size": text.len(),
                "encoding": "utf-8",
                "text": text,
            }),
        ),
        None => ToolOutput::ok(
            format!("{read} bytes of {path} from offset {offset}, as base64: it is not text."),
            json!({
                "path": path,
                "offset": offset,
                "size": read,
                "encoding": "base64",
                "base64": encoded,
            }),
        ),
    })
}

/// Bytes as text, when that is what they are. A read can stop inside a multi-byte character: that
/// is still text.
fn text_of(bytes: Vec<u8>) -> Option<String> {
    if bytes.contains(&0) {
        return None;
    }
    match String::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(e) => {
            let valid = e.utf8_error().valid_up_to();
            let incomplete_end = e.utf8_error().error_len().is_none();
            let bytes = e.into_bytes();
            incomplete_end
                .then(|| {
                    std::str::from_utf8(&bytes[..valid])
                        .ok()
                        .map(str::to_string)
                })
                .flatten()
        }
    }
}

async fn mkdir(ctx: ToolCtx, args: Args) -> ToolResult {
    let path = args.required("path")?;
    let body = json!({ "addr": args.required("console")?, "path": path });
    let answer = ctx.post("/api/ps5/fs/mkdir", &body).await?;
    Ok(ToolOutput::ok(format!("Made {path}."), answer))
}

async fn delete(ctx: ToolCtx, args: Args) -> ToolResult {
    let path = args.required("path")?.to_string();
    let body = json!({ "addr": args.required("console")?, "path": path });
    let done = format!("Deleted {path}.");
    long_operation(ctx, &args, "/api/ps5/fs/delete", body, done).await
}

async fn copy(ctx: ToolCtx, args: Args) -> ToolResult {
    let (body, from, to) = from_to(&args)?;
    let done = format!("Copied {from} to {to}.");
    long_operation(ctx, &args, "/api/ps5/fs/copy", body, done).await
}

async fn move_to(ctx: ToolCtx, args: Args) -> ToolResult {
    let (body, from, to) = from_to(&args)?;
    let done = format!("Moved {from} to {to}.");
    long_operation(ctx, &args, "/api/ps5/fs/move", body, done).await
}

fn from_to(args: &Args) -> Result<(Value, String, String), ToolOutput> {
    let from = args.required("from")?.to_string();
    let to = args.required("to")?.to_string();
    let body = without_nulls(json!({
        "addr": args.required("console")?,
        "from": from,
        "to": to,
        "overwrite": args.flag("overwrite"),
    }));
    Ok((body, from, to))
}

/// Runs a blocking route on its own task, and waits for it for `wait_s` (see the module comment).
pub(super) async fn long_operation(
    ctx: ToolCtx,
    args: &Args,
    path: &'static str,
    mut body: Value,
    done: String,
) -> ToolResult {
    let wait = Duration::from_secs(
        args.number("wait_s")
            .unwrap_or(DEFAULT_WAIT_S)
            .min(MAX_WAIT_S),
    );
    let op_id = args
        .number("op_id")
        .unwrap_or_else(|| ctx.state.ops().next_id());
    body["op_id"] = json!(op_id);
    ctx.state.ops().started(op_id);

    let (ended, end) = oneshot::channel();
    let worker = ctx.clone();
    tokio::spawn(async move {
        let outcome = worker
            .reply(Method::POST, path, &[], Some(&body))
            .await
            .and_then(|reply| success_json(&format!("POST {path}"), &reply));
        worker.state.ops().finished(op_id, outcome.clone());
        let _ = ended.send(outcome);
    });

    match tokio::time::timeout(wait, end).await {
        Ok(Ok(Ok(answer))) => {
            ctx.state.ops().forget(op_id);
            let mut answer = answer;
            if let Some(fields) = answer.as_object_mut() {
                fields.insert("op_id".to_string(), json!(op_id));
            }
            Ok(ToolOutput::ok(done, answer))
        }
        Ok(Ok(Err(failure))) => {
            ctx.state.ops().forget(op_id);
            Err(failure)
        }
        Ok(Err(_)) => Err(ToolOutput::error(
            "That operation's task ended without an answer. Check the state on the console before \
             trying again.",
        )),
        Err(_) => Ok(ToolOutput::ok(
            format!(
                "Still running after {} s, and it keeps running. Follow it with files_op_status \
                 and op_id {op_id}; do not start it again.",
                wait.as_secs()
            ),
            json!({ "op_id": op_id, "finished": false }),
        )),
    }
}

async fn op_status(ctx: ToolCtx, args: Args) -> ToolResult {
    let Some(op_id) = args.number("op_id") else {
        return Err(ToolOutput::error("Missing required argument `op_id`."));
    };
    let known = ctx.state.ops().state(op_id);
    if let Some(Some(outcome)) = &known {
        return Ok(match outcome {
            Ok(result) => ToolOutput::ok(
                format!("Operation {op_id} finished."),
                json!({ "op_id": op_id, "finished": true, "ok": true, "result": result }),
            ),
            Err(failure) => {
                let result = failure.to_result();
                ToolOutput::ok(
                    format!(
                        "Operation {op_id} finished and failed: {}",
                        result["content"][0]["text"]
                            .as_str()
                            .unwrap_or("no reason given")
                    ),
                    json!({
                        "op_id": op_id,
                        "finished": true,
                        "ok": false,
                        "error": result.get("structuredContent"),
                    }),
                )
            }
        });
    }
    let mut query = args.console_query();
    query.push(("op_id", op_id.to_string()));
    let reply = ctx
        .reply(Method::GET, "/api/ps5/fs/op-status", &query, None)
        .await?;
    if !reply.is_success() && known.is_some() {
        // Started here and not over. The console has nothing to say about it (yet, or at all:
        // not every long route reports progress), or `console` names another one.
        return Ok(ToolOutput::ok(
            format!("Operation {op_id} is still running; no progress to report yet."),
            json!({ "op_id": op_id, "finished": false }),
        ));
    }
    if reply.status.as_u16() == 404 {
        return Err(ToolOutput::error_with(
            format!(
                "Operation {op_id} is not running: it finished before this server started \
                 following it, or never started. Check the result on the console (files_stat)."
            ),
            json!({ "op_id": op_id, "status": 404 }),
        ));
    }
    let mut answer = success_json("GET /api/ps5/fs/op-status", &reply)?;
    if let Some(fields) = answer.as_object_mut() {
        fields.insert("finished".to_string(), json!(false));
    }
    let number = |name: &str| answer.get(name).and_then(Value::as_u64).unwrap_or(0);
    let (copied, total) = (number("bytes_copied"), number("total_bytes"));
    let summary = match copied.saturating_mul(100).checked_div(total) {
        Some(percent) => format!("Operation {op_id} is running: {percent} % done."),
        None => format!("Operation {op_id} is running."),
    };
    Ok(ToolOutput::ok(summary, answer))
}

async fn op_cancel(ctx: ToolCtx, args: Args) -> ToolResult {
    let Some(op_id) = args.number("op_id") else {
        return Err(ToolOutput::error("Missing required argument `op_id`."));
    };
    let body = json!({ "addr": args.required("console")?, "op_id": op_id });
    let answer = ctx.post("/api/ps5/fs/op-cancel", &body).await?;
    let summary = if answer.get("cancelled").and_then(Value::as_bool) == Some(true) {
        format!("Asked operation {op_id} to stop.")
    } else {
        format!("Operation {op_id} is not running.")
    };
    Ok(ToolOutput::ok(summary, answer))
}

/// Operations started through this server that were not over when their tool answered.
#[derive(Default)]
pub(crate) struct Ops {
    next: AtomicU64,
    table: Mutex<OpTable>,
}

#[derive(Default)]
struct OpTable {
    /// `None` while it runs.
    ops: HashMap<u64, Option<Result<Value, ToolOutput>>>,
    /// Finished ones, oldest first.
    finished_order: Vec<u64>,
}

impl Ops {
    fn next_id(&self) -> u64 {
        FIRST_OP_ID + self.next.fetch_add(1, Ordering::Relaxed)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, OpTable> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn started(&self, op_id: u64) {
        let mut table = self.lock();
        table.ops.insert(op_id, None);
        table.finished_order.retain(|id| *id != op_id);
    }

    fn finished(&self, op_id: u64, outcome: Result<Value, ToolOutput>) {
        let mut table = self.lock();
        table.ops.insert(op_id, Some(outcome));
        table.finished_order.push(op_id);
        while table.finished_order.len() > KEEP_FINISHED {
            let oldest = table.finished_order.remove(0);
            table.ops.remove(&oldest);
        }
    }

    /// Its tool gave the outcome in its own answer: nobody will ask.
    fn forget(&self, op_id: u64) {
        let mut table = self.lock();
        table.ops.remove(&op_id);
        table.finished_order.retain(|id| *id != op_id);
    }

    /// `None`: not known here. `Some(None)`: running. `Some(Some(_))`: how it ended.
    fn state(&self, op_id: u64) -> Option<Option<Result<Value, ToolOutput>>> {
        self.lock().ops.get(&op_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_told_from_binary() {
        assert_eq!(text_of(b"hello\n".to_vec()).as_deref(), Some("hello\n"));
        assert_eq!(text_of(vec![0x7f, b'E', b'L', 0, 1]), None);
        assert_eq!(text_of(vec![0xff, 0xfe, b'a']), None);
        // Cut in the middle of a character (the read's limit): still text.
        let mut cut = "ゲーム".as_bytes().to_vec();
        cut.pop();
        assert_eq!(text_of(cut).as_deref(), Some("ゲー"));
        assert_eq!(text_of(Vec::new()).as_deref(), Some(""));
    }

    #[test]
    fn picked_ids_stay_exact_for_javascript_clients() {
        let ops = Ops::default();
        let first = ops.next_id();
        assert_eq!(first, FIRST_OP_ID);
        assert_eq!(ops.next_id(), first + 1);
        assert!(first < MAX_OP_ID);
    }

    #[test]
    fn an_operation_is_remembered_until_it_is_read_or_old() {
        let ops = Ops::default();
        assert!(ops.state(1).is_none());
        ops.started(1);
        assert!(matches!(ops.state(1), Some(None)));
        ops.finished(1, Ok(json!({"ok": true})));
        assert!(matches!(ops.state(1), Some(Some(Ok(_)))));
        ops.forget(1);
        assert!(ops.state(1).is_none());

        ops.started(1000);
        for id in 0..(KEEP_FINISHED as u64 + 3) {
            ops.started(id);
            ops.finished(id, Ok(json!({})));
        }
        // The oldest finished ones went; a running one is never dropped for being old.
        assert!(ops.state(0).is_none());
        assert!(ops.state(2).is_none());
        assert!(ops.state(3).is_some());
        assert!(matches!(ops.state(1000), Some(None)));
        assert_eq!(ops.lock().ops.len(), KEEP_FINISHED + 1);
    }
}
