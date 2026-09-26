//! TMDB (Title MetaData Base) proxy: fetch PlayStation Store metadata.
//! The payload returns cached data if available; the engine falls back
//! to fetching from the PS Store directly when the cache misses,
//! then pushes the result back to the payload cache via TmdbStore.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[cfg(not(target_os = "android"))]
use std::io::Read;

#[cfg(not(target_os = "android"))]
const STORE_BASE: &str = "https://store.playstation.com/en-us/product/";

#[cfg(not(target_os = "android"))]
const REGION_PREFIXES: &[&str] = &[
    "IP9100", "UP9000", "UP0006", "UP0002", "UP0700", "UP0177", "UP0082", "UP4040", "UP4108",
    "UP4415", "EP9000", "EP0006", "EP0002", "EP0700", "EP0177", "EP4108", "EP4415", "EP1018",
    "HP9000", "JP9000", "JP9001", "UB1019", "UB0335", "UB1229", "UB0006",
];

#[cfg(not(target_os = "android"))]
const TRAILING_LABELS: &[&str] = &["PREINMASTER00000", "0000000000000000"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbFetchRequest {
    pub title_id: String,
    #[serde(default)]
    pub refresh: bool,
    /// Optional region prefix to narrow the PS Store URL search
    /// (e.g. "UP9000" for US Sony, "EP1018" for EU). When provided,
    /// only that prefix is tried instead of all 24 known prefixes.
    #[serde(default)]
    pub region: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbStoreRequest {
    pub title_id: String,
    pub json: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TmdbFetchResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub np_title_id: Option<String>,
    #[serde(default)]
    pub content_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub sku: Option<String>,
}

fn send_recv(
    addr: &str,
    req_type: FrameType,
    ack_type: FrameType,
    body: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut c = Connection::connect(addr)?;
    let empty: Vec<u8> = Vec::new();
    let body = body.unwrap_or(&empty);
    c.send_frame(req_type, body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected {:?}: {}",
            req_type,
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != ack_type {
        bail!("expected {:?}, got {:?}", ack_type, ft);
    }
    Ok(resp)
}

fn is_valid_title_id(s: &str) -> bool {
    if s.len() != 12 {
        return false;
    }
    let b = s.as_bytes();
    b[0..4].iter().all(|&c| c.is_ascii_uppercase())
        && b[4..9].iter().all(|&c| c.is_ascii_digit())
        && b[9] == b'_'
        && b[10..12].iter().all(|&c| c.is_ascii_digit())
}

/// Check if string is a valid 36-char content ID: `PPPPPP-TITLEID_00-YYYYYYYYYYYYYYYY`
fn is_valid_content_id(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    let b = s.as_bytes();
    b[6] == b'-' && b[19] == b'-'
}

/// Extract the 12-char title_id from a 36-char content_id.
fn title_id_from_content_id(cid: &str) -> Option<&str> {
    if !is_valid_content_id(cid) {
        return None;
    }
    Some(&cid[7..19])
}

/// Normalize input: accepts 12-char title_id or 36-char content_id.
/// Returns the 12-char title_id.
fn normalize_title_id(input: &str) -> Option<String> {
    if is_valid_title_id(input) {
        return Some(input.to_string());
    }
    if is_valid_content_id(input) {
        let tid = title_id_from_content_id(input)?;
        if is_valid_title_id(tid) {
            return Some(tid.to_string());
        }
    }
    None
}

#[cfg(not(target_os = "android"))]
fn fetch_url_sync(url: &str) -> Result<Vec<u8>> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let resp = agent
        .get(url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Accept-Language", "en-US,en;q=0.9")
        .call()?;
    let mut buf = Vec::with_capacity(64 * 1024);
    resp.into_body().into_reader().read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(not(target_os = "android"))]
fn mem_find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(not(target_os = "android"))]
fn extract_jsonld(html: &[u8]) -> Option<String> {
    let close = b"</script>";

    let open_specific = b"<script id=\"mfe-jsonld-tags\" type=\"application/ld+json\">";
    if let Some(pos) = mem_find(html, open_specific) {
        let start = pos + open_specific.len();
        if let Some(end) = mem_find(&html[start..], close) {
            return Some(String::from_utf8_lossy(&html[start..start + end]).into_owned());
        }
    }

    let open_generic = b"type=\"application/ld+json\">";
    let mut search_from = 0;
    while let Some(rel) = mem_find(&html[search_from..], open_generic) {
        let json_start = search_from + rel + open_generic.len();
        if let Some(end) = mem_find(&html[json_start..], close) {
            let body = &html[json_start..json_start + end];
            let body_str = String::from_utf8_lossy(body);
            if body_str.contains("\"@type\"") {
                return Some(body_str.into_owned());
            }
        }
        search_from = json_start + 1;
    }

    None
}

#[cfg(not(target_os = "android"))]
fn parse_store_json(jsonld: &str, content_id: &str, title_id: &str) -> TmdbFetchResponse {
    let v: serde_json::Value = match serde_json::from_str(jsonld) {
        Ok(v) => v,
        Err(_) => {
            return TmdbFetchResponse {
                ok: false,
                title_id: title_id.to_string(),
                error: Some("parse_failed".to_string()),
                ..Default::default()
            }
        }
    };

    let name = v.get("name").and_then(|n| n.as_str()).map(String::from);
    let description = v
        .get("description")
        .and_then(|d| d.as_str())
        .map(String::from);
    let category = v.get("category").and_then(|c| c.as_str()).map(String::from);
    let icon = v
        .get("image")
        .or_else(|| v.get("icon"))
        .and_then(|i| i.as_str())
        .map(String::from);
    let publisher = v
        .get("publisher")
        .and_then(|p| p.as_str())
        .or_else(|| {
            v.get("publisher")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
        })
        .map(String::from);
    let release_date = v
        .get("releaseDate")
        .and_then(|d| d.as_str())
        .map(String::from);
    let genre = v.get("genre").and_then(|g| {
        if let Some(s) = g.as_str() {
            Some(s.to_string())
        } else if let Some(arr) = g.as_array() {
            let names: Vec<String> = arr
                .iter()
                .filter_map(|item| item.as_str().map(String::from))
                .collect();
            if names.is_empty() {
                None
            } else {
                Some(names.join(", "))
            }
        } else {
            None
        }
    });
    let sku = v
        .get("sku")
        .or_else(|| v.get("productID"))
        .and_then(|s| s.as_str())
        .map(String::from);

    TmdbFetchResponse {
        ok: true,
        title_id: title_id.to_string(),
        error: None,
        np_title_id: Some(title_id.to_string()),
        content_id: Some(content_id.to_string()),
        name,
        description,
        icon,
        category,
        publisher,
        release_date,
        genre,
        sku,
    }
}

#[cfg(not(target_os = "android"))]
fn fetch_from_store(
    title_id: &str,
    known_content_id: Option<&str>,
    region: Option<&str>,
) -> Option<(TmdbFetchResponse, String)> {
    if let Some(cid) = known_content_id {
        let url = format!("{}{}", STORE_BASE, cid);
        if let Ok(html) = fetch_url_sync(&url) {
            let product_needle = format!("Product:{}", cid);
            if mem_find(&html, product_needle.as_bytes()).is_some() {
                if let Some(jsonld) = extract_jsonld(&html) {
                    let resp = parse_store_json(&jsonld, cid, title_id);
                    if resp.ok && resp.name.is_some() {
                        return Some((resp, jsonld));
                    }
                }
            }
        }
    }

    // If a specific region prefix is given, only try that one.
    // Otherwise brute-force all known prefixes.
    let prefixes: Vec<&str> = match region {
        Some(r) => vec![r],
        None => REGION_PREFIXES.to_vec(),
    };

    for prefix in &prefixes {
        for label in TRAILING_LABELS {
            let content_id = format!("{}-{}-{}", prefix, title_id, label);
            let url = format!("{}{}", STORE_BASE, content_id);
            let html = match fetch_url_sync(&url) {
                Ok(h) => h,
                Err(_) => continue,
            };
            let product_needle = format!("Product:{}", content_id);
            if mem_find(&html, product_needle.as_bytes()).is_none() {
                continue;
            }
            let jsonld = extract_jsonld(&html)?;
            let resp = parse_store_json(&jsonld, &content_id, title_id);
            if resp.ok && resp.name.is_some() {
                return Some((resp, jsonld));
            }
        }
    }
    None
}

/// Push a metadata JSON blob into the payload's on-console cache.
/// Available on every host (including Android) — it only uses FTX2,
/// not the desktop-only store scrape.
fn tmdb_store_on_payload(addr: &str, title_id: &str, json: &str) -> Result<()> {
    let req = serde_json::json!({ "title_id": title_id, "json": json });
    let resp = send_recv(
        addr,
        FrameType::TmdbStore,
        FrameType::TmdbStoreAck,
        Some(&serde_json::to_vec(&req)?),
    )?;
    let v: serde_json::Value = serde_json::from_slice(&resp)?;
    if v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
        Ok(())
    } else {
        bail!("store failed: {}", v);
    }
}

/// Try the payload cache first. If it returns "not_cached", fall back to
/// fetching from the PS Store (which the engine can do because it runs on
/// the host with full internet access). On success, push the result to
/// the payload cache via TmdbStore.
///
/// `region` optionally narrows the PS Store URL search to a single prefix
/// (e.g. "UP9000" for US Sony) instead of brute-forcing all 24 known
/// prefixes × 2 trailing labels = 48 attempts.
pub fn tmdb_fetch(
    addr: &str,
    input: &str,
    refresh: bool,
    region: Option<&str>,
) -> Result<TmdbFetchResponse> {
    #[cfg(target_os = "android")]
    let _ = region;

    let req = serde_json::json!({ "title_id": input, "refresh": refresh });
    let resp = send_recv(
        addr,
        FrameType::TmdbFetch,
        FrameType::TmdbFetchAck,
        Some(&serde_json::to_vec(&req)?),
    )?;
    let v: serde_json::Value = serde_json::from_slice(&resp)?;

    if v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
        return Ok(serde_json::from_value(v).unwrap_or_default());
    }

    let err = v.get("error").and_then(|e| e.as_str()).unwrap_or("unknown");

    if err == "invalid_title_id" {
        return Ok(TmdbFetchResponse {
            ok: false,
            title_id: input.to_string(),
            error: Some("invalid_title_id".to_string()),
            ..Default::default()
        });
    }

    if err != "not_cached" {
        bail!("{}", err);
    }

    let title_id = match normalize_title_id(input) {
        Some(tid) => tid,
        None => {
            return Ok(TmdbFetchResponse {
                ok: false,
                title_id: input.to_string(),
                error: Some("invalid_title_id".to_string()),
                ..Default::default()
            });
        }
    };

    // Prefer the console first. The PS Store product pages render
    // client-side now: the application/ld+json scrape below returns an
    // empty skeleton, and a full prefix×label sweep can hang for minutes
    // (24 prefixes × 2 labels × ~15 s timeout) before we ever reach the
    // path that works. app.db already knows installed title names —
    // no network, cannot rot. diagnostics::appdb_query is intentionally
    // not used here; it still uses the older whole-file heuristic and
    // returns metadata blobs rather than title names.
    let short = &title_id[..title_id.len().min(9)];
    if let Ok(list) = crate::activity::activity_db_query(addr, "recently_played") {
        if let Some(row) = list
            .rows
            .iter()
            .find(|r| r.title_id.starts_with(short) && r.name.is_some())
        {
            let result = TmdbFetchResponse {
                ok: true,
                title_id: title_id.clone(),
                np_title_id: Some(row.title_id.clone()),
                name: row.name.clone(),
                // Named so the UI can say where this came from rather
                // than implying store data it does not have.
                category: Some("console".to_string()),
                ..Default::default()
            };
            let json = serde_json::to_string(&result).unwrap_or_default();
            let _ = tmdb_store_on_payload(addr, &title_id, &json).ok();
            return Ok(result);
        }
    }

    // Optional last-ditch store scrape for titles not on the console.
    // Only attempt when the caller supplied a full content ID (so we
    // try one URL, not 48) or an explicit region (one prefix × 2 labels).
    // Bare title_ids with no region skip the dead scrape entirely.
    #[cfg(not(target_os = "android"))]
    {
        let known_cid = if is_valid_content_id(input) {
            Some(input)
        } else {
            None
        };
        let try_store = known_cid.is_some() || region.is_some();
        if try_store {
            if let Some((result, _raw_jsonld)) = fetch_from_store(&title_id, known_cid, region) {
                let store_json = serde_json::to_string(&result).unwrap_or_default();
                let _ = tmdb_store_on_payload(addr, &title_id, &store_json).ok();
                return Ok(result);
            }
        }
    }

    Ok(TmdbFetchResponse {
        ok: false,
        title_id,
        error: Some("not_found".to_string()),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_cached_response() {
        let json =
            r#"{"ok":true,"title_id":"CUSA00001","np_title_id":"CUSA00001","name":"Test Game"}"#;
        let resp: TmdbFetchResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.title_id, "CUSA00001");
        assert_eq!(resp.name.as_deref(), Some("Test Game"));
    }

    #[test]
    fn deserialize_not_cached() {
        let json = r#"{"ok":false,"error":"not_cached","title_id":"CUSA00001"}"#;
        let resp: TmdbFetchResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("not_cached"));
    }

    #[test]
    fn default_response() {
        let resp = TmdbFetchResponse::default();
        assert!(!resp.ok);
        assert!(resp.title_id.is_empty());
    }

    #[test]
    fn deserialize_full_response() {
        let json = r#"{
            "ok":true,
            "title_id":"CUSA00411",
            "np_title_id":"CUSA00411",
            "content_id":"UP9000-CUSA00411_00-DDDPACK000000001",
            "name":"Doom Eternal",
            "description":"Slayer",
            "icon":"https://example.com/icon.png",
            "category":"GAME"
        }"#;
        let resp: TmdbFetchResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.title_id, "CUSA00411");
        assert_eq!(resp.name.as_deref(), Some("Doom Eternal"));
        assert_eq!(resp.category.as_deref(), Some("GAME"));
        assert!(resp.icon.is_some());
    }

    #[test]
    fn deserialize_invalid_title_id() {
        let json = r#"{"ok":false,"error":"invalid_title_id","title_id":"bad"}"#;
        let resp: TmdbFetchResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("invalid_title_id"));
    }

    #[test]
    fn deserialize_empty_ok() {
        let json = r#"{"ok":true}"#;
        let resp: TmdbFetchResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
    }

    #[test]
    fn request_serialization() {
        let req = TmdbFetchRequest {
            title_id: "CUSA00001".to_string(),
            refresh: true,
            region: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("CUSA00001"));
        assert!(json.contains("\"refresh\":true"));
    }

    #[test]
    fn request_serialization_with_region() {
        let req = TmdbFetchRequest {
            title_id: "CUSA00001".to_string(),
            refresh: false,
            region: Some("UP9000".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("UP9000"));
        assert!(json.contains("\"refresh\":false"));
    }

    #[test]
    fn title_id_valid() {
        assert!(is_valid_title_id("CUSA00001_00"));
        assert!(is_valid_title_id("PPSA01325_00"));
    }

    #[test]
    fn title_id_invalid() {
        assert!(!is_valid_title_id("cusa00001_00"));
        assert!(!is_valid_title_id("CUSA0001_00"));
        assert!(!is_valid_title_id("CUSA0000100"));
        assert!(!is_valid_title_id("CUSA00001_0"));
        assert!(!is_valid_title_id(""));
        assert!(!is_valid_title_id("123400001_00"));
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn mem_find_finds_substring() {
        assert_eq!(mem_find(b"hello world", b"world"), Some(6));
        assert_eq!(mem_find(b"hello world", b"xyz"), None);
        assert_eq!(mem_find(b"hello", b""), None);
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn extract_jsonld_from_html() {
        let html = br#"<html><script id="mfe-jsonld-tags" type="application/ld+json">{"name":"Test"}</script></html>"#;
        let result = extract_jsonld(html);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Test"));
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn extract_jsonld_missing() {
        let html = b"<html><body>no jsonld</body></html>";
        assert!(extract_jsonld(html).is_none());
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn extract_jsonld_generic_fallback() {
        let html = br#"<html><script type="application/ld+json">{"@type":"VideoGame","name":"Fallback"}</script></html>"#;
        let result = extract_jsonld(html);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Fallback"));
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn extract_jsonld_generic_skips_non_jsonld() {
        let html = br#"<html><script type="application/json">{"data":1}</script><script type="application/ld+json">{"@type":"Game","name":"Real"}</script></html>"#;
        let result = extract_jsonld(html);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Real"));
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn parse_store_json_extracts_fields() {
        let jsonld = r#"{"name":"God of War","description":"Action","category":"GAME","image":"http://img.com/a.png"}"#;
        let resp = parse_store_json(jsonld, "UP9000-CUSA00001_00-LABEL", "CUSA00001_00");
        assert!(resp.ok);
        assert_eq!(resp.name.as_deref(), Some("God of War"));
        assert_eq!(resp.category.as_deref(), Some("GAME"));
        assert_eq!(resp.icon.as_deref(), Some("http://img.com/a.png"));
        assert_eq!(
            resp.content_id.as_deref(),
            Some("UP9000-CUSA00001_00-LABEL")
        );
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn parse_store_json_richer_fields() {
        let jsonld = r#"{
            "name":"Elden Ring",
            "description":"Action RPG",
            "category":"GAME",
            "image":"http://img.com/elden.png",
            "publisher":"Bandai Namco",
            "releaseDate":"2022-02-25",
            "genre":["Action","RPG"],
            "sku":"ELDENRING001"
        }"#;
        let resp = parse_store_json(jsonld, "UP9000-CUSA00001_00-LABEL", "CUSA00001_00");
        assert!(resp.ok);
        assert_eq!(resp.name.as_deref(), Some("Elden Ring"));
        assert_eq!(resp.publisher.as_deref(), Some("Bandai Namco"));
        assert_eq!(resp.release_date.as_deref(), Some("2022-02-25"));
        assert_eq!(resp.genre.as_deref(), Some("Action, RPG"));
        assert_eq!(resp.sku.as_deref(), Some("ELDENRING001"));
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn parse_store_json_publisher_object() {
        let jsonld = r#"{"name":"Game","publisher":{"name":"Sony Interactive"}}"#;
        let resp = parse_store_json(jsonld, "UP9000-CUSA00001_00-LABEL", "CUSA00001_00");
        assert_eq!(resp.publisher.as_deref(), Some("Sony Interactive"));
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn parse_store_json_genre_string() {
        let jsonld = r#"{"name":"Game","genre":"Action"}"#;
        let resp = parse_store_json(jsonld, "UP9000-CUSA00001_00-LABEL", "CUSA00001_00");
        assert_eq!(resp.genre.as_deref(), Some("Action"));
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn parse_store_json_minimal() {
        let jsonld = r#"{}"#;
        let resp = parse_store_json(jsonld, "UP9000-CUSA00001_00-LABEL", "CUSA00001_00");
        assert!(resp.ok);
        assert!(resp.name.is_none());
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn parse_store_json_invalid() {
        let resp = parse_store_json("not json", "UP9000-CUSA00001_00-LABEL", "CUSA00001_00");
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("parse_failed"));
    }

    #[test]
    fn content_id_valid() {
        assert!(is_valid_content_id("UP9000-CUSA00411_00-DDDPACK000000001"));
        assert!(is_valid_content_id("EP0006-CUSA00001_00-PREINMASTER00000"));
    }

    #[test]
    fn content_id_invalid() {
        assert!(!is_valid_content_id("CUSA00001_00"));
        assert!(!is_valid_content_id("UP9000-CUSA00411_00"));
        assert!(!is_valid_content_id(""));
        assert!(!is_valid_content_id("UP9000CUSA0041100DDDPACK000000001"));
    }

    #[test]
    fn title_id_from_cid_extracts() {
        assert_eq!(
            title_id_from_content_id("UP9000-CUSA00411_00-DDDPACK000000001"),
            Some("CUSA00411_00")
        );
    }

    #[test]
    fn normalize_title_id_from_title() {
        assert_eq!(
            normalize_title_id("CUSA00001_00"),
            Some("CUSA00001_00".to_string())
        );
    }

    #[test]
    fn normalize_title_id_from_content() {
        assert_eq!(
            normalize_title_id("UP9000-CUSA00411_00-DDDPACK000000001"),
            Some("CUSA00411_00".to_string())
        );
    }

    #[test]
    fn normalize_title_id_invalid() {
        assert_eq!(normalize_title_id("bad"), None);
        assert_eq!(normalize_title_id(""), None);
    }
}
