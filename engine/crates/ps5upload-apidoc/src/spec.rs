//! The OpenAPI 3.1 document: routes, what each handler takes and answers, and `api/overrides.json`
//! for what the source cannot say. Keys are written sorted, so the same source gives the same file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use quote::ToTokens;
use serde_json::{json, Map, Value};
use syn::Type;

use crate::analyze::{analyze, Analysis, Extractor};
use crate::routes::{self, Route};
use crate::schema::{Dir, FieldInfo, Schemas};
use crate::source::{Def, Index};

/// What `info.version` holds in the file: the engine puts its own version there when it serves it,
/// so a release does not change the file.
pub const VERSION_PLACEHOLDER: &str = "@ENGINE_VERSION@";

/// The line every failure of the check ends with.
pub const REGENERATE: &str = "cargo run -p ps5upload-apidoc";

pub struct Generated {
    pub text: String,
    pub stats: String,
}

pub fn output_path(crates: &Path) -> PathBuf {
    crates.join("ps5upload-engine/api/openapi.json")
}

/// Sorted keys, whatever order the map kept.
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> =
                map.into_iter().map(|(k, v)| (k, canonical(v))).collect();
            let mut out = Map::new();
            for (k, v) in sorted {
                out.insert(k, v);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

/// The routes that hold console keys: named once, in the engine's own catalog.
fn secret_paths(index: &Index) -> Result<Vec<String>, String> {
    let key = "ps5upload_engine::mcp::catalog_routes::HOLD_CONSOLE_KEYS";
    let (_, def) = index
        .defs
        .get(key)
        .ok_or_else(|| format!("{key} is gone: this generator must follow it"))?;
    let Def::Const(item) = def else {
        return Err(format!("{key} is no longer a const"));
    };
    let text = item.expr.to_token_stream().to_string();
    let paths: Vec<String> = text
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    if paths.is_empty() {
        return Err(format!("{key} lists nothing"));
    }
    Ok(paths)
}

/// `(summary, description)` from a handler's doc comment. The first line often repeats the route
/// (`` `GET /api/x?addr=` — what it does ``), which the summary does not need.
fn split_doc(doc: &str) -> (Option<String>, Option<String>) {
    if doc.is_empty() {
        return (None, None);
    }
    // The first paragraph as one line: a doc comment wraps where the source's width says.
    let paragraph = doc
        .lines()
        .map(str::trim)
        .take_while(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut first = paragraph.as_str();
    if let Some(rest) = first.strip_prefix('`') {
        if let Some((code, after)) = rest.split_once('`') {
            let verb = code.split_whitespace().next().unwrap_or_default();
            if verb.chars().all(|c| c.is_ascii_uppercase()) || code.starts_with('/') {
                first = after;
            }
        }
    } else if let Some(word) = first.split_whitespace().next() {
        // `GET /api/x?y= — what it does`, without the backticks.
        if matches!(
            word,
            "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "GET/HEAD"
        ) {
            let route_end = first[word.len()..]
                .trim_start()
                .find(' ')
                .map(|at| first.len() - first[word.len()..].trim_start().len() + at);
            first = route_end.map_or("", |end| &first[end..]);
        }
    }
    // What a route's fields are (`{addr, path}`) is for the schema, not the summary.
    let first = skip_leading_shape(first);
    let first = first.trim_start_matches([' ', ':', '-', '.', ',', '—', '–', '→']);
    let summary = match first.find(". ") {
        Some(end) => &first[..end],
        None => first.trim_end_matches('.'),
    };
    let summary = summary.trim();
    let summary = (!summary.is_empty() && summary.chars().count() <= 160).then(|| {
        let mut chars = summary.chars();
        chars.next().map_or(String::new(), |first| {
            first.to_uppercase().collect::<String>() + chars.as_str()
        })
    });
    (summary, Some(doc.to_string()))
}

/// Text after a leading `` `code` `` or `{fields}`, if any.
fn skip_leading_shape(text: &str) -> &str {
    let text = text.trim_start();
    if let Some(rest) = text.strip_prefix('`') {
        return rest.split_once('`').map_or("", |(_, after)| after);
    }
    if text.starts_with('{') {
        let mut depth = 0;
        for (at, c) in text.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &text[at + 1..];
                    }
                }
                _ => {}
            }
        }
        return "";
    }
    text
}

