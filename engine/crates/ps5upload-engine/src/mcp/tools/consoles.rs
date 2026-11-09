//! Which consoles there are, and how they are: `consoles_list`, `consoles_discover`,
//! `consoles_status`, `consoles_readiness`.
//!
//! `consoles_list` adds up what the engine knows: the app's saved list (`GET /api/consoles`), and
//! the consoles it has met without one, and says where each entry came from.

use std::collections::{BTreeMap, BTreeSet};

use axum::http::Method;
use serde_json::{json, Value};

use super::route::route;
use super::{
    console_optional, console_required, object, Args, Kind, ToolCtx, ToolDef, ToolOutput,
    ToolResult,
};
use crate::mcp::{ApiReply, Caller, McpState};

const LIST_DESCRIPTION: &str =
    "The consoles this engine knows, each with where that knowledge comes from, and the default a \
     read falls back to when it names no console. When the one you need is not here, \
     consoles_discover looks for it on the network, or ask the user for its address. A console \
     paired for Remote Play lists its paired PS5 users by name as `rp_users` (`rp_user` is the one \
     rp_start uses when it names none). `settings` is a console's own settings (Settings › This \
     PS5); one it does not list uses the app's default (settings_get); console_settings_set \
     changes them.";

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "consoles_helper_ensure",
            title: "Make sure the helper runs",
            description: "If the console's helper answers, says so and sends nothing. If not, \
                          sends the helper this engine carries through the console's payload \
                          loader (port 9021, which the user's jailbreak must have opened). It does \
                          not replace a running helper of another version: payloads_send does.",
            kind: Kind::Write { idempotent: true },
            schema: object(json!({ "console": console_required() }), &["console"]),
            run: |ctx, args| Box::pin(helper_ensure(ctx, args)),
        },
        ToolDef {
            name: "consoles_list",
            title: "Known consoles",
            description: LIST_DESCRIPTION,
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: |ctx, args| Box::pin(list(ctx, args)),
        },
        ToolDef {
            name: "console_settings_set",
            title: "Change one console's settings",
            description: "Changes one console's own settings (Settings › This PS5), all or none: \
                          wake_for_uploads, rest_after_upload, auto_bring_up, \
                          auto_redeploy_on_wake, install_path_fallback (true or false) and \
                          keep_awake_mode (off, transfers or always). null goes back to the \
                          app's default. consoles_list shows what each console has. Only what \
                          the user asked for.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "settings": {
                        "type": "object",
                        "description": "Each setting's name and new value (or null).",
                    },
                }),
                &["console", "settings"],
            ),
            run: route!(
                method: "PATCH",
                path: "/api/consoles/settings",
                done: "Changed the console's settings."
            ),
        },
        ToolDef {
            name: "consoles_discover",
            title: "Find consoles on the network",
            description: "Looks for PS5s on the network around the engine's computer, for a few \
                          seconds. From a container without host networking it sees only the \
                          `subnets` named here, and the answer says so.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "How long to look, in ms. Left out, the engine's default.",
                    },
                    "subnets": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "More /24 networks to sweep, like 192.168.1.0/24.",
                    },
                }),
                &[],
            ),
            run: route!(method: "POST", path: "/api/discover", done: "Looked for consoles."),
        },
        ToolDef {
            name: "tailnet_consoles",
            title: "Find consoles on Tailscale",
            description: "Lists the PS5Upload consoles on this device's Tailscale network, met at \
                          home or not: each with its tailnet address, home address when known \
                          (`lan_host`), whether its helper answered over Tailscale (`reachable`), \
                          its key (`id`) and the console list's entry for it (`listed`). Asking \
                          pairs nothing and shows nothing on the PS5. `addr` probes one tailnet \
                          address or MagicDNS name instead. Answers from the last minute unless \
                          `refresh` is true.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "addr": {
                        "type": "string",
                        "description": "One console to probe: its 100.x address or its name in Tailscale.",
                    },
                    "refresh": {
                        "type": "boolean",
                        "description": "Look again now instead of answering from the last minute.",
                    },
                    "all": {
                        "type": "boolean",
                        "description": "Probe every online peer, not only those named like a console (-ps5u).",
                    },
                }),
                &[],
            ),
            run: route!(method: "GET", path: "/api/tailnet/consoles", done: "Looked on Tailscale."),
        },
        ToolDef {
            name: "consoles_status",
            title: "Console status",
            description: "Asks a console's PS5Upload helper how it is: helper version, kernel, \
                          uptime. A failure says why (`reason`: rest mode, not paired, helper \
                          missing, unreachable).",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: |ctx, args| Box::pin(status(ctx, args)),
        },
        ToolDef {
            name: "consoles_connect_timings",
            title: "How the last status checks went",
            description: "The last status check of each console (or of `console`), phase by \
                          phase, in ms: session reused or opened (`handshake_ms`), a remembered \
                          verdict answering (`cached_verdict`), a plain TCP connect (`tcp_ms`), \
                          the status call (`status_ms`), the helper version, the path (lan or \
                          tailnet) and whether a quiet session was dropped. Sends nothing to a \
                          console: for \"why did it take so long to show as connected\".",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: route!(
                path: "/api/ps5/connect-timings",
                done: "The last status checks."
            ),
        },
        ToolDef {
            name: "consoles_readiness",
            title: "Console readiness",
            description: "Whether a console is in a stable state to install a package: `ready`, \
                          and `detail` when it is not.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: |ctx, args| Box::pin(readiness(ctx, args)),
        },
    ]
}

