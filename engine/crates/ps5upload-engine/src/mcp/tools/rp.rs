//! Watching and playing a running Remote Play session: `rp_session`, `rp_frame`, `rp_input`,
//! `rp_input_release`. The routes and what they promise are in `AUTOMATION.md`.
//!
//! A session id guards every route: the routes refuse a session that is not the one running, so
//! input can never reach a stream it was not meant for. `rp_input` therefore wants its session named,
//! as the route does. A frame or a release cannot do harm, so those find the running session when
//! left to.

use serde_json::{json, Value};

use super::{object, without_nulls, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult};

/// A picture the size a model reads well and a client carries easily.
const DEFAULT_MAX_WIDTH: u64 = 1280;

const BUTTONS: [&str; 16] = [
    "cross", "circle", "square", "triangle", "up", "down", "left", "right", "l1", "r1", "l3", "r3",
    "options", "create", "touchpad", "ps",
];

fn console_of_session() -> Value {
    json!({
        "type": "string",
        "description": "The console, as rp_session shows it. Left out, the one streaming.",
    })
}

fn session_of_console() -> Value {
    json!({
        "type": "integer",
        "minimum": 0,
        "description": "The session id from rp_session. Left out, the running one.",
    })
}

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "rp_session",
            title: "Remote Play session",
            description: "What Remote Play is streaming now: `host` and `session` (null when \
                          nothing streams), `playing`, and `attached`, whether an app page shows \
                          it (rp_frame needs one). This server never starts a stream: the user \
                          does, in the app (Play › Start).",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: |ctx, args| Box::pin(session(ctx, args)),
        },
        ToolDef {
            name: "rp_frame",
            title: "See the game's picture",
            description: "The picture the app shows right now, as an image, plus its details: \
                          size, `frame_id`, and `frame_age_ms`, which grows when the stream stalls \
                          or the app window is hidden. Needs the app's Play screen open and \
                          visible. At most one at a time and five a second.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "console": console_of_session(),
                    "session": session_of_console(),
                    "format": {
                        "type": "string",
                        "enum": ["jpeg", "png"],
                        "description": "The image's format. Default jpeg.",
                    },
                    "max_width": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 7680,
                        "description": "Scale down to at most this many pixels wide, 16 or more. \
                                        Default 1280; 0 for the stream's own size.",
                    },
                    "after": {
                        "type": "string",
                        "description": "A `frame_id` from an earlier frame: wait up to 1.5 s for a \
                                        newer picture than that one.",
                    },
                }),
                &[],
            ),
            run: |ctx, args| Box::pin(frame(ctx, args)),
        },
        ToolDef {
            name: "rp_input",
            title: "Press controller buttons",
            description: "Holds a controller state for a short time, then lets go: buttons, \
                          sticks, triggers. It is one more controller, merged with the user's. A \
                          newer input replaces the held one. It answers when the input is sent, \
                          not when the game reacts: take a newer frame to see what happened. After \
                          a failure, look at a frame before sending it again.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "console": {
                        "type": "string",
                        "description": "The console, as rp_session shows it.",
                    },
                    "session": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "The session id from rp_session. Another session running \
                                        now refuses the input, and nothing is sent.",
                    },
                    "buttons": {
                        "type": "array",
                        "items": { "type": "string", "enum": BUTTONS },
                        "description": "The buttons to hold.",
                    },
                    "left": stick("The left stick"),
                    "right": stick("The right stick"),
                    "l2": trigger("The left trigger"),
                    "r2": trigger("The right trigger"),
                    "duration_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 5000,
                        "description": "How long to hold it, in ms. Default 100.",
                    },
                }),
                &["console", "session"],
            ),
            run: |ctx, args| Box::pin(input(ctx, args)),
        },
        ToolDef {
            name: "rp_input_release",
            title: "Let go of the controller",
            description: "Lets go of everything this server's controller holds, at once. The \
                          user's own controller is not touched.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_of_session(),
                    "session": session_of_console(),
                }),
                &[],
            ),
            run: |ctx, args| Box::pin(release(ctx, args)),
        },
    ]
}

fn stick(which: &'static str) -> Value {
    json!({
        "type": "array",
        "items": { "type": "number", "minimum": -1, "maximum": 1 },
        "minItems": 2,
        "maxItems": 2,
        "description": format!("{which} as [x, y], each -1 to 1 (right and down are positive)."),
    })
}

fn trigger(which: &'static str) -> Value {
    json!({
        "type": "number",
        "minimum": 0,
        "maximum": 1,
        "description": format!("{which}, 0 to 1."),
    })
}

