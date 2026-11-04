//! Go live's destinations (#635, rp_golive.rs): the streaming sites, their limits, and the
//! user's saved stream keys.
//!
//! Presets: YouTube, Twitch, Kick, Facebook, and Custom (any rtmp://, rtmps:// or srt:// URL).
//! Each preset's limits (codec, bitrate, resolution, frame rate) are checked against the Remote
//! Play session, since Go live sends the console's video on as it is: a codec a site does not
//! take stops the start (`codec_not_allowed`); a bitrate, resolution or frame rate above what
//! it recommends is a warning.
//!
//! Stream keys are secrets: kept in `<data>/stream-destinations.json`, each encrypted (AES-256-
//! GCM) with a per-install key in `<data>/stream-destinations.key` (readable by this user only),
//! the same way saved connections keep their passwords (remote/store.rs). The API never returns
//! a key: a destination shows its last four characters. Nothing logs one; ffmpeg's output is
//! scrubbed of them (rp_golive.rs `redact`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::rp_stream_api::live_feed::SessionInfo;

/// A streaming site and what it takes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub(crate) struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    /// The server, when it is the same for everyone (key appended after a '/').
    pub server: Option<&'static str>,
    /// The user enters the server (Kick shows each account its own).
    pub needs_server: bool,
    /// Codecs taken: "h264", "h265".
    pub codecs: &'static [&'static str],
    /// Above this the site drops, throttles or warns.
    pub max_kbps: u32,
    pub max_height: u32,
    pub max_fps: u32,
    /// Where the stream key is (a page of the site).
    pub key_page: &'static str,
    /// Twitch: `?bandwidthtest=true` streams without going public.
    pub bandwidth_test: bool,
}

pub(crate) const PRESETS: &[Preset] = &[
    Preset {
        id: "youtube",
        name: "YouTube",
        server: Some("rtmps://a.rtmps.youtube.com/live2"),
        needs_server: false,
        // H.264, and HEVC over enhanced RTMP.
        codecs: &["h264", "h265"],
        // YouTube takes up to 51 Mbps (its 4K ceiling); Remote Play's 15 Mbps at 1080p is fine.
        max_kbps: 51_000,
        max_height: 2160,
        max_fps: 60,
        key_page: "https://www.youtube.com/live_dashboard",
        bandwidth_test: false,
    },
    Preset {
        id: "twitch",
        name: "Twitch",
        // The automatic ingest (verified against ingest.twitch.tv/ingests, 2026-11).
        server: Some("rtmps://ingest.global-contribute.live-video.net/app"),
        needs_server: false,
        codecs: &["h264"],
        max_kbps: 6_000,
        max_height: 1080,
        max_fps: 60,
        key_page: "https://dashboard.twitch.tv/settings/stream",
        bandwidth_test: true,
    },
    Preset {
        id: "kick",
        name: "Kick",
        server: None,
        needs_server: true,
        codecs: &["h264"],
        max_kbps: 8_000,
        max_height: 1080,
        max_fps: 60,
        key_page: "https://kick.com/dashboard/settings/stream",
        bandwidth_test: false,
    },
    Preset {
        id: "facebook",
        name: "Facebook",
        server: Some("rtmps://live-api-s.facebook.com:443/rtmp"),
        needs_server: false,
        codecs: &["h264"],
        max_kbps: 9_000,
        max_height: 1080,
        max_fps: 60,
        key_page: "https://www.facebook.com/live/producer",
        bandwidth_test: false,
    },
    Preset {
        id: "custom",
        name: "Custom",
        server: None,
        needs_server: true,
        codecs: &["h264", "h265"],
        max_kbps: 0,
        max_height: 0,
        max_fps: 0,
        key_page: "",
        bandwidth_test: false,
    },
];

pub(crate) fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// A limit check against the session: `blocking` ones stop Go live from starting.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Check {
    /// "codec", "hdr", "bitrate", "height", "fps".
    pub code: &'static str,
    pub blocking: bool,
    /// The session's value and the site's limit (kbps, lines, fps; codec names for "codec").
    pub have: serde_json::Value,
    pub limit: serde_json::Value,
    pub message: String,
}

