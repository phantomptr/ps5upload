//! Cheats: `cheats_status`, `cheats_list`, `cheats_get`, `cheats_toggle`, `cheats_reload`.

use serde_json::json;

use super::route::{flag, route, text};
use super::{console_optional, console_required, object, Kind, ToolDef};

const TITLE_ID: &str = "The title's id, like PPSA01234.";

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "cheats_status",
            title: "Cheat engine status",
            description: "Whether a console's cheat engine runs, and what it has active.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: route!(path: "/api/ps5/cheats/status", done: "The cheat engine's status."),
        },
        ToolDef {
            name: "cheats_list",
            title: "Games with cheats",
            description: "The games a console has cheat files for.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: route!(path: "/api/ps5/cheats/list", done: "The games with cheats."),
        },
        ToolDef {
            name: "cheats_get",
            title: "A game's cheats",
            description: "One game's cheats on a console, each with its `index` and whether it is \
                          on.",
            kind: Kind::Read,
            schema: object(
                json!({ "console": console_optional(), "title_id": text(TITLE_ID) }),
                &["title_id"],
            ),
            run: route!(path: "/api/ps5/cheats/get", done: "The game's cheats."),
        },
        ToolDef {
            name: "cheats_toggle",
            title: "Turn a cheat on or off",
            description: "Turns one cheat on or off. The game must be running for it to take \
                          effect.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "title_id": text(TITLE_ID),
                    "index": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "The cheat's `index`, from cheats_get.",
                    },
                    "on": flag("On or off. Default on."),
                }),
                &["console", "title_id", "index"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/cheats/toggle",
                done: "Changed the cheat.",
            ),
        },
        ToolDef {
            name: "cheats_reload",
            title: "Reload cheat files",
            description: "Has a console's cheat engine read its cheat files again.",
            kind: Kind::Write { idempotent: true },
            schema: object(json!({ "console": console_required() }), &["console"]),
            run: route!(
                method: "POST",
                path: "/api/ps5/cheats/reload",
                done: "Reloaded the cheat files.",
            ),
        },
    ]
}