async fn session(ctx: ToolCtx, _args: Args) -> ToolResult {
    let answer = ctx.get("/api/ps5/rp/session", &[]).await?;
    let summary = match answer.get("host").and_then(Value::as_str) {
        None => "Nothing is streaming. The user starts Remote Play in the app (Play › Start)."
            .to_string(),
        Some(host) => {
            let session = answer.get("session").cloned().unwrap_or(Value::Null);
            let attached = answer.get("attached").and_then(Value::as_bool) == Some(true);
            format!(
                "Streaming from {host}, session {session}. {}",
                if attached {
                    "An app page shows it, so frames work."
                } else {
                    "No app page shows it: frames need the app's Play screen open."
                }
            )
        }
    };
    Ok(ToolOutput::ok(summary, answer))
}

/// The console and session a frame or release is for: as given, or the running one.
async fn target(ctx: &ToolCtx, args: &Args) -> Result<(String, Option<u64>), ToolOutput> {
    let console = args.text("console").map(str::to_string);
    let session = args.number("session");
    if let (Some(console), Some(session)) = (&console, session) {
        return Ok((console.clone(), Some(session)));
    }
    let running = ctx.get("/api/ps5/rp/session", &[]).await?;
    let running_host = running.get("host").and_then(Value::as_str);
    let Some(console) = console.or_else(|| running_host.map(str::to_string)) else {
        return Err(ToolOutput::error(
            "Nothing is streaming, so there is no picture or controller to use. Ask the user to \
             start Remote Play in the app (Play › Start); this server never starts a stream.",
        ));
    };
    // The running session's id says nothing about another console: the route answers for that.
    let same_console = running_host.is_some_and(|host| {
        crate::console_snapshot::host_key(host) == crate::console_snapshot::host_key(&console)
    });
    let running_session = running
        .get("session")
        .and_then(Value::as_u64)
        .filter(|_| same_console);
    Ok((console, session.or(running_session)))
}

async fn frame(ctx: ToolCtx, args: Args) -> ToolResult {
    let (console, session) = target(&ctx, &args).await?;
    let mut query = vec![
        ("host", console),
        ("json", "1".to_string()),
        ("format", args.text("format").unwrap_or("jpeg").to_string()),
        (
            "max_width",
            args.number("max_width")
                .unwrap_or(DEFAULT_MAX_WIDTH)
                .to_string(),
        ),
    ];
    if let Some(session) = session {
        query.push(("session", session.to_string()));
    }
    if let Some(after) = args.text("after") {
        query.push(("after", after.to_string()));
    }
    let mut answer = ctx.get("/api/ps5/rp/frame", &query).await?;
    let image = answer
        .as_object_mut()
        .and_then(|fields| fields.remove("image_base64"));
    let Some(Value::String(image)) = image else {
        return Err(ToolOutput::error_with(
            "The engine's answer holds no picture.",
            answer,
        ));
    };
    let number = |name: &str| answer.get(name).and_then(Value::as_u64).unwrap_or(0);
    let format = answer
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("jpeg");
    let mime_type = if format == "png" {
        "image/png"
    } else {
        "image/jpeg"
    };
    let summary = format!(
        "Frame {} from {}, {}x{}, {} ms old.",
        answer
            .get("frame_id")
            .and_then(Value::as_str)
            .unwrap_or("(no id)"),
        answer
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or("the console"),
        number("width"),
        number("height"),
        number("frame_age_ms"),
    );
    Ok(ToolOutput::ok(summary, answer).with_image(image, mime_type))
}

async fn input(ctx: ToolCtx, args: Args) -> ToolResult {
    let body = without_nulls(json!({
        "host": args.required("console")?,
        "session": args.number("session"),
        "buttons": args.value("buttons"),
        "left": args.value("left"),
        "right": args.value("right"),
        "l2": args.value("l2"),
        "r2": args.value("r2"),
        "duration_ms": args.number("duration_ms"),
    }));
    let answer = ctx.post("/api/ps5/rp/input", &body).await?;
    let held_for = args.number("duration_ms").unwrap_or(100);
    Ok(ToolOutput::ok(
        format!(
            "Sent, holding for {held_for} ms. That says it was sent, not that the game reacted: \
             take a newer frame to see."
        ),
        answer,
    ))
}

async fn release(ctx: ToolCtx, args: Args) -> ToolResult {
    let (console, session) = target(&ctx, &args).await?;
    let body = without_nulls(json!({ "host": console, "session": session }));
    let answer = ctx.post("/api/ps5/rp/input/release", &body).await?;
    Ok(ToolOutput::ok("Let go of everything.", answer))
}
