use serde_json::{json, Value};

use crate::analyze::analyze;
use crate::routes;
use crate::schema::{apply_rename_all, Dir, Schemas};
use crate::source::Index;
use crate::{crates_dir, spec};

const SOURCE: &str = r#"
    use serde_json::Value;
    use std::collections::HashMap;

    /// A body.
    #[derive(Deserialize, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Body {
        /// Which console.
        #[serde(alias = "console", alias = "host")]
        pub console_addr: Option<String>,
        pub file_size: u64,
        #[serde(default)]
        pub with_default: bool,
        #[serde(rename = "named", skip_serializing_if = "Option::is_none")]
        pub renamed: Option<i32>,
        #[serde(skip_deserializing)]
        pub only_out: String,
        #[serde(skip_serializing)]
        pub only_in: String,
        #[serde(skip)]
        pub never: String,
        #[serde(flatten)]
        pub more: More,
        pub anything: Value,
        pub many: Vec<More>,
        pub map: HashMap<String, u8>,
        #[serde(deserialize_with = "odd", serialize_with = "odd2")]
        pub custom: u8,
    }

    #[derive(Deserialize, Serialize)]
    pub struct More { pub inner: String }

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Plain { FirstOne, SecondOne }

    #[derive(Serialize)]
    #[serde(tag = "status", rename_all = "lowercase")]
    pub enum Tagged {
        Running { done: u64 },
        Failed,
    }

    pub struct NoDerive { pub secret: String }

    #[derive(Deserialize)]
    pub struct Q { pub addr: Option<String>, pub limit: u32 }

    #[derive(Serialize)]
    pub struct Reply { pub ok: bool }

    /// `GET /api/x?addr=` — Reads the thing. And more.
    pub async fn read_it(Query(q): Query<Q>) -> Json<Reply> {
        Json(Reply { ok: true })
    }

    pub async fn with_literal(Json(b): Json<Body>) -> Response {
        if b.file_size == 0 {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "no", "field": "size"}))).into_response();
        }
        (StatusCode::ACCEPTED, Json(json!({"job_id": "x", "count": 1, "on": true, "who": b.renamed}))).into_response()
    }

    pub async fn passes_on(Json(b): Json<Body>) -> Response {
        with_literal(Json(b)).await
    }

    pub async fn guarded(headers: HeaderMap, ConnectInfo(peer): ConnectInfo<SocketAddr>) -> Response {
        if !vinput_api::host_is_local(&headers) { return no(); }
        if !vinput_api::local_allowed(peer.ip()) { return no(); }
        Json(helper_value()).into_response()
    }

    fn helper_value() -> Value { Value::Null }

    #[cfg(not(target_os = "ios"))]
    pub async fn on_most_platforms(Json(b): Json<More>) -> Json<Reply> { Json(Reply { ok: true }) }
    #[cfg(target_os = "ios")]
    pub async fn on_most_platforms() -> Response { refuse() }
    #[cfg(target_os = "ios")]
    pub async fn stub_first() -> Response { refuse() }
    #[cfg(not(target_os = "ios"))]
    pub async fn stub_first(Json(b): Json<More>) -> Json<Reply> { Json(Reply { ok: true }) }

    pub fn sub_router() -> Router {
        Router::new().route("/sub/{id}", delete(read_it))
    }

    pub fn build_app_and_mcp() -> Router {
        let extra = sub_router();
        Router::new()
            .route("/one", get(read_it).post(with_literal))
            .route("/shared", get({ let s = 1; move |q: Query<Q>| read_it(q) }))
            .route("/guarded", post(guarded))
            .merge(extra)
    }
"#;

fn schema_of(text: &str, dir: Dir) -> (Value, Value) {
    let index = Box::leak(Box::new(
        Index::of_source("ps5upload_engine", SOURCE).unwrap(),
    ));
    let mut schemas = Schemas::new(index);
    let ty: syn::Type = syn::parse_str(text).unwrap();
    let schema = schemas.of_type(&ty, "ps5upload_engine", dir, &[]);
    let target = schema["$ref"]
        .as_str()
        .map(|r| r.trim_start_matches("#/components/schemas/").to_string());
    let body = target
        .and_then(|t| {
            let (path, side) = t.rsplit_once('@')?;
            let side = if side == "in" { Dir::In } else { Dir::Out };
            schemas.done.get(&(path.to_string(), side)).cloned()
        })
        .unwrap_or(Value::Null);
    (schema, body)
}

