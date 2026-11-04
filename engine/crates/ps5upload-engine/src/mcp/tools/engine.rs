//! The engine itself: `engine_version`, `engine_status`, `engine_events_recent`.

use serde_json::{json, Value};

use super::{consoles, object, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult};

const DEFAULT_EVENT_MINUTES: u64 = 60;
const DEFAULT_EVENTS: u64 = 200;
const MAX_EVENTS: u64 = 2000;

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "engine_version",
            title: "Engine version",
            description: "The running PS5Upload engine's version and what this build can do.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: |ctx, args| Box::pin(version(ctx, args)),
        },
        ToolDef {
            name: "engine_status",
            title: "Engine status",
            description: "A one-call overview: the engine's version, the consoles it knows, its \
                          jobs, what runs right now (uploads, installs), and whether Remote Play \
                          is streaming. Nothing here touches a console.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: |ctx, args| Box::pin(status(ctx, args)),
        },
        ToolDef {
            name: "engine_events_recent",
            title: "Recent engine events",
            description: "What the engine recorded lately, newest last: connections, helper log \
                          lines, installs, jobs. The first place to look when something went \
                          wrong and nobody saw it.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "minutes": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "How far back to look. Default 60.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_EVENTS,
                        "description": "At most this many, the newest ones. Default 200.",
                    },
                    "cat": {
                        "type": "string",
                        "description": "Only these categories, separated by commas, like \
                                        connection,install.",
                    },
                }),
                &[],
            ),
            run: |ctx, args| Box::pin(events(ctx, args)),
        },
    ]
}

async fn version(ctx: ToolCtx, _args: Args) -> ToolResult {
    let answer = ctx.get("/api/version", &[]).await?;
    let version = answer
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("of unknown version");
    Ok(ToolOutput::ok(
        format!("PS5Upload engine {version}."),
        answer,
    ))
}

/// Keeps the newest `limit` of a list that runs oldest first.
fn keep_newest(events: &mut Vec<Value>, limit: usize) {
    if events.len() > limit {
        events.drain(..events.len() - limit);
    }
}

async fn events(ctx: ToolCtx, args: Args) -> ToolResult {
    let minutes = args.number("minutes").unwrap_or(DEFAULT_EVENT_MINUTES);
    let since = crate::now_ms().saturating_sub(minutes.saturating_mul(60_000));
    let mut query = vec![("since", since.to_string())];
    if let Some(cat) = args.text("cat") {
        query.push(("cat", cat.to_string()));
    }
    let mut answer = ctx.get("/api/event-journal", &query).await?;
    let limit = args
        .number("limit")
        .unwrap_or(DEFAULT_EVENTS)
        .min(MAX_EVENTS);
    let count = match answer.get_mut("events").and_then(Value::as_array_mut) {
        Some(events) => {
            keep_newest(events, limit as usize);
            events.len()
        }
        None => 0,
    };
    Ok(ToolOutput::ok(
        format!("{count} event(s) from the last {minutes} minute(s)."),
        answer,
    ))
}

async fn status(ctx: ToolCtx, _args: Args) -> ToolResult {
    let version = ctx.get("/api/version", &[]).await?;
    // The rest says as much as it can: an overview is most wanted when something is off.
    let jobs = ctx
        .get("/api/jobs", &[])
        .await
        .unwrap_or_else(|_| json!([]));
    let activity = ctx.get("/api/activity", &[]).await.ok();
    let remote_play = ctx.get("/api/ps5/rp/session", &[]).await.ok();
    let known = consoles::known(&ctx.state, &ctx.caller).await;

    let count = |wanted: &str| {
        jobs.as_array().map_or(0, |list| {
            list.iter()
                .filter(|job| job.get("status").and_then(Value::as_str) == Some(wanted))
                .count()
        })
    };
    let running = count("running");
    let streaming_from = remote_play
        .as_ref()
        .and_then(|session| session.get("host"))
        .and_then(Value::as_str);
    let console_count = known
        .get("consoles")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);

    let mut summary = format!(
        "Engine {}: {console_count} known console(s), {running} job(s) running.",
        version
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("of unknown version"),
    );
    match streaming_from {
        Some(host) => summary.push_str(&format!(" Remote Play is streaming from {host}.")),
        None => summary.push_str(" Nothing is streaming."),
    }
    if ctx.state.read_only() {
        summary.push_str(" This server is read-only.");
    }
    Ok(ToolOutput::ok(
        summary,
        json!({
            "engine": version,
            "read_only": ctx.state.read_only(),
            "consoles": known,
            "jobs": {
                "total": jobs.as_array().map_or(0, Vec::len),
                "running": running,
                "done": count("done"),
                "failed": count("failed"),
            },
            "active": activity.as_ref().and_then(|a| a.get("active")),
            "remote_play": remote_play,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_events_are_the_ones_kept() {
        let mut events = vec![json!(1), json!(2), json!(3)];
        keep_newest(&mut events, 2);
        assert_eq!(events, [json!(2), json!(3)]);
        keep_newest(&mut events, 5);
        assert_eq!(events.len(), 2);
    }
}
