//! The app's settings, by name: `settings_get`, `settings_set`. Settings with their own routes
//! (save backups, Collection, Mesh) are there.

use serde_json::json;

use super::route::route;
use super::{object, Kind, ToolDef};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "settings_get",
            title: "App settings",
            description: "The app's settings: their values, which were set on purpose, and a \
                          schema of what each can be. These decide what the app does, like what \
                          happens after an upload.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: route!(path: "/api/settings", done: "The app's settings."),
        },
        ToolDef {
            name: "settings_set",
            title: "Change app settings",
            description: "Changes app settings by name, all or none: a wrong one is named and \
                          nothing changes. null goes back to the default. An open app applies it \
                          within a minute. Only what the user asked for.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "settings": {
                        "type": "object",
                        "description": "Each setting's name and new value, from settings_get.",
                    },
                }),
                &["settings"],
            ),
            run: route!(method: "PATCH", path: "/api/settings", done: "Changed the settings."),
        },
    ]
}
