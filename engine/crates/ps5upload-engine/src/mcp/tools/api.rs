//! `api_catalog` and `api_request`: any route of the engine's API, for what the curated tools do
//! not cover.

use std::collections::BTreeSet;

use axum::http::Method;
use base64::Engine as _;
use serde_json::{json, Map, Value};

use super::{engine_failure, object, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult};
use crate::mcp::catalog_routes::{self, ROUTES};
use crate::openapi;

/// Largest image shown as a picture as well as data.
const MAX_INLINE_IMAGE_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
        name: "api_catalog",
        title: "Find an API route",
        description: "Searches the routes of the engine's HTTP API, by method and path: what \
                      api_request can call. Every word of `search` must appear. The same list as \
                      ps5upload://catalog, for clients that do not read resources.",
        kind: Kind::Read,
        schema: object(
            json!({
                "search": {
                    "type": "string",
                    "description": "Words to look for in a route's path, like `save backup`. \
                                    Left out, every route.",
                },
                "method": {
                    "type": "string",
                    "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"],
                    "description": "Only routes with this method.",
                },
            }),
            &[],
        ),
        run: |ctx, args| Box::pin(catalog(ctx, args)),
    },
    ToolDef {
        name: "api_describe",
        title: "Describe an API route",
        description: "One route of the engine's API: what it does, its query and body fields, its \
                      answers, who may call it, and how its console is named. From the same \
                      document as GET /api/openapi.json, with the schemas it uses. Read this before \
                      calling an unfamiliar route with api_request: never guess a field.",
        kind: Kind::Read,
        schema: object(
            json!({
                "method": {
                    "type": "string",
                    "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"],
                    "description": "The HTTP method. Default GET.",
                },
                "path": {
                    "type": "string",
                    "description": "The route as api_catalog lists it, like /api/jobs/{id}.",
                },
            }),
            &["path"],
        ),
        run: |ctx, args| Box::pin(describe(ctx, args)),
    },
    ToolDef {
        name: "api_request",
        title: "Call the engine's API",
        description: "Calls any route of the engine's HTTP API, for what the other tools do not \
                      cover. ps5upload://catalog lists every route. Most routes name a console as \
                      `addr`, in the query for a GET and in the body for a POST. It can do anything \
                      the API can, including what cannot be undone: confirm with the user first \
                      unless it only reads. Answers with `status` and `body`; any status but 2xx \
                      is an error.",
        kind: Kind::Generic,
        schema: object(
            json!({
                "method": {
                    "type": "string",
                    "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"],
                    "description": "The HTTP method. Default GET.",
                },
                "path": {
                    "type": "string",
                    "description": "The route, starting with /api/, like /api/ps5/volumes. Query \
                                    parameters go in `query`.",
                },
                "query": {
                    "type": "object",
                    "description": "Query parameters: a name and a string, number or boolean each.",
                },
                "body": {
                    "type": "object",
                    "description": "The JSON body. Routes that read one need it, even if only {}.",
                },
            }),
            &["path"],
        ),
        run: |ctx, args| Box::pin(request(ctx, args)),
    },
    ]
}

/// The routes under `/api/` a search finds, in the catalog's order.
fn find_routes(search: &str, method: Option<&str>) -> Vec<Value> {
    let words: Vec<String> = search
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    ROUTES
        .iter()
        .filter(|(route_method, path)| {
            let lower = path.to_ascii_lowercase();
            path.starts_with("/api/")
                && method.is_none_or(|wanted| wanted.eq_ignore_ascii_case(route_method))
                && words.iter().all(|word| lower.contains(word.as_str()))
        })
        .map(|(method, path)| {
            let mut route = json!({ "method": method, "path": path });
            if let Some(summary) = openapi::operation(method, path).and_then(|op| op.get("summary"))
            {
                route["summary"] = summary.clone();
            }
            route
        })
        .collect()
}

/// Every `#/components/schemas/...` name in `value`.
fn schema_names(value: &Value, names: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(name) = map
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|target| target.strip_prefix("#/components/schemas/"))
            {
                names.insert(name.to_string());
            }
            map.values().for_each(|v| schema_names(v, names));
        }
        Value::Array(items) => items.iter().for_each(|v| schema_names(v, names)),
        _ => {}
    }
}

