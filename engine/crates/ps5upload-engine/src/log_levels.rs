//! The log levels, one setting for the app, the engine and the PS5 helper
//! (`ps5upload_core::log` has the model).
//!
//! * `GET /api/logs/level` -> the setting, what is in force per category now, and the categories.
//! * `PUT /api/logs/level` `{level?, categories?, until_ms?, minutes?, reset?}` -> the same. A
//!   level above `info` (global or per category) lasts until `until_ms`, or `minutes` from now
//!   (default 60, at most a day), then falls back to `info` by itself. `categories` replaces the
//!   overrides when given; a category set to `null` loses its override. `reset: true` starts from
//!   the default (`info`, no overrides) before applying the rest.
//! * `GET /api/logs/recent?level=&category=&since_ms=&since_seq=&limit=` -> the engine's newest log
//!   lines that match, oldest first: `level=debug` keeps debug and everything more severe;
//!   `category` takes names separated by commas.
//!
//! The engine keeps the setting in `log-levels.json` in its data folder, so a restart keeps it (a
//! raise still ends on time), and hands it to every helper it talks to (`helper_push`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::log::{self as clog, Levels, Lv};
use serde::Deserialize;
use serde_json::{json, Value};

const FILE: &str = "log-levels.json";
/// How often a raise that ran out is noticed, saved and announced.
const EXPIRY_TICK: Duration = Duration::from_secs(15);
/// Most lines `/api/logs/recent` answers with.
pub(crate) const MAX_RECENT: usize = 2000;
const DEFAULT_RECENT: usize = 200;

fn path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    ps5upload_ava1::pool::data_dir().map(|d| d.join(FILE))
}

fn save(levels: &Levels) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = p.with_extension("json.tmp");
    let ok = serde_json::to_vec_pretty(levels)
        .ok()
        .is_some_and(|bytes| std::fs::write(&tmp, bytes).is_ok());
    if ok {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// At startup: the saved setting (a raise that ran out while the engine was down is dropped),
/// and the ticker that ends raises on time.
pub(crate) fn load_saved() {
    if let Some(p) = path() {
        if let Ok(bytes) = std::fs::read(&p) {
            match serde_json::from_slice::<Levels>(&bytes) {
                Ok(mut l) => {
                    l.expire(clog::now_ms());
                    let l = clog::set_levels(l);
                    if l != Levels::default() {
                        crate::log_info!("log levels: {}", summary(&l));
                    }
                }
                Err(e) => crate::log_warn!("log levels: {} unreadable ({e}): info", p.display()),
            }
        }
    }
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(async {
            let mut tick = tokio::time::interval(EXPIRY_TICK);
            loop {
                tick.tick().await;
                if clog::expire_now() {
                    let l = clog::levels();
                    save(&l);
                    crate::log_info!("log levels: the raise ran out, back to {}", summary(&l));
                }
            }
        });
    }
}

/// `info; rp=trace until 2026-11-07T10:00:00Z`.
pub(crate) fn summary(l: &Levels) -> String {
    let mut s = l.level.name().to_string();
    for (c, v) in &l.categories {
        s.push_str(&format!(", {c}={}", v.name()));
    }
    if let Some(u) = l.until_ms {
        s.push_str(&format!(
            " for {} more min",
            u.saturating_sub(clog::now_ms()) / 60_000
        ));
    }
    s
}

/// The answer of `GET /api/logs/level`, also in `/api/diag/features` and the bug report.
pub(crate) fn levels_json() -> Value {
    let now = clog::now_ms();
    let l = clog::levels();
    let effective: BTreeMap<&str, &str> = l
        .effective_all(now)
        .into_iter()
        .map(|(c, v)| (c, v.name()))
        .collect();
    let raised = l.raised() && l.until_ms.is_some_and(|u| u > now);
    json!({
        "level": l.level,
        "categories": l.categories,
        "until_ms": l.until_ms.filter(|u| *u > now),
        "raised": raised,
        "effective": effective,
        "levels": Lv::ALL.iter().map(|v| v.name()).collect::<Vec<_>>(),
        "category_list": clog::CATEGORIES
            .iter()
            .map(|(name, about)| json!({"name": name, "about": about}))
            .collect::<Vec<_>>(),
        "default_minutes": clog::DEFAULT_RAISE_MS / 60_000,
        "max_minutes": clog::MAX_RAISE_MS / 60_000,
    })
}

