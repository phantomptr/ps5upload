//! The console's Payload Manager: `payloads_list`, `payloads_launch`, `payloads_store`,
//! `payloads_delete`, `payloads_autoload`.

use serde_json::json;

use super::route::{flag, route, text};
use super::{console_required, object, Kind, ToolDef};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "payloads_list",
            title: "Payloads on a console",
            description: "What a console's Payload Manager holds, and its boot autoload list. \
                          `present: false` means Payload Manager does not answer on that console.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_required() }), &["console"]),
            run: route!(
                path: "/api/ps5/pldmgr",
                console: "host",
                done: "Payload Manager's list.",
            ),
        },
        ToolDef {
            name: "payloads_launch",
            title: "Run a stored payload",
            description: "Runs a payload Payload Manager already holds. A payload is a program \
                          with full access to the console: only one the user asked for.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "path": text("The payload's path, as payloads_list shows it."),
                }),
                &["console", "path"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/pldmgr/launch",
                console: "host",
                done: "Asked Payload Manager to run it.",
            ),
        },
        ToolDef {
            name: "payloads_store",
            title: "Store a payload",
            description: "Stores a file from the computer the engine runs on in a console's \
                          Payload Manager. If its folder already exists, the first call refuses \
                          with `needs_confirm`: storing there removes that folder's other files. \
                          Ask the user, then call again with `confirmed`.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "filename": text("The name Payload Manager files it under."),
                    "local_path": text("The file on the engine's computer: its full path."),
                    "confirmed": flag(
                        "The user accepts that the folder's other files are removed.",
                    ),
                }),
                &["console", "filename", "local_path"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/pldmgr/store",
                console: "host",
                done: "Stored the payload.",
            ),
        },
        ToolDef {
            name: "payloads_delete",
            title: "Delete a stored payload",
            description: "Deletes a payload from a console's Payload Manager.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "filename": text("The payload's file name, as payloads_list shows it."),
                }),
                &["console", "filename"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/pldmgr/delete",
                console: "host",
                done: "Deleted the payload.",
            ),
        },
        ToolDef {
            name: "payloads_autoload",
            title: "Set the boot autoload list",
            description: "Sets which payloads Payload Manager runs when the console starts, in \
                          order. A wrong list can leave the console without what the user relies \
                          on after its next start: only as the user asked.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "enabled": flag("Whether to autoload at all."),
                    "list": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "The payloads to run, in order.",
                    },
                }),
                &["console", "enabled", "list"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/pldmgr/autoload",
                console: "host",
                done: "Set the autoload list.",
            ),
        },
    ]
}