/// What `p` says about streaming `s` to it.
pub(crate) fn check(p: &Preset, s: &SessionInfo) -> Vec<Check> {
    let mut v = Vec::new();
    let base = if s.codec == "h264" { "h264" } else { "h265" };
    if s.codec == "h265_hdr" {
        v.push(Check {
            code: "hdr",
            blocking: true,
            have: s.codec.into(),
            limit: serde_json::json!(p.codecs),
            message: format!(
                "Remote Play is in HDR. {} would show its colours wrong: restart Play in H.264 to go live.",
                p.name
            ),
        });
    } else if !p.codecs.contains(&base) {
        v.push(Check {
            code: "codec",
            blocking: true,
            have: base.into(),
            limit: serde_json::json!(p.codecs),
            message: format!(
                "{} takes H.264 only, and Remote Play is sending HEVC. Restart Play in H.264 to go live there.",
                p.name
            ),
        });
    }
    if p.max_kbps > 0 && s.bitrate_kbps > p.max_kbps {
        v.push(Check {
            code: "bitrate",
            blocking: false,
            have: s.bitrate_kbps.into(),
            limit: p.max_kbps.into(),
            message: format!(
                "Your Play bitrate {} is above {}'s {}. Lower it in Play settings (Picture) before going live.",
                mbps(s.bitrate_kbps),
                p.name,
                mbps(p.max_kbps)
            ),
        });
    }
    if p.max_height > 0 && s.height > p.max_height {
        v.push(Check {
            code: "height",
            blocking: false,
            have: s.height.into(),
            limit: p.max_height.into(),
            message: format!("{} takes at most {}p.", p.name, p.max_height),
        });
    }
    if p.max_fps > 0 && s.fps > p.max_fps {
        v.push(Check {
            code: "fps",
            blocking: false,
            have: s.fps.into(),
            limit: p.max_fps.into(),
            message: format!("{} takes at most {} fps.", p.name, p.max_fps),
        });
    }
    v
}

fn mbps(kbps: u32) -> String {
    let m = kbps as f64 / 1000.0;
    if (m - m.round()).abs() < 0.05 {
        format!("{} Mbps", m.round())
    } else {
        format!("{m:.1} Mbps")
    }
}

/// A saved destination as the API shows it (never the key).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Destination {
    #[serde(default)]
    pub id: String,
    pub preset: String,
    /// The user's name for it (defaults to the site's).
    #[serde(default)]
    pub label: String,
    /// Kick: the server URL from the dashboard. Custom: scheme://host[:port] only (the URL as
    /// entered is kept with the key, since it often carries one).
    #[serde(default)]
    pub server: String,
    /// Twitch: stream without going public (`?bandwidthtest=true`).
    #[serde(default)]
    pub bandwidth_test: bool,
    /// Included when Go live starts.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// The secret part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub(crate) struct Secret {
    #[serde(default)]
    pub key: String,
    /// Custom: the full URL as entered.
    #[serde(default)]
    pub url: String,
    /// What the file holds when it can't be read (the key file was replaced or damaged). It is
    /// saved back as it is, never re-encrypted empty: with the right key file back, the stream
    /// key comes back too.
    #[serde(skip)]
    pub sealed: Option<String>,
}

