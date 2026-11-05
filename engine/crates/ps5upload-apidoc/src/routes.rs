//! The routes the engine mounts, read out of the functions that build its router, starting at
//! `build_app_and_mcp` in `lib.rs` and following every `.merge(...)`. Anything this cannot read is
//! an error, not a route left out: a spec that silently misses routes is worse than none.

use std::collections::BTreeMap;

use quote::ToTokens;
use syn::visit::{self, Visit};
use syn::{Expr, ExprClosure};

use crate::source::{Def, Index};

const VERBS: &[&str] = &["get", "post", "put", "delete", "patch"];

#[derive(Clone)]
pub enum Handler {
    /// A named function: its full path, if the source has it.
    Func(String),
    /// A closure, usually one that adds shared state and forwards to a function.
    Closure(Box<ExprClosure>, String),
    /// Something else that makes a handler.
    Opaque,
}

pub struct Route {
    pub method: String,
    pub path: String,
    pub handler: Handler,
}

/// `(module, function)` of the router root.
pub const ROOT: (&str, &str) = ("ps5upload_engine", "build_app_and_mcp");

pub fn scan(index: &Index) -> Result<Vec<Route>, String> {
    let mut out = Vec::new();
    let root = format!("{}::{}", ROOT.0, ROOT.1);
    let mut seen = Vec::new();
    scan_function(index, &root, &mut out, &mut seen)?;
    Ok(out)
}

fn scan_function(
    index: &Index,
    full: &str,
    out: &mut Vec<Route>,
    seen: &mut Vec<String>,
) -> Result<(), String> {
    if seen.iter().any(|s| s == full) {
        return Ok(());
    }
    seen.push(full.to_string());
    let func = index
        .funcs
        .get(full)
        .ok_or_else(|| format!("the router function {full} is gone"))?;
    let mut lets = BTreeMap::new();
    let mut scan = Scan {
        index,
        module: func.module.clone(),
        lets: &mut lets,
        routes: Vec::new(),
        merges: Vec::new(),
        errors: Vec::new(),
    };
    scan.visit_block(&func.block);
    let Scan {
        routes,
        merges,
        errors,
        ..
    } = scan;
    if let Some(problem) = errors.into_iter().next() {
        return Err(format!("{full}: {problem}"));
    }
    out.extend(routes);
    for merged in merges {
        scan_function(index, &merged, out, seen)?;
    }
    Ok(())
}

struct Scan<'a> {
    index: &'a Index,
    module: String,
    /// `let name = some::router(...)`: what a `.merge(name)` means.
    lets: &'a mut BTreeMap<String, Expr>,
    routes: Vec<Route>,
    merges: Vec<String>,
    errors: Vec<String>,
}

fn segments(path: &syn::Path) -> Vec<String> {
    path.segments.iter().map(|s| s.ident.to_string()).collect()
}

