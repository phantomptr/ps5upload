//! The HTTP side of the library: one GET seam, and the redirect rule.
//!
//! Redirects are followed here, not by the client, so every hop is checked. Only the first URL
//! used to be pinned: a user-added (or compromised) Gitea host could answer 302 to any LAN or
//! loopback `http://` address, and the bytes that came back were cached as the payload.

use std::io::Read;
use std::net::IpAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{LibraryError, ResolvedEntry};

/// Release metadata: small JSON, so a whole-request deadline fits.
const METADATA_TIMEOUT: Duration = Duration::from_secs(15);

/// A payload file from a source the user added: no whole-request deadline (a slow line still
/// finishes), but a stalled one ends.
const DOWNLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Where GitHub serves release files from: `github.com/.../releases/download/...` answers 302 to
/// one of these.
const GITHUB_ASSET_HOSTS: &[&str] = &[
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
];

const MAX_REDIRECTS: usize = 5;

/// GitHub requires a user agent, and uses it to spot abuse: the version pinpoints a release.
const HTTP_USER_AGENT: &str = concat!("ps5upload/", env!("CARGO_PKG_VERSION"), " (payloads)");

/// Which timeouts a request runs under: release metadata, or a payload file from a source the
/// user added (the only payload files the app downloads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchKind {
    Metadata,
    Download,
}

/// One answer, redirects not followed.
pub struct HttpResponse {
    pub status: u16,
    pub location: Option<String>,
    pub content_length: Option<u64>,
    /// When the host said it is limiting requests (`Retry-After`, or GitHub's
    /// `x-ratelimit-remaining: 0` with `x-ratelimit-reset`): when to ask again, seconds since the
    /// Unix epoch.
    pub retry_at: Option<u64>,
    pub body: Box<dyn Read>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// When to ask again, from a response's headers: `Retry-After` (seconds), else GitHub's reset time
/// once its count is spent. `None` when the answer says nothing about a limit.
pub(crate) fn retry_at_from_headers(
    retry_after: Option<&str>,
    remaining: Option<&str>,
    reset: Option<&str>,
) -> Option<u64> {
    if let Some(secs) = retry_after.and_then(|v| v.trim().parse::<u64>().ok()) {
        return Some(now_secs().saturating_add(secs.min(24 * 3600)));
    }
    if remaining.map(str::trim) == Some("0") {
        return reset.and_then(|v| v.trim().parse::<u64>().ok());
    }
    None
}

/// One GET, redirects not followed. A test scripts this; production is [`UreqClient`].
pub trait HttpClient: Send + Sync {
    fn get(&self, url: &str, accept: Option<&str>, kind: FetchKind)
        -> Result<HttpResponse, String>;
}

/// The real client. ureq reads `HTTPS_PROXY` and friends from the environment, as before.
pub struct UreqClient {
    metadata: ureq::Agent,
    download: ureq::Agent,
}

impl UreqClient {
    pub fn new() -> Self {
        let metadata = ureq::Agent::config_builder()
            .user_agent(HTTP_USER_AGENT)
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(METADATA_TIMEOUT))
            .build();
        let download = ureq::Agent::config_builder()
            .user_agent(HTTP_USER_AGENT)
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(DOWNLOAD_CONNECT_TIMEOUT))
            .timeout_recv_response(Some(DOWNLOAD_IDLE_TIMEOUT))
            .timeout_per_read(Some(DOWNLOAD_IDLE_TIMEOUT))
            .build();
        Self {
            metadata: ureq::Agent::new_with_config(metadata),
            download: ureq::Agent::new_with_config(download),
        }
    }
}

impl Default for UreqClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient for UreqClient {
    fn get(
        &self,
        url: &str,
        accept: Option<&str>,
        kind: FetchKind,
    ) -> Result<HttpResponse, String> {
        let agent = match kind {
            FetchKind::Metadata => &self.metadata,
            FetchKind::Download => &self.download,
        };
        let mut req = agent.get(url);
        if let Some(a) = accept {
            req = req.header("Accept", a);
        }
        let resp = req.call().map_err(|e| e.to_string())?;
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let location = header("location");
        let content_length = header("content-length").and_then(|v| v.parse().ok());
        let retry_at = retry_at_from_headers(
            header("retry-after").as_deref(),
            header("x-ratelimit-remaining").as_deref(),
            header("x-ratelimit-reset").as_deref(),
        );
        Ok(HttpResponse {
            status: resp.status().as_u16(),
            location,
            content_length,
            retry_at,
            body: Box::new(resp.into_body().into_reader()),
        })
    }
}

/// The scheme, host and port of an absolute URL. `None` for a URL with credentials in it.
fn scheme_and_host(url: &str) -> Option<(String, String, Option<u16>)> {
    let uri: ureq::http::Uri = url.parse().ok()?;
    if uri.authority()?.as_str().contains('@') {
        return None;
    }
    Some((
        uri.scheme_str()?.to_ascii_lowercase(),
        uri.host()?.to_ascii_lowercase(),
        uri.port_u16(),
    ))
}

