//! Rust types as JSON Schema, by the rules serde applies to them. A schema depends on the direction:
//! `skip_serializing`, a `rename` for one side, and what `required` means all differ between a body
//! the engine reads and one it writes.

use std::collections::{BTreeMap, BTreeSet};

use quote::ToTokens;
use serde_json::{json, Map, Value};
use syn::{Attribute, Fields, GenericArgument, PathArguments, Type};

use crate::source::{doc_text, Def, Index};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Dir {
    /// A body or query the engine reads.
    In,
    /// A body the engine writes.
    Out,
}

impl Dir {
    fn suffix(self) -> &'static str {
        match self {
            Dir::In => "@in",
            Dir::Out => "@out",
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct SerdeAttrs {
    pub rename_in: Option<String>,
    pub rename_out: Option<String>,
    pub rename_all_in: Option<String>,
    pub rename_all_out: Option<String>,
    pub aliases: Vec<String>,
    pub default: bool,
    pub skip_in: bool,
    pub skip_out: bool,
    pub skip_if: bool,
    pub flatten: bool,
    pub custom_in: bool,
    pub custom_out: bool,
    pub tag: Option<String>,
    pub content: Option<String>,
    pub untagged: bool,
    pub transparent: bool,
    pub convert_in: Option<String>,
    pub convert_out: Option<String>,
    pub deny_unknown: bool,
}

pub fn serde_attrs(attrs: &[Attribute]) -> SerdeAttrs {
    let mut out = SerdeAttrs::default();
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        // An attribute this cannot read leaves the rest of it unread: the generator's tests pin
        // the forms the source uses.
        let _ = attr.parse_nested_meta(|meta| {
            let name = meta
                .path
                .get_ident()
                .map(ToString::to_string)
                .unwrap_or_default();
            let mut text = None;
            let mut pair = (None, None);
            if meta.input.peek(syn::Token![=]) {
                let value: syn::Expr = meta.value()?.parse()?;
                if let syn::Expr::Lit(lit) = &value {
                    if let syn::Lit::Str(s) = &lit.lit {
                        text = Some(s.value());
                    }
                }
            } else if meta.input.peek(syn::token::Paren) {
                meta.parse_nested_meta(|inner| {
                    let value: syn::LitStr = inner.value()?.parse()?;
                    if inner.path.is_ident("serialize") {
                        pair.1 = Some(value.value());
                    } else if inner.path.is_ident("deserialize") {
                        pair.0 = Some(value.value());
                    }
                    Ok(())
                })?;
            }
            match name.as_str() {
                "rename" => {
                    out.rename_in = text.clone().or(pair.0);
                    out.rename_out = text.or(pair.1);
                }
                "rename_all" => {
                    out.rename_all_in = text.clone().or(pair.0);
                    out.rename_all_out = text.or(pair.1);
                }
                "alias" => out.aliases.extend(text),
                "default" => out.default = true,
                "skip" => {
                    out.skip_in = true;
                    out.skip_out = true;
                }
                "skip_serializing" => out.skip_out = true,
                "skip_deserializing" => out.skip_in = true,
                "skip_serializing_if" => out.skip_if = true,
                "flatten" => out.flatten = true,
                "with" => {
                    out.custom_in = true;
                    out.custom_out = true;
                }
                "deserialize_with" => out.custom_in = true,
                "serialize_with" => out.custom_out = true,
                "tag" => out.tag = text,
                "content" => out.content = text,
                "untagged" => out.untagged = true,
                "transparent" => out.transparent = true,
                "from" | "try_from" => out.convert_in = text,
                "into" => out.convert_out = text,
                "deny_unknown_fields" => out.deny_unknown = true,
                _ => {}
            }
            Ok(())
        });
    }
    out
}

/// `field_name` or `VariantName` as serde's `rename_all` spells it.
pub fn apply_rename_all(name: &str, rule: Option<&str>) -> String {
    let Some(rule) = rule else {
        return name.to_string();
    };
    let words: Vec<String> = if name.contains('_') || name.chars().all(|c| !c.is_uppercase()) {
        name.split('_').map(str::to_lowercase).collect()
    } else {
        let mut words = Vec::new();
        let mut current = String::new();
        for c in name.chars() {
            if c.is_uppercase() && !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            current.extend(c.to_lowercase());
        }
        words.push(current);
        words
    };
    let capitalised = |w: &String| {
        let mut chars = w.chars();
        chars.next().map_or(String::new(), |f| {
            f.to_uppercase().collect::<String>() + chars.as_str()
        })
    };
    match rule {
        "lowercase" => words.concat(),
        "UPPERCASE" => words.concat().to_uppercase(),
        "PascalCase" => words.iter().map(capitalised).collect(),
        "camelCase" => {
            let mut parts = words.iter();
            let first = parts.next().cloned().unwrap_or_default();
            first + &parts.map(capitalised).collect::<String>()
        }
        "snake_case" => words.join("_"),
        "SCREAMING_SNAKE_CASE" => words.join("_").to_uppercase(),
        "kebab-case" => words.join("-"),
        "SCREAMING-KEBAB-CASE" => words.join("-").to_uppercase(),
        _ => name.to_string(),
    }
}

fn derives(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("derive")
            && attr
                .to_token_stream()
                .to_string()
                .split(|c: char| !c.is_alphanumeric())
                .any(|word| word == name)
    })
}

