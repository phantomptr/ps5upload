//! The rest of a console's files: `files_volumes`, `files_write`, `files_chmod`, `files_hash`,
//! `files_extract`, `files_mount`, `files_unmount`, `files_eject`.

use base64::Engine as _;
use serde_json::json;

use super::files::{op_id_argument, wait_argument, OWN_OP_ID};
use super::route::{flag, route, text};
use super::{
    console_optional, console_required, object, Args, Kind, ToolCtx, ToolDef, ToolOutput,
    ToolResult,
};

/// What the route takes.
const MAX_WRITE_BYTES: usize = 256 * 1024;

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "files_volumes",
            title: "Console drives",
            description: "A console's drives and mounts: each `path`, `fs_type`, `free_bytes`, \
                          `total_bytes` and whether it is `writable`.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: route!(path: "/api/ps5/volumes", done: "The console's drives."),
        },
        ToolDef {
            name: "files_write",
            title: "Write a small console file",
            description: "Writes a small file on a console, at most 256 KiB, in one step: from \
                          `text`, or from `base64` for bytes. It replaces a file already there \
                          unless `create_only` is set. For anything larger use upload_file.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "path": text("The file's full path on the console."),
                    "text": text("The contents, as text. Give this or `base64`."),
                    "base64": text("The contents, as base64. Give this or `text`."),
                    "create_only": flag("Refuse if the file is already there."),
                }),
                &["console", "path"],
            ),
            run: |ctx, args| Box::pin(write(ctx, args)),
        },
        ToolDef {
            name: "files_chmod",
            title: "Change file permissions",
            description: "Changes a file or folder's permissions on a console.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "path": text("The file or folder."),
                    "mode": text("The mode in octal, like 0755."),
                    "recursive": flag("Also everything inside a folder."),
                }),
                &["console", "path", "mode"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/fs/chmod",
                done: "Changed the permissions.",
            ),
        },
        ToolDef {
            name: "files_hash",
            title: "Hash a console file",
            description: "A file's checksum, worked out on the console: BLAKE3 by default, or \
                          CRC32. A big file takes a while.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "console": console_required(),
                    "path": text("The file on the console."),
                    "algo": {
                        "type": "string",
                        "enum": ["blake3", "crc32"],
                        "description": "Which checksum. Default blake3.",
                    },
                }),
                &["console", "path"],
            ),
            run: route!(method: "POST", path: "/api/ps5/fs/hash", done: "The file's checksum."),
        },
        ToolDef {
            name: "files_extract",
            title: "Extract an archive on a console",
            description:
                "Unpacks an archive already on a console into a folder on it. It can take \
                          long: past `wait_s` this answers with an op_id for files_op_status.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "path": text("The archive on the console."),
                    "dest": text("The folder to unpack into."),
                    "password": text("The password of an encrypted archive."),
                    "merge": flag(
                        "Unpack into an existing folder. Left out, an existing folder is refused.",
                    ),
                    "replace": flag(
                        "Replace files that collide; otherwise they are kept and counted in \
                         `skipped`. Only when the user asked for it.",
                    ),
                    "op_id": op_id_argument(OWN_OP_ID),
                    "wait_s": wait_argument(),
                }),
                &["console", "path", "dest"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/fs/extract",
                long: true,
                done: "Extracted the archive.",
            ),
        },
        ToolDef {
            name: "files_mount",
            title: "Mount a disk image",
            description:
                "Mounts a disk image file, like an .exfat, on a console, so its files can \
                          be used. Writable unless `read_only` is set.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "image_path": text("The image file on the console."),
                    "mount_name": text(
                        "A name for the mount, under /mnt/ps5upload. Left out, one is made from \
                         the file.",
                    ),
                    "mount_point": text(
                        "The exact path to mount at, instead of a name under /mnt/ps5upload.",
                    ),
                    "read_only": flag("Mount it read-only."),
                }),
                &["console", "image_path"],
            ),
            run: route!(method: "POST", path: "/api/ps5/fs/mount", done: "Mounted the image."),
        },
        ToolDef {
            name: "files_unmount",
            title: "Unmount a disk image",
            description: "Unmounts a disk image from a console. A game running from it stops \
                          working, so check with processes_list first.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "mount_point": text("The mount's `path` from files_volumes."),
                }),
                &["console", "mount_point"],
            ),
            run: route!(method: "POST", path: "/api/ps5/fs/unmount", done: "Unmounted it."),
        },
        ToolDef {
            name: "files_eject",
            title: "Eject a USB drive",
            description: "Unmounts a USB drive on a console so it can be pulled. `usb_eject_busy` \
                          means something on the console holds it open, which `force` overrides.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "mount_point": text("The drive's `path` from files_volumes, like /mnt/usb0."),
                    "force": flag(
                        "Eject even while it is in use. It can lose data: only when the user \
                         asked for it.",
                    ),
                }),
                &["console", "mount_point"],
            ),
            run: route!(method: "POST", path: "/api/ps5/usb/eject", done: "Ejected the drive."),
        },
    ]
}

/// The file's contents as the route wants them.
fn contents_base64(text: Option<&str>, base64: Option<&str>) -> Result<String, String> {
    let engine = base64::engine::general_purpose::STANDARD;
    let bytes = match (text, base64) {
        (Some(_), Some(_)) => return Err("Give `text` or `base64`, not both.".to_string()),
        (None, None) => return Err("Give the contents, as `text` or `base64`.".to_string()),
        (Some(text), None) => text.as_bytes().to_vec(),
        (None, Some(encoded)) => engine
            .decode(encoded)
            .map_err(|e| format!("`base64` is not valid base64: {e}"))?,
    };
    if bytes.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "That is {} bytes, over the {MAX_WRITE_BYTES} this can write: use upload_file.",
            bytes.len()
        ));
    }
    Ok(engine.encode(bytes))
}

async fn write(ctx: ToolCtx, args: Args) -> ToolResult {
    let path = args.required("path")?;
    let raw_text = args.value("text").and_then(|value| value.as_str());
    let bytes_b64 = contents_base64(raw_text, args.text("base64")).map_err(ToolOutput::error)?;
    let body = json!({
        "addr": args.required("console")?,
        "path": path,
        "bytes_b64": bytes_b64,
        "create_only": args.flag("create_only").unwrap_or(false),
    });
    let answer = ctx.post("/api/ps5/fs/write-bytes", &body).await?;
    Ok(ToolOutput::ok(format!("Wrote {path}."), answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_files_contents_come_one_way_and_within_the_limit() {
        assert_eq!(contents_base64(Some("hello"), None).unwrap(), "aGVsbG8=");
        assert_eq!(contents_base64(None, Some("aGVsbG8=")).unwrap(), "aGVsbG8=");
        // An empty file is a file.
        assert_eq!(contents_base64(Some(""), None).unwrap(), "");
        assert!(contents_base64(None, None).is_err());
        assert!(contents_base64(Some("a"), Some("YQ==")).is_err());
        assert!(contents_base64(None, Some("***")).is_err());
        let large = "a".repeat(MAX_WRITE_BYTES + 1);
        assert!(contents_base64(Some(&large), None)
            .unwrap_err()
            .contains("upload_file"));
    }
}
