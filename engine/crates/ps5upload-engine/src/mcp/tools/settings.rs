//! The app's settings, by name: `settings_get`, `settings_set`. Settings with their own routes
//! (save backups, Collection, Mesh) are there.

use serde_json::{json, Value};

use super::route::{self, route, Route};
use super::{object, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult};
use crate::mcp::access;

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
            run: |ctx, args| Box::pin(set(ctx, args)),
        },
    ]
}

/// Whether a setting's name is the switch that decides what agents may do, however it is spelt.
fn names_agent_access(name: &str) -> bool {
    let plain: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let wanted: String = access::SETTING.chars().filter(|c| *c != '_').collect();
    plain == wanted || plain.contains("readonly")
}

/// `PATCH /api/settings`, except the switch that decides what agents may do: that one is the
/// owner's, in the app (`access.rs`).
async fn set(ctx: ToolCtx, args: Args) -> ToolResult {
    let settings = args.value("settings").cloned().unwrap_or(Value::Null);
    if let Some(name) = settings
        .as_object()
        .and_then(|map| map.keys().find(|name| names_agent_access(name)))
    {
        return Err(ToolOutput::error(format!(
            "`{name}` is refused, and nothing changed: whether agents can change things is the \
             owner's to decide, in the app under MCP & API › Connect an agent."
        )));
    }
    static PATCH: Route = Route {
        method: "PATCH",
        path: "/api/settings",
        done: "Changed the settings.",
        ..Route::DEFAULT
    };
    route::run(ctx, args, &PATCH).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_switch_is_named_however_it_is_spelt() {
        for name in [
            "mcp_read_only",
            "MCP_READ_ONLY",
            "mcp-read-only",
            "mcpReadOnly",
            "read_only",
        ] {
            assert!(names_agent_access(name), "{name}");
        }
        for name in ["always_overwrite", "auto_resume", "system_file_read"] {
            assert!(!names_agent_access(name), "{name}");
        }
    }
}
