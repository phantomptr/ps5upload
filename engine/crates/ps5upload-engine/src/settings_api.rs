//! The app's settings with names and types, on every platform.
//!
//! `/api/shared/prefs` (shared_state.rs, client lib/sharedPrefs.ts) keeps them as the page does: each
//! setting under its browser storage key, as the raw string it stores, with the time it changed.
//! The desktop and mobile apps now mirror theirs into it too, so this is the one place a script can
//! read "what will happen when I queue an upload" and change it, whichever app is in use.
//!
//! * `GET /api/settings`: `{rev, settings: {name: value}, set: [names set explicitly], schema}`.
//! * `PATCH /api/settings` `{settings: {name: value}}`: all of it or nothing (400 names what is
//!   wrong). `null` goes back to the default. A later change wins per setting, as between browsers;
//!   an open app applies it on its next look (within a minute, or when its window gets focus).
//!
//! These settings drive what the app does: its upload queue reads them, not the engine. Settings
//! with their own routes (save backups, Collection, Mesh, the log level kept by the engine) are
//! there, not here. Theme, language and other per-device taste stay on the device.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};

use crate::shared_state;

/// The document's name in shared_state.rs.
const DOC: &str = "prefs";

/// How a setting is written in storage.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// A switch and its two stored spellings; anything else stored reads as the default.
    Flag {
        on: &'static str,
        off: &'static str,
    },
    Number {
        min: f64,
        max: f64,
        integer: bool,
    },
    Choice(&'static [&'static str]),
    Text,
    /// A JSON document in the string (per-console maps and lists).
    Doc,
}