#[test]
fn rename_all_spells_like_serde() {
    assert_eq!(
        apply_rename_all("console_addr", Some("camelCase")),
        "consoleAddr"
    );
    assert_eq!(
        apply_rename_all("FirstOne", Some("snake_case")),
        "first_one"
    );
    assert_eq!(
        apply_rename_all("FirstOne", Some("kebab-case")),
        "first-one"
    );
    assert_eq!(apply_rename_all("Running", Some("lowercase")), "running");
    assert_eq!(apply_rename_all("a_b", Some("SCREAMING_SNAKE_CASE")), "A_B");
    assert_eq!(apply_rename_all("a_b", None), "a_b");
}

#[test]
fn a_request_struct_reads_as_serde_would() {
    let (_, body) = schema_of("Body", Dir::In);
    let object = &body["allOf"][0];
    let properties = &object["properties"];
    assert_eq!(properties["consoleAddr"]["type"], json!(["string", "null"]));
    assert_eq!(properties["consoleAddr"]["description"], "Which console.");
    assert_eq!(
        properties["consoleAddr"]["x-aliases"],
        json!(["console", "host"])
    );
    assert_eq!(properties["fileSize"]["minimum"], 0);
    assert!(properties.get("onlyOut").is_none());
    assert!(properties.get("never").is_none());
    assert!(properties.get("onlyIn").is_some());
    assert_eq!(properties["named"]["type"], json!(["integer", "null"]));
    assert_eq!(properties["anything"], json!({}));
    assert_eq!(properties["map"]["additionalProperties"]["minimum"], 0);
    // Its own deserializer: the Rust type does not say what the JSON is.
    assert!(properties["custom"].get("type").is_none());
    let required: Vec<&str> = object["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"fileSize"));
    assert!(!required.contains(&"consoleAddr"));
    assert!(!required.contains(&"withDefault"));
    // The flattened struct sits beside the object.
    assert!(body["allOf"][1]["$ref"]
        .as_str()
        .unwrap()
        .ends_with("More@in"));
}

#[test]
fn a_response_struct_differs_where_serde_differs() {
    let (_, body) = schema_of("Body", Dir::Out);
    let object = &body["allOf"][0];
    assert!(object["properties"].get("onlyOut").is_some());
    assert!(object["properties"].get("onlyIn").is_none());
    let required = object["required"].as_array().unwrap();
    assert!(required.contains(&json!("consoleAddr")));
    assert!(!required.contains(&json!("named")), "skip_serializing_if");
    assert!(object["properties"]["consoleAddr"]
        .get("x-aliases")
        .is_none());
}

#[test]
fn enums_read_as_serde_would() {
    let (_, plain) = schema_of("Plain", Dir::Out);
    assert_eq!(plain["enum"], json!(["first_one", "second_one"]));
    let (_, tagged) = schema_of("Tagged", Dir::Out);
    let variants = tagged["oneOf"].as_array().unwrap();
    assert_eq!(variants.len(), 2);
    let text = tagged.to_string();
    assert!(text.contains(r#""const":"running""#), "{text}");
    assert!(text.contains(r#""const":"failed""#), "{text}");
    assert!(text.contains(r#""done""#), "{text}");
}

#[test]
fn a_type_with_no_derive_never_invents_its_fields() {
    let (_, body) = schema_of("NoDerive", Dir::Out);
    assert!(body.get("properties").is_none());
    assert_eq!(body["x-rust-type"], "NoDerive");
    let (outside, _) = schema_of("other_crate::Thing", Dir::Out);
    assert_eq!(outside["x-rust-type"], "other_crate::Thing");
}

#[test]
fn std_names_and_foreign_values_are_not_taken_for_local_ones() {
    let (value, _) = schema_of("Value", Dir::Out);
    assert_eq!(value, json!({}));
    let (list, _) = schema_of("Vec<Option<String>>", Dir::Out);
    assert_eq!(list["items"]["type"], json!(["string", "null"]));
}

#[test]
fn routes_come_from_every_way_they_are_written() {
    let index = Index::of_source("ps5upload_engine", SOURCE).unwrap();
    let found: Vec<String> = routes::scan(&index)
        .unwrap()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert_eq!(
        found,
        [
            "GET /one",
            "POST /one",
            "GET /shared",
            "POST /guarded",
            "DELETE /sub/{id}"
        ]
    );
}

#[test]
fn an_unreadable_route_is_an_error_and_not_a_gap() {
    for bad in [
        r#"pub fn build_app_and_mcp() { Router::new().route(path_var(), get(x)); }"#,
        r#"pub fn build_app_and_mcp() { Router::new().route("/a", any(x)); }"#,
        r#"pub fn build_app_and_mcp() { Router::new().route("/a", methods); }"#,
        r#"pub fn build_app_and_mcp() { Router::new().merge(who_knows()); }"#,
        r#"pub fn build_app_and_mcp() { Router::new().nest("/a", other); }"#,
    ] {
        let index = Index::of_source("ps5upload_engine", bad).unwrap();
        assert!(routes::scan(&index).is_err(), "{bad}");
    }
}

fn analysis_of(path: &str, method: &str) -> crate::analyze::Analysis {
    let index = Box::leak(Box::new(
        Index::of_source("ps5upload_engine", SOURCE).unwrap(),
    ));
    let found = routes::scan(index).unwrap();
    let route = found
        .iter()
        .find(|r| r.path == path && r.method == method)
        .unwrap();
    let mut schemas = Schemas::new(index);
    analyze(index, &mut schemas, &route.handler)
}

#[test]
fn a_literal_gives_its_keys_and_the_types_the_source_shows() {
    let analysis = analysis_of("/one", "POST");
    let created = &analysis.bodies[&Some(202)][0].0;
    assert_eq!(created["properties"]["job_id"]["type"], "string");
    assert_eq!(created["properties"]["count"]["type"], "integer");
    assert_eq!(created["properties"]["on"]["type"], "boolean");
    assert_eq!(created["properties"]["who"], json!({}));
    assert!(analysis.statuses.contains(&400));
    assert!(analysis.bodies.contains_key(&Some(400)));
}

#[test]
fn a_struct_answer_comes_from_the_signature_and_the_body_alike() {
    let analysis = analysis_of("/one", "GET");
    let list = &analysis.bodies[&None];
    assert_eq!(list.len(), 1);
    assert!(list[0].0["$ref"].as_str().unwrap().ends_with("Reply@out"));
    assert!(list[0].1);
}

#[test]
fn a_closure_forwards_to_the_function_that_describes_it() {
    let analysis = analysis_of("/shared", "GET");
    assert!(analysis.doc.starts_with("`GET /api/x?addr=`"));
    assert_eq!(analysis.extractors.len(), 1);
}

#[test]
fn local_only_checks_are_seen_and_unknown_json_stays_unknown() {
    let analysis = analysis_of("/guarded", "POST");
    assert!(analysis.checks_host && analysis.checks_peer);
    assert!(analysis.bodies.is_empty());
    assert!(analysis.saw_json);
}

#[test]
fn the_real_function_beats_a_stub_for_another_platform() {
    let index = Index::of_source("ps5upload_engine", SOURCE).unwrap();
    for name in ["on_most_platforms", "stub_first"] {
        let func = &index.funcs[&format!("ps5upload_engine::{name}")];
        assert_eq!(func.sig.inputs.len(), 1, "{name}");
    }
}

#[test]
fn the_check_names_what_changed() {
    let old = r#"{"paths":{"/a":{"get":{"x":1}},"/b":{"get":{"x":1}}}}"#;
    let new = r#"{"paths":{"/a":{"get":{"x":2}},"/c":{"post":{"x":1}}}}"#;
    let text = spec::difference(old, new).unwrap();
    assert!(text.contains("new route: POST /c"), "{text}");
    assert!(text.contains("route gone: GET /b"), "{text}");
    assert!(text.contains("changed: GET /a"), "{text}");
    assert!(text.contains(spec::REGENERATE), "{text}");
    assert_eq!(spec::difference(old, old), None);
}

/// The gate: what CI runs in the `libs` leg of the Rust tests. A route or struct changed without
/// regenerating the file fails here, and the message says how to fix it.
#[test]
fn the_committed_file_is_what_the_source_generates() {
    let crates = crates_dir();
    let generated = spec::generate(&crates).expect("the source can be read");
    let again = spec::generate(&crates).expect("the source can be read");
    assert_eq!(
        generated.text, again.text,
        "the output must be deterministic"
    );
    let committed = std::fs::read_to_string(spec::output_path(&crates)).unwrap_or_default();
    if let Some(problem) = spec::difference(&committed, &generated.text) {
        panic!("{problem}");
    }
}

#[test]
fn the_file_keeps_console_keys_out_and_marks_local_routes() {
    let crates = crates_dir();
    let generated = spec::generate(&crates).unwrap();
    let parsed: Value = serde_json::from_str(&generated.text).unwrap();
    let paths = parsed["paths"].as_object().unwrap();
    assert!(!paths.contains_key("/api/consoles/document"));
    assert!(!paths.contains_key("/api/settings-backup/get"));
    assert!(paths.contains_key("/api/openapi.json"));
    let start = &paths["/api/ps5/rp/start"]["post"];
    assert_eq!(start["x-ps5upload"]["access"], "strict-local");
    assert!(start["tags"]
        .as_array()
        .unwrap()
        .contains(&json!("strict-local")));
    assert_eq!(
        paths["/api/jobs"]["get"]["x-ps5upload"]["access"],
        "loopback"
    );
    assert_eq!(paths["/pad"]["get"]["x-ps5upload"]["access"], "lan-open");
    assert_eq!(parsed["info"]["version"], spec::VERSION_PLACEHOLDER);
}
