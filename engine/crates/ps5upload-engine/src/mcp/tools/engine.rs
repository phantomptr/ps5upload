//! The engine itself: `engine_version`, `engine_status`.

use serde_json::{json, Value};

use super::{consoles, object, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult};

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