/// Names that only mean something on a local network.
pub(crate) fn is_local_name(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.parse::<IpAddr>().is_ok()
        || !bare.contains('.')
        || [
            ".local",
            ".lan",
            ".home",
            ".internal",
            ".arpa",
            ".localhost",
        ]
        .iter()
        .any(|s| bare.ends_with(s))
}

/// Where an address points, for the rule of which hosts a source may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostClass {
    /// A public name or address.
    Public,
    /// A home network: a private address (10/8, 172.16/12, 192.168/16, 100.64/10, fc00::/7) or a
    /// LAN name (`nas.local`, `gitea.lan`, a name without a dot).
    Private,
    /// Never: this computer itself (loopback, `localhost`), link-local (169.254/16, fe80::/10,
    /// where cloud metadata answers), unspecified, multicast, broadcast, documentation ranges.
    Forbidden,
}

fn ipv4_class(v4: std::net::Ipv4Addr) -> HostClass {
    let o = v4.octets();
    if v4.is_loopback()
        || v4.is_unspecified()
        || v4.is_link_local()
        || v4.is_multicast()
        || v4.is_broadcast()
        || v4.is_documentation()
        || o[0] == 0
    {
        HostClass::Forbidden
    } else if v4.is_private() || (o[0] == 100 && (o[1] & 0xc0) == 64) {
        HostClass::Private
    } else {
        HostClass::Public
    }
}

/// The class of a host as a URL names it (an IPv6 address in brackets or not).
pub(crate) fn host_class(host: &str) -> HostClass {
    let bare = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if bare.is_empty() {
        return HostClass::Forbidden;
    }
    match bare.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => return ipv4_class(v4),
        Ok(IpAddr::V6(v6)) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return ipv4_class(v4);
            }
            let first = v6.segments()[0];
            return if v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xffc0) == 0xfe80
            {
                HostClass::Forbidden
            } else if (first & 0xfe00) == 0xfc00 {
                HostClass::Private
            } else {
                HostClass::Public
            };
        }
        Err(_) => {}
    }
    if bare == "localhost" || bare.ends_with(".localhost") {
        HostClass::Forbidden
    } else if is_local_name(&bare) {
        HostClass::Private
    } else {
        HostClass::Public
    }
}

/// May a request for this entry land on `url`?
///
/// Always https, and never this computer or a link-local address. A user's own Gitea or Forgejo
/// server may be on the home network and on any port: its own address and port are allowed as
/// added. Beyond that, the default port and public names only: the entry's host and its
/// subdomains; for a source the user added on GitHub, GitHub's release-file hosts too; and for a
/// user's own Gitea or Forgejo server any public name, because such a server may answer from
/// object storage. The user chose to trust that server, and the certificate still has to match
/// the name. A GitHub repo stays on GitHub.
pub(crate) fn hop_allowed(entry: &ResolvedEntry, url: &str) -> bool {
    let Some((scheme, host, port)) = scheme_and_host(url) else {
        return false;
    };
    if scheme != "https" || host_class(&host) == HostClass::Forbidden {
        return false;
    }
    let entry_host = entry.repo_host.as_str();
    let is_github = entry_host == "github.com";
    if entry.is_custom
        && !is_github
        && host == entry_host
        && port.unwrap_or(443) == entry.repo_port.unwrap_or(443)
    {
        return true;
    }
    if !matches!(port, None | Some(443)) || is_local_name(&host) {
        return false;
    }
    if host == entry_host || host.ends_with(&format!(".{entry_host}")) {
        return true;
    }
    if is_github {
        return entry.is_custom && GITHUB_ASSET_HOSTS.contains(&host.as_str());
    }
    entry.is_custom
}

/// Where a redirect points, as an absolute URL. `None` for a form we do not follow.
fn resolve_location(current: &str, location: &str) -> Option<String> {
    let location = location.trim();
    if location.starts_with("https://") || location.starts_with("http://") {
        return Some(location.to_string());
    }
    let (scheme, rest) = current.split_once("://")?;
    let origin = rest.split(['/', '?', '#']).next()?;
    if let Some(net_path) = location.strip_prefix("//") {
        return Some(format!("{scheme}://{net_path}"));
    }
    location
        .starts_with('/')
        .then(|| format!("{scheme}://{origin}{location}"))
}

