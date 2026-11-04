//! Which names a browser may reach this engine by, and who is really asking behind a reverse proxy.
//!
//! The API has no password by decision, and the origin guard in lib.rs exists to stop web pages.
//! A page can pass that guard through DNS rebinding: `evil.example` re-points its own name at the
//! engine's address, and its requests then have a matching `Origin` and `Host`. They do carry that
//! name, so the name is checked: this computer's loopback names, IP literals (nothing can
//! re-point an IP), and names that public DNS never answers are fine. Any other domain needs
//! `PS5UPLOAD_ALLOW_HOST`, which is what someone running a reverse proxy on their own domain sets.
//!
//! `PS5UPLOAD_TRUSTED_PROXY` names the proxies whose `X-Forwarded-For` is believed. Only then does
//! the address behind the proxy count, for the allowlist and the per-address limits alike. Without
//! it the header is ignored, so a client cannot claim to be someone else.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, OnceLock};

use axum::extract::{ConnectInfo, Request};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AllowRule;

/// Names public DNS does not hand out, so an outside page cannot re-point one at this engine.
/// (`ts.net`: Tailscale's names, whose addresses Tailscale sets, not their owner.)
const LOCAL_SUFFIXES: &[&str] = &[
    "local",
    "lan",
    "home",
    "home.arpa",
    "internal",
    "intranet",
    "localdomain",
    "ts.net",
];

/// `PS5UPLOAD_ALLOW_HOST`: a comma-separated list of names (`ps5.example.com`), of `*.example.com`
/// for a name and everything below it, or `*` for any name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostRules {
    any: bool,
    names: Vec<String>,
    suffixes: Vec<String>,
}

impl HostRules {
    pub(crate) fn parse(raw: &str) -> Self {
        let mut rules = Self::default();
        for entry in raw.split(',') {
            let entry = name_of(entry.trim());
            if entry.is_empty() {
                continue;
            }
            if entry == "*" {
                rules.any = true;
            } else if let Some(suffix) = entry.strip_prefix("*.") {
                rules.suffixes.push(suffix.to_string());
            } else {
                rules.names.push(entry);
            }
        }
        rules
    }

    /// The operator named this name.
    fn names(&self, name: &str) -> bool {
        self.any
            || self.names.iter().any(|n| n == name)
            || self.suffixes.iter().any(|s| name == s || is_below(name, s))
    }
}

fn is_below(name: &str, suffix: &str) -> bool {
    name.strip_suffix(suffix)
        .is_some_and(|rest| rest.ends_with('.'))
}

/// The name in a `Host` value or a list entry: no port, no brackets, lower case, no final dot.
pub(crate) fn name_of(authority: &str) -> String {
    split_authority(authority).0
}

/// `(name, port)` of `host`, `host:port`, `[v6]` or `[v6]:port`, the name in its usual form. A
/// bare IPv6 address (more than one colon, no brackets) has no port.
pub(crate) fn split_authority(authority: &str) -> (String, Option<u16>) {
    let authority = authority.trim();
    let (name, port) = if let Some(rest) = authority.strip_prefix('[') {
        match rest.split_once(']') {
            Some((name, tail)) => (name, tail.strip_prefix(':')),
            None => (rest, None),
        }
    } else if authority.matches(':').count() > 1 {
        (authority, None)
    } else {
        match authority.rsplit_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (authority, None),
        }
    };
    (
        name.trim_end_matches('.').to_ascii_lowercase(),
        port.and_then(|p| p.parse().ok()),
    )
}

fn is_loopback_name(name: &str) -> bool {
    name == "localhost"
        || name.ends_with(".localhost")
        || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// `Host` may reach this engine. See the module comment for why each kind of name is safe.
pub(crate) fn host_allowed(host: &str, rules: &HostRules) -> bool {
    let name = name_of(host);
    if name.is_empty() {
        return false;
    }
    is_loopback_name(&name)
        || name.parse::<IpAddr>().is_ok()
        || !name.contains('.')
        || LOCAL_SUFFIXES.iter().any(|s| is_below(&name, s))
        || rules.names(&name)
}

fn host_rules() -> &'static HostRules {
    static RULES: OnceLock<HostRules> = OnceLock::new();
    RULES.get_or_init(|| {
        HostRules::parse(&std::env::var("PS5UPLOAD_ALLOW_HOST").unwrap_or_default())
    })
}

/// The request's `Host` is a name the operator listed in `PS5UPLOAD_ALLOW_HOST`. A proxy on this
/// computer connects from loopback under its public name; this is what lets the routes that
/// answer this computer alone tell that from a re-pointed name.
pub(crate) fn host_named_by_operator(headers: &HeaderMap) -> bool {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|h| host_rules().names(&name_of(h)))
}

