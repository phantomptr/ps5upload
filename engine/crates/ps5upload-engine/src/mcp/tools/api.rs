//! `api_request`: any route of the engine's API, for what the curated tools do not cover.

use axum::http::Method;
use base64::Engine as _;
use serde_json::{json, Map, Value};

use super::{engine_failure, object, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult};

/// Largest image shown as a picture as well as data.
const MAX_INLINE_IMAGE_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn tools() -> Vec<ToolDef> {
    vec![ToolDef {
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
                    "description": "The JSON body. Routes that read one need it, even if only {}.",
                },
            }),
            &["path"],
        ),
        run: |ctx, args| Box::pin(request(ctx, args)),
    }]
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
    if let Some(problem) = path_problem(path) {
        return Err(ToolOutput::error(problem));
    }
    if ctx.state.read_only() && method != "GET" {
        return Err(ToolOutput::error(format!(
            "{method} {path} is refused: this engine's MCP server is read-only \
             (PS5UPLOAD_MCP_READ_ONLY), so only GET requests are allowed. The owner can restart \
             the engine without that setting to allow it."
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