pub async fn get_level() -> Json<Value> {
    Json(levels_json())
}

#[derive(Deserialize, Default)]
pub struct LevelReq {
    /// The global level: error, warn, info, debug or trace.
    #[serde(default)]
    level: Option<String>,
    /// Per-category levels; a category set to null loses its override. Replaces the overrides
    /// when given (with `merge: true`, only the named categories change).
    #[serde(default)]
    categories: Option<BTreeMap<String, Option<String>>>,
    /// Change only the categories named, keeping the other overrides.
    #[serde(default)]
    merge: bool,
    /// When a raise ends, Unix ms.
    #[serde(default)]
    until_ms: Option<u64>,
    /// How long a raise lasts from now, in minutes (instead of `until_ms`).
    #[serde(default)]
    minutes: Option<u64>,
    /// Start from the default (info, no overrides) before the rest.
    #[serde(default)]
    reset: bool,
}

fn bad(msg: String) -> Response {
    crate::json_err(StatusCode::BAD_REQUEST, msg).into_response()
}

/// The setting `req` asks for, from `current` at `now`. Err names what is wrong.
pub(crate) fn apply(current: &Levels, req: &LevelReq, now: u64) -> Result<Levels, String> {
    let mut next = if req.reset {
        Levels::default()
    } else {
        current.clone()
    };
    if let Some(level) = &req.level {
        next.level = Lv::parse(level).ok_or_else(|| format!("unknown level {level:?}"))?;
    }
    if let Some(cats) = &req.categories {
        if !req.merge {
            next.categories.clear();
        }
        for (name, level) in cats {
            let cat = clog::category(name).ok_or_else(|| {
                format!(
                    "unknown category {name:?}; the categories are {}",
                    clog::CATEGORIES
                        .iter()
                        .map(|(c, _)| *c)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            match level {
                None => {
                    next.categories.remove(cat);
                }
                Some(l) => {
                    let l = Lv::parse(l).ok_or_else(|| format!("unknown level {l:?}"))?;
                    next.categories.insert(cat.to_string(), l);
                }
            }
        }
    }
    if let Some(m) = req.minutes {
        next.until_ms = Some(now + m.max(1) * 60_000);
    } else if let Some(u) = req.until_ms {
        next.until_ms = Some(u);
    } else if next.raised() && !current.raised() {
        // A new raise without an end gets the default one (normalize), not a stale end.
        next.until_ms = None;
    } else if next.raised() && current.until_ms.is_some_and(|u| u <= now) {
        next.until_ms = None;
    }
    Ok(next)
}

pub async fn put_level(Json(req): Json<LevelReq>) -> Response {
    let before = clog::levels();
    let next = match apply(&before, &req, clog::now_ms()) {
        Ok(n) => n,
        Err(e) => return bad(e),
    };
    let now_set = clog::set_levels(next);
    if now_set != before {
        crate::log_info!("log levels: {}", summary(&now_set));
        ps5upload_core::events::emit(
            ps5upload_core::events::Cat::System,
            ps5upload_core::events::Level::Info,
            "log_levels",
            None,
            format!("log levels: {}", summary(&now_set)),
        );
        let saved = now_set.clone();
        tokio::task::spawn_blocking(move || save(&saved));
    }
    Json(levels_json()).into_response()
}

#[derive(Deserialize, Default)]
pub struct RecentQuery {
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    since_ms: Option<u64>,
    #[serde(default)]
    since_seq: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
}

pub(crate) fn recent_filter(q: &RecentQuery) -> Result<crate::engine_log::Filter, String> {
    let level = match q.level.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(l) => Some(Lv::parse(l).ok_or_else(|| format!("unknown level {l:?}"))?),
        None => None,
    };
    let mut cats = Vec::new();
    for name in q
        .category
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        cats.push(clog::category(name).ok_or_else(|| format!("unknown category {name:?}"))?);
    }
    Ok(crate::engine_log::Filter {
        level,
        cats,
        since_ms: q.since_ms.unwrap_or(0),
        since_seq: q.since_seq.unwrap_or(0),
        limit: q.limit.unwrap_or(DEFAULT_RECENT).clamp(1, MAX_RECENT),
    })
}

pub async fn recent(Query(q): Query<RecentQuery>) -> Response {
    let f = match recent_filter(&q) {
        Ok(f) => f,
        Err(e) => return bad(e),
    };
    let lines = crate::engine_log::recent(&f);
    let next_seq = lines.last().map(|e| e.seq).unwrap_or(f.since_seq);
    Json(json!({
        "lines": lines,
        "count": lines.len(),
        "next_seq": next_seq,
        "levels": levels_json(),
    }))
    .into_response()
}

// ---- the helper's copy ----

/// What a helper is told: the global level and per-category levels as numbers (1 error .. 5
/// trace), and how many seconds a raise has left (0: none). The helper counts the seconds on its
/// own clock, so the two clocks need not agree.
pub(crate) fn helper_config() -> Value {
    let now = clog::now_ms();
    let l = clog::levels();
    let live = l.until_ms.is_some_and(|u| u > now);
    let cats: BTreeMap<&str, u8> = l
        .categories
        .iter()
        .filter_map(|(c, v)| clog::category(c).map(|c| (c, *v as u8)))
        .collect();
    json!({
        "level": l.level as u8,
        "cats": cats,
        "until_s": if live { l.until_ms.unwrap_or(0).saturating_sub(now).div_ceil(1000) } else { 0 },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(v: Value) -> LevelReq {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_put_changes_what_it_names() {
        let now = 1_800_000_000_000;
        let cur = Levels::default();
        let n = apply(
            &cur,
            &req(json!({"categories": {"rp": "trace"}, "minutes": 30})),
            now,
        )
        .unwrap();
        assert_eq!(n.level, Lv::Info);
        assert_eq!(n.categories.get("rp"), Some(&Lv::Trace));
        assert_eq!(n.until_ms, Some(now + 30 * 60_000));
        // merge keeps the others; null drops one.
        let n2 = apply(
            &n,
            &req(json!({"categories": {"install": "debug", "rp": null}, "merge": true})),
            now,
        )
        .unwrap();
        assert_eq!(n2.categories.get("install"), Some(&Lv::Debug));
        assert!(!n2.categories.contains_key("rp"));
        assert_eq!(n2.until_ms, n.until_ms, "the raise keeps its end");
        // reset starts again.
        let n3 = apply(&n2, &req(json!({"reset": true})), now).unwrap();
        assert_eq!(n3, Levels::default());
    }

    #[test]
    fn unknown_names_are_refused() {
        let cur = Levels::default();
        assert!(apply(&cur, &req(json!({"level": "loud"})), 0).is_err());
        let e = apply(&cur, &req(json!({"categories": {"nope": "debug"}})), 0).unwrap_err();
        assert!(e.contains("rp"), "{e}");
        assert!(recent_filter(&RecentQuery {
            category: Some("rp,nope".into()),
            ..Default::default()
        })
        .is_err());
    }

    #[test]
    fn recent_filters_by_level_and_category() {
        let id = std::process::id();
        crate::engine_log::record_cat("warn", "install", format!("recent-w-{id}"));
        crate::engine_log::record_cat("debug", "rp", format!("recent-d-{id}"));
        let f = recent_filter(&RecentQuery {
            level: Some("warn".into()),
            category: Some("install".into()),
            limit: Some(2000),
            ..Default::default()
        })
        .unwrap();
        let got = crate::engine_log::recent(&f);
        assert!(got.iter().any(|e| e.msg == format!("recent-w-{id}")));
        assert!(!got.iter().any(|e| e.msg == format!("recent-d-{id}")));
        assert!(got.iter().all(|e| e.cat == "install"));
    }

    #[test]
    fn the_helper_gets_numbers() {
        let v = helper_config();
        assert!(v["level"].as_u64().is_some_and(|n| (1..=5).contains(&n)));
        assert!(v["cats"].is_object());
        assert!(v["until_s"].is_u64());
    }
}