fn path_names(path: &str) -> Vec<String> {
    path.split('/')
        .filter_map(|segment| {
            let inner = segment.strip_prefix('{')?.strip_suffix('}')?;
            Some(inner.trim_start_matches('*').to_string())
        })
        .collect()
}

fn parameter(name: &str, place: &str, field: FieldInfo) -> Value {
    let mut schema = field.schema;
    let description = schema.as_object_mut().and_then(|m| m.remove("description"));
    let mut out = json!({
        "name": name,
        "in": place,
        "required": field.required || place == "path",
        "schema": schema,
    });
    if let Some(description) = description {
        out["description"] = description;
    }
    if !field.aliases.is_empty() {
        out["x-aliases"] = json!(field.aliases);
    }
    out
}

fn path_parameters(path: &str, analysis: &Analysis, schemas: &mut Schemas<'_>) -> Vec<Value> {
    let names = path_names(path);
    let mut typed: BTreeMap<String, FieldInfo> = BTreeMap::new();
    for extractor in &analysis.extractors {
        let Extractor::PathArgs { ty, module } = extractor else {
            continue;
        };
        match ty {
            Type::Tuple(tuple) => {
                for (name, elem) in names.iter().zip(tuple.elems.iter()) {
                    typed.insert(
                        name.clone(),
                        FieldInfo {
                            name: name.clone(),
                            schema: schemas.of_type(elem, module, Dir::In, &[]),
                            required: true,
                            aliases: Vec::new(),
                        },
                    );
                }
            }
            other => {
                if let Some(fields) = schemas.fields_of(other, module, Dir::In) {
                    for field in fields {
                        typed.insert(field.name.clone(), field);
                    }
                } else if let [only] = names.as_slice() {
                    typed.insert(
                        only.clone(),
                        FieldInfo {
                            name: only.clone(),
                            schema: schemas.of_type(other, module, Dir::In, &[]),
                            required: true,
                            aliases: Vec::new(),
                        },
                    );
                }
            }
        }
    }
    names
        .iter()
        .map(|name| {
            let field = typed.remove(name).unwrap_or(FieldInfo {
                name: name.clone(),
                schema: json!({ "type": "string" }),
                required: true,
                aliases: Vec::new(),
            });
            parameter(name, "path", field)
        })
        .collect()
}

const CONSOLE_FIELDS: &[&str] = &["addr", "ps5_addr", "host", "ip", "console"];

fn console_field(fields: &[FieldInfo], place: &str) -> Option<Value> {
    let field = fields.iter().find(|f| {
        CONSOLE_FIELDS.contains(&f.name.as_str())
            && (f.name != "host" && f.name != "ip" || f.aliases.iter().any(|a| a == "console"))
    })?;
    Some(json!({
        "in": place,
        "name": field.name,
        "aliases": field.aliases,
        "required": field.required,
    }))
}

/// A response that is the common error, under this status's words.
fn error_response(text: &str) -> Value {
    json!({ "$ref": "#/components/responses/Error", "description": text })
}

struct Built {
    operation: Value,
    response_source: &'static str,
}

fn access_of(route: &Route, analysis: &Analysis) -> &'static str {
    let path = route.path.as_str();
    if path.starts_with("/pkg-host/")
        || path == "/pad"
        || path == "/api/pad/ws"
        || path == "/join"
        || path == "/api/join/ws"
        || path == "/api/join/signal"
        || path == "/share"
        || path == "/api/share/ws"
    {
        return "lan-open";
    }
    match (analysis.checks_peer, analysis.checks_host) {
        (true, _) => "strict-local",
        (false, true) => "host-checked",
        _ => "loopback",
    }
}

