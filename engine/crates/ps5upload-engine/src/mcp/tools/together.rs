//! Friends and viewers of the running Remote Play session: Play Together (`together_*`) and Go live
//! (`golive_*`). Both answer this computer and the addresses `PS5UPLOAD_ALLOW_IP` lets in. Stream
//! keys stay in the engine: `golive_status` shows only each one's last four characters.

use serde_json::{json, Value};

use super::route::{flag, route, text, whole};
use super::{console_required, object, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "together_status",
            title: "Play Together",
            description: "Whether Play Together is open on a console, who is in, and who is \
                          waiting to be let in.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_required() }), &["console"]),
            run: route!(path: "/api/join", done: "Play Together on that console."),
        },
        ToolDef {
            name: "together_open",
            title: "Open Play Together",
            description: "Opens Play Together for the Remote Play session running on a console, \
                          so friends can be invited. together_invite then makes the link.",
            kind: Kind::Write { idempotent: true },
            schema: object(json!({ "console": console_required() }), &["console"]),
            run: route!(method: "POST", path: "/api/join/open", done: "Play Together is open."),
        },
        ToolDef {
            name: "together_invite",
            title: "Invite a friend",
            description: "The invite link and code for friends to join. The same invite comes \
                          back until it expires. With `new`, a new one, and the old link and code \
                          stop working. Whoever has the link can ask to join: share it only as the \
                          user says.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "new": flag("Make a new invite; the old link and code stop working."),
                }),
                &["console"],
            ),
            run: route!(method: "POST", path: "/api/join/invite", done: "The invite."),
        },
        ToolDef {
            name: "together_decide",
            title: "Answer a guest",
            description: "Lets in or turns away a guest who is asking to join: together_status \
                          lists them with their ids. Letting one in gives them a controller on the \
                          user's console, so ask the user.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "id": whole("Who is asking: an id from together_status."),
                    "allow": flag("True to let them in, false to turn them away."),
                }),
                &["id", "allow"],
            ),
            run: route!(method: "POST", path: "/api/join/decide", done: "Answered."),
        },
        ToolDef {
            name: "together_kick",
            title: "Let a guest go",
            description: "Takes a guest out of the game. Their way back in stops working.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "player": whole("The guest's player number, from together_status."),
                }),
                &["console", "player"],
            ),
            run: route!(method: "POST", path: "/api/join/kick", done: "They are out."),
        },
        ToolDef {
            name: "together_settings",
            title: "Play Together settings",
            description: "How many guests may join (1 to 3), and whether PS and Create are taken \
                          from guests who are playing. One set for every console.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "max_players": whole("Guests at most, 1 to 3."),
                    "block_system_buttons": flag(
                        "Take PS and Create from guests who are playing.",
                    ),
                }),
                &[],
            ),
            run: route!(method: "POST", path: "/api/join/settings", done: "Changed."),
        },
        ToolDef {
            name: "together_close",
            title: "Close Play Together",
            description: "Sends every guest out and voids the invites. The session itself goes on.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(json!({ "console": console_required() }), &["console"]),
            run: route!(method: "POST", path: "/api/join/close", done: "Play Together is closed."),
        },
        ToolDef {
            name: "golive_status",
            title: "Go live",
            description: "Whether the session is streaming to other sites, the saved destinations \
                          (each key shown only by its last four characters), what each site \
                          allows, and whether ffmpeg is there.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: route!(path: "/api/stream/status", done: "Go live."),
        },
        ToolDef {
            name: "golive_start",
            title: "Start going live",
            description: "Streams the running Remote Play session to saved destinations, in \
                          public: confirm with the user first, and name the sites. Needs ffmpeg \
                          (golive_status says; POST /api/stream/tools/ffmpeg/install fetches it) \
                          and a running session. Destinations are added in the app or by POST \
                          /api/stream/destinations.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "destinations": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "The destinations' ids, from golive_status.",
                    },
                    "audio_kbps": whole("Audio bitrate in kilobits per second. Left out, the default."),
                }),
                &["destinations"],
            ),
            run: |ctx, args| Box::pin(golive_start(ctx, args)),
        },
        ToolDef {
            name: "golive_stop",
            title: "Stop going live",
            description: "Stops streaming to other sites. The Remote Play session goes on.",
            kind: Kind::Write { idempotent: true },
            schema: object(json!({}), &[]),
            run: route!(method: "POST", path: "/api/stream/stop", done: "No longer live."),
        },
        ToolDef {
            name: "golive_destination_remove",
            title: "Remove a Go live destination",
            description: "Forgets a saved destination and its stream key.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({ "id": text("The destination's id, from golive_status.") }),
                &["id"],
            ),
            run: route!(
                method: "DELETE",
                path: "/api/stream/destinations/{id}",
                done: "Removed it.",
            ),
        },
    ]
}

/// The route takes `{destinations: [{key_id}]}`; an agent has a list of ids.
async fn golive_start(ctx: ToolCtx, args: Args) -> ToolResult {
    let ids: Vec<Value> = args
        .entries()
        .find(|(name, _)| name.as_str() == "destinations")
        .and_then(|(_, value)| value.as_array().cloned())
        .unwrap_or_default();
    if ids.is_empty() {
        return Err(ToolOutput::error(
            "Name at least one destination: golive_status lists them.",
        ));
    }
    let destinations: Vec<Value> = ids.iter().map(|id| json!({ "key_id": id })).collect();
    let mut body = json!({ "destinations": destinations });
    if let Some(audio) = args.number("audio_kbps") {
        body["audio_kbps"] = json!(audio);
    }
    let answer = ctx.post("/api/stream/start", &body).await?;
    Ok(ToolOutput::ok("Live.", answer))
}