/// GET `url`, following redirects that [`hop_allowed`] accepts.
pub(crate) fn fetch(
    client: &dyn HttpClient,
    entry: &ResolvedEntry,
    url: &str,
    accept: Option<&str>,
    kind: FetchKind,
) -> Result<HttpResponse, LibraryError> {
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        if !hop_allowed(entry, &current) {
            return Err(LibraryError::new(
                "redirect_refused",
                format!(
                    "{} sent the download to {current}, which is not allowed for this repo",
                    entry.repo_host
                ),
            ));
        }
        let resp = client
            .get(&current, accept, kind)
            .map_err(|e| LibraryError::new("fetch_failed", format!("fetch {current}: {e}")))?;
        if !(300..400).contains(&resp.status) {
            return Ok(resp);
        }
        let Some(next) = resp
            .location
            .as_deref()
            .and_then(|l| resolve_location(&current, l))
        else {
            return Err(LibraryError::new(
                "fetch_failed",
                format!(
                    "fetch {current}: HTTP {} with no usable Location",
                    resp.status
                ),
            ));
        };
        current = next;
    }
    Err(LibraryError::new(
        "fetch_failed",
        format!("fetch {url}: more than {MAX_REDIRECTS} redirects"),
    ))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::Scripted;
    use super::*;

    fn entry(host: &str, is_custom: bool) -> ResolvedEntry {
        ResolvedEntry {
            id: "x".into(),
            repo_host: host.into(),
            repo_owner: "o".into(),
            repo_name: "n".into(),
            asset_name_hint: String::new(),
            is_custom,
            list_payload: None,
            repo_port: None,
        }
    }

    #[test]
    fn a_github_repo_may_reach_github_only() {
        let e = entry("github.com", false);
        for ok in [
            "https://github.com/o/n/releases/latest",
            "https://api.github.com/repos/o/n/releases",
        ] {
            assert!(hop_allowed(&e, ok), "{ok}");
        }
        for bad in [
            "http://github.com/o/n",
            "https://evil.example/x",
            "https://github.com.evil.example/x",
            "https://release-assets.githubusercontent.com/x?sig=1",
            "https://objects.githubusercontent.com/x",
            "https://notgithub.com/x",
            "https://user@127.0.0.1:19113/api/jobs",
            "http://127.0.0.1:19113/api/jobs",
            "https://192.168.1.10/x",
            "https://[::1]/x",
            "https://github.com:8443/x",
            "ftp://github.com/x",
            "not a url",
        ] {
            assert!(!hop_allowed(&e, bad), "{bad}");
        }
    }

    #[test]
    fn a_catalogue_gitea_host_stays_on_its_own_name() {
        let e = entry("git.etawen.dev", false);
        assert!(hop_allowed(&e, "https://git.etawen.dev/a/b.elf"));
        assert!(hop_allowed(&e, "https://cdn.git.etawen.dev/a/b.elf"));
        assert!(!hop_allowed(&e, "https://s3.amazonaws.com/a/b.elf"));
    }

    #[test]
    fn a_users_own_server_may_use_public_object_storage_but_never_the_lan() {
        let e = entry("forge.example.org", true);
        assert!(hop_allowed(&e, "https://s3.example.net/bucket/a.elf"));
        for bad in [
            "http://s3.example.net/a.elf",
            "https://10.0.0.5/a.elf",
            "https://localhost/a.elf",
            "https://nas.local/a.elf",
            "https://router.lan/a.elf",
            "https://ps5/a.elf",
        ] {
            assert!(!hop_allowed(&e, bad), "{bad}");
        }
    }

    #[test]
    fn redirect_forms() {
        let cur = "https://h.example/a/b?q=1";
        assert_eq!(
            resolve_location(cur, "/c/d").as_deref(),
            Some("https://h.example/c/d")
        );
        assert_eq!(
            resolve_location(cur, "//other.example/x").as_deref(),
            Some("https://other.example/x")
        );
        assert_eq!(
            resolve_location(cur, "https://z.example/").as_deref(),
            Some("https://z.example/")
        );
        assert_eq!(resolve_location(cur, "relative/path"), None);
    }

    #[test]
    fn every_hop_is_checked_not_only_the_first() {
        let http = Scripted::default();
        let start = "https://github.com/o/n/releases/latest";
        let api = "https://api.github.com/repos/o/n/releases/latest";
        http.redirect(start, api);
        http.ok(api, b"{}");
        let e = entry("github.com", false);
        let resp = fetch(&http, &e, start, None, FetchKind::Metadata).unwrap();
        assert_eq!(resp.status, 200);

        // A later hop to loopback is refused before any request goes there.
        http.redirect(api, "http://127.0.0.1:19113/api/jobs");
        let err = fetch(&http, &e, start, None, FetchKind::Metadata)
            .err()
            .unwrap();
        assert_eq!(err.code, "redirect_refused");
        assert!(!http
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|u| u.contains("127.0.0.1")));
    }

    #[test]
    fn a_redirect_loop_ends() {
        let http = Scripted::default();
        let a = "https://github.com/a";
        let b = "https://github.com/b";
        http.redirect(a, b);
        http.redirect(b, a);
        let err = fetch(
            &http,
            &entry("github.com", false),
            a,
            None,
            FetchKind::Metadata,
        )
        .err()
        .unwrap();
        assert_eq!(err.code, "fetch_failed");
        assert!(err.message.contains("redirects"), "{}", err.message);
    }
}
