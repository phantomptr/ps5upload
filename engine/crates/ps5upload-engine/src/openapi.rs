//! `GET /api/openapi.json`: the API described for machines. The file is generated from this source
//! (`cargo run -p ps5upload-apidoc` in `engine/`), and two tests keep it honest: that crate's own,
//! which regenerates and compares, and `mcp/catalog_routes.rs`'s, which reads the router a second
//! way. The routes that hand out console keys are not in it.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde_json::Value;

/// What the file holds where the version goes, so a release does not change it.
const VERSION_PLACEHOLDER: &str = "@ENGINE_VERSION@";

const FILE: &str = include_str!("../api/openapi.json");

/// The document, with this engine's version.
pub(crate) fn document() -> &'static str {
    static DOCUMENT: OnceLock<String> = OnceLock::new();
    DOCUMENT.get_or_init(|| FILE.replacen(VERSION_PLACEHOLDER, env!("CARGO_PKG_VERSION"), 1))
}

/// The document's `paths`, parsed once, for the tools that look a route up.
fn paths() -> &'static BTreeMap<String, Value> {
    static PATHS: OnceLock<BTreeMap<String, Value>> = OnceLock::new();
    PATHS.get_or_init(|| {
        let parsed: Value = serde_json::from_str(FILE).unwrap_or(Value::Null);
        parsed
            .get("paths")
            .and_then(Value::as_object)
            .map(|paths| paths.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    })
}

/// `GET /api/openapi.json`.
pub(crate) async fn serve() -> Response {
    ([(header::CONTENT_TYPE, "application/json")], document()).into_response()
}

/// One route's description: what `api_describe` answers, and `ps5upload://openapi` in small.
pub(crate) fn operation(method: &str, path: &str) -> Option<&'static Value> {
    paths().get(path)?.get(method.to_ascii_lowercase())
}

/// A schema of the document, by name.
pub(crate) fn component(name: &str) -> Option<&'static Value> {
    static SCHEMAS: OnceLock<BTreeMap<String, Value>> = OnceLock::new();
    SCHEMAS
        .get_or_init(|| {
            let parsed: Value = serde_json::from_str(FILE).unwrap_or(Value::Null);
            parsed["components"]["schemas"]
                .as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default()
        })
        .get(name)
}

/// `(METHOD, path)` of everything the document describes.
#[cfg(test)]
pub(crate) fn operations() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (path, methods) in paths() {
        for method in methods.as_object().into_iter().flat_map(|m| m.keys()) {
            out.push((method.to_ascii_uppercase(), path.clone()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_is_openapi_with_this_engines_version() {
        let parsed: Value = serde_json::from_str(document()).expect("the file is JSON");
        assert_eq!(parsed["openapi"], "3.1.0");
        assert_eq!(parsed["info"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(operation("GET", "/api/version").is_some());
        assert!(operation("get", "/api/version").is_some());
        assert!(operation("GET", "/api/nope").is_none());
        assert!(component("Error").is_some());
    }

    #[test]
    fn every_ref_has_its_schema() {
        fn walk(value: &Value, refs: &mut Vec<String>) {
            match value {
                Value::Object(map) => {
                    if let Some(target) = map.get("$ref").and_then(Value::as_str) {
                        refs.push(target.to_string());
                    }
                    map.values().for_each(|v| walk(v, refs));
                }
                Value::Array(items) => items.iter().for_each(|v| walk(v, refs)),
                _ => {}
            }
        }
        let parsed: Value = serde_json::from_str(FILE).unwrap();
        let mut refs = Vec::new();
        walk(&parsed, &mut refs);
        assert!(refs.len() > 100);
        for target in refs {
            let pointer = target.trim_start_matches('#');
            assert!(parsed.pointer(pointer).is_some(), "{target}");
        }
    }
}