async fn list(ctx: ToolCtx, _args: Args) -> ToolResult {
    let known = known(&ctx.state, &ctx.caller).await;
    let count = known
        .get("consoles")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let default = known
        .get("default_console")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(ToolOutput::ok(
        format!("{count} known console(s). Default when a read names none: {default}."),
        known,
    ))
}

async fn status(ctx: ToolCtx, args: Args) -> ToolResult {
    let answer = ctx.get("/api/ps5/status", &args.console_query()).await?;
    let text = |name: &str| {
        answer
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    };
    let summary = format!(
        "The helper answers: version {}, kernel {}.",
        text("version"),
        text("ps5_kernel")
    );
    Ok(ToolOutput::ok(summary, answer))
}

async fn readiness(ctx: ToolCtx, args: Args) -> ToolResult {
    let answer = ctx.get("/api/ps5/readiness", &args.console_query()).await?;
    let ready = answer.get("ready").and_then(Value::as_bool) == Some(true);
    let summary = if ready {
        "The console is ready.".to_string()
    } else {
        format!(
            "The console is not ready: {}",
            answer
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or("no detail given")
        )
    };
    Ok(ToolOutput::ok(summary, answer))
}

/// One console and where the engine knows it from.
#[derive(Default)]
struct Known {
    name: Option<String>,
    /// A wake credential is saved for it. The credential itself is never shown.
    can_wake: bool,
    /// Remote Play keys are saved for it, so `rp_start` can start a stream. Never shown either.
    can_stream: bool,
    /// The PS5 users paired for Remote Play, by name only, and the one a start uses by default.
    rp_users: Vec<String>,
    rp_user: Option<String>,
    /// Its own settings (Settings › This PS5), from the saved list; empty when it has none.
    settings: Option<Value>,
    sources: BTreeSet<&'static str>,
}