fn build_operation(route: &Route, analysis: &Analysis, schemas: &mut Schemas<'_>) -> Built {
    let mut op = Map::new();
    let (summary, description) = split_doc(&analysis.doc);
    if let Some(summary) = summary {
        op.insert("summary".into(), json!(summary));
    }
    if let Some(description) = description {
        op.insert("description".into(), json!(description));
    }
    let mut parameters = path_parameters(&route.path, analysis, schemas);
    let requires_console = analysis.requires_console;
    let mut meta = Map::new();
    let access = access_of(route, analysis);
    meta.insert("access".into(), json!(access));
    let mut console = None;
    let mut websocket = false;
    for extractor in &analysis.extractors {
        match extractor {
            Extractor::Query { ty, module } => match schemas.fields_of(ty, module, Dir::In) {
                Some(fields) => {
                    console = console.or_else(|| console_field(&fields, "query"));
                    for field in fields {
                        let name = field.name.clone();
                        parameters.push(parameter(&name, "query", field));
                    }
                }
                None => {
                    meta.insert("query".into(), json!("free-form"));
                }
            },
            Extractor::Json {
                ty,
                module,
                optional,
            } => {
                let schema = schemas.of_type(ty, module, Dir::In, &[]);
                if let Some(fields) = schemas.fields_of(ty, module, Dir::In) {
                    console = console.or_else(|| console_field(&fields, "body"));
                }
                op.insert(
                    "requestBody".into(),
                    json!({
                        "required": !optional,
                        "content": { "application/json": { "schema": schema } },
                    }),
                );
            }
            Extractor::Form { ty, module } => {
                let schema = schemas.of_type(ty, module, Dir::In, &[]);
                op.insert(
                    "requestBody".into(),
                    json!({
                        "required": true,
                        "content": { "application/x-www-form-urlencoded": { "schema": schema } },
                    }),
                );
            }
            Extractor::Multipart => {
                op.insert(
                    "requestBody".into(),
                    json!({
                        "required": true,
                        "content": { "multipart/form-data": { "schema": {} } },
                    }),
                );
            }
            Extractor::Bytes => {
                op.insert(
                    "requestBody".into(),
                    json!({
                        "required": true,
                        "content": { "application/octet-stream": {
                            "schema": { "type": "string", "contentEncoding": "binary" }
                        } },
                    }),
                );
            }
            Extractor::WebSocket => websocket = true,
            Extractor::PathArgs { .. } => {}
        }
    }
    if let Some(mut console) = console {
        if requires_console {
            console["required"] = json!(true);
        }
        meta.insert("console".into(), console);
    }
    if websocket {
        meta.insert("websocket".into(), json!(true));
    }
    if !parameters.is_empty() {
        op.insert("parameters".into(), Value::Array(parameters));
    }
    let (responses, source) = build_responses(route, analysis, access, websocket);
    op.insert("responses".into(), responses);
    meta.insert("response_schema".into(), json!(source));
    op.insert("x-ps5upload".into(), Value::Object(meta));
    Built {
        operation: Value::Object(op),
        response_source: source,
    }
}

fn union_of(list: &[(Value, bool)]) -> Value {
    match list {
        [(only, _)] => only.clone(),
        many => json!({ "anyOf": many.iter().map(|(s, _)| s.clone()).collect::<Vec<_>>() }),
    }
}

fn status_words(status: u16) -> &'static str {
    match status {
        400 => "Bad request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not found",
        405 => "Method not allowed",
        408 => "Timed out",
        409 => "Conflict",
        410 => "Gone",
        413 => "Too large",
        415 => "Unsupported media type",
        416 => "Range not satisfiable",
        422 => "Cannot be processed",
        423 => "Locked",
        428 => "Precondition required",
        429 => "Too many requests",
        500 => "Engine error",
        501 => "Not implemented",
        502 => "The console or remote end failed",
        503 => "Unavailable",
        504 => "The console or remote end timed out",
        507 => "Not enough space",
        _ => "Refused",
    }
}

/// The success answer's text and whether it is an error shape only.
fn is_error_literal(schema: &Value) -> bool {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|p| p.contains_key("error"))
}

