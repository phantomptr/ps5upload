//! HTTP authentication for WebDAV: Basic, or Digest (RFC 7616) with MD5 or SHA-256, for servers
//! that take nothing else. `qop=auth` and the older form with no qop; `-sess` algorithms and
//! `auth-int` aren't offered by anything this has met and are treated as no Digest at all.

use std::sync::Mutex;

use base64::Engine as _;
use md5::Md5;
use sha2::{Digest as _, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Algorithm {
    Md5,
    Sha256,
}

impl Algorithm {
    fn hash(self, data: &str) -> String {
        let bytes: Vec<u8> = match self {
            Self::Md5 => Md5::digest(data).to_vec(),
            Self::Sha256 => Sha256::digest(data).to_vec(),
        };
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
    fn name(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Sha256 => "SHA-256",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Challenge {
    realm: String,
    nonce: String,
    opaque: Option<String>,
    qop: bool,
    algorithm: Algorithm,
    stale: bool,
}

/// `key=value` pairs of a challenge, quoted or not.
fn pairs(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq]
            .trim()
            .trim_start_matches(',')
            .trim()
            .to_ascii_lowercase();
        rest = rest[eq + 1..].trim_start();
        let value;
        if let Some(quoted) = rest.strip_prefix('"') {
            let mut v = String::new();
            let mut chars = quoted.char_indices();
            let mut end = quoted.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, next)) = chars.next() {
                            v.push(next);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    c => v.push(c),
                }
            }
            value = v;
            rest = quoted[end..].trim_start();
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            value = rest[..end].trim().to_string();
            rest = &rest[end..];
        }
        rest = rest.trim_start_matches(',').trim_start();
        out.push((key, value));
    }
    out
}

fn parse_challenge(header: &str) -> Option<Challenge> {
    let (scheme, params) = header.trim().split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("digest") {
        return None;
    }
    let kv = pairs(params);
    let get = |k: &str| kv.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    let algorithm = match get("algorithm").map(str::to_ascii_uppercase).as_deref() {
        None | Some("MD5") => Algorithm::Md5,
        Some("SHA-256") => Algorithm::Sha256,
        Some(_) => return None,
    };
    let qop = match get("qop") {
        None => false,
        Some(list)
            if list
                .split(',')
                .any(|q| q.trim().eq_ignore_ascii_case("auth")) =>
        {
            true
        }
        Some(_) => return None,
    };
    Some(Challenge {
        realm: get("realm")?.to_string(),
        nonce: get("nonce")?.to_string(),
        opaque: get("opaque").map(str::to_string),
        qop,
        algorithm,
        stale: get("stale").is_some_and(|s| s.eq_ignore_ascii_case("true")),
    })
}

/// The strongest Digest challenge among a response's `WWW-Authenticate` values.
fn best_challenge<'a>(headers: impl Iterator<Item = &'a str>) -> Option<Challenge> {
    let mut found: Vec<Challenge> = headers.filter_map(parse_challenge).collect();
    found.sort_by_key(|c| c.algorithm != Algorithm::Sha256);
    found.into_iter().next()
}

fn quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn response_value(
    c: &Challenge,
    user: &str,
    password: &str,
    method: &str,
    uri: &str,
    nc: u32,
    cnonce: &str,
) -> String {
    let h = |s: &str| c.algorithm.hash(s);
    let ha1 = h(&format!("{user}:{}:{password}", c.realm));
    let ha2 = h(&format!("{method}:{uri}"));
    if c.qop {
        h(&format!("{ha1}:{}:{nc:08x}:{cnonce}:auth:{ha2}", c.nonce))
    } else {
        h(&format!("{ha1}:{}:{ha2}", c.nonce))
    }
}

pub(super) struct DigestState {
    challenge: Challenge,
    nc: u32,
}

/// How a connection signs its requests.
pub(super) enum Auth {
    None,
    Basic(String),
    Digest {
        user: String,
        password: String,
        state: Mutex<DigestState>,
    },
}