/// One field of a request read from a query, or of an object.
pub struct FieldInfo {
    pub name: String,
    pub schema: Value,
    pub required: bool,
    pub aliases: Vec<String>,
}

pub struct Schemas<'a> {
    index: &'a Index,
    /// Full path and direction to schema: everything a `$ref` points at.
    pub done: BTreeMap<(String, Dir), Value>,
    active: BTreeSet<(String, Dir)>,
}

const REF_PREFIX: &str = "#/components/schemas/";

/// The Rust type's name, for a schema that says nothing else about it.
fn opaque(ty: &Type) -> Value {
    let text = ty.to_token_stream().to_string().replace(' ', "");
    json!({ "x-rust-type": text })
}

fn with_description(mut schema: Value, text: &str) -> Value {
    if text.is_empty() {
        return schema;
    }
    if schema.get("$ref").is_some() {
        // 3.1 allows siblings of $ref, and a field's words are about the field.
        schema["description"] = json!(text);
        return schema;
    }
    if let Some(map) = schema.as_object_mut() {
        map.insert("description".into(), json!(text));
    }
    schema
}

fn nullable(schema: Value) -> Value {
    if let Some(kind) = schema.get("type").and_then(Value::as_str) {
        let mut schema = schema.clone();
        let kind = kind.to_string();
        schema["type"] = json!([kind, "null"]);
        return schema;
    }
    if schema
        .as_object()
        .is_some_and(|m| m.is_empty() || m.keys().all(|k| k == "x-rust-type" || k == "description"))
    {
        return schema;
    }
    json!({ "anyOf": [schema, { "type": "null" }] })
}

impl<'a> Schemas<'a> {
    pub fn new(index: &'a Index) -> Self {
        Self {
            index,
            done: BTreeMap::new(),
            active: BTreeSet::new(),
        }
    }

