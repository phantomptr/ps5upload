//! The tools, one file per family, and what they share.
//!
//! A tool is a thin layer over the engine's API: it names the console and fields the way an agent
//! expects, calls the route in-process, and answers with the engine's JSON (`structuredContent`),
//! a short summary, and that JSON as text (for clients that read only text). Anything that goes
//! wrong is a tool error (`isError`) with the engine's own words and a hint, so the model can read
//! it and act on it; JSON-RPC errors are for a call that names no such tool.

mod api;
mod cheats;
pub(crate) mod consoles;
mod disk;
mod engine;
pub(crate) mod files;
mod games;
mod hw;
mod install;
mod jobs;
mod logs;
mod mesh;
mod more_files;
mod notifications;
mod payloads;
mod power;
mod queue;
pub(crate) mod route;
mod rp;
mod saves;
mod settings;
mod share;
mod sources;
mod together;
mod transfer;

use std::sync::OnceLock;

use axum::http::Method;
use futures_util::future::BoxFuture;
use serde_json::{json, Map, Value};

use super::{ApiReply, Caller, McpState};

/// What a tool can do, which its annotations tell a client and read-only mode goes by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Only reads, however it reaches the engine (some reads are POST routes).
    Read,
    /// Changes something, and nothing is lost by it.
    Write { idempotent: bool },
    /// Can lose something, or interrupt the user: clients should ask first.
    Destructive { idempotent: bool },
    /// `api_request`: annotated for the worst it can do, and judges each call itself in read-only
    /// mode.
    Generic,
}

impl Kind {
    fn annotations(self) -> Value {
        let (read_only, destructive, idempotent) = match self {
            Kind::Read => (true, false, true),
            Kind::Write { idempotent } => (false, false, idempotent),
            Kind::Destructive { idempotent } => (false, true, idempotent),
            Kind::Generic => (false, true, false),
        };
        json!({
            "readOnlyHint": read_only,
            "destructiveHint": destructive,
            "idempotentHint": idempotent,
            "openWorldHint": false,
        })
    }
}

/// A tool's code. Both sides of the result are an answer: `Err` lets helpers use `?`.
pub(crate) type ToolResult = Result<ToolOutput, ToolOutput>;
pub(crate) type ToolFn = fn(ToolCtx, Args) -> BoxFuture<'static, ToolResult>;

/// One tool.
pub(crate) struct ToolDef {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub kind: Kind,
    pub schema: Value,
    pub run: ToolFn,
}

fn all() -> &'static [ToolDef] {
    static TOOLS: OnceLock<Vec<ToolDef>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        [
            engine::tools(),
            logs::tools(),
            consoles::tools(),
            jobs::tools(),
            files::tools(),
            more_files::tools(),
            transfer::tools(),
            install::tools(),
            queue::tools(),
            power::tools(),
            hw::tools(),
            games::tools(),
            saves::tools(),
            cheats::tools(),
            payloads::tools(),
            disk::tools(),
            sources::tools(),
            settings::tools(),
            mesh::tools(),
            rp::tools(),
            together::tools(),
            share::tools(),
            notifications::tools(),
            api::tools(),
        ]
        .into_iter()
        .flatten()
        .collect()
    })
}

pub(crate) fn find(name: &str) -> Option<&'static ToolDef> {
    all().iter().find(|tool| tool.name == name)
}

impl ToolDef {
    /// Can change something: refused, and left out of the list, when agents may only look.
    fn changes_things(&self) -> bool {
        matches!(self.kind, Kind::Write { .. } | Kind::Destructive { .. })
    }
}