/// The refusal: what was refused, and the one setting that changes it.
fn refused(name: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        axum::Json(serde_json::json!({
            "error": format!(
                "This engine does not answer to the name {name}. If this is your own server, \
                 restart the engine with PS5UPLOAD_ALLOW_HOST={name}. Names on the public \
                 internet are refused unless listed, so that a web page cannot reach the engine \
                 by pointing its own name at it."
            ),
            "code": "host_not_allowed",
            "setting": "PS5UPLOAD_ALLOW_HOST",
            "host": name,
        })),
    )
        .into_response()
}

/// One log line per refused name, however often it is tried: a tab left open polls every second.
fn first_time_refused(name: &str) -> bool {
    static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let mut guard = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let seen = guard.get_or_insert_with(HashSet::new);
    seen.len() < 64 && seen.insert(name.to_string())
}

/// The refusal for a request whose `Host` this engine does not answer to, if it is one. A missing
/// `Host` is the origin guard's to judge. `/pkg-host/` is what the console fetches, always by
/// the address it was given.
pub(crate) fn refuse_host(headers: &HeaderMap, path: &str) -> Option<Response> {
    if path.starts_with("/pkg-host/") {
        return None;
    }
    let host = headers.get(header::HOST)?.to_str().ok()?;
    if host_allowed(host, host_rules()) {
        return None;
    }
    let name = name_of(host);
    if first_time_refused(&name) {
        crate::log_warn!(
            "refused requests for the name {name}: set PS5UPLOAD_ALLOW_HOST={name} if it is yours"
        );
    }
    Some(refused(&name))
}

/// `Origin` and `Host` name the same server. Ports are compared after filling in the scheme's
/// default (`https://x` is `x:443`). A `Host` with no port says nothing about the port: a proxy's
/// `$host` drops it, and the name alone decides.
pub(crate) fn same_server(scheme: &str, origin_authority: &str, host: &str) -> bool {
    let (origin_name, origin_port) = split_authority(origin_authority);
    let (host_name, host_port) = split_authority(host);
    if origin_name.is_empty() || origin_name != host_name {
        return false;
    }
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    host_port.is_none() || origin_port.or(default) == host_port
}

/// The first value of a proxy's list header (`a, b`: `a` is what the client asked for).
pub(crate) fn first_forwarded<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)?
        .to_str()
        .ok()?
        .split(',')
        .next()
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// A top-level navigation to a page: the app's shell, `/join` or `/pad`, opened from a link on
/// another site. Pages have no side effects and answer nothing readable to that site.
pub(crate) fn is_page_navigation(
    method: &axum::http::Method,
    headers: &HeaderMap,
    path: &str,
) -> bool {
    let is = |name: &str, value: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case(value))
    };
    method == axum::http::Method::GET
        && is("sec-fetch-mode", "navigate")
        && is("sec-fetch-dest", "document")
        && !path.contains("/api/")
}

// ---- the address behind a trusted proxy ----

fn trusted_proxies() -> &'static [AllowRule] {
    static RULES: OnceLock<Vec<AllowRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        crate::parse_allow_ips(&std::env::var("PS5UPLOAD_TRUSTED_PROXY").unwrap_or_default())
    })
}

/// Who a request came from. A proxy adds the address it saw to the end of `X-Forwarded-For`, so the
/// list is read from the end, past the proxies this engine trusts; anything further left was
/// written by the client and proves nothing. A peer that is not a trusted proxy is itself the answer.
pub(crate) fn effective_peer(
    peer: IpAddr,
    forwarded_for: &[&str],
    trusted: &[AllowRule],
) -> IpAddr {
    if !crate::allow_rules_contain(trusted, peer) {
        return peer;
    }
    let mut last = peer;
    for entry in forwarded_for.iter().rev().flat_map(|v| v.rsplit(',')) {
        let Ok(ip) = entry.trim().parse::<IpAddr>() else {
            return last;
        };
        // An IPv4 client through a dual-stack proxy is listed as ::ffff:a.b.c.d.
        let ip = ip.to_canonical();
        if !crate::allow_rules_contain(trusted, ip) {
            return ip;
        }
        last = ip;
    }
    last
}

/// Outermost layer: downstream, `ConnectInfo` is the address behind a trusted proxy. The allowlist,
/// the guest limits and the host checks all read it from there.
pub(crate) async fn apply_trusted_proxy(mut req: Request, next: Next) -> Response {
    let trusted = trusted_proxies();
    if !trusted.is_empty() {
        if let Some(ConnectInfo(peer)) = req.extensions().get::<ConnectInfo<SocketAddr>>().copied()
        {
            let values: Vec<&str> = req
                .headers()
                .get_all("x-forwarded-for")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .collect();
            let who = effective_peer(peer.ip().to_canonical(), &values, trusted);
            req.extensions_mut()
                .insert(ConnectInfo(SocketAddr::new(who, peer.port())));
        }
    }
    next.run(req).await
}

