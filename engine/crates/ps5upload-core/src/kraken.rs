//! Kraken Mount on the console: whether it is there and running, what it mounts, and the few calls
//! of its web API this app makes (rescan, remove a game, its AMPR runtime).
//!
//! Kraken Mount (quer3q/kraken-mount, read at v0.2.0, commit 73cc894) finds debug FPKG `.pkg`
//! files and game folders in its scan roots and puts them on the home screen without installing
//! them. It does not mount disk images (`.exfat`, `.ffpkg`, `.ffpfs`, `.ffpfsc`), and it waits
//! while ShadowMount+ runs (src/main.rs:534-567: it polls for a thread named
//! `shadowmountplus.elf` every 2 s and does nothing meanwhile). The two must never run together.
//!
//! Everything here comes from reading its source; the citations are to that commit. What could
//! only be inferred (the process name the console shows for it) says so where it is used.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::fs_ops::{fs_read_with_timeout, list_dir_with_timeout, ListDirOptions};
use crate::hw::ProcEntry;

/// Its web UI and API: TCP 8097 on every address (src/web.rs:15,138), no sign-in.
pub const API_PORT: u16 = 8097;
/// Its folder (src/main.rs:31).
pub const DATA_DIR: &str = "/data/krakenmount";
/// Its settings, a flat TOML subset (src/config.rs:5).
pub const CONFIG_PATH: &str = "/data/krakenmount/config.toml";
/// The files it copies over every game's fakelib when `inject_ampr` is on (src/cache.rs:21,531-571).
pub const AMPR_DIR: &str = "/data/krakenmount/ampr";
/// The AMPR runtime there: ampr-emu-ng, bundled (v0.4.1 in v0.2.0) or the user's own
/// (src/main.rs:252-256,377-409).
pub const AMPR_FILE: &str = "/data/krakenmount/ampr/libSceAmpr.sprx";
/// One folder per title it has built, named by title id (src/cache.rs:20).
pub const CACHES_DIR: &str = "/data/krakenmount/caches";
/// Its log, started over past 1 MiB (src/sys.rs:6,17).
pub const LOG_PATH: &str = "/data/krakenmount/kraken.log";

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const RPC_TIMEOUT: Duration = Duration::from_secs(5);
/// A remove deletes the game's whole source (a folder of 100+ GB on a USB drive) before it
/// answers (src/web.rs:542-605): far longer than [`CALL_TIMEOUT`].
const REMOVE_TIMEOUT: Duration = Duration::from_secs(600);

/// Whether a process the console lists is Kraken Mount.
///
/// INFERRED: Kraken Mount sets no process or thread name of its own (no setprocname or
/// thr_set_name in src/ or ps5/), so the console shows whatever its loader gave it: the ELF's
/// name, `kraken-mount.elf` (README.md:11-13; the tile starts `/data/krakenmount/kraken-mount.elf`,
/// src/ui.html:1471), when the loader names it after the file, and a generic name such as
/// `payload.elf` when it does not. The name is therefore only a hint; its API answering on
/// [`API_PORT`] is the proof ([`KrakenApi::probe`]).
pub fn is_kraken_process(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("kraken-mount") || n.contains("kraken_mount") || n.contains("krakenmount")
}

/// Whether an ELF about to be sent to the loader is ShadowMount+ (or carries it, as HyperHEN's
/// built-in mount payload does): by its file name, or by the name it gives its own thread
/// (`shadowmountplus.elf`, drakmor/ShadowMountPlus 1.7 src/main.c:44,587) together with its data
/// folder. Kraken Mount's ELF names `shadowmountplus.elf` too, to look for it, but never
/// `/data/shadowmount/`, and it carries its own folder's name, so it is not mistaken for it.
pub fn is_shadowmount_image(label: &str, image: &[u8]) -> bool {
    let file = label
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(label)
        .to_ascii_lowercase();
    if file.contains("shadowmountplus") || file.starts_with("hyperhen_mount") {
        return true;
    }
    let has = |needle: &[u8]| image.windows(needle.len()).any(|w| w == needle);
    has(b"shadowmountplus.elf") && has(b"/data/shadowmount/") && !has(DATA_DIR.as_bytes())
}