    /// The schema says nothing about the JSON: a type the source has no fields for.
    pub fn is_opaque(&self, schema: &Value) -> bool {
        if schema.get("x-rust-type").is_some() {
            return true;
        }
        let Some(target) = schema
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| r.strip_prefix(REF_PREFIX))
        else {
            return false;
        };
        let key = match target.rsplit_once('@') {
            Some((path, "in")) => (path.to_string(), Dir::In),
            Some((path, _)) => (path.to_string(), Dir::Out),
            None => return false,
        };
        self.done
            .get(&key)
            .is_some_and(|d| d.get("x-rust-type").is_some())
    }

    pub fn of_type(&mut self, ty: &Type, module: &str, dir: Dir, generics: &[String]) -> Value {
        match ty {
            Type::Reference(r) => self.of_type(&r.elem, module, dir, generics),
            Type::Paren(p) => self.of_type(&p.elem, module, dir, generics),
            Type::Group(g) => self.of_type(&g.elem, module, dir, generics),
            Type::Slice(s) => {
                json!({ "type": "array", "items": self.of_type(&s.elem, module, dir, generics) })
            }
            Type::Array(a) => {
                json!({ "type": "array", "items": self.of_type(&a.elem, module, dir, generics) })
            }
            Type::Tuple(t) if t.elems.is_empty() => json!({ "type": "null" }),
            Type::Tuple(t) => {
                let items: Vec<Value> = t
                    .elems
                    .iter()
                    .map(|e| self.of_type(e, module, dir, generics))
                    .collect();
                json!({
                    "type": "array",
                    "prefixItems": items,
                    "minItems": t.elems.len(),
                    "maxItems": t.elems.len(),
                })
            }
            Type::Path(p) => self.of_path(ty, &p.path, module, dir, generics),
            _ => opaque(ty),
        }
    }

    fn of_path(
        &mut self,
        ty: &Type,
        path: &syn::Path,
        module: &str,
        dir: Dir,
        generics: &[String],
    ) -> Value {
        let Some(last) = path.segments.last() else {
            return opaque(ty);
        };
        let name = last.ident.to_string();
        let args: Vec<&Type> = match &last.arguments {
            PathArguments::AngleBracketed(a) => a
                .args
                .iter()
                .filter_map(|arg| match arg {
                    GenericArgument::Type(t) => Some(t),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        if path.segments.len() == 1 && generics.contains(&name) {
            return json!({});
        }
        let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        // A type of this codebase comes first: it may share a name with a std one.
        if !is_std_name(&name) || segments.len() > 1 {
            if let Some(full) = self.index.resolve_def(module, &segments) {
                return self.of_def(&full, ty, dir);
            }
        }
        match name.as_str() {
            "String" | "str" | "PathBuf" | "OsString" | "OsStr" | "Path" | "char" => {
                json!({ "type": "string" })
            }
            "Uuid" => json!({ "type": "string", "format": "uuid" }),
            "IpAddr" => json!({ "type": "string", "format": "ip" }),
            "Ipv4Addr" => json!({ "type": "string", "format": "ipv4" }),
            "Ipv6Addr" => json!({ "type": "string", "format": "ipv6" }),
            "SocketAddr" => json!({ "type": "string" }),
            "bool" | "AtomicBool" => json!({ "type": "boolean" }),
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "AtomicU64" | "AtomicU32"
            | "AtomicUsize" | "NonZeroU32" | "NonZeroU64" | "NonZeroUsize" => {
                json!({ "type": "integer", "minimum": 0 })
            }
            "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "AtomicI64" => {
                json!({ "type": "integer" })
            }
            "f32" | "f64" => json!({ "type": "number" }),
            "Option" => match args.first() {
                Some(inner) => nullable(self.of_type(inner, module, dir, generics)),
                None => opaque(ty),
            },
            "Box" | "Arc" | "Rc" | "Cow" | "RwLock" | "Mutex" => match args.last() {
                Some(inner) => self.of_type(inner, module, dir, generics),
                None => json!({ "type": "string" }), // Cow<str> holds a lifetime and a str.
            },
            "Vec" | "VecDeque" | "HashSet" | "BTreeSet" | "IndexSet" | "LinkedList" => {
                match args.first() {
                    Some(inner) => {
                        json!({ "type": "array", "items": self.of_type(inner, module, dir, generics) })
                    }
                    None => opaque(ty),
                }
            }
            "HashMap" | "BTreeMap" | "IndexMap" | "Map" => {
                let values = match args.get(1) {
                    Some(inner) => self.of_type(inner, module, dir, generics),
                    None => json!({}),
                };
                json!({ "type": "object", "additionalProperties": values })
            }
            "Value" if segments.len() == 1 || segments[0] == "serde_json" => json!({}),
            _ => opaque(ty),
        }
    }

    /// A `$ref` to a struct or enum of the source; a type alias is what it stands for.
    fn of_def(&mut self, full: &str, ty: &Type, dir: Dir) -> Value {
        let Some((module, def)) = self.index.defs.get(full).cloned() else {
            return opaque(ty);
        };
        match &def {
            Def::Alias(alias) => {
                let generics = generic_names(&alias.generics);
                self.of_type(&alias.ty, &module, dir, &generics)
            }
            Def::Const(_) => opaque(ty),
            Def::Struct(_) | Def::Enum(_) => {
                let key = (full.to_string(), dir);
                if !self.done.contains_key(&key) && self.active.insert(key.clone()) {
                    let schema = self.build_def(&module, &def, dir, ty);
                    self.active.remove(&key);
                    self.done.insert(key, schema);
                }
                json!({ "$ref": format!("{REF_PREFIX}{full}{}", dir.suffix()) })
            }
        }
    }

    fn build_def(&mut self, module: &str, def: &Def, dir: Dir, ty: &Type) -> Value {
        match def {
            Def::Struct(s) => {
                if !derives(
                    &s.attrs,
                    if dir == Dir::In {
                        "Deserialize"
                    } else {
                        "Serialize"
                    },
                ) {
                    let mut loose = opaque(ty);
                    loose["description"] = json!(
                        "Written by hand in the engine: the source has no field list for this."
                    );
                    return loose;
                }
                let attrs = serde_attrs(&s.attrs);
                let generics = generic_names(&s.generics);
                let converted = if dir == Dir::In {
                    attrs.convert_in.as_deref()
                } else {
                    attrs.convert_out.as_deref()
                };
                if let Some(other) = converted {
                    if let Ok(other) = syn::parse_str::<Type>(other) {
                        return self.of_type(&other, module, dir, &generics);
                    }
                }
                let body = self.struct_body(s, module, dir, &attrs, &generics);
                with_description(body, &doc_text(&s.attrs))
            }
            Def::Enum(e) => {
                if !derives(
                    &e.attrs,
                    if dir == Dir::In {
                        "Deserialize"
                    } else {
                        "Serialize"
                    },
                ) {
                    let mut loose = opaque(ty);
                    loose["description"] = json!(
                        "Written by hand in the engine: the source has no field list for this."
                    );
                    return loose;
                }
                let body = self.enum_body(e, module, dir);
                with_description(body, &doc_text(&e.attrs))
            }
            _ => opaque(ty),
        }
    }

    fn struct_body(
        &mut self,
        s: &syn::ItemStruct,
        module: &str,
        dir: Dir,
        attrs: &SerdeAttrs,
        generics: &[String],
    ) -> Value {
        match &s.fields {
            Fields::Named(named) => self.object_of(named, module, dir, attrs, generics),
            Fields::Unnamed(tuple) if tuple.unnamed.len() == 1 => {
                self.of_type(&tuple.unnamed[0].ty, module, dir, generics)
            }
            Fields::Unnamed(tuple) => {
                let items: Vec<Value> = tuple
                    .unnamed
                    .iter()
                    .map(|f| self.of_type(&f.ty, module, dir, generics))
                    .collect();
                json!({ "type": "array", "prefixItems": items })
            }
            Fields::Unit => json!({ "type": "null" }),
        }
    }

    /// The object for named fields; flattened ones as `allOf` beside it.
    fn object_of(
        &mut self,
        named: &syn::FieldsNamed,
        module: &str,
        dir: Dir,
        attrs: &SerdeAttrs,
        generics: &[String],
    ) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        let mut flattened = Vec::new();
        for field in &named.named {
            let serde = serde_attrs(&field.attrs);
            if serde.flatten {
                flattened.push(self.of_type(&field.ty, module, dir, generics));
                continue;
            }
            let Some(info) = self.field_info(field, module, dir, attrs, generics, &serde) else {
                continue;
            };
            if info.required {
                required.push(json!(info.name));
            }
            let mut schema = info.schema;
            if !info.aliases.is_empty() {
                schema["x-aliases"] = json!(info.aliases);
            }
            properties.insert(info.name, schema);
        }
        let mut object = Map::new();
        object.insert("type".into(), json!("object"));
        object.insert("properties".into(), Value::Object(properties));
        if !required.is_empty() {
            object.insert("required".into(), Value::Array(required));
        }
        if dir == Dir::In && attrs.deny_unknown {
            object.insert("additionalProperties".into(), json!(false));
        }
        if flattened.is_empty() {
            return Value::Object(object);
        }
        flattened.insert(0, Value::Object(object));
        json!({ "allOf": flattened })
    }

    /// One named field as serde treats it, or `None` when this direction skips it.
    fn field_info(
        &mut self,
        field: &syn::Field,
        module: &str,
        dir: Dir,
        container: &SerdeAttrs,
        generics: &[String],
        serde: &SerdeAttrs,
    ) -> Option<FieldInfo> {
        if (dir == Dir::In && serde.skip_in) || (dir == Dir::Out && serde.skip_out) {
            return None;
        }
        let rust_name = field.ident.as_ref()?.to_string();
        let rust_name = rust_name.trim_start_matches("r#");
        let (rename, rule) = match dir {
            Dir::In => (&serde.rename_in, container.rename_all_in.as_deref()),
            Dir::Out => (&serde.rename_out, container.rename_all_out.as_deref()),
        };
        let name = rename
            .clone()
            .unwrap_or_else(|| apply_rename_all(rust_name, rule));
        let custom = if dir == Dir::In {
            serde.custom_in
        } else {
            serde.custom_out
        };
        let is_option = matches!(&field.ty, Type::Path(p)
            if p.path.segments.last().is_some_and(|s| s.ident == "Option"));
        let schema = if custom {
            // Its own (de)serializer: the Rust type says nothing about the JSON.
            opaque(&field.ty)
        } else {
            self.of_type(&field.ty, module, dir, generics)
        };
        let required = match dir {
            Dir::In => !is_option && !serde.default && !container.default,
            Dir::Out => !serde.skip_if,
        };
        Some(FieldInfo {
            name,
            schema: with_description(schema, &doc_text(&field.attrs)),
            required,
            aliases: if dir == Dir::In {
                serde.aliases.clone()
            } else {
                Vec::new()
            },
        })
    }

    fn enum_body(&mut self, e: &syn::ItemEnum, module: &str, dir: Dir) -> Value {
        let attrs = serde_attrs(&e.attrs);
        let rule = if dir == Dir::In {
            attrs.rename_all_in.clone()
        } else {
            attrs.rename_all_out.clone()
        };
        let generics = generic_names(&e.generics);
        let mut plain = Vec::new();
        let mut variants = Vec::new();
        for variant in &e.variants {
            let serde = serde_attrs(&variant.attrs);
            if (dir == Dir::In && serde.skip_in) || (dir == Dir::Out && serde.skip_out) {
                continue;
            }
            let rename = if dir == Dir::In {
                &serde.rename_in
            } else {
                &serde.rename_out
            };
            let name = rename
                .clone()
                .unwrap_or_else(|| apply_rename_all(&variant.ident.to_string(), rule.as_deref()));
            let doc = doc_text(&variant.attrs);
            let fields_rule = if dir == Dir::In {
                serde.rename_all_in.clone()
            } else {
                serde.rename_all_out.clone()
            };
            let variant_attrs = SerdeAttrs {
                rename_all_in: fields_rule.clone(),
                rename_all_out: fields_rule,
                ..SerdeAttrs::default()
            };
            let body = match &variant.fields {
                Fields::Unit => None,
                Fields::Named(named) => {
                    Some(self.object_of(named, module, dir, &variant_attrs, &generics))
                }
                Fields::Unnamed(tuple) if tuple.unnamed.len() == 1 => {
                    Some(self.of_type(&tuple.unnamed[0].ty, module, dir, &generics))
                }
                Fields::Unnamed(tuple) => {
                    let items: Vec<Value> = tuple
                        .unnamed
                        .iter()
                        .map(|f| self.of_type(&f.ty, module, dir, &generics))
                        .collect();
                    Some(json!({ "type": "array", "prefixItems": items }))
                }
            };
            let schema = if attrs.untagged {
                match body {
                    Some(b) => with_description(b, &doc),
                    None => json!({ "type": "null" }),
                }
            } else if let Some(tag) = &attrs.tag {
                let tag_only = json!({
                    "type": "object",
                    "properties": { tag.as_str(): { "const": name } },
                    "required": [tag],
                });
                let merged = match (body, &attrs.content) {
                    (None, _) => tag_only,
                    (Some(b), Some(content)) => json!({
                        "type": "object",
                        "properties": { tag.as_str(): { "const": name }, content.as_str(): b },
                        "required": [tag, content],
                    }),
                    (Some(b), None) => json!({ "allOf": [tag_only, b] }),
                };
                with_description(merged, &doc)
            } else {
                match body {
                    None => {
                        plain.push((name, doc));
                        continue;
                    }
                    Some(b) => with_description(
                        json!({
                            "type": "object",
                            "properties": { name.as_str(): b },
                            "required": [name],
                        }),
                        &doc,
                    ),
                }
            };
            variants.push(schema);
        }
        let plain_schema = (!plain.is_empty()).then(|| {
            let names: Vec<&String> = plain.iter().map(|(n, _)| n).collect();
            json!({ "type": "string", "enum": names })
        });
        match (plain_schema, variants.is_empty()) {
            (Some(names), true) => names,
            (None, false) if variants.len() == 1 => variants.remove(0),
            (None, _) => json!({ "oneOf": variants }),
            (Some(names), false) => {
                variants.insert(0, names);
                json!({ "oneOf": variants })
            }
        }
    }

    /// The fields of a struct read from a query, with flattened structs spread out. `None` when
    /// the type is not a struct of the source (a `HashMap`, say).
    pub fn fields_of(&mut self, ty: &Type, module: &str, dir: Dir) -> Option<Vec<FieldInfo>> {
        let Type::Path(p) = strip_option(ty) else {
            return None;
        };
        let segments: Vec<String> = p
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        let full = self.index.resolve_def(module, &segments)?;
        let (def_module, def) = self.index.defs.get(&full).cloned()?;
        let s = match &def {
            Def::Struct(s) => s,
            Def::Alias(alias) => return self.fields_of(&alias.ty, &def_module, dir),
            _ => return None,
        };
        let Fields::Named(named) = &s.fields else {
            return None;
        };
        let attrs = serde_attrs(&s.attrs);
        let generics = generic_names(&s.generics);
        let mut out = Vec::new();
        for field in &named.named {
            let serde = serde_attrs(&field.attrs);
            if serde.flatten {
                if let Some(inner) = self.fields_of(&field.ty, &def_module, dir) {
                    out.extend(inner);
                }
                continue;
            }
            if let Some(info) = self.field_info(field, &def_module, dir, &attrs, &generics, &serde)
            {
                out.push(info);
            }
        }
        Some(out)
    }
}

pub fn generic_names(generics: &syn::Generics) -> Vec<String> {
    generics
        .params
        .iter()
        .filter_map(|p| match p {
            syn::GenericParam::Type(t) => Some(t.ident.to_string()),
            _ => None,
        })
        .collect()
}

pub fn strip_option(ty: &Type) -> &Type {
    if let Type::Path(p) = ty {
        if let Some(last) = p.path.segments.last() {
            if last.ident == "Option" {
                if let PathArguments::AngleBracketed(a) = &last.arguments {
                    if let Some(GenericArgument::Type(inner)) = a.args.first() {
                        return inner;
                    }
                }
            }
        }
    }
    ty
}

fn is_std_name(name: &str) -> bool {
    matches!(
        name,
        "String"
            | "str"
            | "bool"
            | "char"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "f32"
            | "f64"
            | "Option"
            | "Vec"
            | "Box"
            | "Arc"
            | "Rc"
            | "Cow"
            | "HashMap"
            | "BTreeMap"
            | "HashSet"
            | "BTreeSet"
            | "VecDeque"
            | "PathBuf"
    )
}