/// What a self-hoster should read at start-up about these settings.
pub(crate) fn log_settings() {
    let raw = std::env::var("PS5UPLOAD_TRUSTED_PROXY").unwrap_or_default();
    let bad = crate::invalid_allow_ip_entries(&raw);
    if !bad.is_empty() {
        eprintln!(
            "[ps5upload-engine] WARNING: ignoring PS5UPLOAD_TRUSTED_PROXY entries that are not an \
             IP or CIDR range: {}",
            bad.join(", ")
        );
    }
    let raw = std::env::var("PS5UPLOAD_BASE_URL").unwrap_or_default();
    if normalize_base(&raw).is_none() {
        eprintln!(
            "[ps5upload-engine] WARNING: ignoring PS5UPLOAD_BASE_URL={raw}: it is a path such as \
             /ps5/, not a hostname or a full URL. Serving at the root instead."
        );
    }
}

// ---- hosted below a path ----

/// `PS5UPLOAD_BASE_URL` in one form: `""` for the root, else `/a/b` with no trailing slash. None
/// for what is not a path (a full URL used to become the route prefix `/https://...`).
pub(crate) fn normalize_base(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Some(String::new());
    }
    let ok = trimmed.split('/').all(|seg| {
        !seg.is_empty()
            && seg != "."
            && seg != ".."
            && seg
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
    });
    ok.then(|| format!("/{trimmed}"))
}

/// Where this engine is hosted: `""`, or a prefix such as `/ps5`.
pub(crate) fn base_path() -> &'static str {
    static BASE: OnceLock<String> = OnceLock::new();
    BASE.get_or_init(|| {
        normalize_base(&std::env::var("PS5UPLOAD_BASE_URL").unwrap_or_default()).unwrap_or_default()
    })
}

