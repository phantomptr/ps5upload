//! JSON-RPC 2.0 envelopes and the MCP protocol constants, with no transport in them: the HTTP
//! endpoint (`http.rs`) and the stdio bridge (`stdio.rs`) share these.

use base64::Engine as _;
use serde_json::{json, Value};

/// The revision with per-request metadata and no handshake.
pub(crate) const MODERN_VERSION: &str = "2026-07-28";
/// What `initialize` answers when the client asks for a revision this server does not know.
pub(crate) const LATEST_LEGACY_VERSION: &str = "2025-11-25";
/// Every revision this server speaks, newest first.
pub(crate) const SUPPORTED_VERSIONS: [&str; 5] = [
    MODERN_VERSION,
    LATEST_LEGACY_VERSION,
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];

/// `params._meta` keys of the modern revision.
pub(crate) const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub(crate) const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
pub(crate) const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
pub(crate) const META_SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";

pub(crate) const PARSE_ERROR: i64 = -32700;
pub(crate) const INVALID_REQUEST: i64 = -32600;
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const INVALID_PARAMS: i64 = -32602;
pub(crate) const INTERNAL_ERROR: i64 = -32603;
pub(crate) const HEADER_MISMATCH: i64 = -32020;
pub(crate) const UNSUPPORTED_VERSION: i64 = -32022;

/// A JSON-RPC error. Tool failures are not these: a tool that fails answers a normal result with
/// `isError` (see `tools`), so the model can read what went wrong.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub(crate) fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub(crate) fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(INVALID_REQUEST, message)
    }

    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }

    pub(crate) fn method_not_found(method: &str) -> Self {
        Self::new(METHOD_NOT_FOUND, format!("Method not found: {method}"))
    }

    pub(crate) fn header_mismatch(message: impl Into<String>) -> Self {
        Self::new(HEADER_MISMATCH, message)
    }

    pub(crate) fn unsupported_version(requested: &str) -> Self {
        Self {
            code: UNSUPPORTED_VERSION,
            message: format!("This server does not speak MCP protocol version {requested}."),
            data: Some(json!({ "supported": SUPPORTED_VERSIONS, "requested": requested })),
        }
    }

    fn to_json(&self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(data) = &self.data {
            error["data"] = data.clone();
        }
        error
    }
}

/// A successful response.
pub(crate) fn success(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// An error response. `id` is `None` when the request's id could not be read.
pub(crate) fn failure(id: Option<&Value>, error: &RpcError) -> Value {
    let mut out = json!({ "jsonrpc": "2.0", "error": error.to_json() });
    if let Some(id) = id {
        out["id"] = id.clone();
    }
    out
}

/// One JSON-RPC message, read.
#[derive(Debug, PartialEq)]
pub(crate) enum Incoming<'a> {
    /// Wants an answer.
    Request {
        id: &'a Value,
        method: &'a str,
        params: &'a Value,
    },
    /// Never answered. Nothing a client notifies this stateless server of needs acting on.
    Notification,
    /// A client's answer. This server never asks a client anything, so it is dropped.
    Response,
}

/// What `params` reads as when a request leaves it out.
static NO_PARAMS: Value = Value::Null;

/// Reads one message. An invalid one comes back with its id, when it has a usable one.
pub(crate) fn classify(message: &Value) -> Result<Incoming<'_>, (Option<&Value>, RpcError)> {
    let id = message
        .get("id")
        .filter(|id| id.is_string() || id.is_number());
    let invalid = |why: &str| (id, RpcError::invalid_request(why));
    if !message.is_object() {
        return Err(invalid("A JSON-RPC message must be an object."));
    }
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(invalid(
            "A JSON-RPC message must carry \"jsonrpc\": \"2.0\".",
        ));
    }
    let Some(method) = message.get("method") else {
        return if message.get("result").is_some() || message.get("error").is_some() {
            Ok(Incoming::Response)
        } else {
            Err(invalid(
                "A JSON-RPC message needs a \"method\", or a \"result\" or \"error\".",
            ))
        };
    };
    let Some(method) = method.as_str() else {
        return Err(invalid("\"method\" must be a string."));
    };
    if message.get("id").is_none() {
        return Ok(Incoming::Notification);
    }
    let Some(id) = id else {
        return Err(invalid("\"id\" must be a string or a number, never null."));
    };
    Ok(Incoming::Request {
        id,
        method,
        params: message.get("params").unwrap_or(&NO_PARAMS),
    })
}

