//! The built-in hub: Play Together's signalling hub (the `ps5upload-hub` crate) and the guest
//! pages, run by an engine on a server, for someone who already has it behind a reverse proxy with
//! HTTPS and wants friends' invites to go through their own name only (SELF_HOSTING.md). Off
//! unless `PS5UPLOAD_HUB_LISTEN` is set: nothing here binds or runs otherwise.
//!
//! * `PS5UPLOAD_HUB_LISTEN`: where it listens (`0.0.0.0:19115`, a bare port, or `on` for that
//!   default). A listener of its own, apart from the engine API: it serves `/ws` (the hub),
//!   `/join/`, `/pad/` and `/share/` (the guest pages this engine was built with, so they always
//!   match it), `/health`, and nothing else (404). The engine API must never be on the internet;
//!   this listener is the one a reverse proxy may publish.
//! * `PS5UPLOAD_HUB_PUBLIC_URL`: its public address (`https://play.example.com`, no path). The
//!   invites' hub (`wss://play.example.com/ws`) and page base (`https://play.example.com/`) follow
//!   from it unless `PS5UPLOAD_HUB_URL` / `PS5UPLOAD_PAGE_BASE` say otherwise, and the hub takes
//!   browsers from that origin (beside the website's and the app's, as the standalone hub does).
//!   The engine reaches its own hub over loopback ([`dial`]); links carry the public address.
//! * `PS5UPLOAD_HUB_STUN`: also answer STUN on this UDP address (`0.0.0.0:3478`, a bare port, or
//!   `on`), and put `<public host>:<port>` first in the STUN servers. A reverse proxy does not
//!   carry UDP: the port must reach this machine as it is.
//!
//! The hub's own `HUB_*` limits (ps5upload-hub config.rs) apply as in the standalone hub, and
//! `PS5UPLOAD_TRUSTED_PROXY` (beside loopback) says whose `X-Forwarded-For` it believes.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;

pub(crate) const LISTEN_ENV: &str = "PS5UPLOAD_HUB_LISTEN";
pub(crate) const PUBLIC_URL_ENV: &str = "PS5UPLOAD_HUB_PUBLIC_URL";
pub(crate) const STUN_ENV: &str = "PS5UPLOAD_HUB_STUN";
/// The built-in hub's port when `PS5UPLOAD_HUB_LISTEN` is just `on` (the engine's is 19113).
pub(crate) const DEFAULT_PORT: u16 = 19115;
pub(crate) const DEFAULT_STUN_PORT: u16 = 3478;

/// What the environment asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub listen: SocketAddr,
    /// The public address as a page base with no path (`https://play.example.com/`).
    pub public: Option<String>,
    pub stun: Option<SocketAddr>,
}

/// `on`, a bare port or an address; None (with a warning) for anything else.
fn address(v: &str, default_port: u16) -> Option<SocketAddr> {
    let v = v.trim();
    let any = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
    if matches!(v.to_ascii_lowercase().as_str(), "on" | "true" | "yes") {
        return Some(SocketAddr::new(any, default_port));
    }
    if let Ok(p) = v.parse::<u16>() {
        return (p != 0).then(|| SocketAddr::new(any, p));
    }
    v.parse::<SocketAddr>().ok().filter(|a| a.port() != 0)
}