fn build_responses(
    route: &Route,
    analysis: &Analysis,
    access: &str,
    websocket: bool,
) -> (Value, &'static str) {
    let mut responses = Map::new();
    let mut source = "none";
    let binary_types: Vec<&String> = analysis
        .content_types
        .iter()
        .filter(|kind| !kind.contains("json"))
        .collect();
    let mut plain = analysis.bodies.get(&None).cloned().unwrap_or_default();
    let other_success: Vec<(Value, bool)> = analysis
        .bodies
        .iter()
        .filter(|(status, _)| status.is_some_and(|s| (200..300).contains(&s)))
        .flat_map(|(_, list)| list.iter().cloned())
        .collect();
    // An `{error: ...}` literal is the error shape, under a status the source keeps in a variable.
    // It is documented once, as `default`, and says nothing about the success.
    let had_literals = !plain.is_empty();
    plain.retain(|(s, is_struct)| *is_struct || !is_error_literal(s));
    let saw_json = analysis.saw_json || (had_literals && plain.is_empty());
    if websocket {
        responses.insert(
            "101".into(),
            json!({ "description": "Switches to a WebSocket." }),
        );
        source = "websocket";
    } else if analysis.sse {
        responses.insert(
            "200".into(),
            json!({
                "description": "A stream of server-sent events.",
                "content": { "text/event-stream": { "schema": { "type": "string" } } },
            }),
        );
        source = "stream";
    } else if analysis.html && analysis.bodies.is_empty() {
        responses.insert(
            "200".into(),
            json!({
                "description": "A page.",
                "content": { "text/html": { "schema": { "type": "string" } } },
            }),
        );
        source = "page";
    } else if !plain.is_empty() || !other_success.is_empty() {
        let seen: Vec<&(Value, bool)> = plain.iter().chain(other_success.iter()).collect();
        let structs = seen.iter().filter(|(_, s)| *s).count();
        source = match (structs, seen.len()) {
            (0, _) => "literal",
            (a, b) if a == b => "struct",
            _ => "mixed",
        };
        if !plain.is_empty() {
            responses.insert(
                "200".into(),
                json!({
                    "description": "Success.",
                    "content": { "application/json": { "schema": union_of(&plain) } },
                }),
            );
        }
    } else if !binary_types.is_empty() {
        let mut content = Map::new();
        for kind in &binary_types {
            content.insert(
                (*kind).clone(),
                json!({ "schema": { "type": "string", "contentEncoding": "binary" } }),
            );
        }
        responses.insert(
            "200".into(),
            json!({ "description": "Success.", "content": content }),
        );
        source = "binary";
    } else if saw_json {
        responses.insert(
            "200".into(),
            json!({
                "description": "Success: JSON the engine does not describe field by field, often \
                                what the console sent.",
                "content": { "application/json": { "schema": {} } },
            }),
        );
        source = "json-unknown";
    } else {
        responses.insert(
            "200".into(),
            json!({
                "description": "Success. The handler builds this answer in a way the generator \
                                cannot read.",
            }),
        );
    }
    // Other statuses the handler names, each with what it is known to carry.
    let mut other: BTreeSet<u16> = analysis.statuses.clone();
    other.extend(analysis.bodies.keys().flatten().copied());
    for status in other {
        if status == 200 {
            continue;
        }
        let body = analysis.bodies.get(&Some(status));
        if (200..300).contains(&status) {
            let mut entry = json!({ "description": "Success." });
            if let Some(list) = body {
                entry["content"] = json!({ "application/json": { "schema": union_of(list) } });
            }
            responses.insert(status.to_string(), entry);
        } else if (300..400).contains(&status) {
            responses.insert(
                status.to_string(),
                json!({ "description": "Redirect or no change." }),
            );
        } else {
            let entry = match body {
                None => error_response(status_words(status)),
                Some(list) => {
                    let mut schemas = vec![json!({ "$ref": "#/components/schemas/Error" })];
                    schemas.extend(list.iter().map(|(s, _)| s.clone()));
                    json!({
                        "description": status_words(status),
                        "content": { "application/json": { "schema": { "anyOf": schemas } } },
                    })
                }
            };
            responses.insert(status.to_string(), entry);
        }
    }
    if access != "lan-open" && !route.path.starts_with("/pkg-host") {
        responses.entry("403".to_string()).or_insert_with(|| {
            error_response("Refused by the engine's address, Origin or Host guards.")
        });
    }
    responses.insert(
        "default".into(),
        error_response("Any other failure, in the same shape."),
    );
    (Value::Object(responses), source)
}

