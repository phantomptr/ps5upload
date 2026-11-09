//! The protocol's methods, for both eras. Stateless: all a request needs is in the request.
//!
//! A request's era is read from the request itself: `params._meta` naming a protocol version
//! makes it modern, anything else is legacy. The two differ in how they begin (`server/discover`
//! or `initialize`) and in the envelope of a result (modern results say `resultType` and who
//! answered); everything between is shared.

use serde_json::{json, Map, Value};

use super::jsonrpc::{
    self, RpcError, LATEST_LEGACY_VERSION, META_CLIENT_CAPABILITIES, META_PROTOCOL_VERSION,
    META_SERVER_INFO, SUPPORTED_VERSIONS,
};
use super::tools::{self, ToolCtx};
use super::{prompts, resources, Caller, McpState};

/// The server's name in the protocol.
const SERVER_NAME: &str = "ps5upload";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Era {
    Modern,
    Legacy,
}

impl Era {
    /// A result as this era's client expects it. Legacy clients get it bare: the modern fields
    /// mean nothing to them.
    fn finish(self, mut result: Value) -> Value {
        if self == Era::Modern {
            if let Some(object) = result.as_object_mut() {
                object.insert("resultType".to_string(), json!("complete"));
                let meta = object.entry("_meta").or_insert_with(|| json!({}));
                if let Some(meta) = meta.as_object_mut() {
                    meta.insert(META_SERVER_INFO.to_string(), server_info(false));
                }
            }
        }
        result
    }
}

/// Who answers. `title` is part of `initialize`'s answer only.
fn server_info(with_title: bool) -> Value {
    let mut info = json!({ "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") });
    if with_title {
        info["title"] = json!("PS5Upload");
    }
    info
}

/// What a client is told about using this server.
fn instructions(read_only: bool) -> String {
    let mut text = String::from(
        "PS5Upload's engine: it drives the user's PS5 consoles (files, games, power, hardware, \
         transfers, Remote Play).\n\
         - Local only: this server answers to this computer unless the owner opened the engine \
         with PS5UPLOAD_ALLOW_IP. It has no password; never expose it to a network you do not \
         trust.\n\
         - Pass `console` (the PS5's address) to every tool that touches a console. consoles_list \
         shows the known consoles and the default a read falls back to.\n\
         - Use the curated tools first. For anything they do not cover, api_catalog (or \
         ps5upload://catalog) lists every route of the engine's API and api_request calls one.\n\
         - Destructive tools (their annotations say which: deleting, overwriting, power, closing \
         a game, api_request) need the user's confirmation before you call them. Say what will \
         happen and on which console, and wait for a yes.\n\
         - Remote Play: rp_session says what streams. A console has one Remote Play slot, so call \
         rp_start only when the user asked for a stream (in the app it is Play › Start). rp_frame \
         takes the picture from the app's Play screen when that is open and visible, otherwise \
         from the stream with the ffmpeg Go live uses. A sent input only means it was sent: take a \
         newer frame to see the result.\n\
         - Paths: a `src` or `local_path` is on the computer the engine runs on, which may not be \
         the one you run on; anything else is on the console.\n\
         - Slow work: uploads and downloads are jobs (jobs_wait follows one), installs have \
         install_wait, and files_copy, files_move, files_delete, files_extract and the save tools \
         answer with an op_id when they run long (files_op_status follows it). If a call fails \
         partway, look at the current state before you repeat it.\n\
         - Several uploads or installs in a row: queue_add puts them on the engine's queue for a \
         console, which runs them in order, waits while anything else uses that console, and \
         survives a restart. queue_list shows the same queue every client sees. Nothing on it is \
         retried or restarted unless someone calls queue_resume.",
    );
    if read_only {
        text.push_str(
            "\n- This server is read-only: the owner set it so agents can only look (in the app \
             under MCP & API › Connect an agent, or with PS5UPLOAD_MCP_READ_ONLY in the engine's \
             environment). Tools that can change something are left out of the list and refused. \
             Only the owner can change it; do not try to.",
        );
    }
    text
}

/// What this server offers. A modern client can hear `notifications/tools/list_changed` on a
/// `subscriptions/listen` stream (the owner switches agents between looking and changing things);
/// a legacy one has no stream to hear it on.
fn capabilities(era: Era) -> Value {
    json!({
        "tools": { "listChanged": era == Era::Modern },
        "resources": { "listChanged": false, "subscribe": false },
        "prompts": { "listChanged": false },
    })
}

/// The modern handshake's answer, asked for at any time.
fn discover(state: &McpState) -> Value {
    json!({
        "supportedVersions": SUPPORTED_VERSIONS,
        "capabilities": capabilities(Era::Modern),
        "instructions": instructions(state.read_only()),
    })
}

/// The legacy handshake. The client's version when this server speaks it, otherwise the newest
/// legacy one, for the client to decide. No state is kept from it.
fn initialize(state: &McpState, params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| *v != jsonrpc::MODERN_VERSION && SUPPORTED_VERSIONS.contains(v))
        .unwrap_or(LATEST_LEGACY_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": capabilities(Era::Legacy),
        "serverInfo": server_info(true),
        "instructions": instructions(state.read_only()),
    })
}