/// One game in Kraken Mount's library, as `GET /api/state` lists it (src/web.rs:110-132).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct KrakenGame {
    #[serde(default)]
    pub title_id: String,
    #[serde(default)]
    pub title_name: String,
    /// The `.pkg` file or game folder it mounts from.
    #[serde(default)]
    pub path: String,
    /// `pkg` or `folder`.
    #[serde(default)]
    pub kind: String,
    /// A folder's packing: `plain`, `zstd` or `lz4`; none for a package.
    #[serde(default)]
    pub packing: Option<String>,
    /// `pending`, `checking`, `building`, `ready`, `error` or `duplicate` (src/web.rs:24).
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct StateCache {
    #[serde(default)]
    path: String,
}

/// What `GET /api/state` answers that this app reads (src/web.rs:395-427).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct KrakenState {
    #[serde(default)]
    pub version: String,
    /// `kstuff` or `shadowmount` while it waits for one to start or stop; null otherwise.
    #[serde(default)]
    pub waiting: Option<String>,
    #[serde(default)]
    pub games: Vec<KrakenGame>,
    #[serde(default)]
    cache: Option<StateCache>,
}

impl KrakenState {
    /// The answer is Kraken Mount's, not some other server's on the same port: it names its
    /// caches folder.
    fn is_kraken(&self) -> bool {
        self.cache
            .as_ref()
            .is_some_and(|c| c.path.starts_with(DATA_DIR))
    }
}

/// Its AMPR runtime, as `GET /api/ampr` reports it (src/main.rs:500-513): `bundled`,
/// `downloaded`, `hand` (the user's own, which it keeps), `missing` or `error`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct KrakenAmpr {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// The settings this app acts on, from `config.toml` (src/config.rs:7-35).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KrakenSettings {
    /// Copy `ampr/*` over every game's fakelib (default on).
    pub inject_ampr: bool,
    /// Fetch a newer ampr-emu-ng from GitHub once per start (default on).
    pub ampr_auto_update: bool,
    /// `scanpath` lines: they replace the default roots when there are any.
    pub scan_paths: Vec<String>,
}

impl Default for KrakenSettings {
    fn default() -> Self {
        Self {
            inject_ampr: true,
            ampr_auto_update: true,
            scan_paths: Vec::new(),
        }
    }
}