async fn describe(_ctx: ToolCtx, args: Args) -> ToolResult {
    let path = args.text("path").unwrap_or_default();
    let method = args.text("method").unwrap_or("GET");
    if catalog_routes::holds_console_keys(path) {
        return Err(ToolOutput::error(format!(
            "{path} holds the keys that sign in to a console's users: it is for the app alone."
        )));
    }
    let Some(operation) = openapi::operation(method, path) else {
        return Err(ToolOutput::error(format!(
            "No route {method} {path}. api_catalog finds routes; write {{name}} parts as it lists \
             them."
        )));
    };
    // The schemas it names, and the ones those name.
    let mut names = BTreeSet::new();
    schema_names(operation, &mut names);
    let mut schemas = Map::new();
    while let Some(name) = names.iter().find(|n| !schemas.contains_key(*n)).cloned() {
        match openapi::component(&name) {
            Some(schema) => {
                schema_names(schema, &mut names);
                schemas.insert(name, schema.clone());
            }
            None => {
                schemas.insert(name, Value::Null);
            }
        }
    }
    Ok(ToolOutput::ok(
        format!("{method} {path}."),
        json!({ "method": method, "path": path, "operation": operation, "schemas": schemas }),
    ))
}

async fn catalog(_ctx: ToolCtx, args: Args) -> ToolResult {
    let routes = find_routes(args.text("search").unwrap_or_default(), args.text("method"));
    let summary = format!("{} route(s).", routes.len());
    Ok(ToolOutput::ok(
        summary,
        json!({ "count": routes.len(), "routes": routes }),
    ))
}

/// Why this cannot be a path of the API, if it cannot.
fn path_problem(path: &str) -> Option<String> {
    if !path.starts_with("/api/") {
        return Some(format!(
            "`path` must start with /api/, but it is {path:?}. This tool calls the engine's API \
             only."
        ));
    }
    if path.contains(['?', '#']) {
        return Some("Put query parameters in `query`, not in `path`.".to_string());
    }
    if !path.bytes().all(|b| (0x21..=0x7e).contains(&b)) || path.contains('\\') {
        return Some("`path` holds a character a URL path cannot: percent-encode it.".to_string());
    }
    if path.split('/').any(|segment| {
        let segment = segment.to_ascii_lowercase().replace("%2e", ".");
        segment == ".." || segment == "."
    }) {
        return Some("`path` must not hold `.` or `..` segments.".to_string());
    }
    None
}

/// Query parameters as text. A list has no one way to go in a query: the route's own way is for
/// the caller to spell out.
fn query_pairs(query: Option<&Map<String, Value>>) -> Result<Vec<(&str, String)>, String> {
    let mut out = Vec::new();
    for (name, value) in query.into_iter().flatten() {
        out.push((
            name.as_str(),
            match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                Value::Bool(flag) => flag.to_string(),
                _ => {
                    return Err(format!(
                        "Query parameter `{name}` must be a string, number or boolean."
                    ))
                }
            },
        ));
    }
    Ok(out)
}

