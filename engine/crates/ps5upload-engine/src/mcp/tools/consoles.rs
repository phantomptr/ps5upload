//! Which consoles there are, and how they are: `consoles_list`, `consoles_status`,
//! `consoles_readiness`.
//!
//! The engine has no one list of consoles: the desktop and mobile apps keep theirs themselves. So
//! `consoles_list` adds up what the engine does know, and says where each entry came from.

use std::collections::{BTreeMap, BTreeSet};

use axum::http::Method;
use serde_json::{json, Value};

use super::{console_optional, object, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult};
use crate::mcp::{Caller, McpState};

const LIST_DESCRIPTION: &str =
    "The consoles this engine knows, each with where that knowledge comes from, and the default a \
     read falls back to when it names no console. The engine has no complete list (the desktop \
     app keeps its own): when the one you need is not here, ask the user for its address.";

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "consoles_list",
            title: "Known consoles",
            description: LIST_DESCRIPTION,
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: |ctx, args| Box::pin(list(ctx, args)),
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
    sources: BTreeSet<&'static str>,
}

/// What the engine knows about consoles, for `consoles_list`, `engine_status` and
/// `ps5upload://consoles`. Never carries a saved Remote Play key or wake credential.
pub(crate) async fn known(state: &McpState, caller: &Caller) -> Value {
    let mut consoles: BTreeMap<String, Known> = BTreeMap::new();
    for profile in saved_profiles(state, caller).await {
        let Some(address) = profile.get("host").and_then(Value::as_str) else {
            continue;
        };
        let console = note_source(&mut consoles, address, "saved_list");
        console.name = profile
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        console.can_wake = profile
            .get("wake_credential")
            .and_then(Value::as_str)
            .is_some_and(|credential| !credential.is_empty());
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
            out
        })
        .collect();
    json!({
        "consoles": list,
        "default_console": default,
        "default_source": if configured { "PS5_ADDR" } else { "built_in" },
        "sources_explained": {
            "saved_list": "the self-hosted web UI's saved console list",
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