/// Read the keys this app needs from `config.toml`: `key = value` lines, `#` comments, booleans
/// as `true`/`false`/`1`/`0`, `scanpath` as a string or an array of strings (src/config.rs:46-88).
/// Anything it can't read keeps the default, as Kraken Mount itself does with a bad value.
pub fn parse_config(text: &str) -> KrakenSettings {
    let mut s = KrakenSettings::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.split(" #").next().unwrap_or("").trim();
        let flag = || match value.trim_matches(['"', '\'']) {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        };
        match key.trim() {
            "inject_ampr" => s.inject_ampr = flag().unwrap_or(s.inject_ampr),
            "ampr_auto_update" => s.ampr_auto_update = flag().unwrap_or(s.ampr_auto_update),
            "scanpath" => {
                for part in value.trim_matches(['[', ']']).split(',') {
                    let p = part.trim().trim_matches(['"', '\'']).trim_end_matches('/');
                    if p.starts_with('/') && !s.scan_paths.iter().any(|x| x == p) {
                        s.scan_paths.push(p.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    s
}

/// The roots Kraken Mount scans when `config.toml` names none, in its order (src/storage.rs:66-81):
/// internal storage, then each extended-storage drive's and USB drive's `homebrew` and
/// `etaHEN/games`, then the USB and extended-storage drives' own roots. It only scans a drive that
/// is mounted, and only one level deep (src/scan.rs:149-167): a `.pkg` must sit directly in a root.
pub fn default_scan_roots() -> Vec<String> {
    let mut roots = vec![
        "/data/homebrew".to_string(),
        "/data/etaHEN/games".to_string(),
    ];
    for n in 0..2 {
        roots.push(format!("/mnt/ext{n}/homebrew"));
        roots.push(format!("/mnt/ext{n}/etaHEN/games"));
    }
    roots.extend((0..8).map(|n| format!("/mnt/usb{n}/homebrew")));
    roots.extend((0..8).map(|n| format!("/mnt/usb{n}/etaHEN/games")));
    roots.extend((0..8).map(|n| format!("/mnt/usb{n}")));
    roots.extend((0..2).map(|n| format!("/mnt/ext{n}")));
    roots
}

/// Where a package goes on `drive` (`/data`, `/mnt/ext1`, `/mnt/usb0`, …) for Kraken Mount to
/// find it: the drive's `homebrew` folder, which is a root in its default list on every drive,
/// unless the user's `scanpath` lines replace that list, in which case the first of them on that
/// drive. `None` when no root it scans is on that drive.
pub fn scan_root_for_drive(drive: &str, settings: &KrakenSettings) -> Option<String> {
    let drive = drive.trim_end_matches('/');
    if drive.is_empty() {
        return None;
    }
    if settings.scan_paths.is_empty() {
        return Some(format!("{drive}/homebrew"));
    }
    settings
        .scan_paths
        .iter()
        .find(|p| *p == drive || p.starts_with(&format!("{drive}/")))
        .cloned()
}

/// Kraken Mount's web API on a console.
pub struct KrakenApi {
    base: String,
    agent: ureq::Agent,
}

fn agent(timeout: Duration) -> ureq::Agent {
    // A LAN call: never through a proxy from the environment, and an error status is read, not
    // raised, so its own reason (plain text) reaches the user.
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .proxy(None)
        .http_status_as_error(false)
        .build();
    ureq::Agent::new_with_config(config)
}

impl KrakenApi {
    /// The API of the Kraken Mount running on the console at `addr` (any `host[:port]` form) and
    /// its state, or `None` when nothing that is Kraken Mount answers on [`API_PORT`].
    pub fn probe(addr: &str) -> Option<(KrakenApi, KrakenState)> {
        let host = crate::events::console_host(addr);
        if host.is_empty() {
            return None;
        }
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        let mut api = KrakenApi {
            base: format!("http://{host}:{API_PORT}"),
            agent: agent(PROBE_TIMEOUT),
        };
        let state = api.state().ok()?;
        if !state.is_kraken() {
            return None;
        }
        api.agent = agent(CALL_TIMEOUT);
        Some((api, state))
    }

    fn get(&self, route: &str) -> Result<String> {
        let mut resp = self
            .agent
            .get(format!("{}{route}", self.base))
            .call()
            .with_context(|| format!("Kraken Mount {route}"))?;
        let http = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        if http != 200 {
            bail!("Kraken Mount {route}: HTTP {http} {}", text.trim());
        }
        Ok(text)
    }

    fn post(&self, route: &str, body: &[u8]) -> Result<()> {
        let mut resp = self
            .agent
            .post(format!("{}{route}", self.base))
            .header("Content-Type", "text/plain")
            .send(body)
            .with_context(|| format!("Kraken Mount {route}"))?;
        let http = resp.status().as_u16();
        if !(200..300).contains(&http) {
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            bail!("{}", explain(route, http, text.trim()));
        }
        Ok(())
    }

    pub fn state(&self) -> Result<KrakenState> {
        serde_json::from_str(&self.get("/api/state")?).context("Kraken Mount /api/state")
    }

    pub fn ampr(&self) -> Result<KrakenAmpr> {
        serde_json::from_str(&self.get("/api/ampr")?).context("Kraken Mount /api/ampr")
    }

    /// Wake its scan loop now (src/web.rs:316-319). It answers at once; the scan follows. It
    /// does not scan while a game runs (src/main.rs:615-619).
    pub fn rescan(&self) -> Result<()> {
        self.post("/api/rescan", b"")
    }

    /// Remove a game the way its own web UI does (src/web.rs:542-605): take it off the home
    /// screen and DELETE ITS SOURCE (the `.pkg` file, or the whole game folder), then its cache.
    /// `path` must be the source it lists for the title, or it refuses (the library changed).
    ///
    /// It answers once the source is gone, which may take minutes: the call waits up to
    /// [`REMOVE_TIMEOUT`]. With no answer by then its library says what happened: the game no
    /// longer listed at `path` is a success, else an error saying it may still be removing.
    pub fn remove(&self, title_id: &str, path: &str) -> Result<()> {
        self.remove_within(title_id, path, REMOVE_TIMEOUT)
    }

    fn remove_within(&self, title_id: &str, path: &str, timeout: Duration) -> Result<()> {
        if !valid_title_id(title_id) {
            bail!("not a title id: {title_id}");
        }
        let route = format!("/api/delete/{title_id}");
        let long = KrakenApi {
            base: self.base.clone(),
            agent: agent(timeout),
        };
        match long.post(&route, path.as_bytes()) {
            Err(e) if timed_out(&e) => after_timeout(&route, self.state(), title_id, path, timeout),
            done => done,
        }
    }
}

/// Whether a call failed for want of an answer in time (not a refusal or a broken connection).
fn timed_out(e: &anyhow::Error) -> bool {
    use std::io::ErrorKind::{TimedOut, WouldBlock};
    e.chain().any(|c| match c.downcast_ref::<ureq::Error>() {
        Some(ureq::Error::Timeout(_)) => true,
        Some(ureq::Error::Io(io)) => matches!(io.kind(), TimedOut | WouldBlock),
        _ => c
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| matches!(io.kind(), TimedOut | WouldBlock)),
    })
}

/// A remove that got no answer within `waited`: done when Kraken Mount's library (`state`, read
/// again) no longer lists `title_id` at `path`; else it may still be deleting, or what happened is
/// unknown when the library could not be read.
fn after_timeout(
    route: &str,
    state: Result<KrakenState>,
    title_id: &str,
    path: &str,
    waited: Duration,
) -> Result<()> {
    let mins = waited.as_secs().div_ceil(60);
    match state {
        Ok(s)
            if !s
                .games
                .iter()
                .any(|g| g.title_id == title_id && g.path == path) =>
        {
            Ok(())
        }
        Ok(_) => bail!(
            "{route}: Kraken Mount gave no answer in {mins} min and still lists {title_id} at \
             {path}: it may still be removing it; look again in a few minutes"
        ),
        Err(e) => bail!(
            "{route}: Kraken Mount gave no answer in {mins} min, and its library could not be \
             read ({e:#}): whether {title_id} was removed is unknown"
        ),
    }
}

/// What an error answer means, in words a user can act on.
pub fn explain(route: &str, http: u16, text: &str) -> String {
    let what = match http {
        404 => "Kraken Mount does not have this game in its library".to_string(),
        409 if text.is_empty() => {
            "Kraken Mount is busy (a scan or build, or a game is running); try again in a moment"
                .to_string()
        }
        _ => format!("Kraken Mount answered HTTP {http}"),
    };
    if text.is_empty() || http == 404 {
        format!("{route}: {what}")
    } else {
        format!("{route}: {what} ({text})")
    }
}

/// Four capital letters and five digits, not a system title (src/scan.rs:259-263).
pub fn valid_title_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 9
        && b[..4].iter().all(u8::is_ascii_uppercase)
        && b[4..].iter().all(u8::is_ascii_digit)
        && !id.starts_with("NPXS")
}

/// Kraken Mount on one console, for the Library and the Convert page. Every field is empty or
/// false when it is not there.
#[derive(Debug, Clone, Default, Serialize)]
pub struct KrakenStatus {
    /// `/data/krakenmount` exists: it has run on this console at least once.
    pub installed: bool,
    /// It runs now: its API answered, or a process named like it is listed.
    pub running: bool,
    /// Its API answered from this computer: rescan and remove work.
    pub api: bool,
    pub version: Option<String>,
    /// `kstuff` or `shadowmount` while it waits for one to start or stop.
    pub waiting: Option<String>,
    /// Its library, when the API answered.
    pub games: Vec<KrakenGame>,
    /// The titles it has built: its library's when the API answered, else its cache folders.
    pub title_ids: Vec<String>,
    /// Its AMPR runtime, when the API answered.
    pub ampr: Option<KrakenAmpr>,
    /// From `config.toml`; the defaults when it has none.
    pub settings: KrakenSettings,
    /// The roots it scans: the `scanpath` lines, else its default list.
    pub scan_roots: Vec<String>,
    pub errors: Vec<String>,
}

/// The decision [`collect`] makes, apart from the console: `listing_ok` says `/data/krakenmount`
/// listed; `procs` is the process list when it could be read; `state` is the API's answer.
pub fn decide(
    listing_ok: bool,
    procs: Option<&[ProcEntry]>,
    state: Option<&KrakenState>,
) -> (bool, bool) {
    let by_name = procs.is_some_and(|p| p.iter().any(|e| is_kraken_process(&e.name)));
    let running = state.is_some() || by_name;
    (listing_ok || running, running)
}

/// Kraken Mount's status on the console at `addr`. `procs` is a process list already read (the
/// ShadowMount+ snapshot reads one), or `None` to skip the name check. Never fails outright: a
/// probe that fails is recorded in `errors`.
pub fn collect(addr: &str, procs: Option<&[ProcEntry]>) -> KrakenStatus {
    let mut errors = Vec::new();
    let listing =
        list_dir_with_timeout(addr, DATA_DIR, ListDirOptions::default(), Some(RPC_TIMEOUT));
    let listing_ok = match &listing {
        Ok(_) => true,
        Err(e) => {
            let s = e.to_string();
            if !crate::fs_ops::is_not_found(&s) && !s.contains("_errno_2") {
                errors.push(format!("list {DATA_DIR}: {s}"));
            }
            false
        }
    };
    let probed = KrakenApi::probe(addr);
    let (installed, running) = decide(listing_ok, procs, probed.as_ref().map(|(_, s)| s));
    if !installed {
        return KrakenStatus {
            errors,
            scan_roots: default_scan_roots(),
            ..Default::default()
        };
    }
    let settings =
        match fs_read_with_timeout(addr, CONFIG_PATH, 0, 64 * 1024, Some(RPC_TIMEOUT), false) {
            Ok(bytes) => parse_config(&String::from_utf8_lossy(&bytes)),
            Err(_) => KrakenSettings::default(),
        };
    let scan_roots = if settings.scan_paths.is_empty() {
        default_scan_roots()
    } else {
        settings.scan_paths.clone()
    };
    let mut status = KrakenStatus {
        installed,
        running,
        api: probed.is_some(),
        settings,
        scan_roots,
        ..Default::default()
    };
    match probed {
        Some((api, state)) => {
            status.version = Some(state.version.clone()).filter(|v| !v.is_empty());
            status.waiting = state.waiting.clone();
            status.title_ids = title_ids_of(&state.games);
            status.games = state.games;
            match api.ampr() {
                Ok(a) => status.ampr = Some(a),
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        None => {
            if let Ok(l) = list_dir_with_timeout(
                addr,
                CACHES_DIR,
                ListDirOptions::default(),
                Some(RPC_TIMEOUT),
            ) {
                let mut ids: Vec<String> = l
                    .entries
                    .iter()
                    .filter(|e| e.kind == "dir" && valid_title_id(&e.name))
                    .map(|e| e.name.clone())
                    .collect();
                ids.sort();
                status.title_ids = ids;
            }
        }
    }
    status.errors = errors;
    status
}

/// The titles of a library, once each: a `duplicate` row is a second copy of a title already
/// listed (src/scan.rs:219-229), and a row in `error` was never put on the home screen.
fn title_ids_of(games: &[KrakenGame]) -> Vec<String> {
    let mut ids: Vec<String> = games
        .iter()
        .filter(|g| valid_title_id(&g.title_id) && g.status != "duplicate" && g.status != "error")
        .map(|g| g.title_id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Which mounter runs on a console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mounter {
    /// Neither runs.
    None,
    /// ShadowMount+ (or HyperHEN's built-in copy of it).
    Shadowmount,
    /// Kraken Mount.
    Kraken,
    /// Both at once: Kraken Mount then only waits (see the module note), and nothing should
    /// start either again until one is stopped.
    Both,
    /// The process list could not be read and no API answered.
    Unknown,
}

pub fn active_mounter(smp_running: bool, kraken_running: bool, procs_known: bool) -> Mounter {
    match (smp_running, kraken_running) {
        (true, true) => Mounter::Both,
        (true, false) => Mounter::Shadowmount,
        (false, true) => Mounter::Kraken,
        (false, false) if procs_known => Mounter::None,
        (false, false) => Mounter::Unknown,
    }
}

/// Ask whichever mounter runs to scan now: ShadowMount+ through its API (retry counters cleared),
/// else Kraken Mount through its. Answers which one was asked.
pub fn rescan(addr: &str) -> Result<Mounter> {
    if let Some((api, _)) = crate::smp_api::SmpApi::probe(addr) {
        api.rescan()?;
        return Ok(Mounter::Shadowmount);
    }
    if let Some((api, _)) = KrakenApi::probe(addr) {
        api.rescan()?;
        return Ok(Mounter::Kraken);
    }
    bail!(
        "no mounter's API answers from this computer: ShadowMount+ needs 1.7 or later running, \
         with its API enabled and open to the network in /data/shadowmount/config.ini; Kraken \
         Mount answers on port {API_PORT} while it runs"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn procs(names: &[&str]) -> Vec<ProcEntry> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| ProcEntry {
                pid: 100 + i as i32,
                name: n.to_string(),
            })
            .collect()
    }

    fn state(json: &str) -> KrakenState {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn kraken_process_names() {
        for n in [
            "kraken-mount.elf",
            "Kraken-Mount",
            "kraken_mount.elf",
            "krakenmount",
        ] {
            assert!(is_kraken_process(n), "{n}");
        }
        for n in [
            "shadowmountplus.elf",
            "payload.elf",
            "kstuff.elf",
            "SceShellUI",
            "",
        ] {
            assert!(!is_kraken_process(n), "{n}");
        }
    }

    #[test]
    fn shadowmount_images_are_told_from_kraken_mounts() {
        let smp = b"\x7fELF..shadowmountplus.elf../data/shadowmount/config.ini..";
        let kraken = b"\x7fELF..shadowmountplus.elf../data/krakenmount/ampr..";
        assert!(is_shadowmount_image("/x/payload.elf", smp));
        assert!(!is_shadowmount_image("/x/kraken-mount.elf", kraken));
        assert!(is_shadowmount_image(
            r"C:\p\shadowmountplus.elf",
            b"\x7fELF"
        ));
        assert!(is_shadowmount_image("hyperhen_mount.elf", b"\x7fELF"));
        assert!(!is_shadowmount_image("kstuff.elf", b"\x7fELF..kstuff.."));
    }

    #[test]
    fn its_state_answer_reads_and_identifies_it() {
        // As src/web.rs:409-425 writes it, shortened.
        let s = state(
            r#"{"version":"0.2.0","busy":false,"paused":false,"waiting":null,"running":[],
            "games":[{"title_id":"PPSA01234","title_name":"Game","version":"01.000",
              "path":"/data/homebrew/Game.pkg","kind":"pkg","packing":null,"drive":"/data",
              "size":1,"status":"ready","error":null,"cache_size":2,"cache_built":3,"backport":null},
             {"title_id":"PPSA01234","path":"/mnt/usb0/Game.pkg","kind":"pkg","status":"duplicate"},
             {"title_id":"PPSA09999","path":"/data/homebrew/Bad.pkg","kind":"pkg","status":"error",
              "error":"a retail package"}],
            "drives":[],"cache":{"path":"/data/krakenmount/caches","size":0,"free":0,
              "low_space":false,"ampr_present":true},"log":[]}"#,
        );
        assert!(s.is_kraken());
        assert_eq!(s.version, "0.2.0");
        assert_eq!(s.games[0].path, "/data/homebrew/Game.pkg");
        assert_eq!(title_ids_of(&s.games), vec!["PPSA01234".to_string()]);
        // Some other server on the port, answering JSON that is not its.
        assert!(!state(r#"{"games":[]}"#).is_kraken());
        assert!(!state(r#"{"cache":{"path":"/tmp"}}"#).is_kraken());
    }

    #[test]
    fn running_needs_the_api_or_a_process_named_like_it() {
        let ok = state(r#"{"cache":{"path":"/data/krakenmount/caches"}}"#);
        assert_eq!(decide(false, None, None), (false, false));
        assert_eq!(decide(true, None, None), (true, false));
        assert_eq!(
            decide(true, Some(procs(&["payload.elf"]).as_slice()), None),
            (true, false)
        );
        assert_eq!(
            decide(false, Some(procs(&["kraken-mount.elf"]).as_slice()), None),
            (true, true)
        );
        assert_eq!(decide(false, Some(&[][..]), Some(&ok)), (true, true));
    }

    #[test]
    fn config_keys_it_acts_on() {
        let d = parse_config("");
        assert!(d.inject_ampr && d.ampr_auto_update && d.scan_paths.is_empty());
        let s = parse_config(
            "# comment\ninject_ampr = false\nampr_auto_update = 0\n\
             scanpath = [\"/mnt/usb0/games/\", '/data/pkgs']\nscan_interval_seconds = 10\n",
        );
        assert!(!s.inject_ampr && !s.ampr_auto_update);
        assert_eq!(s.scan_paths, vec!["/mnt/usb0/games", "/data/pkgs"]);
        let one =
            parse_config("scanpath = \"/data/a\"\nscanpath = \"/data/a\"\ninject_ampr = maybe\n");
        assert_eq!(one.scan_paths, vec!["/data/a"]);
        assert!(one.inject_ampr, "a value it can't read keeps the default");
    }

    #[test]
    fn default_roots_match_its_list() {
        let r = default_scan_roots();
        assert_eq!(r[0], "/data/homebrew");
        assert_eq!(r[1], "/data/etaHEN/games");
        assert!(r.contains(&"/mnt/ext1/homebrew".to_string()));
        assert!(r.contains(&"/mnt/usb7".to_string()));
        assert_eq!(r.len(), 2 + 4 + 8 + 8 + 8 + 2);
    }

    #[test]
    fn the_scan_root_for_a_drive() {
        let d = KrakenSettings::default();
        assert_eq!(
            scan_root_for_drive("/data", &d).as_deref(),
            Some("/data/homebrew")
        );
        assert_eq!(
            scan_root_for_drive("/mnt/usb1/", &d).as_deref(),
            Some("/mnt/usb1/homebrew")
        );
        let own = KrakenSettings {
            scan_paths: vec!["/mnt/usb0/games".into(), "/data/pkgs".into()],
            ..Default::default()
        };
        assert_eq!(
            scan_root_for_drive("/data", &own).as_deref(),
            Some("/data/pkgs")
        );
        assert_eq!(scan_root_for_drive("/mnt/usb1", &own), None);
        // `/mnt/usb1` is not on `/mnt/usb10`'s drive, and the other way round.
        let ten = KrakenSettings {
            scan_paths: vec!["/mnt/usb10/x".into()],
            ..Default::default()
        };
        assert_eq!(scan_root_for_drive("/mnt/usb1", &ten), None);
    }

    #[test]
    fn title_ids() {
        assert!(valid_title_id("PPSA01234"));
        assert!(valid_title_id("CUSA00001"));
        assert!(!valid_title_id("NPXS40000"));
        assert!(!valid_title_id("ppsa01234"));
        assert!(!valid_title_id("PPSA0123"));
        assert!(!valid_title_id("../../etc"));
    }

    #[test]
    fn which_mounter() {
        assert_eq!(active_mounter(true, false, true), Mounter::Shadowmount);
        assert_eq!(active_mounter(false, true, false), Mounter::Kraken);
        assert_eq!(active_mounter(true, true, true), Mounter::Both);
        assert_eq!(active_mounter(false, false, true), Mounter::None);
        assert_eq!(active_mounter(false, false, false), Mounter::Unknown);
        assert_eq!(
            serde_json::to_string(&Mounter::Shadowmount).unwrap(),
            "\"shadowmount\""
        );
    }

    #[test]
    fn errors_say_what_to_do() {
        assert!(explain("/api/delete/PPSA01234", 404, "").contains("does not have"));
        assert!(explain("/api/delete/PPSA01234", 409, "a game is running")
            .contains("a game is running"));
        assert!(explain("/api/rescan", 409, "").contains("busy"));
        assert!(explain("/api/rescan", 500, "x").contains("HTTP 500"));
    }

    /// A stand-in Kraken Mount: `/api/state` answers `state` (as it is at the time), a delete
    /// is read and never answered (a whole game folder still being deleted).
    fn slow_kraken(state: std::sync::Arc<std::sync::Mutex<String>>) -> KrakenApi {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { return };
                let state = std::sync::Arc::clone(&state);
                std::thread::spawn(move || {
                    let mut first = String::new();
                    let mut reader = BufReader::new(conn.try_clone().unwrap());
                    if reader.read_line(&mut first).is_err() {
                        return;
                    }
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                    }
                    if first.starts_with("GET /api/state") {
                        let body = state.lock().unwrap().clone();
                        let _ = write!(
                            conn,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                    } else {
                        std::thread::sleep(Duration::from_secs(3));
                    }
                });
            }
        });
        KrakenApi {
            base: format!("http://127.0.0.1:{port}"),
            agent: agent(Duration::from_secs(5)),
        }
    }

    /// A remove with no answer in time: done when the library no longer lists the game at that
    /// path, else an error saying it may still be removing; another path for the title is not it.
    #[test]
    fn a_remove_with_no_answer_is_judged_by_the_library() {
        let listed = |path: &str| {
            format!(
                r#"{{"cache":{{"path":"/data/krakenmount/caches"}},"games":[{{"title_id":"PPSA01234","path":"{path}"}}]}}"#
            )
        };
        let state = std::sync::Arc::new(std::sync::Mutex::new(listed("/data/homebrew/G")));
        let api = slow_kraken(std::sync::Arc::clone(&state));
        let wait = Duration::from_millis(300);
        let e = api
            .remove_within("PPSA01234", "/data/homebrew/G", wait)
            .unwrap_err()
            .to_string();
        assert!(e.contains("may still be removing"), "{e}");
        *state.lock().unwrap() = listed("/mnt/usb0/G");
        api.remove_within("PPSA01234", "/data/homebrew/G", wait)
            .unwrap();
        // The library could not be read either: unknown.
        let gone = KrakenApi {
            base: "http://127.0.0.1:1".into(),
            agent: agent(Duration::from_secs(2)),
        };
        let e = after_timeout(
            "/api/delete/PPSA01234",
            gone.state(),
            "PPSA01234",
            "/data/homebrew/G",
            REMOVE_TIMEOUT,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("unknown") && e.contains("10 min"), "{e}");
    }

    #[test]
    fn no_api_is_none_quickly() {
        let t = std::time::Instant::now();
        assert!(KrakenApi::probe("127.0.0.1:1").is_none());
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}
