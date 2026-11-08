//! Payloads: the engine's library (`payloads_catalog`, `payloads_download`), its updates
//! (`payloads_updates`, `payloads_updates_check`, `payloads_update`, `payloads_rollback`,
//! `payloads_update_settings`), keeping a console's Payload Manager in step
//! (`payloads_pm_choice`, `payloads_pm_sync`), sending one to a
//! console (`payloads_send`), and the console's Payload Manager (`payloads_list`,
//! `payloads_launch`, `payloads_store`, `payloads_delete`, `payloads_autoload`).

use serde_json::json;

use super::route::{flag, route, text, whole};
use super::{console_required, object, Kind, ToolDef};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "payloads_catalog",
            title: "The payload library",
            description: "The payloads the engine's library knows, and which are downloaded on \
                          its computer. Nothing here touches a console.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: route!(path: "/api/payloads", done: "The payload library."),
        },
        ToolDef {
            name: "payloads_download",
            title: "Download a library payload",
            description: "Downloads a payload of the library to the engine's computer, so it can \
                          be sent.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "id": text("The payload's id, from payloads_catalog."),
                    "release": text("A release tag. Left out, the latest."),
                }),
                &["id"],
            ),
            run: route!(
                method: "POST",
                path: "/api/payloads/{id}/download",
                done: "Downloaded the payload.",
            ),
        },
        ToolDef {
            name: "payloads_updates",
            title: "Payload updates, as last checked",
            description: "The last update check (one row per payload: the version here, the \
                          latest, whether an update is available, the version a rollback puts \
                          back), what was applied, each console's last Payload Manager sync, and \
                          the \"Keep payloads up to date\" setting. Asks no network.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: route!(path: "/api/payloads/updates", done: "Payload updates."),
        },
        ToolDef {
            name: "payloads_updates_check",
            title: "Check for payload updates",
            description: "Checks every payload on the engine's computer, and every one kept in a \
                          console's Payload Manager, for a newer release. Releases are cached for \
                          an hour; once the host's rate limit answers, the rest come from the \
                          cache. Nothing is downloaded.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "force": flag("Ask each source even when the cache is fresh."),
                }),
                &[],
            ),
            run: route!(
                method: "POST",
                path: "/api/payloads/updates/check",
                done: "Checked for payload updates.",
            ),
        },
        ToolDef {
            name: "payloads_update",
            title: "Update payloads",
            description: "Downloads the newer release of each payload (`ids`, or all that the \
                          last check found), with the same checks as any download, and keeps the \
                          copy it replaces for payloads_rollback. With `sync_console`, then brings \
                          that console's Payload Manager in step (see payloads_pm_sync).",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({
                    "ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Payload ids, from payloads_updates. Left out, every \
                                        update the last check found.",
                    },
                    "sync_console": text(
                        "A console whose Payload Manager to sync afterwards: its address.",
                    ),
                }),
                &[],
            ),
            run: route!(
                method: "POST",
                path: "/api/payloads/updates/apply",
                done: "Updated the payloads.",
            ),
        },
        ToolDef {
            name: "payloads_rollback",
            title: "Roll a payload back",
            description: "Puts back the copy the last update of a payload replaced, on the \
                          engine's computer; calling it again rolls forward. A console's Payload \
                          Manager gets it on its next sync.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({ "id": text("The payload's id, from payloads_updates.") }),
                &["id"],
            ),
            run: route!(
                method: "POST",
                path: "/api/payloads/{id}/rollback",
                done: "Rolled the payload back.",
            ),
        },
        ToolDef {
            name: "payloads_update_settings",
            title: "Keep payloads up to date",
            description: "Turns \"Keep payloads up to date\" on or off. On, the engine checks at \
                          start and daily, downloads what is newer, and syncs every console's \
                          Payload Manager the user chose payloads for. Off by default.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({ "auto_update": flag("On or off.") }),
                &["auto_update"],
            ),
            run: route!(
                method: "PUT",
                path: "/api/payloads/updates/settings",
                done: "Saved the setting.",
            ),
        },
        ToolDef {
            name: "payloads_pm_choice",
            title: "Choose what Payload Manager keeps",
            description: "Sets which library payloads a console's Payload Manager keeps (`ids`) \
                          and which of them it loads at boot (`autoload_ids`). Nothing changes on \
                          the console until payloads_pm_sync. Only what the user chose.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Payload ids to keep in Payload Manager.",
                    },
                    "autoload_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Of those, the ones it loads at boot.",
                    },
                }),
                &["console"],
            ),
            run: route!(
                method: "PUT",
                path: "/api/payloads/pm-sync",
                console: "host",
                done: "Saved the choice.",
            ),
        },
        ToolDef {
            name: "payloads_pm_sync",
            title: "Sync Payload Manager",
            description: "Brings a console's Payload Manager in step with the chosen payloads: \
                          stores this computer's copy where it differs (Payload Manager writes it \
                          through a temporary file), adds a chosen payload to the boot list once, \
                          renames an entry whose file name changed, and keeps every other entry. \
                          Answers what changed; `present: false` when Payload Manager isn't \
                          running.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(json!({ "console": console_required() }), &["console"]),
            run: route!(
                method: "POST",
                path: "/api/payloads/pm-sync/run",
                console: "host",
                done: "Synced Payload Manager.",
            ),
        },
        ToolDef {
            name: "payloads_send",
            title: "Send a payload to a console",
            description: "Sends a payload to a console's loader, which runs it: a file on the \
                          engine's computer (`path`) or one of the library (`payload_id`). A \
                          payload is a program with full access to the console: only one the user \
                          asked for.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "path": text("A file on the engine's computer: its full path."),
                    "payload_id": text("A payload of the library, from payloads_catalog."),
                    "release": text("With `payload_id`: this release tag."),
                    "port": whole("The loader's port. Default 9021."),
                }),
                &["console"],
            ),
            run: route!(
                method: "POST",
                path: "/api/payload/send",
                console: "host",
                done: "Sent the payload.",
            ),
        },
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