/// The modern metadata every request must carry, checked.
pub(crate) fn check_modern_meta(params: &Value) -> Result<(), RpcError> {
    let meta = params.get("_meta").unwrap_or(&Value::Null);
    let Some(version) = meta.get(META_PROTOCOL_VERSION).and_then(Value::as_str) else {
        return Err(RpcError::invalid_params(format!(
            "params._meta needs a string \"{META_PROTOCOL_VERSION}\"."
        )));
    };
    if !meta
        .get(META_CLIENT_CAPABILITIES)
        .is_some_and(Value::is_object)
    {
        return Err(RpcError::invalid_params(format!(
            "params._meta needs an object \"{META_CLIENT_CAPABILITIES}\"."
        )));
    }
    if !SUPPORTED_VERSIONS.contains(&version) {
        return Err(RpcError::unsupported_version(version));
    }
    Ok(())
}

/// Answers one request of `era`.
pub(crate) async fn call(
    state: &McpState,
    caller: &Caller,
    era: Era,
    method: &str,
    params: &Value,
) -> Result<Value, RpcError> {
    let mut result = match (method, era) {
        ("ping", _) => json!({}),
        ("server/discover", Era::Modern) => discover(state),
        ("initialize", Era::Legacy) => initialize(state, params),
        ("tools/list", _) => tools::list(state.read_only()),
        ("tools/call", _) => call_tool(state, caller, params).await?,
        ("resources/list", _) => resources::list(),
        ("resources/templates/list", _) => resources::templates(),
        ("resources/read", _) => resources::read(state, caller, params).await?,
        ("prompts/list", _) => prompts::list(),
        ("prompts/get", _) => prompts::get(params)?,
        _ => return Err(RpcError::method_not_found(method)),
    };
    if era == Era::Modern && CACHEABLE.contains(&method) {
        cache_hints(&mut result);
    }
    Ok(era.finish(result))
}

/// The methods whose complete modern results carry caching hints (2026-07-28 caching: a client
/// rejects these results without them, #772).
const CACHEABLE: [&str; 6] = [
    "server/discover",
    "tools/list",
    "prompts/list",
    "resources/list",
    "resources/templates/list",
    "resources/read",
];

/// Don't keep it, and keep it to this client: the tool list changes when the owner switches
/// agents to read-only, and a read is the console's live state.
fn cache_hints(result: &mut Value) {
    if let Some(object) = result.as_object_mut() {
        object.insert("ttlMs".to_string(), json!(0));
        object.insert("cacheScope".to_string(), json!("private"));
    }
}

async fn call_tool(state: &McpState, caller: &Caller, params: &Value) -> Result<Value, RpcError> {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err(RpcError::invalid_params(
            "tools/call needs a tool \"name\".",
        ));
    };
    let Some(tool) = tools::find(name) else {
        return Err(RpcError::invalid_params(format!("Unknown tool: {name}")));
    };
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(_) => {
            return Err(RpcError::invalid_params(
                "tools/call \"arguments\" must be an object.",
            ))
        }
    };
    let ctx = ToolCtx {
        state: state.clone(),
        caller: caller.clone(),
    };
    Ok(tools::call(tool, ctx, arguments).await.to_result())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_modern_result_says_it_is_complete_and_who_answered() {
        let out = Era::Modern.finish(json!({"tools": []}));
        assert_eq!(out["resultType"], "complete");
        assert_eq!(out["_meta"][META_SERVER_INFO]["name"], SERVER_NAME);
        assert_eq!(out["tools"], json!([]));

        let out = Era::Legacy.finish(json!({"tools": []}));
        assert_eq!(out, json!({"tools": []}));
    }

    #[test]
    fn cacheable_modern_results_carry_cache_hints() {
        // #772: a 2026-07-28 client refuses server/discover and tools/list without them.
        let mut out = json!({"tools": []});
        cache_hints(&mut out);
        assert_eq!(out["ttlMs"], 0);
        assert_eq!(out["cacheScope"], "private");
        for m in ["server/discover", "tools/list", "resources/read"] {
            assert!(CACHEABLE.contains(&m));
        }
        for m in ["tools/call", "prompts/get", "ping", "initialize"] {
            assert!(!CACHEABLE.contains(&m));
        }
    }

    #[test]
    fn the_metadata_a_modern_request_must_carry() {
        let meta = |value: Value| json!({ "_meta": value });
        let capabilities = json!({});
        assert!(check_modern_meta(&meta(json!({
            META_PROTOCOL_VERSION: jsonrpc::MODERN_VERSION,
            META_CLIENT_CAPABILITIES: capabilities,
        })))
        .is_ok());

        let no_capabilities = check_modern_meta(&meta(json!({
            META_PROTOCOL_VERSION: jsonrpc::MODERN_VERSION,
        })));
        assert_eq!(no_capabilities.unwrap_err().code, jsonrpc::INVALID_PARAMS);

        let no_version = check_modern_meta(&meta(json!({
            META_CLIENT_CAPABILITIES: capabilities,
        })));
        assert_eq!(no_version.unwrap_err().code, jsonrpc::INVALID_PARAMS);
        assert_eq!(
            check_modern_meta(&json!({})).unwrap_err().code,
            jsonrpc::INVALID_PARAMS
        );

        let unknown = check_modern_meta(&meta(json!({
            META_PROTOCOL_VERSION: "1999-01-01",
            META_CLIENT_CAPABILITIES: capabilities,
        })));
        assert_eq!(unknown.unwrap_err().code, jsonrpc::UNSUPPORTED_VERSION);
    }

    #[test]
    fn the_instructions_say_what_the_brief_asks() {
        let text = instructions(false);
        for needle in [
            "Local only",
            "`console`",
            "ps5upload://catalog",
            "api_request",
            "confirmation",
            "Play screen",
        ] {
            assert!(text.contains(needle), "{needle}");
        }
        assert!(!text.contains("read-only"));
        assert!(instructions(true).contains("read-only"));
    }
}
