//! The log levels and the engine's log: `logs_level_get`, `logs_level_set`, `logs_recent`
//! (`/api/logs/level`, `/api/logs/recent`, log_levels.rs).

use serde_json::{json, Value};

use super::route::{flag, route, text, whole};
use super::{object, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult};

const LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "logs_level_get",
            title: "Log levels",
            description: "How much the app, the engine and the PS5 helper write to their logs: \
                          the global level, per-category overrides, what each category writes \
                          now, and when a raise above info ends. Levels are error, warn, info, \
                          debug, trace.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: route!(path: "/api/logs/level", done: "The log levels."),
        },
        ToolDef {
            name: "logs_level_set",
            title: "Set log levels",
            description: "Changes how much is logged, for everything or per category (rp, \
                          install, transfer, files, cleanup, convert, pairing, wake, helper, \
                          connection, together, golive, mesh, api, ui, general). Anything above \
                          info lasts `minutes` (default 60, at most a day) and then falls back to \
                          info by itself. Raise only what a problem needs, then reproduce it and \
                          read logs_recent.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "level": {
                        "type": "string",
                        "enum": LEVELS,
                        "description": "The global level.",
                    },
                    "categories": {
                        "type": "object",
                        "additionalProperties": {
                            "anyOf": [{"type": "string", "enum": LEVELS}, {"type": "null"}],
                        },
                        "description": "Per-category levels, like {\"rp\": \"trace\"}. null \
                                        drops a category's override.",
                    },
                    "merge": flag("Change only the categories named and keep the other \
                                   overrides. Without it, `categories` replaces them."),
                    "minutes": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 1440,
                        "description": "How long a raise above info lasts. Default 60, at most \
                                        1440."
                    },
                    "reset": flag("Back to info everywhere, no overrides, before the rest."),
                }),
                &[],
            ),
            run: route!(method: "PUT", path: "/api/logs/level", done: "Changed the log levels."),
        },
        ToolDef {
            name: "logs_recent",
            title: "Recent engine log lines",
            description: "The engine's newest log lines, oldest first, filtered by level (the \
                          least severe to keep), category and time. More detail than \
                          engine_events_recent; raise a category with logs_level_set first to \
                          see its debug and trace lines.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "level": {
                        "type": "string",
                        "enum": LEVELS,
                        "description": "Keep this level and everything more severe: warn keeps \
                                        warn and error. Default: everything.",
                    },
                    "category": text("Only these categories, separated by commas, like \
                                      rp,helper."),
                    "minutes": whole("Only the last this many minutes."),
                    "since_ms": whole("Only lines from this time on, Unix ms."),
                    "since_seq": whole("Only lines after this one (a line's seq, or the \
                                        next_seq of an earlier answer)."),
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": crate::log_levels::MAX_RECENT,
                        "description": "At most this many, the newest. Default 200.",
                    },
                }),
                &[],
            ),
            run: |ctx, args| Box::pin(recent(ctx, args)),
        },
    ]
}

/// The query `logs_recent` sends: `minutes` becomes `since_ms`.
fn recent_query(args: &Args, now_ms: u64) -> Vec<(&'static str, String)> {
    let mut q = Vec::new();
    if let Some(l) = args.text("level") {
        q.push(("level", l.to_string()));
    }
    if let Some(c) = args.text("category") {
        q.push(("category", c.to_string()));
    }
    let since = match (args.number("since_ms"), args.number("minutes")) {
        (Some(s), _) => Some(s),
        (None, Some(m)) => Some(now_ms.saturating_sub(m.saturating_mul(60_000))),
        _ => None,
    };
    if let Some(s) = since {
        q.push(("since_ms", s.to_string()));
    }
    if let Some(s) = args.number("since_seq") {
        q.push(("since_seq", s.to_string()));
    }
    if let Some(n) = args.number("limit") {
        q.push(("limit", n.to_string()));
    }
    q
}

async fn recent(ctx: ToolCtx, args: Args) -> ToolResult {
    let answer = ctx
        .get("/api/logs/recent", &recent_query(&args, crate::now_ms()))
        .await?;
    let count = answer.get("count").and_then(Value::as_u64).unwrap_or(0);
    Ok(ToolOutput::ok(format!("{count} log line(s)."), answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_become_a_start_time() {
        let args: Args = serde_json::from_value::<serde_json::Map<String, Value>>(json!({
            "level": "debug", "category": "rp,helper", "minutes": 5, "limit": 50
        }))
        .map(Args)
        .unwrap();
        let q = recent_query(&args, 1_000_000);
        assert!(q.contains(&("level", "debug".to_string())));
        assert!(q.contains(&("category", "rp,helper".to_string())));
        assert!(q.contains(&("since_ms", (1_000_000 - 300_000).to_string())));
        assert!(q.contains(&("limit", "50".to_string())));
    }
}
