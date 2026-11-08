//! Remote Play: `rp_session`, `rp_start`, `rp_stop`, `rp_settings`, `rp_passcode`, `rp_keyboard`,
//! `rp_frame`, `rp_input`, `rp_input_release`, `rp_controller`, `rp_pair_attempts`. The routes and what they promise are in
//! `AUTOMATION.md`.
//!
//! A session id guards every route: the routes refuse a session that is not the one running, so
//! input can never reach a stream it was not meant for. `rp_input` therefore wants its session named,
//! as the route does. A frame or a release cannot do harm, so those find the running session when
//! left to.

use serde_json::{json, Value};

use super::route::{flag, route, text, whole};
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
                          nothing streams), `playing`, `attached`, whether an app page shows \
                          it, `user`, the paired PS5 user it signed in (by name), and `stats`, \
                          the last second of the stream: frames, loss, round trip, and sound \
                          (`audio_frames_total` 0 with frames flowing means no sound came; \
                          `audio_other_codec` and `audio_header` say why). rp_start starts a \
                          stream. `last_session` is how the last one started and ended: its \
                          stages with times, and at the end frames, `audio_frames` and loss.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: |ctx, args| Box::pin(session(ctx, args)),
        },
        ToolDef {
            name: "rp_start",
            title: "Start Remote Play",
            description: "Starts a Remote Play stream from a paired console, or joins the one \
                          running there. A console has one Remote Play slot, so only when the user \
                          asked for it. With no app page showing it, it ends after `keep_alive_s` \
                          without a call. `no_credentials` means the console is not paired yet. \
                          A console can have several PS5 users paired (consoles_list shows their \
                          names as `rp_users`): `user` picks one, and the console signs that user \
                          in; left out, the user chosen in the app.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": {
                        "type": "string",
                        "description": "The console: its address, id or name.",
                    },
                    "user": text(
                        "The PS5 user to play as: a name from the console's `rp_users` in \
                         consoles_list, or its number there (1, 2, …). Left out, the user chosen \
                         in the app. A stream already running as another user answers \
                         `busy_user`: rp_stop it first, if the user asked for that.",
                    ),
                    "width": whole("Picture width. Left out, the engine's default."),
                    "height": whole("Picture height. Left out, the engine's default."),
                    "fps": whole("Frames a second. Left out, the engine's default."),
                    "bitrate_kbps": whole("Bitrate in kbit/s. Left out, the engine's default."),
                    "codec": text("The video codec. Left out, the engine's default."),
                    "keep_alive_s": whole(
                        "How long it stays with nobody using it, at most 600 s. Default 300.",
                    ),
                    "keyboard": flag(
                        "Set it up so rp_keyboard can type into the console's on-screen keyboard.",
                    ),
                    "replay_s": whole("Keep this many seconds for instant replay: 15, 30 or 60."),
                    "wait_s": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 60,
                        "description": "Wait up to this long for the first picture, a passcode \
                                        request or a failure. Left out, answers at once.",
                    },
                }),
                &["console"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/rp/start",
                console: "host",
                done: "Asked for the stream. rp_session says how it stands.",
            ),
        },
        ToolDef {
            name: "rp_stop",
            title: "Stop Remote Play",
            description:
                "Ends a Remote Play stream, for anyone watching it too. The console holds \
                          its slot for 12 to 30 s after. Only when the user asked for it.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({
                    "console": console_of_session(),
                    "session": session_of_console(),
                }),
                &[],
            ),
            run: |ctx, args| Box::pin(stop(ctx, args)),
        },
        ToolDef {
            name: "rp_settings",
            title: "Change a running stream",
            description: "What can change while a stream runs: ask for a keyframe, or set instant \
                          replay. Resolution, frame rate, bitrate and codec need rp_stop and \
                          rp_start.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_of_session(),
                    "session": session_of_console(),
                    "keyframe": flag("Ask the console for a full picture now."),
                    "replay_s": whole("Instant replay: 15, 30 or 60 seconds, or 0 for off."),
                    "picture": flag("The picture on the page showing this stream: false turns off its picture, sound and controller while guests, the live feed and replay carry on; true brings it back without a restart."),
                }),
                &[],
            ),
            run: |ctx, args| Box::pin(settings(ctx, args)),
        },
        ToolDef {
            name: "rp_passcode",
            title: "Answer the console's passcode",
            description:
                "Sends the user's sign-in passcode when rp_session says `passcode: true`, \
                          or `cancel` to end the start. Only a passcode the user gave you for \
                          this. It is passed to the console and not kept.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "console": console_of_session(),
                    "session": session_of_console(),
                    "digits": text("The passcode."),
                    "cancel": flag("The user would rather not enter it: the start ends."),
                }),
                &[],
            ),
            run: |ctx, args| Box::pin(passcode(ctx, args)),
        },
        ToolDef {
            name: "rp_keyboard",
            title: "Type on the console",
            description: "Types into the console's on-screen keyboard while it is open, on a \
                          stream started with `keyboard: true`.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "console": console_of_session(),
                    "session": session_of_console(),
                    "text": text("The text the field should hold."),
                    "accept": flag("Confirm the keyboard, as pressing its Done does."),
                }),
                &[],
            ),
            run: |ctx, args| Box::pin(keyboard(ctx, args)),
        },
        ToolDef {
            name: "rp_frame",
            title: "See the game's picture",
            description: "The stream's picture now, as an image, plus its details: size, \
                          `frame_id`, and `frame_age_ms`, which grows when the stream stalls. It \
                          comes from the app's Play screen when that is open and visible, \
                          otherwise from the stream with the ffmpeg Go live uses, at most once a \
                          second. At most one at a time and five a second.",
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
        ToolDef {
            name: "rp_controller",
            title: "What the controller sends",
            description: "What Remote Play sends the PS5's controller, for the running session \
                          (or the last one, `running` false): each controller's last state \
                          (`page` is the app's, with `from` saying which controllers it reads, \
                          their mapping and reader, and the inputs in use; `phone N`, `guest`, \
                          `automation`), buttons by name, `l2`/`r2` 0-255 and sticks, the \
                          trigger peaks since the session started, and `triggers_analog` (false: \
                          only 0 or 255 ever came, the triggers act as buttons). `sent` is all \
                          of them merged, as the console gets it. Ask the user to half-press L2 \
                          and read whether about 128 arrived.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: |ctx, args| Box::pin(controller(ctx, args)),
        },
        ToolDef {
            name: "rp_pair_attempts",
            title: "Recent pairing tries",
            description: "The last few Remote Play pairing tries (newest last): when, the \
                          console, `with` (`helper`: one press in Play › Set up; `pin`: the PIN \
                          the PS5 shows), whether it paired, the PS5's refusal `code` (80108b03 \
                          and the like) and `reason`, and `console`, what the PS5 last said about \
                          its Remote Play switch and signed-in user before the try (null when not \
                          checked). Never a PIN or a key.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: |ctx, args| Box::pin(pair_attempts(ctx, args)),
        },
    ]
}