#[derive(Clone, Copy, Debug)]
enum Default_ {
    Bool(bool),
    Num(f64),
    Str(&'static str),
    Null,
}

impl Default_ {
    fn value(self) -> Value {
        match self {
            Default_::Bool(b) => b.into(),
            Default_::Num(n) => json_number(n),
            Default_::Str(s) => s.into(),
            Default_::Null => Value::Null,
        }
    }
}

struct Setting {
    name: &'static str,
    key: &'static str,
    kind: Kind,
    default: Default_,
    description: &'static str,
}

const TRUE_FALSE: Kind = Kind::Flag {
    on: "true",
    off: "false",
};
const ON_OFF: Kind = Kind::Flag {
    on: "on",
    off: "off",
};
const ONE_ZERO: Kind = Kind::Flag { on: "1", off: "0" };
const LEVELS: Kind = Kind::Choice(&["fast", "balanced", "smallest"]);

/// Every key of `SHARED_KEYS` in client lib/sharedPrefs.ts is either here or in [`LEGACY_KEYS`]
/// (sharedPrefs.test.ts checks it).
const SETTINGS: &[Setting] = &[
    Setting {
        name: "always_overwrite",
        key: "ps5upload.always_overwrite",
        kind: TRUE_FALSE,
        default: Default_::Bool(false),
        description: "Overwrite what is already on the console without asking.",
    },
    Setting {
        name: "show_transfer_files",
        key: "ps5upload.show_transfer_files",
        kind: TRUE_FALSE,
        default: Default_::Bool(true),
        description: "Show the file list while a transfer runs.",
    },
    Setting {
        name: "bandwidth_cap_mbps",
        key: "ps5upload.bandwidth_cap_mbps",
        kind: Kind::Number {
            min: 0.0,
            max: 100_000.0,
            integer: false,
        },
        default: Default_::Num(0.0),
        description: "Upload speed limit in MB/s. 0 means no limit.",
    },
    Setting {
        name: "auto_resume",
        key: "ps5upload.auto_resume",
        kind: TRUE_FALSE,
        default: Default_::Bool(true),
        description: "Try a failed upload again by itself when the failure is recoverable.",
    },
    Setting {
        name: "queue_auto_start",
        key: "ps5upload.queue_auto_start",
        kind: TRUE_FALSE,
        default: Default_::Bool(true),
        description: "Start the console's queue when something is added to it.",
    },
    Setting {
        name: "wake_for_uploads",
        key: "ps5upload.wake_for_uploads",
        kind: TRUE_FALSE,
        default: Default_::Bool(true),
        description: "Wake a resting console when an upload is waiting for it.",
    },
    Setting {
        name: "keep_ps5_awake_mode",
        key: "ps5upload.keep_ps5_awake_mode",
        kind: Kind::Choice(&["off", "transfers", "always"]),
        default: Default_::Str("transfers"),
        description: "When to keep the console from resting by itself.",
    },
    Setting {
        name: "auto_bring_up",
        key: "ps5upload.auto_bring_up",
        kind: TRUE_FALSE,
        default: Default_::Bool(false),
        description: "Run Quick bring-up by itself once a saved console's loader answers.",
    },
    Setting {
        name: "system_file_read",
        key: "ps5upload.system_file_read",
        kind: TRUE_FALSE,
        default: Default_::Bool(false),
        description: "Allow reading system files outside /data, /user and /mnt.",
    },
    Setting {
        name: "rest_after_upload",
        key: "ps5upload.rest_after_upload",
        kind: ON_OFF,
        default: Default_::Bool(false),
        description: "Put the console in rest mode when the queue is done.",
    },
    Setting {
        name: "register_after_upload",
        key: "ps5upload.register_after_upload",
        kind: TRUE_FALSE,
        default: Default_::Bool(true),
        description: "Add an uploaded game folder to the home screen when it lands.",
    },
    Setting {
        name: "auto_install_after_upload",
        key: "ps5upload.auto_install_after_upload",
        kind: ONE_ZERO,
        default: Default_::Bool(true),
        description: "Install a package as soon as it is uploaded to the library.",
    },
    Setting {
        name: "auto_remove_after_install",
        key: "ps5upload.auto_remove_after_install",
        kind: ONE_ZERO,
        default: Default_::Bool(false),
        description: "Delete a package from the library once it has installed.",
    },
    Setting {
        name: "auto_scan_external",
        key: "ps5upload.auto_scan_external",
        kind: ONE_ZERO,
        default: Default_::Bool(true),
        description: "Look for packages on external drives when the Install screen opens.",
    },
    Setting {
        name: "install_path_fallback",
        key: "ps5upload.install_path_fallback",
        kind: ON_OFF,
        default: Default_::Bool(false),
        description: "Let an install fall back to reading the package from its console path.",
    },
    Setting {
        name: "link_install_mode",
        key: "ps5upload.link_install_mode",
        kind: Kind::Doc,
        default: Default_::Null,
        description: "How each console installs a link, by address: direct, stream or download.",
    },
    Setting {
        name: "link_install_insecure",
        key: "ps5upload.link_install_insecure",
        kind: Kind::Doc,
        default: Default_::Null,
        description: "Whether each console accepts a link's bad certificate, by address.",
    },
    Setting {
        name: "convert_make",
        key: "ps5upload.convert.make",
        kind: Kind::Choice(&["pkg", "image", "folder"]),
        default: Default_::Str("pkg"),
        description: "What Convert makes.",
    },
    Setting {
        name: "convert_compression",
        key: "ps5upload.convert.compression",
        kind: LEVELS,
        default: Default_::Str("balanced"),
        description: "How hard a package is compressed.",
    },
    Setting {
        name: "convert_full_verify",
        key: "ps5upload.convert.full_verify",
        kind: ONE_ZERO,
        default: Default_::Bool(false),
        description: "Re-read every byte of the output to check it.",
    },
    Setting {
        name: "convert_image_format",
        key: "ps5upload.convert.image_format",
        kind: Kind::Choice(&["ffpkg", "exfat", "ffpfs"]),
        default: Default_::Str("ffpkg"),
        description: "Which game image Convert makes.",
    },
    Setting {
        name: "convert_image_compress",
        key: "ps5upload.convert.image_compress",
        kind: ONE_ZERO,
        default: Default_::Bool(false),
        description: "Compress the game image.",
    },
    Setting {
        name: "convert_image_level",
        key: "ps5upload.convert.image_level",
        kind: LEVELS,
        default: Default_::Str("balanced"),
        description: "How hard a game image is compressed.",
    },
    Setting {
        name: "convert_output_dir",
        key: "ps5upload.convert.output_dir",
        kind: Kind::Text,
        default: Default_::Str(""),
        description: "Convert's output folder, on the computer the engine runs on.",
    },
    Setting {
        name: "convert_verify_chunk_kib",
        key: "ps5upload.convert.verify_chunk_kib",
        kind: Kind::Number {
            min: 1024.0,
            max: 65536.0,
            integer: true,
        },
        default: Default_::Num(8192.0),
        description: "How much a game image's read-back check reads per call, in KiB.",
    },
    Setting {
        name: "save_path",
        key: "ps5upload.save_path",
        kind: Kind::Text,
        default: Default_::Str("/mnt/usb0/savedata"),
        description: "Where Save Data backs up to on the console.",
    },
    Setting {
        name: "beta_features",
        key: "ps5upload.beta_features",
        kind: ON_OFF,
        default: Default_::Bool(false),
        description: "Show features still being tested.",
    },
    Setting {
        name: "log_level",
        key: "ps5upload.log_level",
        kind: Kind::Choice(&["trace", "debug", "info", "warn", "error"]),
        default: Default_::Str("info"),
        description: "The least severe line the app writes to its log.",
    },
    Setting {
        name: "notif_prune_days",
        key: "ps5upload.notif_prune_days.v1",
        kind: Kind::Number {
            min: 0.0,
            max: 365.0,
            integer: true,
        },
        default: Default_::Num(30.0),
        description: "Days to keep notifications. 0 keeps them for good.",
    },
    Setting {
        name: "screenshots_png_mode",
        key: "ps5upload.screenshots.png_mode",
        kind: Kind::Choice(&["sdr", "hdr", "hdr_plain"]),
        default: Default_::Str("sdr"),
        description: "How a screenshot is written as PNG.",
    },
    Setting {
        name: "saved_searches",
        key: "ps5upload.saved_searches.v1",
        kind: Kind::Doc,
        default: Default_::Null,
        description: "The Search screen's saved searches: a list of {id, name, pattern, minSize}.",
    },
    Setting {
        name: "helper_stopped",
        key: "ps5upload.helperStopped",
        kind: Kind::Doc,
        default: Default_::Null,
        description: "When each console's helper was stopped on purpose, by address, in ms.",
    },
    Setting {
        name: "minion_off",
        key: "ps5upload.minionOff",
        kind: Kind::Doc,
        default: Default_::Null,
        description:
            "The consoles whose Minion page was turned off from here: a list of addresses.",
    },
    Setting {
        name: "c2c_routes",
        key: "ps5upload.c2c.routes.v1",
        kind: Kind::Doc,
        default: Default_::Null,
        description: "Console-to-console routes: \"from>to\" to {mode, directAddr}.",
    },
    Setting {
        name: "sidebar_pinned",
        key: "ps5upload.desktop-sidebar.pinned.v1",
        kind: Kind::Doc,
        default: Default_::Null,
        description: "The sidebar's pinned screens.",
    },
    Setting {
        name: "sidebar_hidden",
        key: "ps5upload.desktop-sidebar.hidden.v1",
        kind: Kind::Doc,
        default: Default_::Null,
        description: "The sidebar's hidden screens.",
    },
    Setting {
        name: "sidebar_closed_sections",
        key: "ps5upload.desktop-sidebar.closed-sections.v1",
        kind: Kind::Doc,
        default: Default_::Null,
        description: "The sidebar's folded sections.",
    },
];

/// Shared keys only older versions wrote: the app reads them once to carry a choice over. They
/// have no typed setting (`keep_ps5_awake_mode` and `/api/save-backup` replaced them).
#[cfg(test)]
const LEGACY_KEYS: &[&str] = &["ps5upload.keep_ps5_awake", "ps5upload.auto_save_backup"];

fn json_number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9e15 {
        Value::from(n as i64)
    } else {
        Value::from(n)
    }
}