impl Plan {
    /// The built-in hub the environment asks for, or None (off). `warn` gets a line for each
    /// value that was ignored.
    pub(crate) fn from_lookup(
        get: &dyn Fn(&str) -> Option<String>,
        warn: &mut Vec<String>,
    ) -> Option<Plan> {
        let set = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let Some(raw) = set(LISTEN_ENV) else {
            for k in [PUBLIC_URL_ENV, STUN_ENV] {
                if set(k).is_some() {
                    warn.push(format!(
                        "ignoring {k}: it is for the built-in hub, which runs only when \
                         {LISTEN_ENV} is set."
                    ));
                }
            }
            return None;
        };
        let Some(listen) = address(&raw, DEFAULT_PORT) else {
            warn.push(format!(
                "ignoring {LISTEN_ENV}={}: use an address and port such as 0.0.0.0:{DEFAULT_PORT}, \
                 a port, or on. The built-in hub is off.",
                raw.trim()
            ));
            return None;
        };
        let public = set(PUBLIC_URL_ENV).and_then(|v| {
            let base = crate::join_net::page_base(&v).filter(|b| {
                // An origin only: the pages and /ws are at its root.
                b.matches('/').count() == 3
            });
            if base.is_none() {
                warn.push(format!(
                    "ignoring {PUBLIC_URL_ENV}={}: use the https:// address the reverse proxy \
                     serves the built-in hub at, with no path, such as https://play.example.com.",
                    v.trim()
                ));
            }
            base
        });
        if public.is_none() {
            warn.push(format!(
                "{LISTEN_ENV} is set without {PUBLIC_URL_ENV}: the built-in hub runs, but \
                 invites use it only if Settings › Play Together names it."
            ));
        }
        let stun = set(STUN_ENV).and_then(|v| {
            let a = address(&v, DEFAULT_STUN_PORT);
            if a.is_none() {
                warn.push(format!(
                    "ignoring {STUN_ENV}={}: use a UDP address and port such as \
                     0.0.0.0:{DEFAULT_STUN_PORT}, a port, or on.",
                    v.trim()
                ));
            }
            a
        });
        Some(Plan {
            listen,
            public,
            stun,
        })
    }

    /// `play.example.com` or `play.example.com:8443`.
    fn public_authority(&self) -> Option<&str> {
        let b = self.public.as_deref()?;
        let rest = &b[b.find("://")? + 3..];
        Some(rest.trim_end_matches('/'))
    }

    /// The hub's public address, as invites name it (`wss://play.example.com/ws`).
    pub(crate) fn hub_url(&self) -> Option<String> {
        let scheme = if self.public.as_deref()?.starts_with("https://") {
            "wss"
        } else {
            "ws"
        };
        Some(format!("{scheme}://{}/ws", self.public_authority()?))
    }

    /// The pages' public address (`https://play.example.com/`).
    pub(crate) fn page_base(&self) -> Option<String> {
        self.public.clone()
    }

    /// The STUN server as the settings name it (`play.example.com:3478`), when it answers STUN.
    pub(crate) fn stun_server(&self) -> Option<String> {
        let port = self.stun?.port();
        let auth = self.public_authority()?;
        let host = match auth.strip_prefix('[') {
            Some(v6) => format!("[{}]", v6.split(']').next()?),
            None => auth.split(':').next()?.to_string(),
        };
        Some(format!("{host}:{port}"))
    }
}

/// The hub's settings: its `HUB_*` ones (as the standalone hub reads them), on this plan's
/// addresses, browsers from `page_bases`' origins allowed beside the defaults, and loopback and
/// `PS5UPLOAD_TRUSTED_PROXY` believed for the client's address.
fn config(
    plan: &Plan,
    page_bases: &[&str],
    get: &dyn Fn(&str) -> Option<String>,
    warn: &mut Vec<String>,
) -> ps5upload_hub::Config {
    let mut c = ps5upload_hub::Config::from_lookup(get).unwrap_or_else(|e| {
        warn.push(format!(
            "the built-in hub ignores its HUB_* settings ({e}) and uses its defaults."
        ));
        ps5upload_hub::Config::default()
    });
    c.http_listen = plan.listen;
    c.metrics_listen = None;
    c.stun_listen = plan.stun.into_iter().collect();
    for b in page_bases {
        let o = crate::join_net::page_origin(b);
        if !c.allowed_origins.contains(&o) {
            c.allowed_origins.push(o);
        }
    }
    let proxies = get("PS5UPLOAD_TRUSTED_PROXY").unwrap_or_default();
    let more = ["127.0.0.0/8", "::1"]
        .into_iter()
        .chain(proxies.split(',').map(str::trim).filter(|s| !s.is_empty()))
        .filter_map(|s| s.parse::<ps5upload_hub::config::Cidr>().ok());
    for p in more {
        if !c.trusted_proxies.contains(&p) {
            c.trusted_proxies.push(p);
        }
    }
    c
}