/// `$ref`s made while building point at `full::path@in`. A type used one way, or the same both ways,
/// gets its plain name; one that differs gets `Name.request` and `Name.response`.
fn name_components(schemas: &Schemas<'_>) -> (BTreeMap<String, String>, Map<String, Value>) {
    let mut by_type: BTreeMap<&str, Vec<Dir>> = BTreeMap::new();
    for (path, dir) in schemas.done.keys() {
        by_type.entry(path.as_str()).or_default().push(*dir);
    }
    let strip = |value: &Value| {
        value
            .to_string()
            .replace("@in\"", "\"")
            .replace("@out\"", "\"")
    };
    // A type merges when both its sides read the same and everything they point at merges too.
    let mut split: BTreeSet<&str> = by_type
        .iter()
        .filter(|(path, dirs)| {
            dirs.len() == 2
                && strip(&schemas.done[&((**path).to_string(), Dir::In)])
                    != strip(&schemas.done[&((**path).to_string(), Dir::Out)])
        })
        .map(|(path, _)| *path)
        .collect();
    loop {
        let mut changed = false;
        for (path, dirs) in &by_type {
            if dirs.len() != 2 || split.contains(path) {
                continue;
            }
            let text = schemas.done[&((*path).to_string(), Dir::In)].to_string();
            let needle = |p: &&&str| text.contains(&format!("/{p}@"));
            if split.iter().any(|p| needle(&p)) {
                split.insert(path);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Plain names, qualified by module only where two types share one.
    let mut names: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for path in by_type.keys() {
        names
            .entry(path.rsplit("::").next().unwrap_or(path))
            .or_default()
            .push(path);
    }
    let base_name = |path: &str| -> String {
        let (parent, last) = path.rsplit_once("::").unwrap_or(("", path));
        if names.get(last).map_or(0, Vec::len) <= 1 {
            return last.to_string();
        }
        let qualifier = parent
            .replace("ps5upload_engine", "engine")
            .replace("ps5upload_", "")
            .replace("::", ".");
        format!("{qualifier}.{last}")
    };
    let mut renames = BTreeMap::new();
    let mut out = Map::new();
    for ((path, dir), schema) in &schemas.done {
        let plain = base_name(path);
        let final_name = if split.contains(path.as_str()) {
            match dir {
                Dir::In => format!("{plain}.request"),
                Dir::Out => format!("{plain}.response"),
            }
        } else {
            plain
        };
        let suffix = match dir {
            Dir::In => "@in",
            Dir::Out => "@out",
        };
        renames.insert(format!("{path}{suffix}"), final_name.clone());
        out.entry(final_name).or_insert_with(|| schema.clone());
    }
    (renames, out)
}

fn rename_refs(value: &mut Value, renames: &BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(target)) = map.get_mut("$ref") {
                if let Some(name) = target
                    .strip_prefix("#/components/schemas/")
                    .and_then(|t| renames.get(t))
                {
                    *target = format!("#/components/schemas/{name}");
                }
            }
            for v in map.values_mut() {
                rename_refs(v, renames);
            }
        }
        Value::Array(items) => {
            for v in items {
                rename_refs(v, renames);
            }
        }
        _ => {}
    }
}