async fn controller(ctx: ToolCtx, _args: Args) -> ToolResult {
    let answer = ctx.get("/api/ps5/rp/controller", &[]).await?;
    let summary = if answer.get("session").is_none_or(Value::is_null) {
        "No Remote Play session has sent controller input since the engine started.".to_string()
    } else {
        let n = |k: &str| answer["sent"][k].as_u64().unwrap_or(0);
        format!(
            "{} session {}: the console was last sent L2 {} R2 {}; peaks L2 {} R2 {}.",
            if answer["running"] == true {
                "Running"
            } else {
                "Last"
            },
            answer["session"],
            answer["sent"]["state"]["l2"],
            answer["sent"]["state"]["r2"],
            n("peak_l2"),
            n("peak_r2"),
        )
    };
    Ok(ToolOutput::ok(summary, answer))
}

async fn pair_attempts(ctx: ToolCtx, _args: Args) -> ToolResult {
    let answer = ctx.get("/api/ps5/rp/pair/attempts", &[]).await?;
    let tries = answer["attempts"].as_array().map_or(0, Vec::len);
    let summary = match answer["attempts"].as_array().and_then(|a| a.last()) {
        None => "No pairing tried since the engine started.".to_string(),
        Some(last) if last["ok"] == true => format!("{tries} kept; the last one paired."),
        Some(last) => format!(
            "{tries} kept; the last one was refused{}.",
            last["code"]
                .as_str()
                .map(|c| format!(" with {c}"))
                .unwrap_or_default()
        ),
    };
    Ok(ToolOutput::ok(summary, answer))
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
        None => "Nothing is streaming. rp_start starts a stream if the user asked for one; in the \
                 app it is Play › Start."
            .to_string(),
        Some(host) => {
            let session = answer.get("session").cloned().unwrap_or(Value::Null);
            let attached = answer.get("attached").and_then(Value::as_bool) == Some(true);
            let user = answer
                .get("user")
                .and_then(Value::as_str)
                .map(|u| format!(" as {u}"))
                .unwrap_or_default();
            format!(
                "Streaming from {host}{user}, session {session}. {}",
                if attached {
                    "An app page shows it."
                } else {
                    "No app page shows it: frames then come from the stream, with ffmpeg."
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
            "Nothing is streaming, so there is no picture or controller to use. rp_start starts a \
             stream if the user asked for one; in the app it is Play › Start.",
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

/// Calls a route that names the console and session, found as [`target`] finds them, with the
/// tool's other arguments as its fields.
async fn session_call(
    ctx: &ToolCtx,
    args: &Args,
    path: &str,
    fields: &[&str],
) -> Result<Value, ToolOutput> {
    let (console, session) = target(ctx, args).await?;
    let mut body = without_nulls(json!({ "host": console, "session": session }));
    for field in fields {
        if let Some(value) = args.value(field) {
            body[*field] = value.clone();
        }
    }
    ctx.post(path, &body).await
}

async fn stop(ctx: ToolCtx, args: Args) -> ToolResult {
    let answer = session_call(&ctx, &args, "/api/ps5/rp/stop", &[]).await?;
    Ok(ToolOutput::ok(
        "Stopped the stream. The console holds its slot for 12 to 30 s.",
        answer,
    ))
}

async fn settings(ctx: ToolCtx, args: Args) -> ToolResult {
    let fields = ["keyframe", "replay_s", "picture"];
    let answer = session_call(&ctx, &args, "/api/ps5/rp/settings", &fields).await?;
    Ok(ToolOutput::ok("Changed the stream.", answer))
}

async fn passcode(ctx: ToolCtx, args: Args) -> ToolResult {
    let fields = ["digits", "cancel"];
    let answer = session_call(&ctx, &args, "/api/ps5/rp/passcode", &fields).await?;
    Ok(ToolOutput::ok(
        "Passed that on. rp_session says how the start stands.",
        answer,
    ))
}

async fn keyboard(ctx: ToolCtx, args: Args) -> ToolResult {
    let fields = ["text", "accept"];
    let answer = session_call(&ctx, &args, "/api/ps5/rp/keyboard", &fields).await?;
    Ok(ToolOutput::ok("Sent to the keyboard.", answer))
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