/// What the engine knows about consoles, for `consoles_list`, `engine_status` and
/// `ps5upload://consoles`. Never carries a saved Remote Play key or wake credential.
pub(crate) async fn known(state: &McpState, caller: &Caller) -> Value {
    let mut consoles: BTreeMap<String, Known> = BTreeMap::new();
    let saved = saved_list(state, caller).await;
    for profile in &saved.consoles {
        let Some(address) = profile.get("host").and_then(Value::as_str) else {
            continue;
        };
        let console = note_source(&mut consoles, address, "saved_list");
        console.name = profile
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        let says = |name: &str| profile.get(name).and_then(Value::as_bool) == Some(true);
        console.can_wake = says("has_wake_credential")
            || profile
                .get("wake_credential")
                .and_then(Value::as_str)
                .is_some_and(|credential| !credential.is_empty());
        console.can_stream = says("has_rp_credentials");
        console.settings = profile
            .get("settings")
            .filter(|v| v.as_object().is_some_and(|m| !m.is_empty()))
            .cloned();
        for user in profile
            .get("rp_users")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = user.get("name").and_then(Value::as_str) else {
                continue;
            };
            if user.get("chosen").and_then(Value::as_bool) == Some(true) {
                console.rp_user = Some(name.to_string());
            }
            console.rp_users.push(name.to_string());
        }
    }
    for address in &saved.paired_not_listed {
        let console = note_source(&mut consoles, address, "paired");
        console.can_stream = true;
    }
    for address in crate::helper_mirror::answered_consoles() {
        note_source(&mut consoles, &address, "answered_status");
    }
    for address in crate::mesh_api::greeted_consoles() {
        note_source(&mut consoles, &address, "mesh");
    }
    for address in crate::console_snapshot::snapshot().keys() {
        note_source(&mut consoles, address, "saved_titles");
    }

    let default = crate::console_snapshot::host_key(state.default_console());
    let configured = std::env::var_os("PS5_ADDR").is_some();
    // The built-in default is the address of a direct cable on one setup: on its own it says
    // nothing about this user's consoles, so it is listed only when there is nothing else.
    if configured || consoles.is_empty() {
        note_source(&mut consoles, &default, "default");
    }
    let list: Vec<Value> = consoles
        .iter()
        .map(|(address, console)| {
            let mut out = json!({
                "address": address,
                "sources": console.sources,
                "is_default": *address == default,
            });
            if let Some(name) = &console.name {
                out["name"] = json!(name);
            }
            if console.can_wake {
                out["can_wake"] = json!(true);
            }
            if console.can_stream {
                out["can_stream"] = json!(true);
            }
            if !console.rp_users.is_empty() {
                out["rp_users"] = json!(console.rp_users);
            }
            if let Some(user) = &console.rp_user {
                out["rp_user"] = json!(user);
            }
            if let Some(settings) = &console.settings {
                out["settings"] = settings.clone();
            }
            out
        })
        .collect();
    json!({
        "consoles": list,
        "default_console": default,
        "default_source": if configured { "PS5_ADDR" } else { "built_in" },
        "sources_explained": {
            "saved_list": "the app's saved console list",
            "paired": "paired through this engine, though not on the saved list",
            "answered_status": "answered a status call since this engine started",
            "mesh": "this engine opened a session with it since it started",
            "saved_titles": "its installed titles were read at some point",
            "default": "the engine's default console; the address is not known to be in use",
        },
    })
}

/// A console's entry, with one more place it is known from.
fn note_source<'a>(
    consoles: &'a mut BTreeMap<String, Known>,
    address: &str,
    source: &'static str,
) -> &'a mut Known {
    let console = consoles
        .entry(crate::console_snapshot::host_key(address))
        .or_default();
    console.sources.insert(source);
    console
}

/// The saved console list.
#[derive(Default)]
struct SavedList {
    consoles: Vec<Value>,
    paired_not_listed: Vec<String>,
}

/// The app's console list, with no key in it. An engine older than that route has only the web
/// UI's document. Nothing saved is not an error.
async fn saved_list(state: &McpState, caller: &Caller) -> SavedList {
    let roster = state
        .api_call(caller, Method::GET, "/api/consoles", &[], None)
        .await
        .ok()
        .filter(ApiReply::is_success)
        .and_then(|reply| reply.json());
    let Some(roster) = roster else {
        return SavedList {
            consoles: saved_profiles(state, caller).await,
            paired_not_listed: Vec::new(),
        };
    };
    let list = |name: &str| roster.get(name).and_then(Value::as_array).cloned();
    SavedList {
        consoles: list("consoles").unwrap_or_default(),
        paired_not_listed: list("paired_hosts_not_listed")
            .unwrap_or_default()
            .iter()
            .filter_map(|host| host.as_str().map(str::to_string))
            .collect(),
    }
}

/// The web UI's saved console list, which only that build fills. Nothing there is not an error.
pub(super) async fn saved_profiles(state: &McpState, caller: &Caller) -> Vec<Value> {
    let Ok(reply) = state
        .api_call(caller, Method::GET, "/api/shared/consoles", &[], None)
        .await
    else {
        return Vec::new();
    };
    if !reply.is_success() {
        return Vec::new();
    }
    reply
        .json()
        .and_then(|doc| doc.get("value")?.get("profiles")?.as_array().cloned())
        .unwrap_or_default()
}

async fn helper_ensure(ctx: ToolCtx, args: Args) -> ToolResult {
    let console = args.text("console").unwrap_or_default().to_string();
    let query = [("addr", console.clone())];
    let status = ctx
        .reply(Method::GET, "/api/ps5/status", &query, None)
        .await?;
    if status.is_success() {
        let answer = json!({ "running": true, "sent": false, "status": status.json() });
        return Ok(ToolOutput::ok("The helper is running.", answer));
    }
    let body = json!({ "ps5_addr": console });
    let sent = ctx.post("/api/pkg/payload-restore", &body).await?;
    let answer = json!({ "running": false, "sent": true, "send": sent });
    Ok(ToolOutput::ok(
        "Sent the helper. consoles_status says when it answers: give it a few seconds.",
        answer,
    ))
}