/// The answer to `tools/list`, in a fixed order. When agents may only look, the tools that change
/// things are left out (`api_request` stays: it still takes GET).
pub(crate) fn list(read_only: bool) -> Value {
    let tools: Vec<Value> = all()
        .iter()
        .filter(|tool| !(read_only && tool.changes_things()))
        .map(|tool| {
            json!({
                "name": tool.name,
                "title": tool.title,
                "description": tool.description,
                "inputSchema": tool.schema,
                "annotations": tool.kind.annotations(),
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// Runs a tool: checks read-only mode and the arguments, then the tool itself.
pub(crate) async fn call(
    tool: &ToolDef,
    ctx: ToolCtx,
    arguments: Map<String, Value>,
) -> ToolOutput {
    // Read now, not when the client listed the tools: the owner can flip it at any time, and a
    // client may call a tool from a list it fetched before.
    if ctx.state.read_only() && tool.changes_things() {
        return ToolOutput::error(format!(
            "{} is refused: {}",
            tool.name,
            super::access::refusal(ctx.state.access().locked())
        ));
    }
    if let Err(problem) = check_arguments(&tool.schema, &arguments) {
        return ToolOutput::error(problem);
    }
    (tool.run)(ctx, Args(arguments))
        .await
        .unwrap_or_else(|e| e)
        .without_keys()
}

/// Checks arguments against a tool's schema, the small part of JSON Schema the tools use. The same
/// mistakes would otherwise be read as "not given": a misspelt `addr` for `console` would quietly
/// ask the default console instead of the one meant.
fn check_arguments(schema: &Value, arguments: &Map<String, Value>) -> Result<(), String> {
    let none = Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&none);
    for name in arguments.keys() {
        if !properties.contains_key(name) {
            let known: Vec<&str> = properties.keys().map(String::as_str).collect();
            return Err(if known.is_empty() {
                format!("This tool takes no arguments, but got `{name}`.")
            } else {
                format!(
                    "Unknown argument `{name}`. This tool takes: {}.",
                    known.join(", ")
                )
            });
        }
    }
    let required = schema.get("required").and_then(Value::as_array);
    for name in required.into_iter().flatten().filter_map(Value::as_str) {
        let given = arguments.get(name).is_some_and(|value| match value {
            Value::Null => false,
            Value::String(text) => !text.trim().is_empty(),
            _ => true,
        });
        if !given {
            return Err(format!("Missing required argument `{name}`."));
        }
    }
    for (name, value) in arguments {
        if let (false, Some(rule)) = (value.is_null(), properties.get(name)) {
            check_value(name, rule, value)?;
        }
    }
    Ok(())
}

fn check_value(name: &str, rule: &Value, value: &Value) -> Result<(), String> {
    let kind = rule.get("type").and_then(Value::as_str).unwrap_or_default();
    let right_type = match kind {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => true,
    };
    if !right_type {
        return Err(format!("`{name}` must be of type {kind}."));
    }
    if let Some(allowed) = rule.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            let allowed: Vec<String> = allowed.iter().map(Value::to_string).collect();
            return Err(format!("`{name}` must be one of {}.", allowed.join(", ")));
        }
    }
    if let (Some(number), Some(minimum)) =
        (value.as_f64(), rule.get("minimum").and_then(Value::as_f64))
    {
        if number < minimum {
            return Err(format!("`{name}` must be at least {minimum}."));
        }
    }
    if let (Some(number), Some(maximum)) =
        (value.as_f64(), rule.get("maximum").and_then(Value::as_f64))
    {
        if number > maximum {
            return Err(format!("`{name}` must be at most {maximum}."));
        }
    }
    if let Some(items) = value.as_array() {
        let count = items.len() as u64;
        if rule
            .get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|min| count < min)
            || rule
                .get("maxItems")
                .and_then(Value::as_u64)
                .is_some_and(|max| count > max)
        {
            return Err(format!("`{name}` has the wrong number of items."));
        }
        if let Some(item_rule) = rule.get("items") {
            for item in items {
                check_value(name, item_rule, item)?;
            }
        }
    }
    Ok(())
}

/// A tool's arguments, checked against its schema.
#[derive(Debug, Default)]
pub(crate) struct Args(Map<String, Value>);

impl Args {
    /// A string argument, trimmed. Blank counts as not given.
    pub(super) fn text(&self, name: &str) -> Option<&str> {
        self.0
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }

    /// A required string argument. The schema check has already refused one that is missing; this
    /// keeps a tool's code free of an `unwrap` all the same.
    pub(super) fn required(&self, name: &str) -> Result<&str, ToolOutput> {
        self.text(name)
            .ok_or_else(|| ToolOutput::error(format!("Missing required argument `{name}`.")))
    }

    pub(super) fn number(&self, name: &str) -> Option<u64> {
        self.0.get(name).and_then(Value::as_u64)
    }

    pub(super) fn flag(&self, name: &str) -> Option<bool> {
        self.0.get(name).and_then(Value::as_bool)
    }

    /// Every argument as it came.
    pub(super) fn entries(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.0.iter()
    }

    /// An argument as it came, for fields passed to a route unchanged.
    pub(super) fn value(&self, name: &str) -> Option<&Value> {
        self.0.get(name).filter(|value| !value.is_null())
    }

    /// The console, as `addr` in a route's query. Absent: the route falls back to its default.
    pub(super) fn console_query(&self) -> Vec<(&'static str, String)> {
        self.text("console")
            .map(|console| vec![("addr", console.to_string())])
            .unwrap_or_default()
    }
}

/// What a tool needs to do its work.
#[derive(Clone)]
pub(crate) struct ToolCtx {
    pub state: McpState,
    pub caller: Caller,
}

impl ToolCtx {
    /// One API call with its answer as it came, whatever the status.
    pub(super) async fn reply(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<ApiReply, ToolOutput> {
        self.state
            .api_call(&self.caller, method, path, query, body)
            .await
            .map_err(ToolOutput::error)
    }

    /// `GET`, with a success's JSON. A failure is the tool's error.
    pub(super) async fn get(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Value, ToolOutput> {
        let reply = self.reply(Method::GET, path, query, None).await?;
        success_json(&format!("GET {path}"), &reply)
    }

    /// `POST` with a JSON body, with a success's JSON. A failure is the tool's error.
    pub(super) async fn post(&self, path: &str, body: &Value) -> Result<Value, ToolOutput> {
        let reply = self.reply(Method::POST, path, &[], Some(body)).await?;
        success_json(&format!("POST {path}"), &reply)
    }

    /// `POST` with no body (the job routes take none), with a success's JSON.
    pub(super) async fn post_empty(&self, path: &str) -> Result<Value, ToolOutput> {
        let reply = self.reply(Method::POST, path, &[], None).await?;
        success_json(&format!("POST {path}"), &reply)
    }
}

/// A success's JSON. A non-2xx answer, or a 200 that says `"ok": false`, is the tool's error.
fn success_json(label: &str, reply: &ApiReply) -> Result<Value, ToolOutput> {
    if !reply.is_success() {
        return Err(engine_failure(label, reply));
    }
    let Some(json) = reply.json() else {
        let text = reply.text();
        return Ok(if text.is_empty() {
            json!({})
        } else {
            json!({ "text": text })
        });
    };
    if json.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(engine_failure(label, reply));
    }
    Ok(json)
}

/// A tool's answer.
#[derive(Debug, Clone)]
pub(crate) struct ToolOutput {
    content: Vec<Value>,
    structured: Option<Value>,
    is_error: bool,
}

impl ToolOutput {
    /// What leaves the server: every key out, whatever route it came from. Tools read keys inside
    /// (a wake needs its credential); only their results are held to this.
    fn without_keys(mut self) -> Self {
        // A failure is sentences, not an answer, and can quote what it failed on.
        if self.is_error {
            for block in &mut self.content {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    let clean = ps5upload_core::secret_text::without_secrets(text).into_owned();
                    block["text"] = json!(clean);
                }
            }
        }
        let mut changed = false;
        if let Some(structured) = self.structured.as_mut() {
            changed = crate::mcp::catalog_routes::redact_keys(structured);
        }
        if changed {
            let plain = self.structured.as_ref().map(Value::to_string);
            for block in &mut self.content {
                if let (Some(text), Some(plain)) =
                    (block.get("text").and_then(Value::as_str), &plain)
                {
                    if serde_json::from_str::<Value>(text).is_ok_and(|v| v.is_object()) {
                        block["text"] = json!(plain);
                    }
                }
            }
        }
        self
    }

    /// A success: `summary` for a person, `structured` for a program, and the same JSON again as
    /// text for clients that read only text.
    pub(super) fn ok(summary: impl Into<String>, structured: Value) -> Self {
        let structured = structured_object(structured);
        Self {
            content: vec![
                text_block(summary.into()),
                text_block(structured.to_string()),
            ],
            structured: Some(structured),
            is_error: false,
        }
    }

    /// A failure with nothing more to say than a sentence.
    pub(super) fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![text_block(message.into())],
            structured: None,
            is_error: true,
        }
    }

    /// A failure with what is known about it: as with a success, the same JSON again as text.
    pub(super) fn error_with(message: impl Into<String>, structured: Value) -> Self {
        let structured = structured_object(structured);
        Self {
            content: vec![
                text_block(message.into()),
                text_block(structured.to_string()),
            ],
            structured: Some(structured),
            is_error: true,
        }
    }

    /// Adds a picture, first, so a client that shows one block shows that.
    pub(super) fn with_image(mut self, base64_data: String, mime_type: &str) -> Self {
        self.content.insert(
            0,
            json!({ "type": "image", "data": base64_data, "mimeType": mime_type }),
        );
        self
    }

    pub(crate) fn to_result(&self) -> Value {
        let mut result = json!({ "content": self.content });
        if let Some(structured) = &self.structured {
            result["structuredContent"] = structured.clone();
        }
        if self.is_error {
            result["isError"] = json!(true);
        }
        result
    }
}