/// `path` without this engine's prefix.
pub(crate) fn strip_base(path: &str) -> &str {
    let base = base_path();
    if base.is_empty() {
        return path;
    }
    path.strip_prefix(base)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> HostRules {
        HostRules::default()
    }

    #[test]
    fn every_local_way_of_reaching_the_engine_passes() {
        for host in [
            "127.0.0.1:19113",
            "localhost:19113",
            "LOCALHOST",
            "[::1]:19113",
            "tauri.localhost",
            "192.168.1.10:19113",
            "10.0.0.2",
            "8.8.8.8:19113", // a public IP literal: an address cannot be re-pointed
            "[fd00::1]:19113",
            "nas:19113",
            "nas.local:19113",
            "nas.local.:19113",
            "ps5upload.lan",
            "ps5upload.home",
            "ps5.home.arpa",
            "ps5.internal",
            "host.docker.internal:19113",
            "box.tailnet-abc.ts.net",
        ] {
            assert!(host_allowed(host, &none()), "{host}");
        }
    }

    #[test]
    fn a_public_domain_needs_to_be_listed() {
        for host in [
            "evil.example:19113",
            "evil.example",
            "ps5.mydomain.com",
            "127.0.0.1.nip.io",
        ] {
            assert!(!host_allowed(host, &none()), "{host}");
        }
        assert!(!host_allowed("", &none()));
        // a suffix must match whole labels
        assert!(!host_allowed("evillocal.com", &none()));
        assert!(!host_allowed("notts.net", &none()));
        assert!(!host_allowed("local.evil.example", &none()));
    }

    #[test]
    fn the_list_takes_names_wildcards_and_everything() {
        let rules = HostRules::parse(" ps5.mydomain.com:443 , *.home.example,, ");
        assert!(host_allowed("PS5.MyDomain.com", &rules));
        assert!(host_allowed("ps5.mydomain.com:8443", &rules));
        assert!(host_allowed("a.home.example", &rules));
        assert!(host_allowed("a.b.home.example", &rules));
        assert!(host_allowed("home.example", &rules));
        assert!(!host_allowed("other.mydomain.com", &rules));
        assert!(!host_allowed("xhome.example", &rules));
        assert!(host_allowed("anything.example", &HostRules::parse("*")));
    }

    #[test]
    fn the_refusal_says_which_setting_to_use() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, "evil.example:19113".parse().unwrap());
        let r = refuse_host(&h, "/api/jobs").expect("refused");
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            r.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        // The console fetches packages by address; a name there is not the browser's business.
        assert!(refuse_host(&h, "/pkg-host/1/a.pkg").is_none());
        h.insert(header::HOST, "nas.local:19113".parse().unwrap());
        assert!(refuse_host(&h, "/api/jobs").is_none());
        // no Host at all is for the origin guard
        assert!(refuse_host(&HeaderMap::new(), "/api/jobs").is_none());
    }

    #[tokio::test]
    async fn the_refusal_body_names_the_setting_and_the_host() {
        let body = axum::body::to_bytes(refused("evil.example").into_body(), 4096)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["code"], "host_not_allowed");
        assert_eq!(v["setting"], "PS5UPLOAD_ALLOW_HOST");
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains("PS5UPLOAD_ALLOW_HOST=evil.example"));
    }

    #[test]
    fn a_base_path_is_a_path() {
        assert_eq!(normalize_base("").as_deref(), Some(""));
        assert_eq!(normalize_base("/").as_deref(), Some(""));
        assert_eq!(normalize_base(" /ps5/ ").as_deref(), Some("/ps5"));
        assert_eq!(normalize_base("ps5").as_deref(), Some("/ps5"));
        assert_eq!(normalize_base("/a/b/").as_deref(), Some("/a/b"));
        for bad in [
            "https://ps5.home/",
            "ps5.home:8080",
            "/a b/",
            "/a//b",
            "/../x",
            "/a?b",
        ] {
            assert_eq!(normalize_base(bad), None, "{bad}");
        }
    }

    #[test]
    fn same_server_fills_in_default_ports() {
        assert!(same_server("https", "x", "x:443"));
        assert!(same_server("http", "x", "x:80"));
        assert!(same_server("http", "x:19113", "x:19113"));
        assert!(same_server("https", "X", "x"));
        // $host drops the port
        assert!(same_server("https", "x:8443", "x"));
        assert!(!same_server("http", "x", "x:443"));
        assert!(!same_server("https", "x:8443", "x:443"));
        assert!(!same_server("http", "x:1", "x:2"));
        assert!(!same_server("http", "y", "x"));
        assert!(same_server("http", "[::1]:5", "[::1]:5"));
        assert!(!same_server("http", "", ""));
    }

    #[test]
    fn navigation_is_a_get_of_a_document_outside_the_api() {
        use axum::http::Method;
        let mut h = HeaderMap::new();
        assert!(!is_page_navigation(&Method::GET, &h, "/join"));
        h.insert("sec-fetch-mode", "navigate".parse().unwrap());
        h.insert("sec-fetch-dest", "document".parse().unwrap());
        assert!(is_page_navigation(&Method::GET, &h, "/join"));
        assert!(is_page_navigation(&Method::GET, &h, "/"));
        assert!(!is_page_navigation(&Method::POST, &h, "/join"));
        assert!(!is_page_navigation(&Method::GET, &h, "/api/jobs"));
        assert!(!is_page_navigation(&Method::GET, &h, "/ps5/api/jobs"));
        h.insert("sec-fetch-dest", "image".parse().unwrap());
        assert!(!is_page_navigation(&Method::GET, &h, "/join"));
    }

    #[test]
    fn the_address_behind_a_proxy_is_only_believed_from_a_trusted_one() {
        let trusted = crate::parse_allow_ips("127.0.0.1,10.0.0.0/8");
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        // not a trusted proxy: the header proves nothing
        assert_eq!(
            effective_peer(ip("192.168.1.9"), &["1.2.3.4"], &trusted),
            ip("192.168.1.9")
        );
        // no setting at all
        assert_eq!(
            effective_peer(ip("127.0.0.1"), &["1.2.3.4"], &[]),
            ip("127.0.0.1")
        );
        // the client wrote the left part; the proxy added what it saw
        assert_eq!(
            effective_peer(ip("127.0.0.1"), &["6.6.6.6, 192.168.1.9"], &trusted),
            ip("192.168.1.9")
        );
        // through two trusted proxies
        assert_eq!(
            effective_peer(ip("127.0.0.1"), &["192.168.1.9, 10.0.0.2"], &trusted),
            ip("192.168.1.9")
        );
        // two header lines read as one list
        assert_eq!(
            effective_peer(ip("127.0.0.1"), &["192.168.1.9", "10.0.0.2"], &trusted),
            ip("192.168.1.9")
        );
        // dual-stack form
        assert_eq!(
            effective_peer(ip("127.0.0.1"), &["::ffff:192.168.1.9"], &trusted),
            ip("192.168.1.9")
        );
        // a proxy that adds nothing, and junk
        assert_eq!(
            effective_peer(ip("127.0.0.1"), &[], &trusted),
            ip("127.0.0.1")
        );
        assert_eq!(
            effective_peer(ip("127.0.0.1"), &["unknown"], &trusted),
            ip("127.0.0.1")
        );
    }
}
