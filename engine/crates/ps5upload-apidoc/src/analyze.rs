//! What one handler takes and answers, read from its signature and its body. A response is only
//! described by what the source shows: a struct it serializes, or the keys of a `json!` literal.
//! When the source shows neither, the spec says so instead of guessing.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use serde_json::{json, Map, Value};
use syn::visit::{self, Visit};
use syn::visit_mut::{self, VisitMut};
use syn::{Expr, FnArg, GenericArgument, Pat, PathArguments, ReturnType, Type};

use crate::routes::Handler;
use crate::schema::{Dir, Schemas};
use crate::source::{doc_text, Func, Index};

/// What a handler reads from the request.
#[derive(Clone)]
pub enum Extractor {
    Json {
        ty: Type,
        module: String,
        optional: bool,
    },
    Query {
        ty: Type,
        module: String,
    },
    PathArgs {
        ty: Type,
        module: String,
    },
    Form {
        ty: Type,
        module: String,
    },
    Multipart,
    WebSocket,
    Bytes,
}

#[derive(Clone)]
enum Shape {
    Ty(Box<Type>, String),
    Schema(Value),
}

#[derive(Default)]
pub struct Analysis {
    pub doc: String,
    pub extractors: Vec<Extractor>,
    /// Status (`None`: the route's plain success) to the schemas seen for it, and whether each came
    /// from a struct.
    pub bodies: BTreeMap<Option<u16>, Vec<(Value, bool)>>,
    pub statuses: BTreeSet<u16>,
    pub content_types: BTreeSet<String>,
    pub sse: bool,
    pub html: bool,
    /// Answers JSON the source shows no fields for (what the console sent, say).
    pub saw_json: bool,
    /// Refuses a request that names no console.
    pub requires_console: bool,
    /// Calls `host_is_local` somewhere on its way.
    pub checks_host: bool,
    /// Calls `local_allowed` somewhere on its way.
    pub checks_peer: bool,
}

