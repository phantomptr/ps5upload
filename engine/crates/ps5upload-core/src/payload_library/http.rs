//! The HTTP side of the library: one GET seam, and the redirect rule.
//!
//! Redirects are followed here, not by the client, so every hop is checked. Only the first URL
//! used to be pinned: a user-added (or compromised) Gitea host could answer 302 to any LAN or
//! loopback `http://` address, and the bytes that came back were cached as the payload.

use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;

use super::{LibraryError, ResolvedEntry};

/// Release metadata: small JSON, so a whole-request deadline fits.
const METADATA_TIMEOUT: Duration = Duration::from_secs(15);
/// A connection that sends nothing for this long is dead. An asset has no whole-request
/// deadline: 120 s made anything over ~50 MB on a 2-4 Mbit/s link fail every time.
const ASSET_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const ASSET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// GitHub answers a release asset with a redirect to one of these.
const GITHUB_ASSET_HOSTS: &[&str] = &[
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
];

const MAX_REDIRECTS: usize = 5;

/// GitHub requires a user agent, and uses it to spot abuse: the version pinpoints a release.
const HTTP_USER_AGENT: &str = concat!("ps5upload/", env!("CARGO_PKG_VERSION"), " (payloads)");

/// Which timeouts a request runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchKind {
    Metadata,
    Asset,
}

/// One answer, redirects not followed.
pub struct HttpResponse {
    pub status: u16,
    pub location: Option<String>,
    pub content_length: Option<u64>,
    pub body: Box<dyn Read>,
}

/// One GET, redirects not followed. A test scripts this; production is [`UreqClient`].
pub trait HttpClient: Send + Sync {
    fn get(&self, url: &str, accept: Option<&str>, kind: FetchKind)
        -> Result<HttpResponse, String>;
}

/// The real client. ureq reads `HTTPS_PROXY` and friends from the environment, as before.
pub struct UreqClient {
    metadata: ureq::Agent,
    asset: ureq::Agent,
}

impl UreqClient {
    pub fn new() -> Self {
        let metadata = ureq::Agent::config_builder()
            .user_agent(HTTP_USER_AGENT)
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(METADATA_TIMEOUT))
            .build();
        let asset = ureq::Agent::config_builder()
            .user_agent(HTTP_USER_AGENT)
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(ASSET_CONNECT_TIMEOUT))
            .timeout_recv_response(Some(ASSET_IDLE_TIMEOUT))
            .timeout_per_read(Some(ASSET_IDLE_TIMEOUT))
            .build();
        Self {
            metadata: ureq::Agent::new_with_config(metadata),
            asset: ureq::Agent::new_with_config(asset),
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
            FetchKind::Asset => &self.asset,
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
        Ok(HttpResponse {
            status: resp.status().as_u16(),
            location,
            content_length,
            body: Box::new(resp.into_body().into_reader()),
        })
    }
}

/// The scheme and host of an absolute URL.
fn scheme_and_host(url: &str) -> Option<(String, String, Option<u16>)> {
    let uri: ureq::http::Uri = url.parse().ok()?;
    Some((
        uri.scheme_str()?.to_ascii_lowercase(),
        uri.host()?.to_ascii_lowercase(),
        uri.port_u16(),
    ))
}

/// Names that only mean something on a local network.
fn is_local_name(host: &str) -> bool {
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

/// May a request for this entry land on `url`?
///
/// Always https on the default port, and never an address or a LAN name. Beyond that: the
/// entry's host and its subdomains, GitHub's asset CDNs for a GitHub repo, and for a user's own
/// Gitea or Forgejo server any public name, because such a server may hand files out from object
/// storage on another host. The user chose to trust that server, and the certificate still has
/// to match the name.
pub(crate) fn hop_allowed(entry: &ResolvedEntry, url: &str) -> bool {
    let Some((scheme, host, port)) = scheme_and_host(url) else {
        return false;
    };
    if scheme != "https" || !matches!(port, None | Some(443)) || is_local_name(&host) {
        return false;
    }
    let entry_host = entry.repo_host.as_str();
    if host == entry_host || host.ends_with(&format!(".{entry_host}")) {
        return true;
    }
    if entry_host == "github.com" {
        return GITHUB_ASSET_HOSTS.contains(&host.as_str());
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
            checksum_file: String::new(),
            is_custom,
        }
    }

    #[test]
    fn a_github_repo_may_reach_github_and_its_asset_cdns_only() {
        let e = entry("github.com", false);
        for ok in [
            "https://github.com/o/n/releases/download/v1/a.elf",
            "https://api.github.com/repos/o/n/releases",
            "https://release-assets.githubusercontent.com/x?sig=1",
            "https://objects.githubusercontent.com/x",
        ] {
            assert!(hop_allowed(&e, ok), "{ok}");
        }
        for bad in [
            "http://github.com/o/n",
            "https://evil.example/x",
            "https://github.com.evil.example/x",
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
        let start = "https://github.com/o/n/releases/download/v1/a.elf";
        let cdn = "https://release-assets.githubusercontent.com/a";
        http.redirect(start, cdn);
        http.ok(cdn, b"\x7FELF");
        let e = entry("github.com", false);
        let resp = fetch(&http, &e, start, None, FetchKind::Asset).unwrap();
        assert_eq!(resp.status, 200);

        // A later hop to loopback is refused before any request goes there.
        http.redirect(cdn, "http://127.0.0.1:19113/api/jobs");
        let err = fetch(&http, &e, start, None, FetchKind::Asset)
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
            FetchKind::Asset,
        )
        .err()
        .unwrap();
        assert_eq!(err.code, "fetch_failed");
        assert!(err.message.contains("redirects"), "{}", err.message);
    }
}