/// A stored string as its value; what cannot be read is the default, as the app itself treats it.
fn decode(s: &Setting, raw: Option<&str>) -> Value {
    let Some(raw) = raw else {
        return s.default.value();
    };
    match s.kind {
        Kind::Flag { on, off } => {
            if raw == on {
                true.into()
            } else if raw == off {
                false.into()
            } else {
                s.default.value()
            }
        }
        Kind::Number { min, max, integer } => match raw.trim().parse::<f64>() {
            Ok(n) if n.is_finite() && n >= min && n <= max && (!integer || n.fract() == 0.0) => {
                json_number(n)
            }
            _ => s.default.value(),
        },
        Kind::Choice(all) => {
            if all.contains(&raw) {
                raw.into()
            } else {
                s.default.value()
            }
        }
        Kind::Text => raw.into(),
        Kind::Doc => serde_json::from_str(raw).unwrap_or_else(|_| s.default.value()),
    }
}

/// A value as the string the app stores (None: back to the default), or what is wrong with it.
fn encode(s: &Setting, v: &Value) -> Result<Option<String>, String> {
    if v.is_null() {
        return Ok(None);
    }
    let bad = |want: &str| format!("{} must be {want}", s.name);
    match s.kind {
        Kind::Flag { on, off } => {
            let b = v.as_bool().ok_or_else(|| bad("true or false"))?;
            Ok(Some(if b { on } else { off }.to_string()))
        }
        Kind::Number { min, max, integer } => {
            let n = v.as_f64().ok_or_else(|| bad("a number"))?;
            if !n.is_finite() || n < min || n > max || (integer && n.fract() != 0.0) {
                return Err(bad(&format!(
                    "{}from {min} to {max}",
                    if integer { "a whole number " } else { "" }
                )));
            }
            Ok(Some(json_number(n).to_string()))
        }
        Kind::Choice(all) => match v.as_str() {
            Some(c) if all.contains(&c) => Ok(Some(c.to_string())),
            _ => Err(bad(&format!("one of {}", all.join(", ")))),
        },
        Kind::Text => v
            .as_str()
            .map(|t| Some(t.to_string()))
            .ok_or_else(|| bad("text")),
        Kind::Doc => {
            if v.is_object() || v.is_array() {
                Ok(Some(v.to_string()))
            } else {
                Err(bad("a JSON object or list"))
            }
        }
    }
}