/// Merges an override into a value: objects key by key, anything else replaced.
fn merge(into: &mut Value, over: &Value) {
    match (into, over) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(k) {
                    Some(existing) => merge(existing, v),
                    None => {
                        a.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (slot, v) => *slot = v.clone(),
    }
}

fn family_of(path: &str, families: &[(String, String)]) -> String {
    let mut best: Option<&(String, String)> = None;
    for family in families {
        if (path == family.0 || path.starts_with(&format!("{}/", family.0)))
            && best.is_none_or(|b| family.0.len() > b.0.len())
        {
            best = Some(family);
        }
    }
    if let Some((_, tag)) = best {
        return tag.clone();
    }
    let mut parts = path.split('/').filter(|s| !s.is_empty());
    match (parts.next(), parts.next()) {
        (Some("api"), Some(word)) => word.to_string(),
        _ => "pages".to_string(),
    }
}

pub fn generate(crates: &Path) -> Result<Generated, String> {
    let index = Index::load(crates)?;
    let secrets = secret_paths(&index)?;
    let overrides_path = crates.join("ps5upload-engine/api/overrides.json");
    let overrides_text = std::fs::read_to_string(&overrides_path)
        .map_err(|e| format!("cannot read {}: {e}", overrides_path.display()))?;
    let overrides: Value = serde_json::from_str(&overrides_text)
        .map_err(|e| format!("{}: {e}", overrides_path.display()))?;

    let found = routes::scan(&index)?;
    let families: Vec<(String, String)> = overrides["families"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(prefix, tag)| (prefix.clone(), tag.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut schemas = Schemas::new(&index);
    let mut paths: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    let mut tags = BTreeSet::new();
    let mut seen_keys = BTreeSet::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut no_summary = Vec::new();
    let mut none_list = Vec::new();
    for route in &found {
        let key = format!("{} {}", route.method, route.path);
        if secrets.contains(&route.path) {
            continue;
        }
        if !seen_keys.insert(key.clone()) {
            return Err(format!("{key} is mounted twice"));
        }
        let analysis = analyze(&index, &mut schemas, &route.handler);
        let built = build_operation(route, &analysis, &mut schemas);
        *counts.entry(built.response_source).or_default() += 1;
        if built.response_source == "none" {
            none_list.push(key.clone());
        }
        let mut operation = built.operation;
        if starts_a_job(&operation) {
            operation["x-ps5upload"]["long_running"] = json!({
                "id_field": "job_id",
                "status": "/api/jobs/{id}",
                "cancel": "/api/jobs/{id}/cancel",
            });
        }
        if let Some(extra) = overrides["routes"].get(&key) {
            merge(&mut operation, extra);
        }
        if operation.get("summary").is_none() {
            no_summary.push(key.clone());
        }
        let family = family_of(&route.path, &families);
        tags.insert(family.clone());
        let mut op_tags = vec![json!(family)];
        let access = operation["x-ps5upload"]["access"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if access == "strict-local" {
            op_tags.push(json!("strict-local"));
            tags.insert("strict-local".to_string());
        }
        operation["tags"] = Value::Array(op_tags);
        paths
            .entry(route.path.clone())
            .or_default()
            .insert(route.method.to_ascii_lowercase(), operation);
    }
    // An override of a route that is gone is a mistake to hear about.
    if let Some(listed) = overrides["routes"].as_object() {
        for key in listed.keys() {
            if !seen_keys.contains(key) {
                return Err(format!(
                    "api/overrides.json describes {key}, which nothing mounts"
                ));
            }
        }
    }

    let (renames, mut components) = name_components(&schemas);
    let mut paths_value = serde_json::to_value(&paths).map_err(|e| e.to_string())?;
    rename_refs(&mut paths_value, &renames);
    let mut components_value = Value::Object(std::mem::take(&mut components));
    rename_refs(&mut components_value, &renames);
    components_value["Error"] = json!({
        "type": "object",
        "description": "Every failure of an /api/ route. Some routes add their own fields beside \
                        these (`reason`, `field`, `op_id`).",
        "properties": {
            "error": { "type": "string", "description": "What went wrong, in words." },
            "code": {
                "type": "string",
                "description": "A short snake_case name. Added from the status when a route gave \
                                none: 404 is not_found.",
            },
        },
        "required": ["error"],
    });

    // Words the types cannot say (a field's upper bound), merged into a named schema.
    if let Some(listed) = overrides["schemas"].as_object() {
        for (name, extra) in listed {
            match components_value.get_mut(name) {
                Some(schema) => merge(schema, extra),
                None => {
                    return Err(format!(
                        "api/overrides.json describes schema {name}, which no route uses"
                    ))
                }
            }
        }
    }

    let tag_words = &overrides["tags"];
    let tag_list: Vec<Value> = tags
        .iter()
        .map(|tag| {
            let mut entry = json!({ "name": tag });
            if let Some(words) = tag_words.get(tag) {
                entry["description"] = words.clone();
            }
            entry
        })
        .collect();

    let document = json!({
        "openapi": "3.1.0",
        "info": merged_info(&overrides),
        "servers": [{ "url": "http://127.0.0.1:19113", "description": "The engine on this computer, on its default port." }],
        "tags": tag_list,
        "paths": paths_value,
        "components": {
            "schemas": components_value,
            "responses": {
                "Error": {
                    "description": "A failure, as JSON.",
                    "content": { "application/json": {
                        "schema": { "$ref": "#/components/schemas/Error" }
                    } },
                },
            },
        },
    });
    let mut text = serde_json::to_string_pretty(&canonical(document)).map_err(|e| e.to_string())?;
    text.push('\n');
    let stats = format!(
        "{} operations, {} schemas, {} bytes; response schema by source: {:?}\nno summary ({}):\n  {}\nno response schema ({}):\n  {}",
        seen_keys.len(),
        renames.len(),
        text.len(),
        counts,
        no_summary.len(),
        no_summary.join("\n  "),
        none_list.len(),
        none_list.join("\n  "),
    );
    Ok(Generated { text, stats })
}

/// The answer hands back a `job_id`, which `/api/jobs/{id}` follows. Workshop jobs answer there too.
fn starts_a_job(operation: &Value) -> bool {
    let Some(responses) = operation["responses"].as_object() else {
        return false;
    };
    responses.iter().any(|(status, response)| {
        status.starts_with('2') && {
            let schema = &response["content"]["application/json"]["schema"];
            schema["$ref"]
                .as_str()
                .is_some_and(|r| r.ends_with("JobCreated@out"))
                || schema["properties"].get("job_id").is_some()
        }
    })
}

fn merged_info(overrides: &Value) -> Value {
    let mut info = json!({
        "title": "PS5Upload engine API",
        "version": VERSION_PLACEHOLDER,
        "license": { "name": "GPL-3.0-or-later" },
    });
    if let Some(words) = overrides.get("info") {
        merge(&mut info, words);
    }
    info
}

/// Which operations differ between two documents, for the check's message.
pub fn difference(current: &str, generated: &str) -> Option<String> {
    if current == generated {
        return None;
    }
    let operations = |text: &str| -> BTreeMap<String, String> {
        let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let mut out = BTreeMap::new();
        if let Some(paths) = parsed["paths"].as_object() {
            for (path, methods) in paths {
                if let Some(methods) = methods.as_object() {
                    for (method, op) in methods {
                        out.insert(
                            format!("{} {path}", method.to_ascii_uppercase()),
                            op.to_string(),
                        );
                    }
                }
            }
        }
        out
    };
    let old = operations(current);
    let new = operations(generated);
    let mut lines = vec!["api/openapi.json is out of date.".to_string()];
    for key in new.keys().filter(|k| !old.contains_key(*k)) {
        lines.push(format!("  new route: {key}"));
    }
    for key in old.keys().filter(|k| !new.contains_key(*k)) {
        lines.push(format!("  route gone: {key}"));
    }
    for (key, op) in &new {
        if old.get(key).is_some_and(|o| o != op) {
            lines.push(format!("  changed: {key}"));
        }
    }
    lines.push(format!("Regenerate it: (cd engine && {REGENERATE})"));
    Some(lines.join("\n"))
}