fn generic_args(segment: &syn::PathSegment) -> Vec<&Type> {
    match &segment.arguments {
        PathArguments::AngleBracketed(a) => a
            .args
            .iter()
            .filter_map(|arg| match arg {
                GenericArgument::Type(t) => Some(t),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn extractor_of(ty: &Type, module: &str, optional: bool) -> Option<Extractor> {
    let Type::Path(p) = ty else {
        return None;
    };
    let last = p.path.segments.last()?;
    let args = generic_args(last);
    let first = args.first().copied();
    match last.ident.to_string().as_str() {
        "Json" => Some(Extractor::Json {
            ty: first?.clone(),
            module: module.to_string(),
            optional,
        }),
        "Query" => Some(Extractor::Query {
            ty: first?.clone(),
            module: module.to_string(),
        }),
        "Path" => Some(Extractor::PathArgs {
            ty: first?.clone(),
            module: module.to_string(),
        }),
        "Form" => Some(Extractor::Form {
            ty: first?.clone(),
            module: module.to_string(),
        }),
        "Multipart" => Some(Extractor::Multipart),
        "WebSocketUpgrade" => Some(Extractor::WebSocket),
        "Bytes" | "BodyStream" | "Body" => Some(Extractor::Bytes),
        "Option" | "Result" => extractor_of(first?, module, true),
        _ => None,
    }
}

fn same_kind(a: &Extractor, b: &Extractor) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

fn add_extractor(into: &mut Vec<Extractor>, found: Extractor) {
    if !into.iter().any(|known| same_kind(known, &found)) {
        into.push(found);
    }
}

struct ReplaceSelf<'a>(&'a Type);

impl VisitMut for ReplaceSelf<'_> {
    fn visit_type_mut(&mut self, ty: &mut Type) {
        if let Type::Path(p) = ty {
            if p.qself.is_none() && p.path.is_ident("Self") {
                *ty = self.0.clone();
                return;
            }
        }
        visit_mut::visit_type_mut(self, ty);
    }
}

/// `Result<T, E>` and `Option<T>` are `T` once `?` or a match has been through them.
fn peel(shape: Shape) -> Shape {
    if let Shape::Ty(ty, module) = &shape {
        if let Type::Path(p) = &**ty {
            if let Some(last) = p.path.segments.last() {
                let name = last.ident.to_string();
                if name == "Result" || name == "Option" {
                    if let Some(inner) = generic_args(last).first() {
                        return peel(Shape::Ty(Box::new((*inner).clone()), module.clone()));
                    }
                }
            }
        }
    }
    shape
}

const PASS_THROUGH: &[&str] = &[
    "map_err",
    "context",
    "with_context",
    "clone",
    "to_owned",
    "ok",
    "ok_or",
    "ok_or_else",
    "unwrap",
    "expect",
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_or_default",
    "into_inner",
    "into_owned",
    "cloned",
    "copied",
    "as_ref",
];

struct BodyScan<'s, 'a> {
    index: &'a Index,
    schemas: &'s mut Schemas<'a>,
    module: String,
    env: BTreeMap<String, Shape>,
    out: &'s mut Analysis,
    /// The status of the tuple being read, in `(StatusCode::X, Json(...))`.
    status: Option<u16>,
    calls: Vec<String>,
    /// Reading the handler itself, not a helper it calls: a helper's rule may apply to some of
    /// its callers only.
    entry: bool,
}

fn status_number(name: &str) -> Option<u16> {
    Some(match name {
        "OK" => 200,
        "CREATED" => 201,
        "ACCEPTED" => 202,
        "NO_CONTENT" => 204,
        "PARTIAL_CONTENT" => 206,
        "NOT_MODIFIED" => 304,
        "BAD_REQUEST" => 400,
        "UNAUTHORIZED" => 401,
        "FORBIDDEN" => 403,
        "NOT_FOUND" => 404,
        "METHOD_NOT_ALLOWED" => 405,
        "NOT_ACCEPTABLE" => 406,
        "REQUEST_TIMEOUT" => 408,
        "CONFLICT" => 409,
        "GONE" => 410,
        "PRECONDITION_FAILED" => 412,
        "PAYLOAD_TOO_LARGE" => 413,
        "UNSUPPORTED_MEDIA_TYPE" => 415,
        "RANGE_NOT_SATISFIABLE" => 416,
        "EXPECTATION_FAILED" => 417,
        "UNPROCESSABLE_ENTITY" => 422,
        "LOCKED" => 423,
        "PRECONDITION_REQUIRED" => 428,
        "TOO_MANY_REQUESTS" => 429,
        "INTERNAL_SERVER_ERROR" => 500,
        "NOT_IMPLEMENTED" => 501,
        "BAD_GATEWAY" => 502,
        "SERVICE_UNAVAILABLE" => 503,
        "GATEWAY_TIMEOUT" => 504,
        "INSUFFICIENT_STORAGE" => 507,
        _ => return None,
    })
}

fn status_of_path(expr: &Expr) -> Option<u16> {
    let Expr::Path(p) = expr else {
        return None;
    };
    let mut parts = p.path.segments.iter().rev();
    let name = parts.next()?;
    let owner = parts.next()?;
    if owner.ident != "StatusCode" {
        return None;
    }
    status_number(&name.ident.to_string())
}

fn last_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

fn split_commas(stream: TokenStream) -> Vec<Vec<TokenTree>> {
    let mut parts = vec![Vec::new()];
    for token in stream {
        if matches!(&token, TokenTree::Punct(p) if p.as_char() == ',') {
            parts.push(Vec::new());
        } else if let Some(part) = parts.last_mut() {
            part.push(token);
        }
    }
    parts.retain(|p| !p.is_empty());
    parts
}

impl BodyScan<'_, '_> {
    fn path_segments(path: &syn::Path) -> Vec<String> {
        path.segments.iter().map(|s| s.ident.to_string()).collect()
    }

    /// The return type of a function, `Self` filled in.
    fn return_shape(&self, full: &str) -> Option<Shape> {
        let func = self.index.funcs.get(full)?;
        let ReturnType::Type(_, ty) = &func.sig.output else {
            return None;
        };
        let mut ty = (**ty).clone();
        if let Some(self_ty) = &func.self_ty {
            ReplaceSelf(self_ty).visit_type_mut(&mut ty);
        }
        Some(Shape::Ty(Box::new(ty), func.module.clone()))
    }

    fn infer(&mut self, expr: &Expr) -> Option<Shape> {
        match expr {
            Expr::Path(p) => {
                if let Some(known) = p
                    .path
                    .get_ident()
                    .and_then(|n| self.env.get(&n.to_string()))
                {
                    return Some(known.clone());
                }
                // A function handed over by name: `spawn_blocking(me_json)`.
                let full = self
                    .index
                    .resolve_func(&self.module, &Self::path_segments(&p.path))?;
                self.return_shape(&full)
            }
            Expr::Try(t) => self.infer(&t.expr).map(peel),
            Expr::Await(a) => self.infer(&a.base),
            Expr::Paren(p) => self.infer(&p.expr),
            Expr::Group(g) => self.infer(&g.expr),
            Expr::Reference(r) => self.infer(&r.expr),
            Expr::Block(b) => self.infer_block(&b.block),
            Expr::Unsafe(b) => self.infer_block(&b.block),
            Expr::Closure(c) => self.infer(&c.body),
            Expr::If(i) => self.infer_block(&i.then_branch),
            Expr::Match(m) => m.arms.iter().find_map(|arm| self.infer(&arm.body)),
            Expr::Struct(s) => Some(Shape::Ty(
                Box::new(Type::Path(syn::TypePath {
                    qself: None,
                    path: s.path.clone(),
                })),
                self.module.clone(),
            )),
            Expr::Lit(lit) => Some(Shape::Schema(match &lit.lit {
                syn::Lit::Str(_) => json!({ "type": "string" }),
                syn::Lit::Bool(_) => json!({ "type": "boolean" }),
                syn::Lit::Int(_) => json!({ "type": "integer" }),
                syn::Lit::Float(_) => json!({ "type": "number" }),
                _ => return None,
            })),
            Expr::Macro(m) => {
                let name = m.mac.path.segments.last()?.ident.to_string();
                match name.as_str() {
                    "json" => Some(Shape::Schema(self.literal(m.mac.tokens.clone()))),
                    "format" => Some(Shape::Schema(json!({ "type": "string" }))),
                    _ => None,
                }
            }
            Expr::Call(call) => self.infer_call(call),
            Expr::MethodCall(call) => self.infer_method(call),
            _ => None,
        }
    }

    fn infer_block(&mut self, block: &syn::Block) -> Option<Shape> {
        for stmt in &block.stmts {
            if let syn::Stmt::Local(local) = stmt {
                self.bind_local(local);
            }
        }
        match block.stmts.last()? {
            syn::Stmt::Expr(expr, None) => self.infer(expr),
            _ => None,
        }
    }

    fn infer_call(&mut self, call: &syn::ExprCall) -> Option<Shape> {
        let Expr::Path(func) = &*call.func else {
            return None;
        };
        let name = func.path.segments.last()?.ident.to_string();
        match name.as_str() {
            "Ok" | "Some" | "new" if call.args.len() == 1 && func.path.segments.len() <= 2 => {
                let known = self
                    .index
                    .resolve_func(&self.module, &Self::path_segments(&func.path));
                if let (Some(full), "new") = (known, name.as_str()) {
                    return self.return_shape(&full);
                }
                self.infer(call.args.first()?)
            }
            "spawn_blocking" | "to_value" | "from" | "timeout" => self.infer(call.args.last()?),
            _ => {
                let full = self
                    .index
                    .resolve_func(&self.module, &Self::path_segments(&func.path))?;
                self.return_shape(&full)
            }
        }
    }

    fn infer_method(&mut self, call: &syn::ExprMethodCall) -> Option<Shape> {
        let name = call.method.to_string();
        if PASS_THROUGH.contains(&name.as_str()) {
            let inner = self.infer(&call.receiver)?;
            return Some(if name.starts_with("unwrap") || name == "expect" {
                peel(inner)
            } else {
                inner
            });
        }
        if name == "and_then" || name == "map" {
            // `.and_then(|r| r)` changes nothing; otherwise what the closure gives.
            if let Some(Expr::Closure(closure)) = call.args.first() {
                let identity = match (&*closure.body, closure.inputs.first()) {
                    (Expr::Path(body), Some(Pat::Ident(input))) => body.path.is_ident(&input.ident),
                    _ => false,
                };
                return if identity {
                    self.infer(&call.receiver)
                } else {
                    self.infer(&closure.body)
                };
            }
        }
        match name.as_str() {
            "len" | "count" => {
                return Some(Shape::Schema(json!({ "type": "integer", "minimum": 0 })))
            }
            "to_string" | "to_string_lossy" | "display" => {
                return Some(Shape::Schema(json!({ "type": "string" })))
            }
            _ if name.starts_with("is_") => {
                return Some(Shape::Schema(json!({ "type": "boolean" })))
            }
            _ => {}
        }
        // A method of a type the source has: by the receiver's type if it is known; otherwise only
        // a name unlike any std method, and only when one type has it.
        let receiver = self.infer(&call.receiver).map(peel);
        if let Some(Shape::Ty(ty, module)) = &receiver {
            if let Type::Path(p) = &**ty {
                let segments = Self::path_segments(&p.path);
                if let Some(owner) = self.index.resolve_def(module, &segments) {
                    let full = format!("{owner}::{name}");
                    if self.index.funcs.contains_key(&full) {
                        return self.return_shape(&full);
                    }
                }
            }
        }
        if !name.contains('_') {
            return None;
        }
        let candidates = self.index.methods.get(&name)?;
        match candidates.iter().collect::<Vec<_>>().as_slice() {
            [only] => self.return_shape(only),
            _ => None,
        }
    }

    /// The schema of a `json!` literal: its keys are real, a value's type only when the source
    /// shows it.
    fn literal(&mut self, tokens: TokenStream) -> Value {
        let mut tokens = tokens.into_iter().collect::<Vec<_>>();
        if tokens.len() == 1 {
            if let TokenTree::Group(group) = &tokens[0] {
                match group.delimiter() {
                    Delimiter::Brace => return self.literal_object(group.stream()),
                    Delimiter::Bracket => {
                        let items: Vec<Value> = split_commas(group.stream())
                            .into_iter()
                            .map(|item| self.literal(item.into_iter().collect()))
                            .collect();
                        let mut array = json!({ "type": "array" });
                        if let Some(first) = items.first() {
                            if items.iter().all(|i| i == first) {
                                array["items"] = first.clone();
                            }
                        }
                        return array;
                    }
                    _ => {}
                }
            }
        }
        if tokens.len() == 1 {
            if let TokenTree::Ident(ident) = &tokens[0] {
                if ident == "null" {
                    return json!({ "type": "null" });
                }
                if ident == "true" || ident == "false" {
                    return json!({ "type": "boolean" });
                }
            }
        }
        let stream: TokenStream = tokens.drain(..).collect();
        let Ok(expr) = syn::parse2::<Expr>(stream) else {
            return json!({});
        };
        match self.infer(&expr) {
            Some(Shape::Schema(schema)) => schema,
            Some(Shape::Ty(ty, module)) => {
                let schema = self.schemas.of_type(&ty, &module, Dir::Out, &[]);
                without_rust_type(schema)
            }
            None => json!({}),
        }
    }

    fn literal_object(&mut self, stream: TokenStream) -> Value {
        let mut properties = Map::new();
        for mut entry in split_commas(stream) {
            // `"key": value`, the colon being one token.
            if entry.len() < 3 {
                continue;
            }
            let Some(key) = literal_key(&entry[0]) else {
                continue;
            };
            let value: TokenStream = entry.drain(2..).collect();
            let schema = self.literal(value);
            properties.insert(key, schema);
        }
        json!({ "type": "object", "properties": properties })
    }

    fn bind_pattern(&mut self, pat: &Pat, shape: Option<Shape>) {
        let Some(shape) = shape else {
            if let Pat::Ident(name) = pat {
                self.env.remove(&name.ident.to_string());
            }
            return;
        };
        match pat {
            Pat::Ident(name) => {
                self.env.insert(name.ident.to_string(), shape);
            }
            Pat::Type(typed) => self.bind_pattern(&typed.pat, Some(shape)),
            Pat::TupleStruct(ts) => {
                let name = ts.path.segments.last().map(|s| s.ident.to_string());
                if matches!(name.as_deref(), Some("Ok" | "Some")) {
                    if let [only] = ts.elems.iter().collect::<Vec<_>>().as_slice() {
                        self.bind_pattern(only, Some(peel(shape)));
                    }
                }
            }
            _ => {}
        }
    }

    fn bind_local(&mut self, local: &syn::Local) {
        let declared = match &local.pat {
            Pat::Type(typed) => Some(Shape::Ty(typed.ty.clone(), self.module.clone())),
            _ => None,
        };
        let shape =
            declared.or_else(|| local.init.as_ref().and_then(|init| self.infer(&init.expr)));
        self.bind_pattern(&local.pat, shape);
    }

    fn note_json(&mut self, arg: &Expr) {
        let shape = self.infer(arg).map(peel);
        let status = self.status;
        let (schema, from_struct) = match shape {
            Some(Shape::Schema(schema)) => (schema, false),
            Some(Shape::Ty(ty, module)) => {
                let schema = self.schemas.of_type(&ty, &module, Dir::Out, &[]);
                if self.schemas.is_opaque(&schema) {
                    if status.is_none_or(|s| s == 200) {
                        self.out.saw_json = true;
                    }
                    return;
                }
                (schema, true)
            }
            None => {
                if status.is_none_or(|s| s == 200) {
                    self.out.saw_json = true;
                }
                return;
            }
        };
        if schema.as_object().is_some_and(Map::is_empty) {
            if status.is_none_or(|s| s == 200) {
                self.out.saw_json = true;
            }
            return;
        }
        let key = status.filter(|s| *s != 200);
        let list = self.out.bodies.entry(key).or_default();
        if !list.iter().any(|(known, _)| known == &schema) {
            list.push((schema, from_struct));
        }
    }
}

/// A value inside a literal whose type could not be followed says nothing rather than a Rust name.
fn without_rust_type(schema: Value) -> Value {
    if schema.get("x-rust-type").is_some() && schema.get("description").is_none() {
        json!({})
    } else {
        schema
    }
}

fn literal_key(token: &TokenTree) -> Option<String> {
    let TokenTree::Literal(literal) = token else {
        return None;
    };
    let parsed: syn::LitStr = syn::parse2(literal.to_token_stream()).ok()?;
    Some(parsed.value())
}

impl BodyScan<'_, '_> {
    /// A helper that answers with what it is given: `json_ok(&value)`, `json_response(code, value)`,
    /// `answer(move || work())`. What it is given is the answer.
    fn note_wrapper(&mut self, full: &str, call: &syn::ExprCall) {
        let Some(callee) = self.index.funcs.get(full) else {
            return;
        };
        if !answers_a_request(callee) {
            return;
        }
        let body = callee.block.to_token_stream().to_string();
        let status = call.args.first().and_then(status_of_path);
        let before = self.status;
        if status.is_some() {
            self.status = status;
        }
        for (input, arg) in callee.sig.inputs.iter().zip(call.args.iter()) {
            let FnArg::Typed(typed) = input else {
                continue;
            };
            let Pat::Ident(param) = &*typed.pat else {
                continue;
            };
            let shown = ["Json", "to_value", "to_vec"]
                .iter()
                .any(|f| body.contains(&format!("{f} ({})", param.ident)));
            if shown || (matches!(arg, Expr::Closure(_)) && body.contains("Json")) {
                self.note_json(arg);
            }
        }
        self.status = before;
    }
}

impl<'ast> Visit<'ast> for BodyScan<'_, '_> {
    /// A macro's arguments are code too: `with_remote!(r => list(&r).await)`, `vec![...]`. Only
    /// those that parse as expressions are read.
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let name = mac
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        if name == "json" {
            return;
        }
        let tokens: Vec<TokenTree> = mac.tokens.clone().into_iter().collect();
        let after_arrow = tokens.windows(2).position(|pair| {
            matches!((&pair[0], &pair[1]), (TokenTree::Punct(a), TokenTree::Punct(b))
                if a.as_char() == '=' && b.as_char() == '>')
        });
        let code: TokenStream = match after_arrow {
            Some(at) => tokens[at + 2..].iter().cloned().collect(),
            None => tokens.iter().cloned().collect(),
        };
        if let Ok(expr) = syn::parse2::<Expr>(code) {
            self.visit_expr(&expr);
        }
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        visit::visit_local(self, local);
        self.bind_local(local);
    }

    fn visit_expr_match(&mut self, m: &'ast syn::ExprMatch) {
        let shape = self.infer(&m.expr);
        self.visit_expr(&m.expr);
        for arm in &m.arms {
            self.bind_pattern(&arm.pat, shape.clone());
            self.visit_arm(arm);
        }
    }

    fn visit_expr_let(&mut self, l: &'ast syn::ExprLet) {
        let shape = self.infer(&l.expr);
        self.visit_expr(&l.expr);
        self.bind_pattern(&l.pat, shape);
    }

    fn visit_expr_tuple(&mut self, tuple: &'ast syn::ExprTuple) {
        let mut items = tuple.elems.iter();
        let first = items.next();
        // `(header::CONTENT_TYPE, "image/png")`
        if let (Some(first), Some(Expr::Lit(lit))) = (first, tuple.elems.iter().nth(1)) {
            if last_name(first).as_deref() == Some("CONTENT_TYPE") {
                if let syn::Lit::Str(s) = &lit.lit {
                    self.out.content_types.insert(s.value());
                }
            }
        }
        let status = first.and_then(status_of_path);
        let before = self.status;
        if status.is_some() {
            self.status = status;
        }
        visit::visit_expr_tuple(self, tuple);
        self.status = before;
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let Expr::Path(func) = &*call.func {
            let name = func
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default();
            if name == "Html" {
                self.out.html = true;
            }
            if name == "Json" && call.args.len() == 1 {
                self.note_json(&call.args[0]);
            } else if let Some(full) = self
                .index
                .resolve_func(&self.module, &Self::path_segments(&func.path))
            {
                if !SKIP_HELPERS.contains(&name.as_str()) {
                    self.note_wrapper(&full, call);
                }
                self.calls.push(full);
                self.visit_expr(&call.func);
                // `Json(req)` handed to another handler is its request, not this one's answer.
                for arg in &call.args {
                    let forwarded = matches!(arg, Expr::Call(inner)
                        if last_name(&inner.func).as_deref() == Some("Json"));
                    if !forwarded {
                        self.visit_expr(arg);
                    }
                }
                return;
            }
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        // `.header(header::CONTENT_TYPE, "image/png")`
        if call.method == "header" && call.args.len() == 2 {
            if let (Some("CONTENT_TYPE"), Expr::Lit(lit)) =
                (last_name(&call.args[0]).as_deref(), &call.args[1])
            {
                if let syn::Lit::Str(s) = &lit.lit {
                    self.out.content_types.insert(s.value());
                }
            }
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        let expr = Expr::Path(path.clone());
        if let Some(status) = status_of_path(&expr) {
            self.out.statuses.insert(status);
        }
        match last_name(&expr).as_deref() {
            Some("host_is_local") => self.out.checks_host = true,
            Some("local_allowed") => self.out.checks_peer = true,
            Some("required_console_addr") if self.entry => self.out.requires_console = true,
            _ => {}
        }
        visit::visit_expr_path(self, path);
    }
}

/// True for the return types that hold the handler's answer: helpers worth following.
fn answers_a_request(func: &Func) -> bool {
    let ReturnType::Type(_, ty) = &func.sig.output else {
        return false;
    };
    let text = ty.to_token_stream().to_string();
    text.contains("Response") || text.contains("Json") || text.contains("Sse")
}

fn note_signature(analysis: &mut Analysis, schemas: &mut Schemas<'_>, func: &Func) {
    let ReturnType::Type(_, ty) = &func.sig.output else {
        return;
    };
    let text = ty.to_token_stream().to_string().replace(' ', "");
    if text.contains("Sse<") {
        analysis.sse = true;
    }
    if text.contains("Html<") {
        analysis.html = true;
    }
    if let Some(inner) = json_type_in(ty) {
        let schema = schemas.of_type(&inner, &func.module, Dir::Out, &[]);
        if schema.get("x-rust-type").is_none() && !schema.as_object().is_some_and(Map::is_empty) {
            let list = analysis.bodies.entry(None).or_default();
            if !list.iter().any(|(known, _)| known == &schema) {
                list.push((schema, true));
            }
        }
    }
}

/// `T` in `Json<T>`, `Result<Json<T>, E>` and `(StatusCode, Json<T>)`.
fn json_type_in(ty: &Type) -> Option<Type> {
    match ty {
        Type::Tuple(t) => t.elems.iter().find_map(json_type_in),
        Type::Path(p) => {
            let last = p.path.segments.last()?;
            let args = generic_args(last);
            match last.ident.to_string().as_str() {
                "Json" => args.first().map(|t| (*t).clone()),
                "Result" => json_type_in(args.first()?),
                _ => None,
            }
        }
        _ => None,
    }
}

const SKIP_HELPERS: &[&str] = &["json_err"];

/// Reads a handler: the function's, or the closure's and the function it forwards to.
pub fn analyze<'a>(index: &'a Index, schemas: &mut Schemas<'a>, handler: &Handler) -> Analysis {
    let mut out = Analysis::default();
    let mut queue: Vec<(String, u8)> = Vec::new();
    let mut seen = BTreeSet::new();
    match handler {
        Handler::Func(full) => queue.push((full.clone(), 0)),
        Handler::Closure(closure, module) => {
            for input in &closure.inputs {
                if let Pat::Type(typed) = input {
                    if let Some(found) = extractor_of(&typed.ty, module, false) {
                        add_extractor(&mut out.extractors, found);
                    }
                }
            }
            let mut scan = BodyScan {
                index,
                schemas,
                module: module.clone(),
                env: BTreeMap::new(),
                out: &mut out,
                status: None,
                calls: Vec::new(),
                entry: true,
            };
            scan.visit_expr(&closure.body);
            let calls = std::mem::take(&mut scan.calls);
            queue.extend(calls.into_iter().map(|c| (c, 0)));
        }
        Handler::Opaque => {}
    }
    let mut first_named = true;
    while let Some((full, depth)) = queue.pop() {
        if !seen.insert(full.clone()) {
            continue;
        }
        let Some(func) = index.funcs.get(&full) else {
            continue;
        };
        let is_entry = depth == 0;
        if is_entry && !answers_a_request(func) && !matches!(handler, Handler::Func(_)) {
            // A closure's call that is not the handler it forwards to.
            continue;
        }
        if is_entry {
            for input in &func.sig.inputs {
                if let FnArg::Typed(typed) = input {
                    if let Some(found) = extractor_of(&typed.ty, &func.module, false) {
                        add_extractor(&mut out.extractors, found);
                    }
                }
            }
            if first_named {
                out.doc = doc_text(&func.attrs);
                first_named = false;
            }
        }
        note_signature(&mut out, schemas, func);
        let mut scan = BodyScan {
            index,
            schemas,
            module: func.module.clone(),
            env: BTreeMap::new(),
            out: &mut out,
            status: None,
            calls: Vec::new(),
            entry: is_entry,
        };
        for input in &func.sig.inputs {
            if let FnArg::Typed(typed) = input {
                let shape = Shape::Ty(typed.ty.clone(), func.module.clone());
                scan.bind_pattern(&typed.pat, Some(shape));
            }
        }
        scan.visit_block(&func.block);
        let calls = std::mem::take(&mut scan.calls);
        if depth < 2 {
            for call in calls {
                let name = call.rsplit("::").next().unwrap_or_default();
                if SKIP_HELPERS.contains(&name) || seen.contains(&call) {
                    continue;
                }
                let Some(callee) = index.funcs.get(&call) else {
                    continue;
                };
                // Helpers that answer the request, and same-file helpers that may hold its guards.
                if answers_a_request(callee) || callee.module == func.module {
                    queue.push((call, depth + 1));
                }
            }
        }
    }
    out
}