async fn request(ctx: ToolCtx, args: Args) -> ToolResult {
    let method = args.text("method").unwrap_or("GET").to_ascii_uppercase();
    let path = args.required("path")?;
    // First: whatever else is wrong with the path, this is the answer an agent needs.
    if catalog_routes::reaches_agent_access(path) {
        return Err(ToolOutput::error(format!(
            "{method} {path} is refused: whether agents can change things is the owner's to \
             decide, in the app under MCP & API › Connect an agent."
        )));
    }
    if let Some(problem) = path_problem(path) {
        return Err(ToolOutput::error(problem));
    }
    if catalog_routes::holds_console_keys(path) {
        return Err(ToolOutput::error(format!(
            "{path} is refused: it holds the keys that sign in to a console, which stay with the \
             app. GET /api/consoles says whether a console has keys."
        )));
    }
    if ctx.state.read_only() && method != "GET" {
        return Err(ToolOutput::error(format!(
            "{method} {path} is refused (only GET requests are allowed): {}",
            crate::mcp::access::refusal(ctx.state.access().locked())
        )));
    }
    let Ok(http_method) = Method::from_bytes(method.as_bytes()) else {
        return Err(ToolOutput::error(format!(
            "{method} is not an HTTP method."
        )));
    };
    let query =
        query_pairs(args.value("query").and_then(Value::as_object)).map_err(ToolOutput::error)?;
    let label = format!("{method} {path}");
    let reply = ctx
        .reply(http_method, path, &query, args.value("body"))
        .await?;
    if !reply.is_success() {
        return Err(engine_failure(&label, &reply));
    }
    let status = reply.status.as_u16();
    let summary = format!("{label} answered {status}.");
    if let Some(body) = reply.json() {
        if body.get("ok").and_then(Value::as_bool) == Some(false) {
            return Err(engine_failure(&label, &reply));
        }
        return Ok(ToolOutput::ok(
            summary,
            json!({ "status": status, "body": body }),
        ));
    }
    if let Ok(text) = std::str::from_utf8(&reply.body) {
        return Ok(ToolOutput::ok(
            summary,
            json!({ "status": status, "body": text }),
        ));
    }
    // Bytes: base64 in the data, and a picture when it is one a client can show.
    let encoded = base64::engine::general_purpose::STANDARD.encode(&reply.body);
    let output = ToolOutput::ok(
        format!(
            "{summary} {} bytes of {}.",
            reply.body.len(),
            reply.content_type
        ),
        json!({
            "status": status,
            "content_type": reply.content_type,
            "size": reply.body.len(),
            "base64": encoded,
        }),
    );
    Ok(
        if reply.content_type.starts_with("image/") && reply.body.len() <= MAX_INLINE_IMAGE_BYTES {
            output.with_image(encoded, &reply.content_type)
        } else {
            output
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_paths_of_the_api_are_taken() {
        for fine in [
            "/api/version",
            "/api/jobs/1/cancel",
            "/api/a%20b",
            "/api/ps5/rp/live.ts",
        ] {
            assert_eq!(path_problem(fine), None, "{fine}");
        }
        for bad in [
            "",
            "/",
            "/mcp",
            "/pkg-host/a/b",
            "/rp-player",
            "api/version",
            "http://example.com/api/version",
            "/apiary",
            "/api/../mcp",
            "/api/%2e%2e/mcp",
            "/api/%2E%2e/mcp",
            "/api/./version",
            "/api/version?addr=1",
            "/api/version#x",
            "/api/a b",
            "/api/ゲーム",
            "/api/a\\b",
            "/api/a\nb",
        ] {
            assert!(path_problem(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn a_search_finds_routes_of_the_api_only() {
        let paths = |search: &str, method: Option<&str>| -> Vec<String> {
            find_routes(search, method)
                .iter()
                .map(|route| route["path"].as_str().unwrap().to_string())
                .collect()
        };
        let found = paths("ps5 volumes", None);
        assert_eq!(found, ["/api/ps5/volumes"]);
        assert_eq!(paths("PS5 VOLUMES", Some("get")), ["/api/ps5/volumes"]);
        assert!(paths("ps5 volumes", Some("DELETE")).is_empty());
        assert!(paths("no such route anywhere", None).is_empty());
        let every = paths("", None);
        assert!(every.len() > 300);
        assert!(every.iter().all(|path| path.starts_with("/api/")));
    }

    #[test]
    fn query_values_become_text() {
        let query = json!({"a": "x", "b": 3, "c": true});
        let pairs = query_pairs(query.as_object()).unwrap();
        assert_eq!(
            pairs,
            [
                ("a", "x".to_string()),
                ("b", "3".to_string()),
                ("c", "true".to_string())
            ]
        );
        assert!(query_pairs(None).unwrap().is_empty());
        let query = json!({"a": [1]});
        assert!(query_pairs(query.as_object()).unwrap_err().contains("`a`"));
    }
}