/// The protocol version a request's `_meta` names: what makes it a modern request, whatever its
/// value (a wrong one is answered as wrong, not as a legacy request).
pub(crate) fn meta_protocol_version(params: &Value) -> Option<&Value> {
    params.get("_meta")?.get(META_PROTOCOL_VERSION)
}

/// The `Mcp-Name` header's subject: what a request names, on the three methods that name
/// something.
pub(crate) fn named_subject<'a>(method: &str, params: &'a Value) -> Option<&'a str> {
    let key = match method {
        "tools/call" | "prompts/get" => "name",
        "resources/read" => "uri",
        _ => return None,
    };
    params.get(key)?.as_str()
}

const SENTINEL_START: &str = "=?base64?";
const SENTINEL_END: &str = "?=";

/// `name` as an `Mcp-Name` header value: as it is when a header can carry it, otherwise base64
/// between the sentinels.
pub(crate) fn encode_header_name(name: &str) -> String {
    let plain = !name.is_empty()
        && name.bytes().all(|b| (0x20..=0x7e).contains(&b))
        && !name.starts_with(' ')
        && !name.ends_with(' ')
        && !name.starts_with(SENTINEL_START);
    if plain {
        return name.to_string();
    }
    format!(
        "{SENTINEL_START}{}{SENTINEL_END}",
        base64::engine::general_purpose::STANDARD.encode(name)
    )
}

/// What an `Mcp-Name` header says: its value, or what the base64 between the sentinels decodes
/// to. `None`: the sentinels are there but what is between them does not decode to text.
pub(crate) fn decode_header_name(value: &str) -> Option<String> {
    let Some(encoded) = value
        .strip_prefix(SENTINEL_START)
        .and_then(|rest| rest.strip_suffix(SENTINEL_END))
    else {
        return Some(value.to_string());
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_read_as_what_they_are() {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
        assert!(matches!(
            classify(&request),
            Ok(Incoming::Request {
                method: "ping",
                params: Value::Null,
                ..
            })
        ));
        let notification = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert_eq!(classify(&notification), Ok(Incoming::Notification));
        let answer = json!({"jsonrpc": "2.0", "id": 1, "result": {}});
        assert_eq!(classify(&answer), Ok(Incoming::Response));
    }

    #[test]
    fn an_invalid_message_keeps_its_id_when_it_has_a_usable_one() {
        let no_method = json!({"jsonrpc": "2.0", "id": 7});
        let (id, error) = classify(&no_method).unwrap_err();
        assert_eq!(id, Some(&json!(7)));
        assert_eq!(error.code, INVALID_REQUEST);

        // A null id is not an id.
        let null_id = json!({"jsonrpc": "2.0", "id": null, "method": "ping"});
        let (id, error) = classify(&null_id).unwrap_err();
        assert_eq!(id, None);
        assert_eq!(error.code, INVALID_REQUEST);

        let old = json!({"id": 1, "method": "ping"});
        assert_eq!(classify(&old).unwrap_err().1.code, INVALID_REQUEST);
        assert_eq!(classify(&json!(3)).unwrap_err().1.code, INVALID_REQUEST);
    }

    #[test]
    fn an_error_without_a_known_id_leaves_the_id_out() {
        let out = failure(None, &RpcError::new(PARSE_ERROR, "no"));
        assert!(out.get("id").is_none());
        assert_eq!(out["error"]["code"], PARSE_ERROR);
        let out = failure(Some(&json!("a")), &RpcError::unsupported_version("1999"));
        assert_eq!(out["id"], "a");
        assert_eq!(out["error"]["data"]["requested"], "1999");
        assert_eq!(out["error"]["data"]["supported"][0], MODERN_VERSION);
    }

    #[test]
    fn a_header_name_round_trips_through_the_sentinels() {
        assert_eq!(encode_header_name("files_list"), "files_list");
        assert_eq!(
            encode_header_name("ps5upload://consoles/1.2.3.4/status"),
            "ps5upload://consoles/1.2.3.4/status"
        );
        for awkward in [
            "ゲーム",
            " leading",
            "trailing ",
            "new\nline",
            "",
            "=?base64?x?=",
        ] {
            let header = encode_header_name(awkward);
            assert!(header.starts_with(SENTINEL_START), "{header}");
            assert_eq!(decode_header_name(&header).as_deref(), Some(awkward));
        }
        // Without both sentinels the value is taken as it is.
        assert_eq!(decode_header_name("=?base64?abc").unwrap(), "=?base64?abc");
        // Between the sentinels it must decode to text.
        assert_eq!(decode_header_name("=?base64?***?="), None);
        assert_eq!(decode_header_name("=?base64?//4=?="), None);
    }
}