impl Secret {
    /// The strings that must never be shown or logged.
    pub(crate) fn sensitive(&self) -> Vec<String> {
        let mut v = Vec::new();
        if !self.key.is_empty() {
            v.push(self.key.clone());
        }
        if !self.url.is_empty() {
            v.push(self.url.clone());
            // Its path and query (where a key sits) on their own too.
            if let Some(rest) = self.url.split_once("://").map(|x| x.1) {
                if let Some((_, path)) = rest.split_once('/') {
                    if path.len() >= 4 {
                        v.push(path.to_string());
                    }
                }
                if let Some((_, q)) = rest.split_once('?') {
                    if q.len() >= 4 {
                        v.push(q.to_string());
                    }
                }
            }
        }
        v
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DestinationView {
    #[serde(flatten)]
    pub dest: Destination,
    /// "…abcd": the key's last four characters.
    pub key_hint: String,
    /// The saved key can't be read any more: the site has to be added again.
    pub key_lost: bool,
    pub name: &'static str,
}

/// What an add or change asks for.
#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct DestinationInput {
    /// Absent on a change: the saved one (the client sends only what changes: `{"enabled":false}`).
    #[serde(default)]
    pub preset: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
    /// Absent on a change: keep the saved key.
    #[serde(default)]
    pub key: Option<String>,
    /// Custom: the URL.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub bandwidth_test: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// A validation error: a stable code and a sentence.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Invalid(pub &'static str, pub String);

/// Characters a stream key or URL never needs and ffmpeg's tee muxer would read as syntax.
fn bad_char(c: char) -> bool {
    c.is_control() || c.is_whitespace() || matches!(c, '|' | '[' | ']' | '\\' | '\'' | '"')
}

pub(crate) fn validate_key(key: &str) -> Result<String, Invalid> {
    let k = key.trim();
    if k.len() < 4 {
        return Err(Invalid(
            "key_missing",
            "Paste the stream key from the site.".into(),
        ));
    }
    if k.len() > 512 || k.chars().any(bad_char) || k.contains("://") {
        return Err(Invalid(
            "key_invalid",
            "That doesn't look like a stream key: paste only the key, without spaces.".into(),
        ));
    }
    Ok(k.to_string())
}

/// A server or custom URL: rtmp://, rtmps:// (and srt:// for Custom), with a host.
pub(crate) fn validate_url(url: &str, allow_srt: bool, need_tls: bool) -> Result<String, Invalid> {
    let u = url.trim().trim_end_matches('/');
    let bad = || {
        Invalid(
            "url_invalid",
            if allow_srt {
                "Enter a server address starting with rtmp://, rtmps:// or srt://.".to_string()
            } else {
                "Enter the server address starting with rtmps://.".to_string()
            },
        )
    };
    let (scheme, rest) = u.split_once("://").ok_or_else(bad)?;
    let scheme = scheme.to_ascii_lowercase();
    let ok = match scheme.as_str() {
        "rtmps" => true,
        "rtmp" => !need_tls,
        "srt" => allow_srt,
        _ => false,
    };
    let host = rest.split(['/', '?']).next().unwrap_or("");
    if !ok || host.is_empty() || u.len() > 1024 || u.chars().any(bad_char) {
        return Err(bad());
    }
    Ok(format!("{scheme}://{rest}"))
}

/// scheme://host[:port] of a URL (what Custom shows), without a `user:password@` in it: that
/// stays in the encrypted secret only.
pub(crate) fn origin_of(url: &str) -> String {
    match url.split_once("://") {
        Some((s, rest)) => {
            let authority = rest.split(['/', '?']).next().unwrap_or("");
            let host = authority.rsplit('@').next().unwrap_or("");
            format!("{s}://{host}")
        }
        None => String::new(),
    }
}

/// `key` as the last part of the server's path: `rtmp://host/app?auth=x` + `s1` is
/// `rtmp://host/app/s1?auth=x`, not a query that ends in the key.
fn join_key(server: &str, key: &str) -> String {
    let (path, query) = match server.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (server, None),
    };
    let mut u = format!("{}/{}", path.trim_end_matches('/'), key);
    if let Some(q) = query {
        // Facebook's keys bring a query of their own.
        u.push(if key.contains('?') { '&' } else { '?' });
        u.push_str(q);
    }
    u
}

/// The URL ffmpeg publishes to, and its muxer ("flv" or "mpegts").
pub(crate) fn publish_url(d: &Destination, s: &Secret) -> Option<(String, &'static str)> {
    let p = preset(&d.preset)?;
    if s.sealed.is_some() {
        return None;
    }
    let url = match p.id {
        "custom" => {
            let base = if s.url.is_empty() { &d.server } else { &s.url };
            if base.to_ascii_lowercase().starts_with("srt://") {
                return Some((
                    if s.key.is_empty() {
                        base.clone()
                    } else {
                        let sep = if base.contains('?') { '&' } else { '?' };
                        format!("{base}{sep}streamid={}", s.key)
                    },
                    "mpegts",
                ));
            }
            if s.key.is_empty() {
                base.clone()
            } else {
                join_key(base, &s.key)
            }
        }
        _ => {
            let server = p.server.map(str::to_string).unwrap_or(d.server.clone());
            if server.is_empty() || s.key.is_empty() {
                return None;
            }
            let mut u = join_key(&server, &s.key);
            if p.bandwidth_test && d.bandwidth_test {
                u.push_str(if u.contains('?') { "&" } else { "?" });
                u.push_str("bandwidthtest=true");
            }
            u
        }
    };
    Some((url, "flv"))
}

/// Builds the destination and its secret from an add (`old`: the saved one, on a change).
pub(crate) fn from_input(
    inp: &DestinationInput,
    old: Option<&(Destination, Secret)>,
) -> Result<(Destination, Secret), Invalid> {
    let p = preset(inp.preset.trim()).ok_or_else(|| {
        Invalid(
            "preset_unknown",
            "Pick YouTube, Twitch, Kick, Facebook or Custom.".into(),
        )
    })?;
    let mut secret = old.map(|o| o.1.clone()).unwrap_or_default();
    // A change that leaves the site's secret alone works even when that secret can't be read:
    // ticking a site off must not ask for its key.
    let unchanged = old.is_some_and(|o| o.0.preset == p.id);
    if let Some(k) = inp.key.as_deref() {
        secret.key = if p.id == "custom" && k.trim().is_empty() {
            String::new()
        } else {
            validate_key(k)?
        };
        secret.sealed = None;
    }
    let mut server = old.map(|o| o.0.server.clone()).unwrap_or_default();
    if p.id == "custom" {
        if let Some(u) = inp.url.as_deref().or(inp.server.as_deref()) {
            let u = validate_url(u, true, false)?;
            server = origin_of(&u);
            secret.url = u;
            secret.sealed = None;
        }
        if secret.url.is_empty() && secret.sealed.is_none() {
            return Err(Invalid(
                "url_invalid",
                "Enter the server address starting with rtmp://, rtmps:// or srt://.".into(),
            ));
        }
    } else if p.needs_server {
        if let Some(u) = inp.server.as_deref() {
            server = validate_url(u, false, true)?;
        }
        if server.is_empty() {
            return Err(Invalid(
                "server_missing",
                format!("Paste the Server URL from your {} dashboard.", p.name),
            ));
        }
    } else {
        server.clear();
    }
    if secret.key.is_empty() && p.id != "custom" && !unchanged {
        return Err(Invalid(
            "key_missing",
            "Paste the stream key from the site.".into(),
        ));
    }
    let label = inp
        .label
        .as_deref()
        .map(|l| l.trim().chars().take(60).collect::<String>())
        .or_else(|| old.map(|o| o.0.label.clone()))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| p.name.to_string());
    Ok((
        Destination {
            id: old.map(|o| o.0.id.clone()).unwrap_or_default(),
            preset: p.id.to_string(),
            label,
            server,
            bandwidth_test: p.bandwidth_test
                && inp
                    .bandwidth_test
                    .unwrap_or(old.is_some_and(|o| o.0.bandwidth_test)),
            enabled: inp.enabled.unwrap_or(old.is_none_or(|o| o.0.enabled)),
        },
        secret,
    ))
}

