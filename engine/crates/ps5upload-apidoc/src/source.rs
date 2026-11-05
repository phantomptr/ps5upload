//! The Rust source as an index: every struct, enum, type alias, const and function of the crates
//! the API's types come from, by module, with each module's `use` lines to resolve names by.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use syn::{Attribute, Item};

/// A crate the API's types can come from: its name in `use` lines, and its directory.
pub const CRATES: &[(&str, &str)] = &[
    ("ps5upload_engine", "ps5upload-engine"),
    ("ps5upload_core", "ps5upload-core"),
    ("ps5upload_fpkg", "ps5upload-fpkg"),
    ("ps5upload_pkg", "ps5upload-pkg"),
    ("ps5upload_ava1", "ps5upload-ava1"),
];

#[derive(Clone)]
pub enum Def {
    Struct(syn::ItemStruct),
    Enum(syn::ItemEnum),
    Alias(syn::ItemType),
    Const(syn::ItemConst),
}

#[derive(Clone)]
pub struct Func {
    pub module: String,
    pub sig: syn::Signature,
    pub attrs: Vec<Attribute>,
    pub block: syn::Block,
    /// The `impl` type, for a method: what `Self` means.
    pub self_ty: Option<syn::Type>,
}

#[derive(Default)]
pub struct Module {
    /// Name in scope to the path it stands for.
    pub uses: BTreeMap<String, Vec<String>>,
    pub globs: Vec<Vec<String>>,
}

#[derive(Default)]
pub struct Index {
    pub defs: BTreeMap<String, (String, Def)>,
    pub funcs: BTreeMap<String, Func>,
    pub modules: BTreeMap<String, Module>,
    /// A plain name to every full path that defines it.
    pub by_name: BTreeMap<String, BTreeSet<String>>,
    /// A method's name to its full paths (`module::Type::name`).
    pub methods: BTreeMap<String, BTreeSet<String>>,
}

impl Index {
    pub fn load(crates_dir: &Path) -> Result<Self, String> {
        let mut index = Index::default();
        for (name, dir) in CRATES {
            let src = crates_dir.join(dir).join("src");
            let lib = src.join("lib.rs");
            let root = if lib.exists() {
                lib
            } else {
                src.join("main.rs")
            };
            index.read_file(&root, name, &src)?;
        }
        Ok(index)
    }

    /// An index of one module's source, for tests.
    #[cfg(test)]
    pub fn of_source(module: &str, text: &str) -> Result<Self, String> {
        let parsed = syn::parse_file(text).map_err(|e| e.to_string())?;
        let mut index = Index::default();
        index.read_items(parsed.items, module, Path::new("."), Path::new("."))?;
        Ok(index)
    }