// ── The pages ───────────────────────────────────────────────────────────────────────────────────

/// The share page is hand-written in the website's folder (the image builds copy it in).
const SHARE_HTML: &str = include_str!("../../../../site/share/index.html");

struct Page {
    raw: &'static str,
    gzip: Vec<u8>,
    /// From the page's bytes, so it changes when the engine (and its page) does.
    tag: String,
}

impl Page {
    fn new(raw: &'static str) -> Page {
        use sha2::Digest;
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        let gzip = gz
            .write_all(raw.as_bytes())
            .and_then(|()| gz.finish())
            .unwrap_or_default();
        let d = sha2::Sha256::digest(raw.as_bytes());
        let tag = d[..12].iter().map(|b| format!("{b:02x}")).collect();
        Page { raw, gzip, tag }
    }
}

/// The three pages, compressed once (on first use).
fn pages() -> &'static [Page; 3] {
    static P: OnceLock<[Page; 3]> = OnceLock::new();
    P.get_or_init(|| {
        [
            Page::new(crate::join::PAGE_HTML),
            Page::new(crate::pad_link::PAGE_HTML),
            Page::new(SHARE_HTML),
        ]
    })
}

/// The client takes gzip (`Accept-Encoding`, not at q=0).
fn takes_gzip(h: &HeaderMap) -> bool {
    h.get_all(header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|part| {
            let mut it = part.split(';');
            let name = it.next().unwrap_or("").trim().to_ascii_lowercase();
            let q0 = it.any(|p| {
                p.trim()
                    .strip_prefix("q=")
                    .and_then(|q| q.trim().parse::<f32>().ok())
                    .is_some_and(|q| q == 0.0)
            });
            (name == "gzip" || name == "*") && !q0
        })
}