pub(crate) fn key_hint(s: &Secret) -> String {
    let k: Vec<char> = s.key.chars().collect();
    if k.len() < 8 {
        // Too short to show any of it safely.
        return if k.is_empty() {
            String::new()
        } else {
            "…".into()
        };
    }
    format!("…{}", k[k.len() - 4..].iter().collect::<String>())
}

pub(crate) fn view(d: &Destination, s: &Secret) -> DestinationView {
    DestinationView {
        dest: d.clone(),
        key_hint: key_hint(s),
        key_lost: s.sealed.is_some(),
        name: preset(&d.preset).map_or("Custom", |p| p.name),
    }
}

/// The saved destinations.
pub(crate) struct Store {
    dir: PathBuf,
    key: [u8; 32],
    state: Mutex<Vec<(Destination, Secret)>>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(flatten)]
    dest: Destination,
    /// base64(nonce || AES-256-GCM(JSON secret)).
    #[serde(default)]
    secret: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct FileShape {
    version: u32,
    destinations: Vec<Stored>,
}

const FILE: &str = "stream-destinations.json";
const KEY_FILE: &str = "stream-destinations.key";

impl Store {
    pub(crate) fn open(dir: &Path) -> anyhow::Result<Self> {
        let in_dir = || format!("using {}", dir.display());
        std::fs::create_dir_all(dir).with_context(in_dir)?;
        let key = load_or_create_key(&dir.join(KEY_FILE))?;
        let mut state = Vec::new();
        let path = dir.join(FILE);
        if path.exists() {
            let reading = || format!("reading {}", path.display());
            let bytes = std::fs::read(&path).with_context(reading)?;
            let shape: FileShape = serde_json::from_slice(&bytes).with_context(reading)?;
            for st in shape.destinations {
                let secret = match st.secret {
                    None => Secret::default(),
                    Some(blob) => decrypt(&key, &blob).unwrap_or_else(|| {
                        // Not this key's (the key file was replaced): the user adds the site
                        // again, or puts the old key file back. Never the key itself in a log.
                        crate::log_warn!(
                            "golive: the saved stream key of {} can't be read; add the site again",
                            st.dest.id
                        );
                        Secret {
                            sealed: Some(blob),
                            ..Default::default()
                        }
                    }),
                };
                state.push((st.dest, secret));
            }
        }
        Ok(Store {
            dir: dir.to_path_buf(),
            key,
            state: Mutex::new(state),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(Destination, Secret)>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn list(&self) -> Vec<DestinationView> {
        self.lock().iter().map(|(d, s)| view(d, s)).collect()
    }

    pub(crate) fn get(&self, id: &str) -> Option<(Destination, Secret)> {
        self.lock().iter().find(|(d, _)| d.id == id).cloned()
    }

    pub(crate) fn add(&self, inp: &DestinationInput) -> Result<DestinationView, Invalid> {
        let (mut d, s) = from_input(inp, None)?;
        let mut st = self.lock();
        d.id = new_id(&d.preset, &st);
        // Saved first, then kept: a failed save leaves the list as it was.
        let mut next = st.clone();
        next.push((d.clone(), s.clone()));
        self.save(&next)
            .map_err(|e| Invalid("disk", format!("Could not save it: {e}")))?;
        *st = next;
        Ok(view(&d, &s))
    }

    pub(crate) fn update(
        &self,
        id: &str,
        inp: &DestinationInput,
    ) -> Result<DestinationView, Invalid> {
        let mut st = self.lock();
        let i = st
            .iter()
            .position(|(d, _)| d.id == id)
            .ok_or_else(|| Invalid("not_found", "No such destination.".into()))?;
        let mut inp = inp.clone();
        if inp.preset.is_empty() {
            inp.preset = st[i].0.preset.clone();
        }
        let (d, s) = from_input(&inp, Some(&st[i]))?;
        let mut next = st.clone();
        next[i] = (d.clone(), s.clone());
        self.save(&next)
            .map_err(|e| Invalid("disk", format!("Could not save it: {e}")))?;
        *st = next;
        Ok(view(&d, &s))
    }

    pub(crate) fn remove(&self, id: &str) -> bool {
        let mut st = self.lock();
        let mut next = st.clone();
        next.retain(|(d, _)| d.id != id);
        if next.len() == st.len() || self.save(&next).is_err() {
            return false;
        }
        *st = next;
        true
    }

    fn save(&self, st: &[(Destination, Secret)]) -> anyhow::Result<()> {
        let shape = FileShape {
            version: 1,
            destinations: st
                .iter()
                .map(|(d, s)| {
                    Ok(Stored {
                        dest: d.clone(),
                        secret: Some(match &s.sealed {
                            Some(blob) => blob.clone(),
                            None => encrypt(&self.key, s)?,
                        }),
                    })
                })
                .collect::<anyhow::Result<_>>()?,
        };
        let path = self.dir.join(FILE);
        let tmp = self.dir.join(format!("{FILE}.tmp"));
        write_private(&tmp, &serde_json::to_vec_pretty(&shape)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

static STORE: Mutex<Option<Arc<Store>>> = Mutex::new(None);
/// The last open's failure, so a status poll a second logs it once.
static STORE_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// The folder they live in. Tests keep to one of their own.
fn store_dir() -> Option<PathBuf> {
    if cfg!(test) {
        return Some(
            std::env::temp_dir().join(format!("ps5upload-golive-store-{}", std::process::id())),
        );
    }
    crate::remote::store::data_dir()
}

/// The saved destinations (in the engine's data folder).
pub(crate) fn store() -> Result<Arc<Store>, String> {
    store_from(&STORE, store_dir())
}

/// A failure is not kept: a file that was locked or damaged is read again on the next call, so
/// fixing it needs no restart.
fn store_from(
    slot: &Mutex<Option<Arc<Store>>>,
    dir: Option<PathBuf>,
) -> Result<Arc<Store>, String> {
    let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_ref() {
        return Ok(s.clone());
    }
    let opened = dir
        .ok_or_else(|| "no home folder to keep stream keys in; set PS5UPLOAD_DATA_DIR".to_string())
        .and_then(|dir| Store::open(&dir).map_err(|e| format!("{e:#}")));
    let mut last = STORE_ERROR.lock().unwrap_or_else(|e| e.into_inner());
    match opened {
        Ok(s) => {
            let s = Arc::new(s);
            *g = Some(s.clone());
            *last = None;
            Ok(s)
        }
        Err(e) => {
            if last.as_deref() != Some(e.as_str()) {
                crate::log_warn!("golive: the saved destinations can't be read: {e}");
                *last = Some(e.clone());
            }
            Err(e)
        }
    }
}

fn new_id(preset: &str, taken: &[(Destination, Secret)]) -> String {
    loop {
        let mut r = [0u8; 3];
        let _ = getrandom::fill(&mut r);
        let id = format!("{preset}-{:02x}{:02x}{:02x}", r[0], r[1], r[2]);
        if !taken.iter().any(|(d, _)| d.id == id) {
            return id;
        }
    }
}

fn load_or_create_key(path: &Path) -> anyhow::Result<[u8; 32]> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice()) {
                return Ok(key);
            }
            // Not a key (damaged): kept aside, not overwritten, in case it can be put back.
            let _ = std::fs::rename(path, path.with_extension("bad"));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // Any other failure (a lock, permissions) is not "no key": making a new one would
        // write over the old and lose every saved stream key for good.
        Err(e) => anyhow::bail!("reading {}: {e}", path.display()),
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|e| anyhow::anyhow!("reading OS randomness: {e}"))?;
    // Whole or not at all: a crash between creating the file and writing it left an empty "key"
    // that the next start set aside, and every saved stream key went with it.
    let tmp = path.with_extension("key.tmp");
    write_private(&tmp, &key)?;
    std::fs::rename(&tmp, path)?;
    Ok(key)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

fn encrypt(key: &[u8; 32], secret: &Secret) -> anyhow::Result<String> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use base64::Engine;
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| anyhow::anyhow!("reading OS randomness: {e}"))?;
    let plain = serde_json::to_vec(secret)?;
    let sealed = cipher
        .encrypt(&Nonce::from(nonce), plain.as_slice())
        .map_err(|_| anyhow::anyhow!("could not encrypt the stream key"))?;
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&sealed);
    Ok(base64::engine::general_purpose::STANDARD.encode(blob))
}

fn decrypt(key: &[u8; 32], blob: &str) -> Option<Secret> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .ok()?;
    if bytes.len() < 12 {
        return None;
    }
    let (nonce, sealed) = bytes.split_at(12);
    let nonce: [u8; 12] = nonce.try_into().ok()?;
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let plain = cipher.decrypt(&Nonce::from(nonce), sealed).ok()?;
    serde_json::from_slice(&plain).ok()
}

#[cfg(test)]
#[path = "golive_dest_tests.rs"]
mod tests;