fn text_block(text: String) -> Value {
    json!({ "type": "text", "text": text })
}

/// `structuredContent` must be an object: a client's schema check refuses the whole result
/// otherwise. A route that answers a list has it under `items`.
fn structured_object(value: Value) -> Value {
    match value {
        Value::Object(_) => value,
        Value::Array(_) => json!({ "items": value }),
        other => json!({ "value": other }),
    }
}

/// The tool error for an API answer that says something failed, in the engine's words.
pub(super) fn engine_failure(label: &str, reply: &ApiReply) -> ToolOutput {
    let status = reply.status.as_u16();
    let body = reply.json();
    let field = |name: &str| {
        body.as_ref()
            .and_then(|b| b.get(name))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
    };
    let error = field("error").or_else(|| field("err"));
    let reason = field("reason");
    let mut said: Vec<&str> = Vec::new();
    for part in [error, field("message"), field("detail"), reason]
        .into_iter()
        .flatten()
    {
        if !said.contains(&part) {
            said.push(part);
        }
    }
    let plain = reply.text();
    if said.is_empty() && body.is_none() && !plain.trim().is_empty() {
        said.push(plain.trim());
    }
    let what = if said.is_empty() {
        format!("{label} answered HTTP {status}.")
    } else {
        format!("{label} answered HTTP {status}: {}", said.join(" / "))
    };
    let hint = hint_for(reply, error, reason, &said.join(" "));
    let message = match hint {
        Some(hint) => format!("{what}\nHint: {hint}"),
        None => what,
    };
    let mut structured = json!({ "status": status });
    if let Some(error) = error {
        structured["error"] = json!(error);
    }
    if let Some(reason) = reason {
        structured["reason"] = json!(reason);
    }
    if let Some(hint) = hint {
        structured["hint"] = json!(hint);
    }
    if let Some(body) = body {
        structured["body"] = body;
    } else if !plain.is_empty() {
        structured["body"] = json!(plain);
    }
    ToolOutput::error_with(message, structured)
}