fn entries_of(value: Option<&Value>) -> Map<String, Value> {
    value
        .and_then(|v| v.get("keys"))
        .and_then(|k| k.as_object())
        .cloned()
        .unwrap_or_default()
}

fn typed(entries: &Map<String, Value>) -> (Map<String, Value>, Vec<&'static str>) {
    let mut out = Map::new();
    let mut set = Vec::new();
    for s in SETTINGS {
        let raw = entries.get(s.key).and_then(|e| e.get("v"));
        if raw.is_some_and(|v| !v.is_null()) {
            set.push(s.name);
        }
        out.insert(s.name.to_string(), decode(s, raw.and_then(|v| v.as_str())));
    }
    (out, set)
}

/// One setting's description in the JSON Schema dialect.
fn describe(s: &Setting) -> Value {
    let mut o = json!({
        "description": s.description,
        "default": s.default.value(),
        "x-stored-as": s.key,
    });
    match s.kind {
        Kind::Flag { .. } => o["type"] = "boolean".into(),
        Kind::Number { min, max, integer } => {
            o["type"] = if integer { "integer" } else { "number" }.into();
            o["minimum"] = json_number(min);
            o["maximum"] = json_number(max);
        }
        Kind::Choice(all) => {
            o["type"] = "string".into();
            o["enum"] = json!(all);
        }
        Kind::Text => o["type"] = "string".into(),
        Kind::Doc => o["type"] = json!(["object", "array", "null"]),
    }
    o
}

fn schema() -> Value {
    let mut props = Map::new();
    for s in SETTINGS {
        props.insert(s.name.to_string(), describe(s));
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "PS5Upload settings",
        "type": "object",
        "properties": props,
        "additionalProperties": false,
    })
}

/// JSON Schema of the `prefs` document as it is stored (`GET /api/shared/prefs/schema`).
pub(crate) fn document_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "PS5Upload shared settings, as stored",
        "description": "The typed view is GET /api/settings.",
        "type": "object",
        "properties": {
            "keys": {
                "type": "object",
                "additionalProperties": {
                    "type": "object",
                    "required": ["v", "at"],
                    "properties": {
                        "v": {"type": ["string", "null"], "description": "The stored string; null when removed."},
                        "at": {"type": "number", "description": "When it changed, in ms since 1970."},
                    },
                },
            },
        },
    })
}