impl Scan<'_> {
    fn path_of(&mut self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Lit(lit) => match &lit.lit {
                syn::Lit::Str(s) => Some(s.value()),
                _ => None,
            },
            Expr::Reference(r) => match &*r.expr {
                // `&format!("{base}/")`: the reverse-proxy prefix's own root, which is `/` again.
                Expr::Macro(m) if m.mac.path.is_ident("format") => None,
                other => self.path_of(other),
            },
            Expr::Path(p) => {
                let full = self.index.resolve_def(&self.module, &segments(&p.path))?;
                let (_, def) = self.index.defs.get(&full)?;
                match def {
                    Def::Const(c) => match &*c.expr {
                        Expr::Lit(lit) => match &lit.lit {
                            syn::Lit::Str(s) => Some(s.value()),
                            _ => None,
                        },
                        _ => None,
                    },
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn handlers_of(
        &mut self,
        expr: &Expr,
        into: &mut Vec<(String, Handler)>,
    ) -> Result<(), String> {
        match expr {
            Expr::Call(call) => {
                let Expr::Path(func) = &*call.func else {
                    return Err("a route's method is not a plain call".into());
                };
                let Some(last) = func.path.segments.last() else {
                    return Err("a route's method has no name".into());
                };
                let verb = last.ident.to_string();
                if !VERBS.contains(&verb.as_str()) {
                    return Err(format!("add the method `{verb}` to the scan"));
                }
                let arg = call.args.first().ok_or("a route's method has no handler")?;
                into.push((verb.to_ascii_uppercase(), self.handler_of(arg)));
                Ok(())
            }
            Expr::MethodCall(call) => {
                let name = call.method.to_string();
                self.handlers_of(&call.receiver, into)?;
                if VERBS.contains(&name.as_str()) {
                    let arg = call.args.first().ok_or("a route's method has no handler")?;
                    into.push((name.to_ascii_uppercase(), self.handler_of(arg)));
                }
                // `.layer(...)` and the like change nothing about what a route is.
                Ok(())
            }
            other => Err(format!(
                "add this way to write a route's methods to the scan: {}",
                other.to_token_stream()
            )),
        }
    }

    fn handler_of(&mut self, expr: &Expr) -> Handler {
        match expr {
            Expr::Path(p) => match self.index.resolve_func(&self.module, &segments(&p.path)) {
                Some(full) => Handler::Func(full),
                None => Handler::Opaque,
            },
            Expr::Closure(c) => Handler::Closure(Box::new(c.clone()), self.module.clone()),
            // `get({ let shared = shared.clone(); move |...| ... })`
            Expr::Block(b) => match b.block.stmts.last() {
                Some(syn::Stmt::Expr(tail, None)) => self.handler_of(tail),
                _ => Handler::Opaque,
            },
            _ => Handler::Opaque,
        }
    }

    fn router_named(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Call(call) => {
                let Expr::Path(func) = &*call.func else {
                    return None;
                };
                self.index.resolve_func(&self.module, &segments(&func.path))
            }
            Expr::Path(p) => {
                let name = p.path.get_ident()?.to_string();
                self.router_named(self.lets.get(&name)?)
            }
            _ => None,
        }
    }
}

impl<'ast> Visit<'ast> for Scan<'_> {
    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let (syn::Pat::Ident(name), Some(init)) = (&local.pat, &local.init) {
            self.lets
                .insert(name.ident.to_string(), (*init.expr).clone());
        }
        visit::visit_local(self, local);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        // The receiver first, so routes come out in the order they are written.
        self.visit_expr(&call.receiver);
        match call.method.to_string().as_str() {
            "route" if call.args.len() == 2 => {
                let mut args = call.args.iter();
                let (Some(path), Some(methods)) = (args.next(), args.next()) else {
                    return;
                };
                let Some(path_text) = self.path_of(path) else {
                    if !matches!(path, Expr::Reference(_)) {
                        self.errors.push(format!(
                            "add this route's path to the scan: {}",
                            path.to_token_stream()
                        ));
                    }
                    return;
                };
                let mut handlers = Vec::new();
                match self.handlers_of(methods, &mut handlers) {
                    Ok(()) if handlers.is_empty() => {
                        self.errors.push(format!("no method found for {path_text}"));
                    }
                    Ok(()) => {
                        for (method, handler) in handlers {
                            self.routes.push(Route {
                                method,
                                path: path_text.clone(),
                                handler,
                            });
                        }
                    }
                    Err(problem) => self.errors.push(format!("{path_text}: {problem}")),
                }
            }
            "merge" if call.args.len() == 1 => match self.router_named(&call.args[0]) {
                Some(full) => self.merges.push(full),
                None => self.errors.push(format!(
                    "cannot follow this merged router: {}",
                    call.args[0].to_token_stream()
                )),
            },
            "nest" | "nest_service" | "route_service" => {
                let text = call.args[0].to_token_stream().to_string();
                // Only the reverse-proxy prefix, which wraps every route the same way.
                if text != "base" {
                    self.errors
                        .push(format!("add `.{}({text}, ...)` to the scan", call.method));
                }
            }
            _ => {}
        }
        for arg in &call.args {
            if call.method != "route" {
                self.visit_expr(arg);
            }
        }
    }
}
