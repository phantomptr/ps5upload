//! Moving files between this computer and a console: `upload_file`, `upload_dir`,
//! `upload_archive`, `files_download`. Each starts a job and answers at once: `jobs_wait` follows
//! it, and `jobs_cancel` stops it.

use serde_json::{json, Value};

use super::route::{flag, route, text};
use super::{
    console_required, object, without_nulls, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult,
};

const OVERWRITES: Kind = Kind::Destructive { idempotent: false };

fn bandwidth() -> Value {
    json!({
        "type": "number",
        "minimum": 0,
        "description": "Limit this upload to so many MB/s. Left out or 0, no limit of its own.",
    })
}

fn excludes() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": "Names or patterns to leave out.",
    })
}

fn tx_id() -> Value {
    text(
        "The id of an earlier transfer of the same thing, to resume it. Left out, this is a new \
         transfer.",
    )
}

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "upload_file",
            title: "Upload a file to a console",
            description: "Sends one file from the computer the engine runs on to a console. It \
                          replaces a file already at `dest`, so check first and confirm with the \
                          user if one may be there. Answers with a job_id: jobs_wait follows it.",
            kind: OVERWRITES,
            schema: object(
                json!({
                    "console": console_required(),
                    "src": text("The file on the engine's computer: its full path."),
                    "dest": text("The file's full path on the console, name included."),
                    "bandwidth_cap_mbps": bandwidth(),
                    "tx_id": tx_id(),
                }),
                &["console", "src", "dest"],
            ),
            run: route!(method: "POST", path: "/api/transfer/file", done: "Upload started."),
        },
        ToolDef {
            name: "upload_dir",
            title: "Upload a folder to a console",
            description: "Sends a folder from the computer the engine runs on to a console. The \
                          folder's contents land directly in `dest_root`, replacing files with \
                          the same names. Answers with a job_id: jobs_wait follows it.",
            kind: OVERWRITES,
            schema: object(
                json!({
                    "console": console_required(),
                    "src_dir": text("The folder on the engine's computer: its full path."),
                    "dest_root": text(
                        "The final folder on the console, like /data/homebrew/PPSA01234.",
                    ),
                    "excludes": excludes(),
                    "skip_existing": {
                        "type": "string",
                        "enum": ["fast", "safe"],
                        "description": "Skip files the console already has, to resume: `fast` \
                                        compares size and time, `safe` the contents.",
                    },
                    "bandwidth_cap_mbps": bandwidth(),
                    "tx_id": tx_id(),
                }),
                &["console", "src_dir", "dest_root"],
            ),
            run: route!(method: "POST", path: "/api/transfer/dir", done: "Upload started."),
        },
        ToolDef {
            name: "upload_archive",
            title: "Upload an archive's contents",
            description: "Unpacks a zip, 7z, rar or tar on the computer the engine runs on and \
                          sends its contents to a console, so they land already extracted in \
                          `dest_root`, replacing files with the same names. `tar` covers .tar, \
                          .tar.zst/.tzst, .tar.gz/.tgz, .tar.xz/.txz, .tar.bz2/.tbz2 and a lone \
                          .zst (one file, named without .zst). Rar works in the desktop app's \
                          engine only. Answers with a job_id: jobs_wait follows it.",
            kind: OVERWRITES,
            schema: object(
                json!({
                    "console": console_required(),
                    "archive": text("The archive on the engine's computer: its full path."),
                    "format": {
                        "type": "string",
                        "enum": ["zip", "7z", "rar", "tar"],
                        "description": "The archive's format.",
                    },
                    "dest_root": text("The final folder on the console."),
                    "password": text("The password of an encrypted rar."),
                    "excludes": excludes(),
                    "bandwidth_cap_mbps": bandwidth(),
                    "tx_id": tx_id(),
                }),
                &["console", "archive", "format", "dest_root"],
            ),
            run: |ctx, args| Box::pin(archive(ctx, args)),
        },
        ToolDef {
            name: "files_download",
            title: "Download from a console",
            description: "Copies a file or folder from a console to the computer the engine runs \
                          on, inside `dest_dir`. Answers with a job_id: jobs_wait follows it.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "src_path": text("The file or folder on the console."),
                    "dest_dir": text(
                        "The folder on the engine's computer it lands inside, under its own name.",
                    ),
                    "kind": {
                        "type": "string",
                        "enum": ["file", "folder"],
                        "description": "Whether `src_path` is a file or a folder (files_stat says).",
                    },
                    "unsafe_read": flag(
                        "Also allow system folders like /system. Only when the user asked for it.",
                    ),
                }),
                &["console", "src_path", "dest_dir", "kind"],
            ),
            run: route!(method: "POST", path: "/api/transfer/download", done: "Download started."),
        },
    ]
}

/// The route for each format, and its name for the archive's path.
fn archive_route(format: &str) -> Option<(&'static str, &'static str)> {
    match format {
        "zip" => Some(("/api/transfer/zip", "zip_path")),
        "7z" => Some(("/api/transfer/7z", "archive_path")),
        "rar" => Some(("/api/transfer/rar", "archive_path")),
        "tar" => Some(("/api/transfer/tar", "archive_path")),
        _ => None,
    }
}

async fn archive(ctx: ToolCtx, args: Args) -> ToolResult {
    let format = args.required("format")?;
    let Some((path, field)) = archive_route(format) else {
        return Err(ToolOutput::error(format!(
            "`format` must be zip, 7z, rar or tar, not {format}."
        )));
    };
    let mut body = without_nulls(json!({
        "addr": args.required("console")?,
        "dest_root": args.required("dest_root")?,
        "excludes": args.value("excludes"),
        "bandwidth_cap_mbps": args.value("bandwidth_cap_mbps"),
        "tx_id": args.text("tx_id"),
        "password": args.value("password"),
    }));
    body[field] = json!(args.required("archive")?);
    let answer = ctx.post(path, &body).await?;
    let job = answer
        .get("job_id")
        .and_then(Value::as_str)
        .unwrap_or("(no id given)");
    Ok(ToolOutput::ok(
        format!("Upload started. Job {job}: jobs_wait follows it."),
        answer,
    ))
}