fn answer(doc: &shared_state::Doc) -> Value {
    let (settings, set) = typed(&entries_of(doc.value.as_ref()));
    json!({"rev": doc.rev, "settings": settings, "set": set})
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

/// `GET /api/settings`.
pub async fn get() -> Response {
    let mut v = answer(&shared_state::read(DOC));
    v["schema"] = schema();
    Json(v).into_response()
}

#[derive(serde::Deserialize)]
pub struct PatchBody {
    settings: Map<String, Value>,
}

/// Each change as (storage key, stored string); or what is wrong, naming every bad setting.
fn plan_patch(changes: &Map<String, Value>) -> Result<Vec<(&'static str, Option<String>)>, Value> {
    let mut plan = Vec::with_capacity(changes.len());
    let mut wrong = Map::new();
    for (name, v) in changes {
        match SETTINGS.iter().find(|s| s.name == name) {
            None => {
                wrong.insert(name.clone(), "there is no such setting".into());
            }
            Some(s) => match encode(s, v) {
                Ok(raw) => plan.push((s.key, raw)),
                Err(m) => {
                    wrong.insert(name.clone(), m.into());
                }
            },
        }
    }
    if wrong.is_empty() {
        Ok(plan)
    } else {
        Err(json!({
            "error": "some settings could not be changed; nothing was changed",
            "code": "bad_settings",
            "settings": wrong,
        }))
    }
}

/// The new document's entries: each change stamped later than what it replaces, so it wins over
/// the last change here even with a browser's clock a little ahead.
fn apply_patch(value: &mut Value, plan: Vec<(&'static str, Option<String>)>, now: f64) {
    if !value.is_object() {
        *value = json!({});
    }
    if !value.get("keys").is_some_and(|k| k.is_object()) {
        value["keys"] = json!({});
    }
    let keys = value["keys"].as_object_mut().expect("just made an object");
    for (key, raw) in plan {
        let at = keys
            .get(key)
            .and_then(|e| e.get("at"))
            .and_then(|a| a.as_f64())
            .map_or(now, |prev| now.max(prev + 1.0));
        keys.insert(key.to_string(), json!({"v": raw, "at": json_number(at)}));
    }
}

/// `PATCH /api/settings`.
pub async fn patch(Json(body): Json<PatchBody>) -> Response {
    let plan = match plan_patch(&body.settings) {
        Ok(p) => p,
        Err(v) => return (StatusCode::BAD_REQUEST, Json(v)).into_response(),
    };
    let done = tokio::task::spawn_blocking(move || {
        shared_state::update(DOC, |value| apply_patch(value, plan, now_ms()))
    })
    .await;
    match done {
        Ok(Ok(doc)) => Json(answer(&doc)).into_response(),
        Ok(Err(m)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": m, "code": "save_failed"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string(), "code": "save_failed"})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(name: &str) -> &'static Setting {
        SETTINGS.iter().find(|s| s.name == name).unwrap()
    }

    /// What the app itself reads from each stored spelling.
    #[test]
    fn stored_strings_read_the_way_the_app_reads_them() {
        let s = setting("auto_resume");
        assert_eq!(decode(s, None), json!(true));
        assert_eq!(decode(s, Some("false")), json!(false));
        assert_eq!(decode(s, Some("garbage")), json!(true));
        assert_eq!(
            decode(setting("always_overwrite"), Some("true")),
            json!(true)
        );
        assert_eq!(
            decode(setting("rest_after_upload"), Some("on")),
            json!(true)
        );
        assert_eq!(
            decode(setting("auto_install_after_upload"), Some("0")),
            json!(false)
        );
        assert_eq!(
            decode(setting("bandwidth_cap_mbps"), Some("12.5")),
            json!(12.5)
        );
        assert_eq!(decode(setting("bandwidth_cap_mbps"), Some("x")), json!(0));
        assert_eq!(decode(setting("convert_make"), Some("nope")), json!("pkg"));
        assert_eq!(
            decode(setting("minion_off"), Some("[\"1.2.3.4\"]")),
            json!(["1.2.3.4"])
        );
        assert_eq!(decode(setting("minion_off"), Some("{bad")), Value::Null);
    }

    #[test]
    fn values_are_stored_in_the_apps_own_spelling() {
        let e = |n: &str, v: Value| encode(setting(n), &v).unwrap();
        assert_eq!(e("auto_resume", json!(false)).unwrap(), "false");
        assert_eq!(e("rest_after_upload", json!(true)).unwrap(), "on");
        assert_eq!(e("auto_scan_external", json!(false)).unwrap(), "0");
        assert_eq!(e("bandwidth_cap_mbps", json!(50)).unwrap(), "50");
        assert_eq!(e("bandwidth_cap_mbps", json!(1.5)).unwrap(), "1.5");
        assert_eq!(e("keep_ps5_awake_mode", json!("always")).unwrap(), "always");
        assert_eq!(
            e("minion_off", json!(["1.2.3.4"])).unwrap(),
            "[\"1.2.3.4\"]"
        );
        assert_eq!(e("auto_resume", Value::Null), None);
        // Every value reads back as itself.
        for (n, v) in [
            ("auto_resume", json!(false)),
            ("bandwidth_cap_mbps", json!(7.25)),
            ("convert_image_level", json!("smallest")),
            ("save_path", json!("/data/saves")),
            (
                "c2c_routes",
                json!({"a>b": {"mode": "direct", "directAddr": ""}}),
            ),
        ] {
            let raw = e(n, v.clone()).unwrap();
            assert_eq!(decode(setting(n), Some(&raw)), v, "{n}");
        }
    }

    #[test]
    fn a_patch_is_all_or_nothing_and_says_what_is_wrong() {
        let good = json!({"auto_resume": false, "bandwidth_cap_mbps": 10});
        assert_eq!(plan_patch(good.as_object().unwrap()).unwrap().len(), 2);
        let bad = json!({
            "auto_resume": "yes",
            "no_such": 1,
            "bandwidth_cap_mbps": -1,
            "convert_verify_chunk_kib": 1024.5,
            "keep_ps5_awake_mode": "never",
            "save_path": 3,
            "minion_off": "x",
            "queue_auto_start": true,
        });
        let e = plan_patch(bad.as_object().unwrap()).unwrap_err();
        let wrong = e["settings"].as_object().unwrap();
        assert_eq!(wrong.len(), 7, "{e}");
        assert!(!wrong.contains_key("queue_auto_start"));
    }

    #[test]
    fn a_patched_setting_is_newer_than_what_it_replaces() {
        let mut doc = json!({"keys": {
            "ps5upload.auto_resume": {"v": "true", "at": 5000},
            "ps5upload.future": {"v": "kept", "at": 1},
        }});
        let plan = plan_patch(json!({"auto_resume": false}).as_object().unwrap()).unwrap();
        // A browser's clock ahead of this one's.
        apply_patch(&mut doc, plan, 1000.0);
        assert_eq!(
            doc["keys"]["ps5upload.auto_resume"],
            json!({"v": "false", "at": 5001})
        );
        assert_eq!(doc["keys"]["ps5upload.future"]["v"], "kept");

        let plan = plan_patch(json!({"auto_resume": null}).as_object().unwrap()).unwrap();
        apply_patch(&mut doc, plan, 9000.0);
        assert_eq!(
            doc["keys"]["ps5upload.auto_resume"],
            json!({"v": null, "at": 9000})
        );
        let (typed_now, set) = typed(&entries_of(Some(&doc)));
        assert_eq!(typed_now["auto_resume"], json!(true), "back to the default");
        assert!(!set.contains(&"auto_resume"));
        // A document never saved starts from nothing.
        let mut fresh = Value::Null;
        let plan = plan_patch(json!({"beta_features": true}).as_object().unwrap()).unwrap();
        apply_patch(&mut fresh, plan, 1.0);
        assert_eq!(fresh["keys"]["ps5upload.beta_features"]["v"], "on");
    }

    #[test]
    fn every_setting_has_a_unique_name_and_a_schema() {
        let s = schema();
        let props = s["properties"].as_object().unwrap();
        assert_eq!(props.len(), SETTINGS.len());
        for x in SETTINGS {
            assert!(!x.description.is_empty());
            assert!(props[x.name]["type"] != Value::Null, "{}", x.name);
        }
        let keys: std::collections::HashSet<_> = SETTINGS.iter().map(|s| s.key).collect();
        assert_eq!(keys.len(), SETTINGS.len());
        assert!(LEGACY_KEYS.iter().all(|k| !keys.contains(k)));
    }
}