impl Auth {
    pub(super) fn basic(user: &str, password: &str) -> Self {
        Self::Basic(format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        ))
    }

    /// This connection's Digest form, when the server offers Digest in `challenges`. Preferred
    /// to Basic, which sends the password as it is over plain HTTP.
    pub(super) fn digest_from<'a>(
        user: &str,
        password: &str,
        challenges: impl Iterator<Item = &'a str>,
    ) -> Option<Self> {
        Some(Self::Digest {
            user: user.to_string(),
            password: password.to_string(),
            state: Mutex::new(DigestState {
                challenge: best_challenge(challenges)?,
                nc: 0,
            }),
        })
    }

    /// The `Authorization` value for `method` on `url`.
    pub(super) fn header(&self, method: &str, url: &str) -> Option<String> {
        match self {
            Self::None => None,
            Self::Basic(value) => Some(value.clone()),
            Self::Digest {
                user,
                password,
                state,
            } => {
                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                s.nc += 1;
                let nc = s.nc;
                let c = &s.challenge;
                let uri = request_target(url);
                let cnonce = uuid::Uuid::new_v4().simple().to_string();
                let response = response_value(c, user, password, method, uri, nc, &cnonce);
                let mut out = format!(
                    "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", \
                     algorithm={}, response=\"{response}\"",
                    quote(user),
                    quote(&c.realm),
                    quote(&c.nonce),
                    quote(uri),
                    c.algorithm.name(),
                );
                if c.qop {
                    out.push_str(&format!(", qop=auth, nc={nc:08x}, cnonce=\"{cnonce}\""));
                }
                if let Some(opaque) = &c.opaque {
                    out.push_str(&format!(", opaque=\"{}\"", quote(opaque)));
                }
                Some(out)
            }
        }
    }

    /// After a 401: take the new nonce it offers. True when the request is worth sending again:
    /// the server says the old one is stale, or simply offers another, as servers that never say
    /// `stale` do. A wrong password with the same nonce isn't; one with a new nonce each time costs
    /// one more request, and fails then.
    pub(super) fn renew<'a>(&self, challenges: impl Iterator<Item = &'a str>) -> bool {
        let Self::Digest { state, .. } = self else {
            return false;
        };
        let Some(fresh) = best_challenge(challenges) else {
            return false;
        };
        let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
        if !fresh.stale && fresh.nonce == s.challenge.nonce {
            return false;
        }
        s.challenge = fresh;
        s.nc = 0;
        true
    }
}

/// The path and query of a URL, what Digest signs as its `uri`.
fn request_target(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    after_scheme
        .find('/')
        .map_or("/", |slash| &after_scheme[slash..])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 2617 section 3.5's worked example for MD5, and the same request by SHA-256 as Python's
    /// hashlib works it out.
    #[test]
    fn matches_the_worked_examples() {
        for (algorithm, want) in [
            ("MD5", "6629fae49393a05397450978507c4ef1"),
            (
                "SHA-256",
                "5abdd07184ba512a22c53f41470e5eea7dcaa3a93a59b630c13dfe0a5dc6e38b",
            ),
        ] {
            let c = parse_challenge(&format!(
                "Digest realm=\"testrealm@host.com\", qop=\"auth,auth-int\", \
                 algorithm={algorithm}, nonce=\"dcd98b7102dd2f0e8b11d0f600bfb0c093\", \
                 opaque=\"5ccc069c403ebaf9f0171e9517f40e41\""
            ))
            .unwrap();
            assert_eq!(c.realm, "testrealm@host.com");
            assert!(c.qop);
            let got = response_value(
                &c,
                "Mufasa",
                "Circle Of Life",
                "GET",
                "/dir/index.html",
                1,
                "0a4f113b",
            );
            assert_eq!(got, want, "{algorithm}");
        }
    }

    #[test]
    fn only_what_it_can_answer_is_taken() {
        assert!(parse_challenge("Basic realm=\"x\"").is_none());
        assert!(parse_challenge("Digest realm=\"x\", nonce=\"n\", algorithm=MD5-sess").is_none());
        assert!(parse_challenge("Digest realm=\"x\", nonce=\"n\", qop=\"auth-int\"").is_none());
        let old = parse_challenge("Digest realm=\"a\\\"b\", nonce=n").unwrap();
        assert_eq!(old.realm, "a\"b");
        assert!(!old.qop);
        let both = best_challenge(
            [
                "Basic realm=\"x\"",
                "Digest realm=\"x\", nonce=\"m\"",
                "Digest realm=\"x\", nonce=\"s\", algorithm=SHA-256",
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(both.algorithm, Algorithm::Sha256);
    }

    #[test]
    fn a_header_signs_the_path_and_counts_up() {
        let a = Auth::digest_from(
            "u",
            "p",
            ["Digest realm=\"r\", nonce=\"n\", qop=\"auth\""].into_iter(),
        )
        .unwrap();
        let first = a
            .header("PROPFIND", "https://nas:5006/dav/a%20b?x=1")
            .unwrap();
        assert!(first.contains("uri=\"/dav/a%20b?x=1\""), "{first}");
        assert!(first.contains("nc=00000001"), "{first}");
        assert!(a
            .header("GET", "http://nas")
            .unwrap()
            .contains("nc=00000002"));
        assert!(a.header("GET", "http://nas").unwrap().contains("uri=\"/\""));
        assert!(
            !a.renew(["Digest realm=\"r\", nonce=\"n\""].into_iter()),
            "a wrong password"
        );
        assert!(a.renew(["Digest realm=\"r\", nonce=\"n\", stale=true"].into_iter()));
        assert!(
            a.renew(["Digest realm=\"r\", nonce=\"m\""].into_iter()),
            "a new nonce"
        );
        let next = a.header("GET", "http://nas/").unwrap();
        assert!(
            next.contains("nonce=\"m\"") && next.contains("response="),
            "{next}"
        );
    }
}