/// What to do about a failure, when that is known.
fn hint_for(
    reply: &ApiReply,
    error: Option<&str>,
    reason: Option<&str>,
    said: &str,
) -> Option<&'static str> {
    // The codes Remote Play automation answers with, and the routes that answer to this computer
    // alone (AUTOMATION.md).
    let exact = |code: &str| -> Option<&'static str> {
        Some(match code {
            "no_session" => {
                "Nothing is streaming from that console. rp_start starts a stream if the user \
                 asked for one; in the app it is Play › Start."
            }
            "stale_session" => "Another session runs now: call rp_session for its id.",
            "no_viewer" => {
                "The stream runs, but no app page shows it and this engine has no ffmpeg to make \
                 a picture. Ask the user to open the app's Play screen and keep it visible, or to \
                 install ffmpeg from Go live."
            }
            "no_picture" => "The page has not shown a picture yet: try again in a moment.",
            "busy" | "rate_limited" => {
                "One frame at a time, at most five a second: wait a moment (`retry_ms` says how \
                 long) and try again."
            }
            "viewer_busy" => "The page's connection is full: try again shortly.",
            "timeout" => {
                "The page did not answer in time. Check that the app's Play screen is visible."
            }
            "no_newer_frame" => {
                "No newer picture came: the game's screen may not be changing, or the stream \
                 stalled. Take a frame without `after`."
            }
            "page_error" => {
                "The page could not make the image. In HDR sessions it can come out blank: switch \
                 Video format to SDR for automation."
            }
            "no_credentials" => {
                "This engine holds no Remote Play keys for that console. Ask the user to pair it \
                 in the app (Connection screen): the console shows a code."
            }
            "busy_other" => {
                "Another console's session runs, and there is one at a time: rp_session names it. \
                 Stop it only if the user says so."
            }
            "restart_needed" => {
                "That can only be set when a session starts: rp_stop, then rp_start with the new \
                 value. Ask the user first, because it interrupts the stream."
            }
            "no_passcode_asked" => "The console is not asking for a passcode now.",
            "start_failed" => {
                "The stream did not start. Check that Remote Play is enabled on the console, then \
                 try again after about 30 s: the console holds its slot for a while."
            }
            "local_only" | "host_not_local" => {
                "This answers only to the engine's own computer, addressed as localhost, 127.0.0.1 \
                 or [::1]. PS5UPLOAD_ALLOW_IP does not open it."
            }
            _ => return None,
        })
    };
    if let Some(hint) = error.and_then(exact) {
        return Some(hint);
    }
    let said = said.to_ascii_lowercase();
    let says = |needles: &[&str]| needles.iter().any(|needle| said.contains(*needle));
    let reason_is = |code: &str| reason == Some(code);
    if reason_is("console_rest_mode") || says(&["console_rest_mode", "rest mode"]) {
        return Some(
            "The console is in rest mode. Wake it only if the user wants that (power_wake).",
        );
    }
    if reason_is("helper_not_ava1") || says(&["helper_not_ava1"]) {
        return Some(
            "The console has no PS5Upload helper running, or an old one. Ask the user to open the \
             app and send it from the Connection screen.",
        );
    }
    if reason_is("not_paired") || says(&["not_paired"]) {
        return Some(
            "This engine is not paired with that console yet. Ask the user to pair it in the app \
             (Connection screen): the console shows a code.",
        );
    }
    if says(&["addr is required"]) {
        return Some("Pass `console`: this acts only on a console it is told about.");
    }
    if reason_is("ava1_unreachable")
        || says(&[
            "ava1_unreachable",
            "did not answer",
            "connection refused",
            "timed out",
            "no route to host",
            "unreachable",
        ])
    {
        return Some(
            "The console did not answer. Check the address (consoles_list), that the console is \
             on and on the network, and that the PS5Upload helper is running (consoles_status).",
        );
    }
    let allow = reply
        .headers
        .get(axum::http::header::ALLOW)
        .and_then(|v| v.to_str().ok());
    match reply.status.as_u16() {
        // A route's own 404 (no such job, no such file) has its own words and no `code`.
        404 if reply.body.is_empty()
            || reply.json().is_some_and(|body| body["code"] == "not_found") =>
        {
            Some("There is no such route: api_catalog and ps5upload://catalog list them all.")
        }
        405 => Some(if allow.is_some() {
            "That route does not take this method: api_catalog lists each route's methods."
        } else {
            "That route does not take this method."
        }),
        415 => Some("That route takes a JSON body: pass `body`, even an empty object."),
        400 | 422 if said.contains("deserialize") || said.contains("missing field") => {
            Some("A field is missing or has the wrong type in what was sent.")
        }
        426 => Some("That route is a WebSocket, which a tool call cannot open."),
        _ => None,
    }
}

