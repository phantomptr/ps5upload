//! Tools that are one call of one route: the tool's arguments are the route's fields, under the
//! same names, except `console`, which goes by the route's own name for it. A [`route!`] states the
//! route; [`run`] does the rest, with the same errors as any other tool.

use axum::http::Method;
use serde_json::{json, Map, Value};

use super::{files, success_json, Args, ToolCtx, ToolOutput, ToolResult};
use crate::mcp::encode_component;

/// One route, as a tool calls it.
pub(super) struct Route {
    pub method: &'static str,
    /// May name arguments as `{name}`: each goes in the path, percent-encoded.
    pub path: &'static str,
    /// The route's name for the console.
    pub console: &'static str,
    /// A `POST` that reads its fields from the query. A `GET` always does.
    pub in_query: bool,
    /// The route can block for long: the tool takes `wait_s` and `op_id` (see `files.rs`).
    pub long: bool,
    /// What a success says.
    pub done: &'static str,
}

impl Route {
    pub const DEFAULT: Route = Route {
        method: "GET",
        path: "",
        console: "addr",
        in_query: false,
        long: false,
        done: "Done.",
    };
}

/// A tool's `run` for a route. A closure with its own `static`, so it stays a plain function
/// pointer like every other tool's.
macro_rules! route {
    ($($field:ident: $value:expr),* $(,)?) => {
        |ctx, args| {
            static ROUTE: $crate::mcp::tools::route::Route = $crate::mcp::tools::route::Route {
                $($field: $value,)*
                ..$crate::mcp::tools::route::Route::DEFAULT
            };
            Box::pin($crate::mcp::tools::route::run(ctx, args, &ROUTE))
        }
    };
}
pub(super) use route;

/// A string argument's schema.
pub(super) fn text(description: &'static str) -> Value {
    json!({ "type": "string", "description": description })
}

/// A whole-number argument's schema.
pub(super) fn whole(description: &'static str) -> Value {
    json!({ "type": "integer", "minimum": 0, "description": description })
}

/// A yes-or-no argument's schema.
pub(super) fn flag(description: &'static str) -> Value {
    json!({ "type": "boolean", "description": description })
}

/// A value as it goes in a query or a path.
fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Where an argument goes.
#[derive(Default)]
struct Request<'a> {
    path: String,
    query: Vec<(&'a str, String)>,
    body: Map<String, Value>,
}

fn build<'a>(route: &'a Route, args: &'a Args) -> Request<'a> {
    let in_query = route.in_query || matches!(route.method, "GET" | "DELETE");
    let mut request = Request {
        path: route.path.to_string(),
        ..Request::default()
    };
    for (name, value) in args.entries() {
        if value.is_null() || (route.long && name == "wait_s") {
            continue;
        }
        let placeholder = format!("{{{name}}}");
        if request.path.contains(&placeholder) {
            request.path = request
                .path
                .replace(&placeholder, &encode_component(&as_text(value)));
            continue;
        }
        let field = if name == "console" {
            route.console
        } else {
            name.as_str()
        };
        if in_query {
            request.query.push((field, as_text(value)));
        } else {
            request.body.insert(field.to_string(), value.clone());
        }
    }
    request
}

/// Calls the route with the tool's arguments.
pub(super) async fn run(ctx: ToolCtx, args: Args, route: &'static Route) -> ToolResult {
    let request = build(route, &args);
    if request.path.contains('{') {
        return Err(ToolOutput::error(format!(
            "A required argument of this tool is missing: {} needs it.",
            request.path
        )));
    }
    let body = Value::Object(request.body);
    if route.long {
        return files::long_operation(ctx, &args, route.path, body, route.done.to_string()).await;
    }
    let Ok(method) = Method::from_bytes(route.method.as_bytes()) else {
        return Err(ToolOutput::error(format!(
            "{} is not an HTTP method.",
            route.method
        )));
    };
    let sends_body = !matches!(route.method, "GET" | "DELETE");
    let reply = ctx
        .reply(
            method,
            &request.path,
            &request.query,
            sends_body.then_some(&body),
        )
        .await?;
    let answer = success_json(&format!("{} {}", route.method, request.path), &reply)?;
    let summary = match answer.get("job_id").and_then(Value::as_str) {
        Some(job) => format!("{} Job {job}: jobs_wait follows it.", route.done),
        None => route.done.to_string(),
    };
    Ok(ToolOutput::ok(summary, answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(value: Value) -> Args {
        match value {
            Value::Object(map) => Args(map),
            _ => Args::default(),
        }
    }

    #[test]
    fn arguments_go_where_the_route_reads_them() {
        let post = Route {
            method: "POST",
            path: "/api/x/{id}/go",
            console: "host",
            ..Route::DEFAULT
        };
        let given = args(json!({"console": "1.2.3.4", "id": "a/b", "n": 3, "none": null}));
        let request = build(&post, &given);
        assert_eq!(request.path, "/api/x/a%2Fb/go");
        assert!(request.query.is_empty());
        assert_eq!(
            Value::Object(request.body),
            json!({"host": "1.2.3.4", "n": 3})
        );

        let get = Route {
            path: "/api/y",
            ..Route::DEFAULT
        };
        let given = args(json!({"console": "1.2.3.4", "on": true}));
        let mut query = build(&get, &given).query;
        query.sort();
        assert_eq!(
            query,
            [("addr", "1.2.3.4".to_string()), ("on", "true".to_string())]
        );

        let post_in_query = Route {
            method: "POST",
            in_query: true,
            ..Route::DEFAULT
        };
        let given = args(json!({"console": "1.2.3.4"}));
        let request = build(&post_in_query, &given);
        assert_eq!(request.query, [("addr", "1.2.3.4".to_string())]);
        assert!(request.body.is_empty());
    }

    #[test]
    fn a_long_routes_wait_stays_with_the_tool() {
        let long = Route {
            method: "POST",
            long: true,
            ..Route::DEFAULT
        };
        let given = args(json!({"wait_s": 5, "path": "/x"}));
        let request = build(&long, &given);
        assert_eq!(Value::Object(request.body), json!({"path": "/x"}));
    }
}
