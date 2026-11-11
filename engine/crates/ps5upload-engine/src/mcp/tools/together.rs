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
                          waiting to be let in. Internet guests show `via: internet` with their \
                          connection's `path` (direct, or failed with the ICE state) and round \
                          trip; `internet.peers` lists those still connecting too. Each guest's \
                          `link` says how they are connected: `link.path` (`lan`: the home \
                          network, `tailscale`, `internet`: direct through both routers) and \
                          `link.rtt_ms`; for internet guests also (round trip, `sent_kbps`, bytes queued and \
                          buffered on the data channel, the backlog's peaks, `send_waits`; \
                          `video`: `track` when the picture goes as an RTP video track, else \
                          `channel`; `audio`: `track` when the sound goes as an Opus RTP track, else \
                          `channel`; `track`: frames_sent, rtp_packets, nacks, plis, firs, loss, \
                          rtcp_rtt_ms, bwe_kbps against stream_kbps, and `slow` when the \
                          estimate stays under the stream's bitrate: lower the stream's quality; \
                          the connection check: `measuring` for the track's first 2.5 s, \
                          `under_s` seconds under the rate, `fit_kbps` what it carries with 20% \
                          to spare; `behind_jumps` times the link fell behind and the guest was \
                          skipped to the next keyframe, `behind_dropped` the pictures that cost, `wait_max_ms` the longest a \
                          picture waited to go out, `queue_peak_bytes`, `drain_kbps`; \
                          `udp_sent` / `udp_waited` / `udp_lost`: datagrams at the engine's \
                          socket, those it had no room for at once, those that never went), \
                          `skipped` pictures and `key_asks`, and `page`: what the guest's own \
                          page last said about its picture (frames drawn, dropped, late, \
                          decode_ms, stalls, stall_ms) with `quality` in a word (smooth, \
                          stutters, poor) — use it to tell why a friend's picture stutters. The invite's \
                          key, code and links are left out (`invite.hidden`): together_invite \
                          gives the link when the user asks for one.",
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
            description: "The invite link and code for friends to join. `share.url` is the one \
                          link to give: as Settings › Play Together › Invites work says, the \
                          internet link (`share.works: anywhere`; it works on the same Wi-Fi \
                          too, and the connection then stays on it), or the home network's link \
                          (`share.works: wifi`, with `share.why`: `setting` when invites work \
                          only on the Wi-Fi, `offline` when the invite service can't be reached). \
                          The same invite comes back until it expires. With `new`, a new one, \
                          and the old link and code stop working. `internet: false` keeps to the \
                          home network's link; `internet: true` insists on the internet one (an \
                          error when invites work only on the Wi-Fi). Whoever has a link can ask \
                          to join: share it only as the user says.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "new": flag("Make a new invite; the old link and code stop working."),
                    "internet": flag(
                        "Left out: as Invites work says. True: the internet link (friends \
                         anywhere join peer to peer, the hub only introducing them). False: the \
                         home network's link only.",
                    ),
                }),
                &["console"],
            ),
            run: |ctx, args| Box::pin(invite(ctx, args)),
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
            name: "together_profiles",
            title: "Play Together profiles",
            description: "The profiles signed in on a console (the one in front is the host's \
                          own), which profile each guest's player (2 to 4) plays as, and whether \
                          the helper can seat guests in them. together_settings changes the \
                          choice (together_owner_access for the host's own profile).",
            kind: Kind::Read,
            schema: object(json!({ "console": console_required() }), &["console"]),
            run: route!(path: "/api/join/profiles", done: "The console's profiles."),
        },
        ToolDef {
            name: "together_settings",
            title: "Play Together settings",
            description: "How many guests may join (1 to 3), whether PS and Create are taken \
                          from guests who are playing, and where invites work (`invites`: \
                          anywhere, or only on the Wi-Fi), the signalling hub, the STUN servers, asking the router for a port (UPnP). \
                          One set for every console. With `addr` (the console), `guest_profiles` \
                          says which signed-in profile each guest's player plays as on that \
                          console (together_profiles lists them): the guest is seated in it and \
                          the TV never asks. Anything that lets a friend play as the host's own \
                          PS5 profile (its saves and trophies) is together_owner_access, which \
                          asks the user first: this tool refuses `owner: true` and `stream_seat`.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "addr": {
                        "type": "string",
                        "description": "The console `guest_profiles` are for (its address). Required with them.",
                    },
                    "guest_profiles": guest_profiles_schema(false),
                    "max_players": whole("Guests at most, 1 to 3."),
                    "block_system_buttons": flag(
                        "Take PS and Create from guests who are playing.",
                    ),
                    "stream_seat": flag(
                        "Not here: a guest on the stream's own controller plays as the host's PS5 \
                         profile, so it is together_owner_access, which asks the user first.",
                    ),
                    "invites": {
                        "type": "string",
                        "enum": ["anywhere", "wifi"],
                        "description": "Invites work: `anywhere` (the default: one link that \
                                        works on the Wi-Fi and over the internet) or `wifi` \
                                        (the home network's link only). Kept across restarts.",
                    },
                    "internet_invites": flag(
                        "The older name of `invites`: true is anywhere, false only on the Wi-Fi.",
                    ),
                    "hub_url": text("The signalling hub, a wss:// address."),
                    "stun": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "STUN servers as host:port (at most 8; empty: none).",
                    },
                    "upnp": flag(
                        "Ask the router for a port (UPnP, NAT-PMP) while an internet invite is \
                         open.",
                    ),
                }),
                &[],
            ),
            run: |ctx, args| Box::pin(settings(ctx, args)),
        },
        ToolDef {
            name: "together_owner_access",
            title: "Let a friend play as the host",
            description: "Lets a friend play as the host's own PS5 profile, with its saves, \
                          trophies and purchases: only with the user's explicit yes, asked for \
                          this change. `stream_seat` on lets a guest take Player 1, the stream's \
                          own controller signed in as the host's PS5 user, while the host's device \
                          shows no picture and no phone plays (off: every guest gets their own \
                          controller and picks a user on the TV). `guest_profiles` with `addr` \
                          sets each guest player's profile as together_settings does, and may \
                          name the host's own profile (the one in front) as `owner: true`.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({
                    "addr": {
                        "type": "string",
                        "description": "The console `guest_profiles` are for (its address). Required with them.",
                    },
                    "guest_profiles": guest_profiles_schema(true),
                    "stream_seat": flag(
                        "Let a guest play as the host on Player 1 (the stream's own controller, \
                         signed in as the host's PS5 user).",
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

/// `guest_profiles` as a tool takes it. `owner` only in together_owner_access.
fn guest_profiles_schema(owner: bool) -> Value {
    let mut item = json!({
        "type": "object",
        "properties": {
            "player": { "type": "integer", "minimum": 2, "maximum": 4 },
            "user": {
                "type": "integer",
                "description": "The profile's id, from together_profiles.",
            },
            "name": { "type": "string" },
        },
        "required": ["player", "user"],
    });
    if owner {
        item["properties"]["owner"] = json!({
            "type": "boolean",
            "description": "The user confirmed a friend may play as their own profile (its \
                            saves and trophies).",
        });
    }
    json!({
        "type": "array",
        "description": "The profile each guest's player plays as, replacing the last choice. A \
                        player left out: the TV asks the guest who they are. Empty: the TV asks \
                        every guest.",
        "items": item,
    })
}

/// What together_settings leaves to together_owner_access: a friend playing as the host's own
/// profile, on a helper pad (`owner: true`) or on the stream's controller (`stream_seat`). That
/// tool is marked destructive, so a client asks the user before it runs; this one is not.
fn owner_access_asked(args: &Args) -> Option<&'static str> {
    if args.value("stream_seat").is_some() {
        return Some("stream_seat");
    }
    let owner = args
        .value("guest_profiles")
        .and_then(Value::as_array)
        .is_some_and(|v| v.iter().any(|p| p.get("owner") == Some(&Value::Bool(true))));
    owner.then_some("owner: true")
}

async fn settings(ctx: ToolCtx, args: Args) -> ToolResult {
    if let Some(what) = owner_access_asked(&args) {
        return Err(ToolOutput::error(format!(
            "together_settings does not take `{what}`: it lets a friend play as the host's own PS5 \
             profile (its saves and trophies). Ask the user, then use together_owner_access."
        )));
    }
    let mut body = serde_json::Map::new();
    for (name, value) in args.entries() {
        if !value.is_null() {
            body.insert(name.clone(), value.clone());
        }
    }
    let answer = ctx.post("/api/join/settings", &Value::Object(body)).await?;
    Ok(ToolOutput::ok("Changed.", answer))
}

/// The invite as the host's app asks for it: `internet` left out is `share` (the one link, as
/// Invites work says); given, it is passed on as it is.
fn invite_body(args: &Args) -> Value {
    let mut body = serde_json::Map::new();
    for (name, value) in args.entries() {
        if !value.is_null() {
            body.insert(name.clone(), value.clone());
        }
    }
    if !body.contains_key("internet") {
        body.insert("share".into(), Value::Bool(true));
    }
    Value::Object(body)
}

async fn invite(ctx: ToolCtx, args: Args) -> ToolResult {
    let answer = ctx.post("/api/join/invite", &invite_body(&args)).await?;
    Ok(ToolOutput::ok("The invite.", answer))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: Value) -> Args {
        match v {
            Value::Object(m) => Args(m),
            _ => Args::default(),
        }
    }

    /// together_invite without `internet` asks for the one link (as Invites work says); with it,
    /// the agent's choice goes as it is.
    #[test]
    fn an_invite_follows_invites_work_unless_told() {
        let b = invite_body(&args(json!({"console": "1.2.3.4"})));
        assert_eq!(b, json!({"console": "1.2.3.4", "share": true}));
        let b = invite_body(&args(
            json!({"console": "1.2.3.4", "internet": false, "new": true}),
        ));
        assert_eq!(
            b,
            json!({"console": "1.2.3.4", "internet": false, "new": true})
        );
        let tools = tools();
        let s = &tools
            .iter()
            .find(|t| t.name == "together_settings")
            .unwrap()
            .schema;
        assert_eq!(
            s["properties"]["invites"]["enum"],
            json!(["anywhere", "wifi"])
        );
    }

    /// The host's own profile goes through together_owner_access alone, which is marked
    /// destructive so a client asks the user; together_settings refuses it and names that tool.
    #[test]
    fn the_host_s_own_profile_is_a_tool_of_its_own() {
        let tools = tools();
        let tool = |name: &str| tools.iter().find(|t| t.name == name).unwrap();
        let owner = tool("together_owner_access");
        assert_eq!(owner.kind.annotations()["destructiveHint"], true);
        assert!(
            owner.schema["properties"]["guest_profiles"]["items"]["properties"]
                .get("owner")
                .is_some()
        );
        let plain = tool("together_settings");
        assert_eq!(plain.kind.annotations()["destructiveHint"], false);
        assert!(
            plain.schema["properties"]["guest_profiles"]["items"]["properties"]
                .get("owner")
                .is_none()
        );

        assert_eq!(
            owner_access_asked(&args(json!({"stream_seat": true}))),
            Some("stream_seat")
        );
        assert_eq!(
            owner_access_asked(&args(json!({"stream_seat": false}))),
            Some("stream_seat")
        );
        assert_eq!(
            owner_access_asked(&args(json!({
                "addr": "1.2.3.4",
                "guest_profiles": [{"player": 2, "user": 1, "owner": true}],
            }))),
            Some("owner: true")
        );
        assert_eq!(
            owner_access_asked(&args(json!({
                "addr": "1.2.3.4",
                "guest_profiles": [{"player": 2, "user": 1, "owner": false}],
                "max_players": 2,
            }))),
            None
        );
    }
}
