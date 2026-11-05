//! What takes a console's space, and clearing what it no longer needs: `disk_usage_start`,
//! `disk_usage_status`, `disk_usage_cancel`, `cleanup_scan`, `cleanup_scan_status`, `cleanup_apply`,
//! `cleanup_apply_status`, `cleanup_cancel`.

use serde_json::json;

use super::route::{route, text, whole};
use super::{console_optional, console_required, object, Kind, ToolDef};

const WALK_ID: &str = "The walk's id, from disk_usage_start.";
const SCAN_ID: &str = "The scan's id, from cleanup_scan.";

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "disk_usage_start",
            title: "Measure a console folder",
            description: "Starts walking a console folder to add up what takes its space. Answers \
                          with an `id`: disk_usage_status follows it until `state` says it ended, \
                          and `tree` then holds the sizes.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "console": console_optional(),
                    "path": text("The folder, like /data."),
                }),
                &["path"],
            ),
            run: route!(method: "POST", path: "/api/ps5/disk-usage", done: "The walk started."),
        },
        ToolDef {
            name: "disk_usage_status",
            title: "Follow a folder walk",
            description: "How a walk from disk_usage_start stands, and what it has found.",
            kind: Kind::Read,
            schema: object(json!({ "id": text(WALK_ID) }), &["id"]),
            run: route!(path: "/api/ps5/disk-usage/{id}", done: "The walk's state."),
        },
        ToolDef {
            name: "disk_usage_cancel",
            title: "Stop a folder walk",
            description: "Stops a walk from disk_usage_start.",
            kind: Kind::Read,
            schema: object(json!({ "id": text(WALK_ID) }), &["id"]),
            run: route!(
                method: "POST",
                path: "/api/ps5/disk-usage/{id}/cancel",
                done: "Stopped the walk.",
            ),
        },
        ToolDef {
            name: "cleanup_scan",
            title: "Find what can be cleaned up",
            description: "Starts a console clean-up scan: leftovers a console no longer needs. It \
                          deletes nothing. Answers with an id: cleanup_scan_status follows it.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: route!(
                method: "POST",
                path: "/api/ps5/cleanup/scan",
                done: "The scan started.",
            ),
        },
        ToolDef {
            name: "cleanup_scan_status",
            title: "Follow a clean-up scan",
            description: "How a clean-up scan stands, and the items it found, each with its id, \
                          size and what it is.",
            kind: Kind::Read,
            schema: object(
                json!({ "console": console_optional(), "id": whole(SCAN_ID) }),
                &["id"],
            ),
            run: route!(path: "/api/ps5/cleanup/scan/{id}", done: "The scan's state."),
        },
        ToolDef {
            name: "cleanup_apply",
            title: "Delete what a scan found",
            description:
                "Deletes the chosen items of a clean-up scan, for good. Show the user the \
                          items and get a yes first. Refused while an install or transfer runs on \
                          that console. Answers with an id: cleanup_apply_status follows it.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "id": whole(SCAN_ID),
                    "item_ids": {
                        "type": "array",
                        "items": { "type": "integer", "minimum": 0 },
                        "minItems": 1,
                        "description": "The items to delete, by their ids in the scan's answer.",
                    },
                }),
                &["console", "id", "item_ids"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/cleanup/scan/{id}/apply",
                done: "The clean-up started.",
            ),
        },
        ToolDef {
            name: "cleanup_apply_status",
            title: "Follow a clean-up",
            description: "How a clean-up from cleanup_apply stands.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "console": console_optional(),
                    "id": whole("The clean-up's id, from cleanup_apply."),
                }),
                &["id"],
            ),
            run: route!(path: "/api/ps5/cleanup/apply/{id}", done: "The clean-up's state."),
        },
        ToolDef {
            name: "health_scan",
            title: "Check a console's health",
            description: "Runs every health check on a console: the helper, folders it needs, \
                          junk, Remote Play, the clock. Each check says what it found and which \
                          repair applies; health_fix applies one.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: route!(path: "/api/ps5/health/scan", done: "The health checks."),
        },
        ToolDef {
            name: "health_junk",
            title: "List junk files",
            description: "The exact files health_fix's clean_junk would delete, with their sizes.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: route!(path: "/api/ps5/health/junk", done: "The junk files."),
        },
        ToolDef {
            name: "health_fix",
            title: "Repair something on a console",
            description: "Applies one named repair from health_scan: create_tool_dirs, clean_junk \
                          (deletes the files health_junk lists; show them to the user first), \
                          enable_remote_play, sync_clock.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "action": {
                        "type": "string",
                        "enum": ["create_tool_dirs", "clean_junk", "enable_remote_play", "sync_clock"],
                        "description": "The repair.",
                    },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "For clean_junk: only these of the listed files. Left out, \
                                        every file on the list at this moment.",
                    },
                }),
                &["console", "action"],
            ),
            run: route!(method: "POST", path: "/api/ps5/health/fix", done: "Repaired."),
        },
        ToolDef {
            name: "cleanup_cancel",
            title: "Stop a clean-up",
            description: "Stops a running clean-up scan or clean-up. What was already deleted \
                          stays deleted.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "id": whole("The scan's or clean-up's id."),
                }),
                &["console", "id"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/cleanup/cancel",
                done: "Asked it to stop.",
            ),
        },
    ]
}