fn serve_page(page: &Page, h: &HeaderMap) -> Response {
    let gz = takes_gzip(h) && !page.gzip.is_empty();
    let etag = if gz {
        format!("\"{}-gz\"", page.tag)
    } else {
        format!("\"{}\"", page.tag)
    };
    let fresh = h
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    let mut r = if fresh {
        StatusCode::NOT_MODIFIED.into_response()
    } else if gz {
        page.gzip.clone().into_response()
    } else {
        page.raw.into_response()
    };
    let hs = r.headers_mut();
    let set = |hs: &mut HeaderMap, k: header::HeaderName, v: &str| {
        if let Ok(v) = HeaderValue::from_str(v) {
            hs.insert(k, v);
        }
    };
    set(hs, header::CONTENT_TYPE, "text/html; charset=utf-8");
    if gz && !fresh {
        set(hs, header::CONTENT_ENCODING, "gzip");
    }
    set(hs, header::ETAG, &etag);
    set(hs, header::VARY, "accept-encoding");
    // The page asks the server each time (a 304 when unchanged): it must match the engine.
    set(hs, header::CACHE_CONTROL, "no-cache");
    // The invite is in the address's #fragment, which never leaves the browser; nothing else
    // from it goes anywhere either. The pages are never framed.
    set(hs, header::REFERRER_POLICY, "no-referrer");
    set(hs, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(
        hs,
        header::CONTENT_SECURITY_POLICY,
        "frame-ancestors 'none'",
    );
    r
}

/// The pages' routes (the hub's `/ws` and `/health` are its own).
fn pages_router() -> axum::Router {
    axum::Router::new()
        .route(
            "/join/",
            get(|h: HeaderMap| async move { serve_page(&pages()[0], &h) }),
        )
        .route(
            "/pad/",
            get(|h: HeaderMap| async move { serve_page(&pages()[1], &h) }),
        )
        .route(
            "/share/",
            get(|h: HeaderMap| async move { serve_page(&pages()[2], &h) }),
        )
        // The address without its slash (a link typed by hand); the #fragment stays.
        .route("/join", get(|| async { Redirect::permanent("/join/") }))
        .route("/pad", get(|| async { Redirect::permanent("/pad/") }))
        .route("/share", get(|| async { Redirect::permanent("/share/") }))
}

// ── Running ──────────────────────────────────────────────────────────────────────────────────────

struct Running {
    hub: ps5upload_hub::Hub,
    /// The public hub's `host[:port]`, when there is one.
    public_host: Option<String>,
    public_url: Option<String>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// Where the engine connects for the hub at `url`: its own built-in hub over loopback when `url`
/// is the built-in hub's public address (no trip out through the reverse proxy and back), None
/// for any other hub.
pub(crate) fn dial(url: &str) -> Option<String> {
    let g = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    let r = g.as_ref()?;
    let host = crate::join_net::hub_host(url)?;
    let path = url.parse::<axum::http::Uri>().ok()?.path().to_string();
    if r.public_host.as_deref() != Some(host.as_str()) || path != "/ws" {
        return None;
    }
    let a = r.hub.http_addr;
    let ip = match a.ip() {
        IpAddr::V4(v) if v.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v) if v.is_unspecified() => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    Some(format!("ws://{}/ws", SocketAddr::new(ip, a.port())))
}

/// The built-in hub as the settings show it (read-only): null when it is off.
pub(crate) fn view() -> serde_json::Value {
    let g = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    match g.as_ref() {
        None => serde_json::Value::Null,
        Some(r) => serde_json::json!({
            "port": r.hub.http_addr.port(),
            "public_url": r.public_url,
            "stun_ports": r.hub.stun_addrs.iter().map(|a| a.port()).collect::<Vec<_>>(),
        }),
    }
}

/// Starts the hub of `plan` on `listener`.
async fn launch(
    plan: &Plan,
    cfg: ps5upload_hub::Config,
    listener: tokio::net::TcpListener,
) -> std::io::Result<()> {
    // Compressed once, off the runtime's threads.
    let _ = tokio::task::spawn_blocking(|| {
        pages();
    })
    .await;
    let log: ps5upload_hub::LogSink = Arc::new(|line: &str| {
        crate::log_at!(Info, "together", "{line}");
    });
    let hub = ps5upload_hub::Hub::start_on(cfg, log, listener, Some(pages_router())).await?;
    let public_host = plan.hub_url().and_then(|u| crate::join_net::hub_host(&u));
    *RUNNING.lock().unwrap_or_else(|e| e.into_inner()) = Some(Running {
        hub,
        public_host,
        public_url: plan.public.clone(),
    });
    Ok(())
}

/// The built-in hub when the environment asks for it (called once as the engine starts);
/// nothing at all otherwise.
pub(crate) async fn start() {
    let d = crate::self_host::defaults();
    let Some(plan) = d.built_in.clone() else {
        return;
    };
    let get = |k: &str| std::env::var(k).ok();
    let mut warn = Vec::new();
    let cfg = config(&plan, &[d.page_base.as_str()], &get, &mut warn);
    for w in warn {
        crate::stderr_quiet(&format!("[ps5upload-engine] WARNING: {w}"));
        crate::log_at!(Warn, "together", "{w}");
    }
    let listener = match tokio::net::TcpListener::bind(plan.listen).await {
        Ok(l) => l,
        Err(e) => {
            let w = format!(
                "the built-in hub can't listen on {}: {e}. Play Together invites through it won't \
                 work until it can.",
                plan.listen
            );
            crate::stderr_quiet(&format!("[ps5upload-engine] WARNING: {w}"));
            crate::log_at!(Warn, "together", "{w}");
            return;
        }
    };
    match launch(&plan, cfg, listener).await {
        Ok(()) => {
            let line = format!(
                "built-in hub listening on {} (public: {})",
                plan.listen,
                plan.public.as_deref().unwrap_or("not set")
            );
            crate::stderr_quiet(&format!("[ps5upload-engine] {line}"));
            crate::log_info!("{line}");
        }
        Err(e) => crate::log_at!(Warn, "together", "the built-in hub didn't start: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn off_unless_asked_for() {
        let mut w = Vec::new();
        assert_eq!(Plan::from_lookup(&env(&[]), &mut w), None);
        assert!(w.is_empty());
        // The other two alone do nothing, and say so.
        let mut w = Vec::new();
        let get = env(&[
            (PUBLIC_URL_ENV, "https://play.example.com"),
            (STUN_ENV, "on"),
        ]);
        assert_eq!(Plan::from_lookup(&get, &mut w), None);
        assert_eq!(w.len(), 2, "{w:?}");
        let mut w = Vec::new();
        assert_eq!(
            Plan::from_lookup(&env(&[(LISTEN_ENV, "nope")]), &mut w),
            None
        );
        assert_eq!(w.len(), 1);
        // And nothing in the defaults changes.
        let d = crate::self_host::Defaults::from_lookup(env(&[]), &mut Vec::new());
        assert!(d.built_in.is_none());
    }

    #[test]
    fn the_public_address_gives_the_hub_the_pages_and_stun() {
        let mut w = Vec::new();
        let p = Plan::from_lookup(
            &env(&[
                (LISTEN_ENV, "on"),
                (PUBLIC_URL_ENV, "https://Play.Example.com/"),
                (STUN_ENV, "3478"),
            ]),
            &mut w,
        )
        .unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(p.listen, "0.0.0.0:19115".parse().unwrap());
        assert_eq!(p.stun, Some("0.0.0.0:3478".parse().unwrap()));
        assert_eq!(p.hub_url().as_deref(), Some("wss://play.example.com/ws"));
        assert_eq!(p.page_base().as_deref(), Some("https://play.example.com/"));
        assert_eq!(p.stun_server().as_deref(), Some("play.example.com:3478"));

        let p = Plan::from_lookup(
            &env(&[
                (LISTEN_ENV, "127.0.0.1:20000"),
                (PUBLIC_URL_ENV, "https://play.example.com:8443"),
            ]),
            &mut w,
        )
        .unwrap();
        assert_eq!(p.listen, "127.0.0.1:20000".parse().unwrap());
        assert_eq!(
            p.hub_url().as_deref(),
            Some("wss://play.example.com:8443/ws")
        );
        assert_eq!(p.stun_server(), None, "no STUN asked for");

        // A path, or plain http to another computer, is not a public address.
        for bad in ["https://play.example.com/hub", "http://play.example.com"] {
            let mut w = Vec::new();
            let p = Plan::from_lookup(
                &env(&[(LISTEN_ENV, "19115"), (PUBLIC_URL_ENV, bad)]),
                &mut w,
            )
            .unwrap();
            assert_eq!(p.public, None, "{bad}");
            assert!(w[0].contains(PUBLIC_URL_ENV), "{w:?}");
        }
    }

    #[test]
    fn the_defaults_follow_the_built_in_hub_unless_set_themselves() {
        let base = [
            (LISTEN_ENV, "on"),
            (PUBLIC_URL_ENV, "https://play.example.com"),
            (STUN_ENV, "on"),
        ];
        let mut w = Vec::new();
        let d = crate::self_host::Defaults::from_lookup(env(&base), &mut w);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(d.hub_url, "wss://play.example.com/ws");
        assert_eq!(d.page_base, "https://play.example.com/");
        assert_eq!(
            d.stun,
            [
                "play.example.com:3478",
                "stun.cloudflare.com:3478",
                "stun.l.google.com:19302"
            ]
        );
        assert_eq!(
            d.sources(&d.hub_url, &d.page_base, &d.stun),
            serde_json::json!({"hub_url": "environment", "page_base": "environment", "stun": "environment"})
        );
        let mut both = base.to_vec();
        both.push(("PS5UPLOAD_HUB_URL", "wss://other.example.com/ws"));
        both.push(("PS5UPLOAD_PAGE_BASE", "https://pages.example.com/"));
        let d = crate::self_host::Defaults::from_lookup(env(&both), &mut w);
        assert_eq!(d.hub_url, "wss://other.example.com/ws");
        assert_eq!(d.page_base, "https://pages.example.com/");
    }

    #[test]
    fn the_hub_takes_the_public_origin_and_believes_loopback_and_the_proxy() {
        let plan = Plan {
            listen: "127.0.0.1:0".parse().unwrap(),
            public: Some("https://play.example.com/".into()),
            stun: None,
        };
        let get = env(&[("PS5UPLOAD_TRUSTED_PROXY", "172.18.0.0/16, bad")]);
        let mut w = Vec::new();
        let c = config(&plan, &["https://play.example.com/"], &get, &mut w);
        assert!(w.is_empty());
        assert!(c
            .allowed_origins
            .contains(&"https://play.example.com".to_string()));
        assert!(c
            .allowed_origins
            .contains(&"https://ps5upload.phantomptr.com".to_string()));
        assert!(c.allowed_origins.contains(&"tauri://localhost".to_string()));
        assert!(c.metrics_listen.is_none() && c.stun_listen.is_empty());
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(c
            .trusted_proxies
            .iter()
            .any(|p| p.contains(ip("127.0.0.1"))));
        assert!(c
            .trusted_proxies
            .iter()
            .any(|p| p.contains(ip("172.18.0.5"))));
        assert!(!c.trusted_proxies.iter().any(|p| p.contains(ip("8.8.8.8"))));
    }

    #[test]
    fn accept_encoding() {
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::ACCEPT_ENCODING, HeaderValue::from_str(v).unwrap());
            m
        };
        assert!(takes_gzip(&h("gzip, deflate, br")));
        assert!(takes_gzip(&h("br;q=1.0, GZIP;q=0.5")));
        assert!(takes_gzip(&h("*")));
        assert!(!takes_gzip(&h("gzip;q=0")));
        assert!(!takes_gzip(&h("br, identity")));
        assert!(!takes_gzip(&HeaderMap::new()));
    }

    // ── On a loopback port, end to end ───────────────────────────────────────────────────────────

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One request, the whole answer (status line and headers as text, the body as bytes).
    async fn http(addr: SocketAddr, method: &str, path: &str, extra: &str) -> (String, Vec<u8>) {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(
            format!("{method} {path} HTTP/1.1\r\nhost: x\r\nconnection: close\r\n{extra}\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        let mut out = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(10), s.read_to_end(&mut out))
            .await
            .unwrap()
            .unwrap();
        let end = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (
            String::from_utf8_lossy(&out[..end]).to_ascii_lowercase(),
            out[end + 4..].to_vec(),
        )
    }

    async fn ws_with_origin(addr: SocketAddr, origin: &str) -> Result<(), u16> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://{addr}/ws").into_client_request().unwrap();
        req.headers_mut()
            .insert(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
        match tokio_tungstenite::connect_async(req).await {
            Ok(_) => Ok(()),
            Err(tokio_tungstenite::tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
            Err(e) => panic!("{e}"),
        }
    }

    /// The built-in hub on a loopback port: an engine room hosted on it through the loopback
    /// path while its link names the public address, a guest joined over WebSocket and answered,
    /// the origins it takes, the pages it serves (compressed, cached by ETag) and nothing of the
    /// engine API.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_built_in_hub_hosts_a_room_serves_the_pages_and_nothing_else() {
        let public = "https://play.example.test/";
        let plan = Plan {
            listen: "127.0.0.1:0".parse().unwrap(),
            public: Some(public.into()),
            stun: None,
        };
        let cfg = config(&plan, &[public], &|_| None, &mut Vec::new());
        let listener = tokio::net::TcpListener::bind(plan.listen).await.unwrap();
        let addr = listener.local_addr().unwrap();
        launch(&plan, cfg, listener).await.unwrap();
        let public_hub = plan.hub_url().unwrap();
        assert_eq!(public_hub, "wss://play.example.test/ws");
        assert_eq!(dial(&public_hub), Some(format!("ws://{addr}/ws")));
        assert_eq!(dial("wss://hub.phantomptr.com/ws"), None, "another hub");
        assert_eq!(view()["port"], addr.port());

        // The engine hosts a room on the public address: it gets there over loopback, as the
        // address play.example.test does not exist.
        crate::join::set_test_hub(&public_hub, true);
        let room = crate::join_net::hub::Room::start(
            crate::join_net::Kind::Join,
            Box::new(|| true),
            public_hub.clone(),
        )
        .unwrap();
        let until = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let v = room.json("K", None, None);
            if v["state"] == "hosted" {
                // The link names the public hub, never the loopback one.
                assert_eq!(v["hub"], "play.example.test", "{v}");
                assert!(
                    v["url"]
                        .as_str()
                        .unwrap()
                        .contains("&hub=play.example.test"),
                    "{v}"
                );
                break;
            }
            assert!(tokio::time::Instant::now() < until, "never hosted: {v}");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // A guest (no Origin, as a test client) joins the room, offers and gets the answer.
        let guest =
            crate::join_net::tests::connect_guest(&format!("ws://{addr}/ws"), &room.id()).await;
        assert!(guest.answer.starts_with("v=0"), "{}", guest.answer);

        // A browser on the public page may open /ws; one on another site may not.
        assert_eq!(
            ws_with_origin(addr, "https://play.example.test").await,
            Ok(())
        );
        assert_eq!(ws_with_origin(addr, "https://evil.example").await, Err(403));

        // The pages: gzip when asked for, the same bytes as the engine's own, ETag and 304.
        for (path, raw) in [
            ("/join/", crate::join::PAGE_HTML),
            ("/pad/", crate::pad_link::PAGE_HTML),
            ("/share/", SHARE_HTML),
        ] {
            let (head, body) = http(addr, "GET", path, "accept-encoding: gzip, br\r\n").await;
            assert!(head.starts_with("http/1.1 200"), "{path}: {head}");
            assert!(
                head.contains("content-type: text/html; charset=utf-8"),
                "{head}"
            );
            assert!(head.contains("content-encoding: gzip"), "{head}");
            assert!(head.contains("referrer-policy: no-referrer"), "{head}");
            assert!(head.contains("x-content-type-options: nosniff"), "{head}");
            assert!(head.contains("cache-control: no-cache"), "{head}");
            assert!(
                body.len() * 2 < raw.len(),
                "{path} compressed: {}",
                body.len()
            );
            let mut plain = String::new();
            std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&body[..]), &mut plain)
                .unwrap();
            assert_eq!(plain, raw, "{path}");
            let etag = head
                .lines()
                .find_map(|l| l.strip_prefix("etag: "))
                .unwrap()
                .to_string();
            let (head, body) = http(
                addr,
                "GET",
                path,
                &format!("accept-encoding: gzip\r\nif-none-match: {etag}\r\n"),
            )
            .await;
            assert!(head.starts_with("http/1.1 304"), "{head}");
            assert!(body.is_empty());
            let (head, body) = http(addr, "GET", path, "").await;
            assert!(head.starts_with("http/1.1 200") && !head.contains("content-encoding"));
            assert_eq!(body, raw.as_bytes());
        }
        let (head, _) = http(addr, "GET", "/join", "").await;
        assert!(
            head.starts_with("http/1.1 308") && head.contains("location: /join/"),
            "{head}"
        );
        let (head, body) = http(addr, "GET", "/health", "").await;
        assert!(head.starts_with("http/1.1 200"));
        assert_eq!(body, b"ok\n");

        // Nothing of the engine API, and no metrics.
        for path in [
            "/api/join",
            "/api/join/settings",
            "/api/events",
            "/api/version",
            "/pkg-host/x",
            "/metrics",
            "/",
            "/join/x",
        ] {
            let (head, _) = http(addr, "GET", path, "").await;
            assert!(head.starts_with("http/1.1 404"), "{path}: {head}");
        }
        let (head, _) = http(addr, "POST", "/api/join/settings", "content-length: 0\r\n").await;
        assert!(head.starts_with("http/1.1 404"), "{head}");
        drop(guest);
        drop(room);
        *RUNNING.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