// Schema helpers, so each tool states only what is its own.

/// A tool's input schema.
pub(super) fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// `console` on a read, which falls back to the engine's default console.
pub(super) fn console_optional() -> Value {
    json!({
        "type": "string",
        "description": "The PS5's address, like 192.168.1.100. Left out, the engine's default \
                        console is asked: consoles_list says which that is.",
    })
}

/// `console` on anything that changes something: never left to a default.
pub(super) fn console_required() -> Value {
    json!({
        "type": "string",
        "description": "The PS5's address, like 192.168.1.100 (consoles_list shows the known ones).",
    })
}

/// A JSON object without its null fields: what a route's optional fields look like left out.
pub(super) fn without_nulls(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, field| !field.is_null());
    }
    value
}

/// An amount of bytes the way a person reads it.
pub(super) fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;
    use axum::http::{HeaderMap, StatusCode};

    fn reply(status: u16, body: &'static str) -> ApiReply {
        ApiReply {
            status: StatusCode::from_u16(status).unwrap(),
            content_type: "application/json".into(),
            headers: HeaderMap::new(),
            body: Bytes::from_static(body.as_bytes()),
        }
    }

    fn message(output: &ToolOutput) -> String {
        output.to_result()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn a_tool_failure_loses_what_it_quotes() {
        let out =
            ToolOutput::error("GET https://me:hunter2@nas/x?key=abc123: HTTP 502".to_string())
                .without_keys();
        let text = out.content[0]["text"].as_str().unwrap();
        assert!(
            !text.contains("hunter2") && !text.contains("abc123"),
            "{text}"
        );
        assert!(text.contains("HTTP 502"), "{text}");
    }

    #[test]
    fn every_tool_has_a_name_clients_accept_and_says_what_it_can_do() {
        let tools = all();
        let mut names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        assert!(names.len() >= 85, "{}", names.len());
        for tool in tools {
            assert!(
                !tool.name.is_empty()
                    && tool.name.len() <= 64
                    && tool
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{}",
                tool.name
            );
            assert!(!tool.title.is_empty(), "{}", tool.name);
            assert!(tool.description.len() > 20, "{}", tool.name);
            assert_eq!(tool.schema["type"], "object", "{}", tool.name);
            assert_eq!(tool.schema["additionalProperties"], false, "{}", tool.name);
            for required in tool.schema["required"].as_array().unwrap() {
                let required = required.as_str().unwrap();
                assert!(
                    tool.schema["properties"].get(required).is_some(),
                    "{} requires {required}, which it does not define",
                    tool.name
                );
            }
            for (name, rule) in tool.schema["properties"].as_object().unwrap() {
                assert!(
                    rule["description"].as_str().is_some_and(|d| d.len() > 5),
                    "{}.{name} has no description",
                    tool.name
                );
            }
        }
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two tools share a name");
    }

    #[test]
    fn the_tools_the_design_asks_for_are_all_there() {
        for name in [
            "engine_version",
            "engine_status",
            "consoles_list",
            "consoles_status",
            "consoles_readiness",
            "jobs_list",
            "jobs_get",
            "jobs_cancel",
            "jobs_pause",
            "jobs_resume",
            "jobs_wait",
            "files_list",
            "files_stat",
            "files_mkdir",
            "files_delete",
            "files_copy",
            "files_move",
            "files_read",
            "files_op_status",
            "files_op_cancel",
            "power_control",
            "power_wake",
            "hw_info",
            "hw_temps",
            "hw_storage",
            "processes_list",
            "games_installed",
            "games_launch",
            "games_stop",
            "rp_session",
            "rp_frame",
            "rp_input",
            "rp_input_release",
            "notifications_list",
            "api_catalog",
            "api_request",
            "upload_file",
            "upload_dir",
            "upload_archive",
            "files_download",
            "files_volumes",
            "files_write",
            "files_extract",
            "install_pkg",
            "install_wait",
            "saves_backup_now",
            "cheats_toggle",
            "payloads_list",
            "disk_usage_start",
            "cleanup_apply",
            "sources_fetch",
            "drive_sync",
            "notifications_clear",
            "consoles_discover",
            "rp_start",
            "rp_stop",
            "rp_settings",
            "rp_passcode",
            "rp_keyboard",
            "payloads_catalog",
            "payloads_download",
            "payloads_send",
            "files_hash",
            "shell_run",
            "settings_get",
            "settings_set",
        ] {
            assert!(find(name).is_some(), "{name}");
        }
    }

    #[test]
    fn annotations_say_what_a_tool_can_do() {
        let hints = |name: &str| {
            let v = find(name).unwrap().kind.annotations();
            (
                v["readOnlyHint"].as_bool().unwrap(),
                v["destructiveHint"].as_bool().unwrap(),
            )
        };
        assert_eq!(hints("files_list"), (true, false));
        assert_eq!(hints("files_read"), (true, false));
        assert_eq!(hints("files_delete"), (false, true));
        assert_eq!(hints("power_control"), (false, true));
        assert_eq!(hints("games_stop"), (false, true));
        assert_eq!(hints("files_mkdir"), (false, false));
        assert_eq!(hints("rp_input"), (false, false));
        assert_eq!(hints("api_request"), (false, true));
        assert_eq!(hints("api_catalog"), (true, false));
        assert_eq!(hints("upload_file"), (false, true));
        assert_eq!(hints("install_pkg"), (false, false));
        assert_eq!(hints("cleanup_scan"), (true, false));
        assert_eq!(hints("cleanup_apply"), (false, true));
        assert_eq!(hints("saves_restore"), (false, true));
        assert_eq!(hints("games_unregister"), (false, true));
        assert_eq!(hints("rp_stop"), (false, true));
        assert_eq!(hints("shell_run"), (false, true));
        assert_eq!(hints("payloads_send"), (false, true));
        assert_eq!(hints("consoles_discover"), (true, false));
        for tool in all() {
            assert_eq!(
                tool.kind.annotations()["openWorldHint"],
                false,
                "{}",
                tool.name
            );
        }
    }

    #[test]
    fn a_console_can_only_be_defaulted_on_a_read() {
        for tool in all() {
            let takes_console = tool.schema["properties"].get("console").is_some();
            let requires_console = tool.schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r == "console");
            let may_change = !matches!(tool.kind, Kind::Read);
            // These Remote Play tools find the console from the running session.
            let finds_its_console = matches!(
                tool.name,
                "rp_input_release" | "rp_stop" | "rp_settings" | "rp_passcode" | "rp_keyboard"
            );
            if takes_console && may_change && !finds_its_console {
                assert!(requires_console, "{}", tool.name);
            }
        }
    }

    fn schema() -> Value {
        object(
            json!({
                "console": console_required(),
                "path": {"type": "string", "description": "A path."},
                "size": {"type": "integer", "minimum": 1, "maximum": 10, "description": "A size."},
                "mode": {"type": "string", "enum": ["a", "b"], "description": "A mode."},
                "pair": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2, "description": "Two."},
            }),
            &["console"],
        )
    }

    fn check(args: Value) -> Result<(), String> {
        check_arguments(&schema(), args.as_object().unwrap())
    }

    #[test]
    fn a_misspelt_argument_is_refused_not_ignored() {
        let problem = check(json!({"console": "1.2.3.4", "addr": "1.2.3.4"})).unwrap_err();
        assert!(
            problem.contains("`addr`") && problem.contains("console"),
            "{problem}"
        );
    }

    #[test]
    fn required_arguments_must_say_something() {
        assert!(check(json!({})).unwrap_err().contains("`console`"));
        assert!(check(json!({"console": null})).is_err());
        assert!(check(json!({"console": "  "})).is_err());
        assert!(check(json!({"console": "1.2.3.4"})).is_ok());
    }

    #[test]
    fn values_are_checked_against_their_rules() {
        let ok = |extra: Value| {
            let mut args = extra;
            args["console"] = json!("c");
            check(args)
        };
        assert!(ok(json!({"size": 5, "mode": "a", "pair": [1, 2.5], "path": "/x"})).is_ok());
        assert!(ok(json!({"size": null})).is_ok());
        assert!(ok(json!({"size": "5"})).is_err());
        assert!(ok(json!({"size": 1.5})).is_err());
        assert!(ok(json!({"size": 0})).unwrap_err().contains("at least"));
        assert!(ok(json!({"size": 11})).unwrap_err().contains("at most"));
        assert!(ok(json!({"mode": "c"})).unwrap_err().contains("one of"));
        assert!(ok(json!({"pair": [1]})).is_err());
        assert!(ok(json!({"pair": [1, 2, 3]})).is_err());
        assert!(ok(json!({"pair": [1, "x"]})).is_err());
    }

    #[test]
    fn a_tool_without_arguments_says_so() {
        let schema = object(json!({}), &[]);
        let arguments = json!({"x": 1});
        let problem = check_arguments(&schema, arguments.as_object().unwrap()).unwrap_err();
        assert!(problem.contains("no arguments"), "{problem}");
    }

    #[test]
    fn structured_content_is_always_an_object() {
        let output = ToolOutput::ok("x", json!([1, 2])).to_result();
        assert_eq!(output["structuredContent"], json!({"items": [1, 2]}));
        assert_eq!(output["content"][1]["text"], r#"{"items":[1,2]}"#);
        let output = ToolOutput::ok("x", json!(3)).to_result();
        assert_eq!(output["structuredContent"], json!({"value": 3}));
        let output = ToolOutput::ok("x", json!({"a": 1})).to_result();
        assert_eq!(output["structuredContent"], json!({"a": 1}));
        assert!(output.get("isError").is_none());
    }

    #[test]
    fn a_failure_keeps_the_engines_words_and_adds_a_hint() {
        let output = engine_failure(
            "GET /api/ps5/status",
            &reply(
                502,
                r#"{"error":"NODE_STATUS: console_rest_mode: asleep","reason":"console_rest_mode","power":"rest"}"#,
            ),
        );
        let result = output.to_result();
        assert_eq!(result["isError"], true);
        let text = message(&output);
        assert!(text.contains("HTTP 502"), "{text}");
        assert!(text.contains("asleep"), "{text}");
        assert!(text.contains("rest mode"), "{text}");
        assert_eq!(result["structuredContent"]["reason"], "console_rest_mode");
        assert_eq!(result["structuredContent"]["body"]["power"], "rest");
    }

    #[test]
    fn automation_codes_get_their_own_hint() {
        let text = message(&engine_failure(
            "GET /api/ps5/rp/frame",
            &reply(409, r#"{"error":"no_viewer"}"#),
        ));
        assert!(
            text.contains("no_viewer") && text.contains("Play"),
            "{text}"
        );
        let text = message(&engine_failure(
            "GET /api/ps5/rp/frame",
            &reply(403, r#"{"error":"local_only"}"#),
        ));
        assert!(text.contains("localhost"), "{text}");
    }

    #[test]
    fn plain_text_and_empty_failures_still_read() {
        let text = message(&engine_failure("POST /api/x", &reply(500, "it broke")));
        assert!(text.contains("it broke"), "{text}");
        let text = message(&engine_failure("GET /api/x", &reply(404, "")));
        assert!(
            text.contains("HTTP 404") && text.contains("catalog"),
            "{text}"
        );
        let text = message(&engine_failure(
            "GET /api/x",
            &reply(404, r#"{"error":"Not Found","code":"not_found"}"#),
        ));
        assert!(text.contains("api_catalog"), "{text}");
        let text = message(&engine_failure(
            "GET /api/jobs/1",
            &reply(404, r#"{"error":"job not found"}"#),
        ));
        assert!(!text.contains("api_catalog"), "{text}");
    }

    #[test]
    fn a_200_that_says_ok_false_is_a_failure() {
        let err =
            success_json("POST /api/x", &reply(200, r#"{"ok":false,"err":"no"}"#)).unwrap_err();
        assert!(message(&err).contains("no"));
        let value = success_json("POST /api/x", &reply(200, r#"{"ok":true}"#)).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(
            success_json("POST /api/x", &reply(200, "")).unwrap(),
            json!({})
        );
        assert_eq!(
            success_json("GET /api/x", &reply(200, "plain")).unwrap(),
            json!({"text": "plain"})
        );
    }
}