    /// `dir` is where this file's child modules live.
    fn read_file(&mut self, file: &Path, module: &str, dir: &Path) -> Result<(), String> {
        let text =
            fs::read_to_string(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let parsed =
            syn::parse_file(&text).map_err(|e| format!("cannot parse {}: {e}", file.display()))?;
        let file_dir = file.parent().unwrap_or(dir).to_path_buf();
        self.read_items(parsed.items, module, dir, &file_dir)
    }

    fn read_items(
        &mut self,
        items: Vec<Item>,
        module: &str,
        dir: &Path,
        file_dir: &Path,
    ) -> Result<(), String> {
        self.modules.entry(module.to_string()).or_default();
        for item in items {
            match item {
                Item::Mod(m) if is_test_only(&m.attrs) => {}
                Item::Mod(m) => {
                    let name = ident_name(&m.ident);
                    let child = format!("{module}::{name}");
                    if let Some((_, inner)) = m.content {
                        self.read_items(inner, &child, &dir.join(&name), file_dir)?;
                    } else if let Some((file, child_dir)) =
                        module_file(&m.attrs, dir, file_dir, &name)
                    {
                        self.read_file(&file, &child, &child_dir)?;
                    }
                }
                Item::Use(u) => {
                    let scope = self.modules.entry(module.to_string()).or_default();
                    read_use(&u.tree, &mut Vec::new(), scope);
                }
                Item::Struct(s) if !is_test_only(&s.attrs) => {
                    self.add_def(module, s.ident.to_string(), Def::Struct(s));
                }
                Item::Enum(e) if !is_test_only(&e.attrs) => {
                    self.add_def(module, e.ident.to_string(), Def::Enum(e));
                }
                Item::Type(t) if !is_test_only(&t.attrs) => {
                    self.add_def(module, t.ident.to_string(), Def::Alias(t));
                }
                Item::Const(c) if !is_test_only(&c.attrs) => {
                    self.add_def(module, c.ident.to_string(), Def::Const(c));
                }
                Item::Fn(f) if !is_test_only(&f.attrs) => {
                    let path = format!("{module}::{}", f.sig.ident);
                    // The same name under two `cfg`s: a stub for a platform that lacks the
                    // feature, and the real one. The real one takes what the request has and does
                    // more with it.
                    if self.funcs.get(&path).is_some_and(|known| {
                        weight(&known.sig, &known.block) >= weight(&f.sig, &f.block)
                    }) {
                        continue;
                    }
                    self.by_name
                        .entry(f.sig.ident.to_string())
                        .or_default()
                        .insert(path.clone());
                    self.funcs.insert(
                        path,
                        Func {
                            module: module.to_string(),
                            sig: f.sig,
                            attrs: f.attrs,
                            block: *f.block,
                            self_ty: None,
                        },
                    );
                }
                Item::Impl(i) if !is_test_only(&i.attrs) && i.trait_.is_none() => {
                    let self_name = last_segment_of_type(&i.self_ty).unwrap_or_default();
                    for inner in i.items {
                        if let syn::ImplItem::Fn(f) = inner {
                            let path = format!("{module}::{self_name}::{}", f.sig.ident);
                            self.methods
                                .entry(f.sig.ident.to_string())
                                .or_default()
                                .insert(path.clone());
                            self.funcs.insert(
                                path,
                                Func {
                                    module: module.to_string(),
                                    sig: f.sig,
                                    attrs: f.attrs,
                                    block: f.block,
                                    self_ty: Some((*i.self_ty).clone()),
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn add_def(&mut self, module: &str, name: String, def: Def) {
        let path = format!("{module}::{name}");
        self.by_name.entry(name).or_default().insert(path.clone());
        self.defs.insert(path, (module.to_string(), def));
    }

    /// The full path of the item `segments` means when written in `module`, if the index has one.
    /// `is_known` says which table to look in.
    fn resolve_in(
        &self,
        module: &str,
        segments: &[String],
        is_known: &dyn Fn(&str) -> bool,
        depth: u8,
    ) -> Option<String> {
        if segments.is_empty() || depth == 0 {
            return None;
        }
        let crate_name = module.split("::").next().unwrap_or(module);
        let first = segments[0].as_str();
        let rest = &segments[1..];
        let join = |base: &str, rest: &[String]| {
            if rest.is_empty() {
                base.to_string()
            } else {
                format!("{base}::{}", rest.join("::"))
            }
        };
        let absolute = match first {
            "crate" => Some(join(crate_name, rest)),
            "self" => Some(join(module, rest)),
            "super" => {
                let parent = module.rsplit_once("::").map_or(module, |(p, _)| p);
                if rest.first().map(String::as_str) == Some("super") {
                    return self.resolve_in(parent, rest, is_known, depth - 1);
                }
                Some(join(parent, rest))
            }
            name if CRATES.iter().any(|(c, _)| *c == name) => Some(join(name, rest)),
            _ => None,
        };
        if let Some(path) = absolute {
            return self.follow(&path, is_known, depth);
        }
        // In this module, or a child module of it.
        let local = format!("{module}::{}", segments.join("::"));
        if is_known(&local) {
            return Some(local);
        }
        // Through a `use` line.
        if let Some(scope) = self.modules.get(module) {
            if let Some(target) = scope.uses.get(first) {
                let mut full = target.clone();
                full.extend_from_slice(rest);
                if full != segments {
                    if let Some(found) = self.resolve_in(module, &full, is_known, depth - 1) {
                        return Some(found);
                    }
                }
            }
            for glob in &scope.globs {
                let mut full = glob.clone();
                full.extend_from_slice(segments);
                if let Some(found) = self.resolve_in(module, &full, is_known, depth - 1) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// An absolute path that may be a re-export.
    fn follow(&self, path: &str, is_known: &dyn Fn(&str) -> bool, depth: u8) -> Option<String> {
        if is_known(path) {
            return Some(path.to_string());
        }
        let (parent, name) = path.rsplit_once("::")?;
        let scope = self.modules.get(parent)?;
        if let Some(target) = scope.uses.get(name) {
            return self.resolve_in(parent, target, is_known, depth.saturating_sub(1));
        }
        for glob in &scope.globs {
            let mut full = glob.clone();
            full.push(name.to_string());
            if let Some(found) = self.resolve_in(parent, &full, is_known, depth.saturating_sub(1)) {
                return Some(found);
            }
        }
        // `a::b::Type::method` with `Type` re-exported through `a::b`.
        let (grand, owner) = parent.rsplit_once("::")?;
        let scope = self.modules.get(grand)?;
        let target = scope.uses.get(owner)?;
        let mut full = target.clone();
        full.push(name.to_string());
        self.resolve_in(grand, &full, is_known, depth.saturating_sub(1))
    }

    /// The struct, enum, alias or const `segments` names in `module`. A plain name that no scope
    /// explains falls back on being the only item of that name in the same crate.
    pub fn resolve_def(&self, module: &str, segments: &[String]) -> Option<String> {
        let known = |p: &str| self.defs.contains_key(p);
        self.resolve_in(module, segments, &known, 8)
            .or_else(|| self.only_of_name(module, segments, &known))
    }

    pub fn resolve_func(&self, module: &str, segments: &[String]) -> Option<String> {
        let known = |p: &str| self.funcs.contains_key(p);
        if segments.len() > 1 && segments[0] == "Self" {
            return None;
        }
        self.resolve_in(module, segments, &known, 8)
            .or_else(|| self.only_of_name(module, segments, &known))
    }

    fn only_of_name(
        &self,
        module: &str,
        segments: &[String],
        known: &dyn Fn(&str) -> bool,
    ) -> Option<String> {
        let name = segments.last()?;
        // Only for a bare name or a written `module::name`; an outside path stays outside.
        let crate_name = module.split("::").next().unwrap_or(module);
        let first = segments.first()?;
        // Imported from elsewhere (`use serde_json::Value`): not ours, whatever else has the name.
        let imported = self.modules.get(module).and_then(|m| m.uses.get(first));
        if imported.is_some() {
            return None;
        }
        if segments.len() > 1 && !self.is_own_module(crate_name, first) {
            return None;
        }
        let same_crate: Vec<&String> = self
            .by_name
            .get(name)?
            .iter()
            .filter(|p| known(p) && p.split("::").next() == Some(crate_name))
            .filter(|p| segments.len() == 1 || p.contains(&format!("::{first}::")))
            .collect();
        match same_crate.as_slice() {
            [only] => Some((*only).clone()),
            _ => None,
        }
    }

    fn is_own_module(&self, crate_name: &str, name: &str) -> bool {
        let needle = format!("::{name}");
        self.modules
            .keys()
            .any(|m| m.starts_with(crate_name) && m.ends_with(&needle))
    }
}

fn ident_name(ident: &syn::Ident) -> String {
    ident.to_string().trim_start_matches("r#").to_string()
}

pub fn last_segment_of_type(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        syn::Type::Reference(r) => last_segment_of_type(&r.elem),
        _ => None,
    }
}

fn module_file(
    attrs: &[Attribute],
    dir: &Path,
    file_dir: &Path,
    name: &str,
) -> Option<(PathBuf, PathBuf)> {
    for attr in attrs {
        if attr.path().is_ident("path") {
            if let syn::Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(lit) = &nv.value {
                    if let syn::Lit::Str(s) = &lit.lit {
                        return Some((file_dir.join(s.value()), dir.join(name)));
                    }
                }
            }
        }
    }
    let flat = dir.join(format!("{name}.rs"));
    if flat.exists() {
        return Some((flat, dir.join(name)));
    }
    let nested = dir.join(name).join("mod.rs");
    nested.exists().then(|| (nested, dir.join(name)))
}

fn weight(sig: &syn::Signature, block: &syn::Block) -> (usize, usize) {
    (sig.inputs.len(), block.stmts.len())
}

/// `#[cfg(test)]`, alone or in an `all(...)`; not `cfg(not(test))`.
fn is_test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg") && {
            let text = quote::quote!(#attr).to_string().replace(' ', "");
            text.contains("cfg(test)") || text.contains("all(test")
        }
    })
}

fn read_use(tree: &syn::UseTree, prefix: &mut Vec<String>, scope: &mut Module) {
    match tree {
        syn::UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            read_use(&p.tree, prefix, scope);
            prefix.pop();
        }
        syn::UseTree::Name(n) => {
            let name = n.ident.to_string();
            if name == "self" {
                if let Some(last) = prefix.last().cloned() {
                    scope.uses.insert(last, prefix.clone());
                }
            } else {
                let mut full = prefix.clone();
                full.push(name.clone());
                scope.uses.insert(name, full);
            }
        }
        syn::UseTree::Rename(r) => {
            let mut full = prefix.clone();
            if r.ident != "self" {
                full.push(r.ident.to_string());
            }
            scope.uses.insert(r.rename.to_string(), full);
        }
        syn::UseTree::Glob(_) => scope.globs.push(prefix.clone()),
        syn::UseTree::Group(g) => {
            for item in &g.items {
                read_use(item, prefix, scope);
            }
        }
    }
}

/// A doc comment's text, lines joined by `\n`, with the one leading space off each.
pub fn doc_text(attrs: &[Attribute]) -> String {
    let mut lines = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta {
            if let syn::Expr::Lit(lit) = &nv.value {
                if let syn::Lit::Str(s) = &lit.lit {
                    let value = s.value();
                    lines.push(value.strip_prefix(' ').unwrap_or(&value).to_string());
                }
            }
        }
    }
    lines.join("\n").trim().to_string()
}
