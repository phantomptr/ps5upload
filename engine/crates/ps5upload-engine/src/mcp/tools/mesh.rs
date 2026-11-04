//! Other devices on a console, and the drives shared through it: `devices_list`, `drive_list`,
//! `drive_sync`, `drive_pause`.

use serde_json::json;

use super::route::{flag, route, text};
use super::{console_required, object, Kind, ToolDef};

const DRIVE: &str = "The drive's id, from drive_list.";

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "devices_list",
            title: "Devices on a console",
            description: "The devices using a console now (computers, phones, consoles) and what \
                          each is doing. Asking also tells the console this engine is here.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_required() }), &["console"]),
            run: route!(path: "/api/mesh/state", done: "The console's devices."),
        },
        ToolDef {
            name: "drive_list",
            title: "Shared drives",
            description: "The Mesh Drives on a console, their members and how each stands.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_required() }), &["console"]),
            run: route!(path: "/api/drive", done: "The console's drives."),
        },
        ToolDef {
            name: "drive_sync",
            title: "Sync a shared drive now",
            description: "Has this device look at both sides of a Mesh Drive now, rather than at \
                          its next round.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({ "console": console_required(), "drive": text(DRIVE) }),
                &["console", "drive"],
            ),
            run: route!(method: "POST", path: "/api/drive/sync", done: "Asked for a sync."),
        },
        ToolDef {
            name: "drive_pause",
            title: "Pause a shared drive",
            description: "Pauses or resumes one member of a Mesh Drive.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "drive": text(DRIVE),
                    "member": text("The member's id, from drive_list."),
                    "paused": flag("Pause or resume. Default pause."),
                }),
                &["console", "drive", "member"],
            ),
            run: route!(method: "POST", path: "/api/drive/pause", done: "Changed the member."),
        },
    ]
}
