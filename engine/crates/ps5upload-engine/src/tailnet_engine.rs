//! The engine's own tailnet node (Settings › Tailscale), the system's Tailscale app, and "use
//! the tailnet when away".
//!
//! * Settings and keys live in `<data>/tailnet-app/` (`settings.json`, the core's
//!   `tailnet-state.json`). The auth key is used once and never stored.
//! * Consoles put on Tailscale ("Put this PS5 on Tailscale", `tailnet_api.rs`) are remembered
//!   with their tailnet address in `consoles.json`, keyed by their LAN address.
//! * The home network always comes first. A prober checks every remembered console's LAN
//!   address (TCP 9120) every few seconds while a tailnet way exists. Only when the LAN does not
//!   answer and the console is on the tailnet does [`ps5upload_core::route`] send the helper
//!   (9120), Remote Play (9295/TCP, 9296/UDP) and discovery (9302/UDP) to its tailnet address:
//!   through local mappings of the engine's own node (`builtin`), or straight to the 100.x
//!   address when the system's Tailscale app carries this device (`system`, see
//!   `tailnet_system.rs`).
//! * Everything about the decision stays in memory: `GET /api/tailnet/route?addr=` explains it.
//!   One log line and one `tailnet` event when a console switches path; nothing periodic.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ps5upload_core::route::{Router, Via};
use ps5upload_tailnet as tn;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::tailnet_system as sys;

pub const DEFAULT_CONTROL_URL: &str = ps5upload_core::tailnet::DEFAULT_CONTROL_URL;
const PROBE_EVERY: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_millis(900);
/// The ports a console serves to the tailnet (payload/src/tailnet_node.c): (port, udp).
pub const ROUTED: &[(u16, bool)] = &[(9120, false), (9295, false), (9296, true), (9302, true)];
/// What a wake says when the PS5 may be away from this device's network.
pub const WAKE_AWAY_NOTE: &str = "If this PS5 is not on the same network as this device, it can't be woken from here: a PS5 in rest mode is off Tailscale, and waking works only on its home network.";
/// "Test it now": how long a console is sent through Tailscale on demand at most.
const TEST_PATH_MAX: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    #[serde(default)]
    pub control_url: String,
    #[serde(default)]
    pub hostname: String,
    /// With the system's Tailscale app on this device: "" not asked yet, "system" use it,
    /// "own" sign PS5Upload in as its own device.
    #[serde(default)]
    pub system_choice: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownConsole {
    /// The console's LAN address as the app knows it.
    pub host: String,
    pub ip: Ipv4Addr,
    #[serde(default)]
    pub name: String,
    /// When "Try Tailscale now" last reached this console through Tailscale (unix ms; 0 never):
    /// the Connection panel's step 3 stays done.
    #[serde(default)]
    pub tested_at: u64,
}

/// A device in this device's tailnet peer list, from the engine's own node or the system app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerSeen {
    /// The MagicDNS name's first label ("living-room-ps5u").
    pub name: String,
    pub ip: Ipv4Addr,
    pub online: bool,
    /// "builtin" or "system".
    pub source: &'static str,
}

/// Which tailnet way this device has right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// The engine's own node runs.
    Builtin,
    /// The system's Tailscale app carries this device (and the user did not pick "own").
    System,
    None,
}

impl Backend {
    fn as_str(self) -> &'static str {
        match self {
            Backend::Builtin => "builtin",
            Backend::System => "system",
            Backend::None => "none",
        }
    }
}

/// What the router knows about one console at a moment: the pure input of [`decide`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Inputs {
    /// The home address answered (None: not probed yet).
    pub lan_ok: Option<bool>,
    /// "Test it now": go through the tailnet even though home answers.
    pub forced: bool,
    pub tailnet_ip: Option<Ipv4Addr>,
    /// Is the console online on the tailnet: the node's peer list, the system CLI's, or the
    /// system probe (None: unknown).
    pub online: Option<bool>,
}

/// Where a console goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Lan,
    Builtin(Ipv4Addr),
    System(Ipv4Addr),
}

/// The routing rule: home first, always; the tailnet only when home does not answer (or a test
/// asks for it) and the console is on the tailnet. The built-in node needs the console online
/// in its netmap; the system app is used unless something says the console is offline.
pub fn decide(backend: Backend, i: Inputs) -> Decision {
    if i.lan_ok != Some(false) && !i.forced {
        return Decision::Lan;
    }
    let Some(ip) = i.tailnet_ip else {
        return Decision::Lan;
    };
    match backend {
        Backend::None => Decision::Lan,
        Backend::Builtin if i.online == Some(true) => Decision::Builtin(ip),
        Backend::Builtin => Decision::Lan,
        Backend::System if i.online != Some(false) => Decision::System(ip),
        Backend::System => Decision::Lan,
    }
}

#[derive(Default)]
struct ConsoleState {
    /// None: not probed yet.
    lan_ok: Option<bool>,
    checked: Option<Instant>,
    /// System mode: whether the console's tailnet address answered on 9120.
    tail_ok: Option<bool>,
    maps: HashMap<(u16, bool), u16>,
    /// The last path told ("lan", "builtin", "system"), its switch time (unix ms).
    path: Option<&'static str>,
    switched_ms: u64,
    /// "Test it now" until then.
    forced_until: Option<Instant>,
}

pub struct TailnetEngine {
    dir: Option<PathBuf>,
    node: Mutex<Option<Arc<tn::Node>>>,
    settings: Mutex<Settings>,
    known: Mutex<Vec<KnownConsole>>,
    state: Mutex<HashMap<String, ConsoleState>>,
    /// The node's last error (a refused sign-in, an unreachable control server), cleared once it
    /// runs: Settings › Tailscale says it in plain words instead of spinning on "Connecting…".
    last_error: Mutex<String>,
    /// Something the user should know while it runs (the key expires soon, or has).
    last_warning: Mutex<String>,
}

static ENGINE: OnceLock<Arc<TailnetEngine>> = OnceLock::new();

pub fn engine() -> &'static Arc<TailnetEngine> {
    ENGINE.get_or_init(|| {
        let dir = crate::remote::store::data_dir().map(|d| d.join("tailnet-app"));
        Arc::new(TailnetEngine::new(dir))
    })
}

fn load<T: for<'de> Deserialize<'de>>(dir: &Option<PathBuf>, name: &str) -> Option<T> {
    let text = std::fs::read_to_string(dir.as_ref()?.join(name)).ok()?;
    serde_json::from_str(&text).ok()
}

fn save<T: Serialize>(dir: &Option<PathBuf>, name: &str, v: &T) {
    let Some(dir) = dir else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = dir.join(format!("{name}.tmp"));
    if let Ok(text) = serde_json::to_vec_pretty(v) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join(name));
        }
    }
}

/// At engine start: run the node if the user turned it on, install the router, start the prober.
pub fn boot() {
    let e = engine().clone();
    ps5upload_core::route::install(e.clone());
    if e.settings.lock().unwrap_or_else(|p| p.into_inner()).enabled {
        if let Err(err) = e.start("") {
            crate::log_at!(Warn, "tailnet", "tailnet: not started: {err:#}");
        }
    }
    let prober = e.clone();
    let _ = std::thread::Builder::new()
        .name("tailnet-prober".into())
        .spawn(move || loop {
            prober.probe_all();
            std::thread::sleep(PROBE_EVERY);
        });
}

fn tcp_answers(host: &str, port: u16) -> bool {
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    addrs
        .into_iter()
        .any(|a| TcpStream::connect_timeout(&a, PROBE_TIMEOUT).is_ok())
}

fn lan_answers(host: &str) -> bool {
    tcp_answers(host, 9120)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl TailnetEngine {
    fn new(dir: Option<PathBuf>) -> Self {
        TailnetEngine {
            settings: Mutex::new(load(&dir, "settings.json").unwrap_or_default()),
            known: Mutex::new(load(&dir, "consoles.json").unwrap_or_default()),
            dir,
            node: Mutex::new(None),
            state: Mutex::new(HashMap::new()),
            last_error: Mutex::new(String::new()),
            last_warning: Mutex::new(String::new()),
        }
    }

    fn node(&self) -> Option<Arc<tn::Node>> {
        lock(&self.node).clone()
    }

    /// This device's tailnet way now: the engine's own node first (the user set it up), else
    /// the system app unless the user chose "own".
    pub fn backend(&self) -> Backend {
        if self.node().is_some() {
            return Backend::Builtin;
        }
        if lock(&self.settings).system_choice != "own" && sys::detect().detected {
            return Backend::System;
        }
        Backend::None
    }

    /// The name this device asks for: the one set, else its own name + "-ps5u".
    fn effective_hostname(s: &Settings) -> String {
        if s.hostname.trim().is_empty() {
            sys::default_device_name()
        } else {
            s.hostname.clone()
        }
    }

    fn start(&self, authkey: &str) -> anyhow::Result<()> {
        let mut slot = lock(&self.node);
        if slot.is_some() {
            return Ok(());
        }
        let Some(dir) = self.dir.clone() else {
            anyhow::bail!("no data folder for the tailnet keys; set PS5UPLOAD_DATA_DIR");
        };
        let s = lock(&self.settings).clone();
        let cfg = tn::Config {
            state_dir: dir,
            control_url: if s.control_url.is_empty() {
                DEFAULT_CONTROL_URL.to_string()
            } else {
                s.control_url.clone()
            },
            auth_key: authkey.to_string(),
            hostname: Self::effective_hostname(&s),
            os: tn::os_name().to_string(),
            // Test beds and private Headscale servers with their own CA; DERP-only for tests.
            ca_file: std::env::var_os("PS5UPLOAD_TAILNET_CA_FILE").map(PathBuf::from),
            no_direct: std::env::var("PS5UPLOAD_TAILNET_NO_DIRECT").is_ok_and(|v| v == "1"),
            ..Default::default()
        };
        // A fresh start says nothing of the last run's trouble.
        self.clear_problems();
        let node = tn::Node::start(&cfg, |ev| match ev {
            tn::Event::State(s) => {
                crate::log_at!(Info, "tailnet", "tailnet: {s}");
                if s == "running" {
                    // In: the error that kept it out (a retry got through) is over.
                    lock(&engine().last_error).clear();
                }
            }
            tn::Event::Path(p) => crate::log_at!(Info, "tailnet", "tailnet: {p}"),
            tn::Event::Warning(w) => {
                crate::log_at!(Warn, "tailnet", "tailnet: {w}");
                *lock(&engine().last_warning) = w;
            }
            tn::Event::Error(w) => {
                crate::log_at!(Warn, "tailnet", "tailnet: {w}");
                *lock(&engine().last_error) = w;
            }
            tn::Event::Log(l) => crate::log_at!(Debug, "tailnet", "tailnet: {l}"),
            tn::Event::LoginUrl(_) => {
                crate::log_at!(Info, "tailnet", "tailnet: waiting for sign-in")
            }
            tn::Event::NetmapChanged => {}
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        *slot = Some(Arc::new(node));
        Ok(())
    }

    fn clear_problems(&self) {
        lock(&self.last_error).clear();
        lock(&self.last_warning).clear();
    }

    fn stop(&self) {
        let node = lock(&self.node).take();
        drop(node);
        for st in lock(&self.state).values_mut() {
            st.maps.clear();
        }
    }

    /// enable / disable / logout / rename / renew / system_choice from Settings › Tailscale.
    /// `choice` is op `system_choice`'s "system" or "own".
    pub fn op(
        &self,
        op: &str,
        control_url: &str,
        hostname: &str,
        authkey: &str,
        choice: &str,
    ) -> anyhow::Result<Value> {
        match op {
            "enable" => {
                let changed = {
                    let mut s = lock(&self.settings);
                    let url = control_url.trim();
                    if !url.is_empty()
                        && !url.starts_with("https://")
                        && !url.starts_with("http://")
                    {
                        anyhow::bail!("control URL must be http:// or https://");
                    }
                    let changed = (!url.is_empty() && url != s.control_url)
                        || (!hostname.trim().is_empty() && hostname.trim() != s.hostname);
                    if !url.is_empty() {
                        s.control_url = url.to_string();
                    }
                    if !hostname.trim().is_empty() {
                        s.hostname = hostname.trim().to_string();
                    }
                    s.enabled = true;
                    // Signing PS5Upload in is the "own device" answer to the system app question.
                    if s.system_choice.is_empty() && sys::detect().detected {
                        s.system_choice = "own".into();
                    }
                    save(&self.dir, "settings.json", &*s);
                    changed
                };
                if changed || !authkey.is_empty() {
                    self.stop();
                }
                self.start(authkey.trim())?;
            }
            // Off keeps the sign-in (a pause); logout forgets it. `enabled` is saved either way,
            // so a restart keeps it off.
            "disable" | "logout" => {
                self.stop();
                let mut s = lock(&self.settings);
                s.enabled = false;
                save(&self.dir, "settings.json", &*s);
                if op == "logout" {
                    if let Some(d) = &self.dir {
                        let _ = std::fs::remove_file(d.join("tailnet-state.json"));
                    }
                }
            }
            // This device's name on the tailnet, any time: kept while off, and applied now
            // (the node starts again under it) while on. The address does not change.
            "rename" => {
                let name = hostname.trim();
                if name.is_empty() || name.len() > 63 {
                    anyhow::bail!("a device name is 1 to 63 characters");
                }
                let restart = {
                    let mut s = lock(&self.settings);
                    if s.hostname == name {
                        false
                    } else {
                        s.hostname = name.to_string();
                        save(&self.dir, "settings.json", &*s);
                        s.enabled
                    }
                };
                if restart {
                    self.stop();
                    self.start("")?;
                }
            }
            // Renew the sign-in, same node: a login URL follows in the status.
            "renew" => {
                let Some(node) = self.node() else {
                    anyhow::bail!("Tailscale is not on for this device");
                };
                node.reauth().map_err(|e| anyhow::anyhow!("{e}"))?;
                std::thread::sleep(Duration::from_millis(300));
            }
            "system_choice" => {
                if choice != "system" && choice != "own" {
                    anyhow::bail!("choice must be \"system\" or \"own\"");
                }
                let mut s = lock(&self.settings);
                s.system_choice = choice.to_string();
                save(&self.dir, "settings.json", &*s);
            }
            "status" | "" => {}
            // Done by the route before this (tailnet_api::engine_op): here they only answer the
            // status, like "status".
            "remember" | "test_path" => {}
            other => anyhow::bail!("unknown op {other:?}"),
        }
        Ok(self.status_json())
    }

    /// Where the engine would dial `host:port` now: a mapping or a tailnet address when away,
    /// else None.
    pub fn route(&self, host: &str, port: u16, udp: bool) -> Option<SocketAddr> {
        Router::dial(self, &ps5upload_core::events::console_host(host), port, udp)
    }

    /// A console put on Tailscale: remember its tailnet address for when it is away. An empty
    /// `name` keeps the one remembered. A console first added away at its tailnet address
    /// (`host` = that address) and now remembered under its home address is one console: the
    /// tailnet-address entry goes, its test result kept.
    pub fn remember(&self, host: &str, ip: Ipv4Addr, name: &str) {
        let host = ps5upload_core::events::console_host(host);
        let mut known = lock(&self.known);
        let placeholder = ip.to_string();
        let name = if name.is_empty() {
            known
                .iter()
                .find(|k| k.ip == ip && !k.name.is_empty())
                .map(|k| k.name.clone())
                .unwrap_or_default()
        } else {
            name.to_string()
        };
        let stale_placeholder = host != placeholder && known.iter().any(|k| k.host == placeholder);
        if !stale_placeholder
            && known
                .iter()
                .any(|k| k.host == host && k.ip == ip && k.name == name)
        {
            return;
        }
        let moved = known.iter().any(|k| k.host == host && k.ip != ip);
        let tested_at = known
            .iter()
            .filter(|k| k.ip == ip && (k.host == host || k.host == placeholder))
            .map(|k| k.tested_at)
            .max()
            .unwrap_or(0);
        known.retain(|k| k.host != host && !(stale_placeholder && k.host == placeholder));
        known.push(KnownConsole {
            host: host.clone(),
            ip,
            name,
            tested_at,
        });
        save(&self.dir, "consoles.json", &*known);
        drop(known);
        if moved {
            // A new tailnet address: the mappings made for the old one would dial nobody.
            if let Some(s) = lock(&self.state).get_mut(&host) {
                s.maps.clear();
            }
        }
    }

    /// Every address this console (by its tailnet address) is remembered under.
    pub fn known_hosts_for(&self, ip: Ipv4Addr) -> Vec<String> {
        lock(&self.known)
            .iter()
            .filter(|k| k.ip == ip)
            .map(|k| k.host.clone())
            .collect()
    }

    /// The name remembered for a tailnet address, if any.
    pub fn known_name_for(&self, ip: Ipv4Addr) -> String {
        lock(&self.known)
            .iter()
            .find(|k| k.ip == ip && !k.name.is_empty())
            .map(|k| k.name.clone())
            .unwrap_or_default()
    }

    /// The peers this device sees on the tailnet: the engine's own node's, and the system app's
    /// where its CLI can be read (never on a phone). `true` when some list could be read at all.
    pub fn peers_seen(&self) -> (Vec<PeerSeen>, bool) {
        let mut out = Vec::new();
        let mut readable = false;
        if let Some(node) = self.node() {
            let st = node.status();
            readable = st.state == "running";
            for p in st.peers {
                if let Some(ip) = p.ip {
                    out.push(PeerSeen {
                        name: p.name.split('.').next().unwrap_or("").to_string(),
                        ip,
                        online: p.online,
                        source: "builtin",
                    });
                }
            }
        }
        if let Some(st) = sys::status() {
            readable = true;
            for p in st.peers {
                if let Some(ip) = p.ipv4 {
                    if !out.iter().any(|o| o.ip == ip) {
                        out.push(PeerSeen {
                            name: p.name,
                            ip,
                            online: p.online,
                            source: "system",
                        });
                    }
                }
            }
        }
        (out, readable)
    }

    /// Where to dial a tailnet address's `port` from this device, and which way: a local mapping
    /// of the engine's own node, or the address itself through the system app. None without a
    /// tailnet way.
    pub fn tailnet_dial(&self, ip: Ipv4Addr, port: u16) -> Option<(SocketAddr, &'static str)> {
        if let Some(node) = self.node() {
            if let Ok(p) = node.map(tn::Proto::Tcp, ip, port, 0) {
                return Some((SocketAddr::from(([127, 0, 0, 1], p)), "builtin"));
            }
        }
        if sys::detect().detected {
            return Some((SocketAddr::from((ip, port)), "system"));
        }
        None
    }

    pub fn known_ip(&self, host: &str) -> Option<Ipv4Addr> {
        let host = ps5upload_core::events::console_host(host);
        lock(&self.known)
            .iter()
            .find(|k| k.host == host)
            .map(|k| k.ip)
    }

    /// A console on Tailscale whose home address did not answer the last probe: asleep, or away
    /// from this device's network (the router cannot tell which while it rests).
    pub fn home_silent(&self, host: &str) -> bool {
        let host = ps5upload_core::events::console_host(host);
        self.known_ip(&host).is_some()
            && lock(&self.state).get(&host).and_then(|s| s.lan_ok) == Some(false)
    }

    fn known_name(&self, host: &str) -> String {
        lock(&self.known)
            .iter()
            .find(|k| k.host == host)
            .map(|k| k.name.clone())
            .unwrap_or_default()
    }

    /// This device's own tailnet IPv4, from whichever way it has (for the console's allow-list).
    pub fn self_ip(&self) -> Option<Ipv4Addr> {
        match self.backend() {
            Backend::Builtin => self.node()?.status().ip,
            Backend::System => sys::detect().ipv4,
            Backend::None => None,
        }
    }

    /// "Test it now": send `host` through the tailnet for a while even though home answers
    /// (`secs` 0 ends it). Needs a tailnet way and the console's tailnet address.
    pub fn test_path(&self, host: &str, secs: u64) -> anyhow::Result<()> {
        let host = ps5upload_core::events::console_host(host);
        if secs > 0 {
            if self.backend() == Backend::None {
                anyhow::bail!("this device is not on Tailscale: sign in first");
            }
            if self.known_ip(&host).is_none() {
                anyhow::bail!("this PS5 is not on Tailscale yet: put it on Tailscale first");
            }
        }
        let until =
            (secs > 0).then(|| Instant::now() + Duration::from_secs(secs).min(TEST_PATH_MAX));
        {
            let mut st = lock(&self.state);
            let s = st.entry(host.clone()).or_default();
            s.forced_until = until;
        }
        self.note_path(&host);
        Ok(())
    }

    fn forced(&self, host: &str) -> bool {
        let mut st = lock(&self.state);
        let Some(s) = st.get_mut(host) else {
            return false;
        };
        match s.forced_until {
            Some(t) if Instant::now() < t => true,
            Some(_) => {
                s.forced_until = None;
                false
            }
            None => false,
        }
    }

    fn probe_all(&self) {
        let backend = self.backend();
        if backend == Backend::None {
            return;
        }
        let known: Vec<KnownConsole> = lock(&self.known).clone();
        for k in known {
            let ok = lan_answers(&k.host);
            // System mode, away: does its tailnet address answer (the app's own path)?
            let tail =
                (backend == Backend::System && !ok).then(|| tcp_answers(&k.ip.to_string(), 9120));
            {
                let mut st = lock(&self.state);
                let s = st.entry(k.host.clone()).or_default();
                s.lan_ok = Some(ok);
                s.checked = Some(Instant::now());
                if let Some(t) = tail {
                    s.tail_ok = Some(t);
                }
            }
            self.note_path(&k.host);
        }
    }

    /// One log line and one `tailnet` event when a console switches path; nothing otherwise.
    fn note_path(&self, host: &str) {
        let path = match self.decision(host) {
            Decision::Lan => "lan",
            Decision::Builtin(_) => "builtin",
            Decision::System(_) => "system",
        };
        // Once per console: the probe (a connect, for the system app) runs only until it worked.
        if path != "lan"
            && self.forced(host)
            && self.tested_at(host) == 0
            && self.tailnet_answers(host)
        {
            self.note_tested(host);
        }
        let prev = {
            let mut st = lock(&self.state);
            let s = st.entry(host.to_string()).or_default();
            let prev = s.path.replace(path);
            if prev != Some(path) {
                s.switched_ms = crate::now_ms();
            }
            prev
        };
        if prev.is_some_and(|p| p != path) || (prev.is_none() && path != "lan") {
            let name = self.known_name(host);
            crate::log_at!(
                Info,
                "tailnet",
                "tailnet: {host} {}",
                if path == "lan" {
                    "is back on the home network"
                } else {
                    "does not answer at home: using Tailscale"
                }
            );
            crate::event_bus::bus().publish_json(
                "tailnet",
                &json!({ "host": host, "name": name.split('.').next().unwrap_or(""), "path": path }),
            );
        }
    }

    fn tested_at(&self, host: &str) -> u64 {
        lock(&self.known)
            .iter()
            .find(|k| k.host == host)
            .map(|k| k.tested_at)
            .unwrap_or(0)
    }

    /// "Try Tailscale now" reached it: say so for good (the panel's step 3 stays done).
    fn note_tested(&self, host: &str) {
        let mut known = lock(&self.known);
        let mut changed = false;
        for k in known.iter_mut().filter(|k| k.host == host) {
            if k.tested_at == 0 {
                changed = true;
            }
            k.tested_at = crate::now_ms();
        }
        if changed {
            save(&self.dir, "consoles.json", &*known);
        }
    }

    /// Does the console answer through Tailscale now: online in the node's netmap, or (system
    /// app) its tailnet address takes a connection on 9120.
    fn tailnet_answers(&self, host: &str) -> bool {
        let Some(ip) = self.known_ip(host) else {
            return false;
        };
        match self.backend() {
            Backend::Builtin => self.online(Backend::Builtin, host, ip) == Some(true),
            Backend::System => tcp_answers(&ip.to_string(), 9120),
            Backend::None => false,
        }
    }

    /// Is the console online on the tailnet, as far as this device can tell?
    fn online(&self, backend: Backend, host: &str, ip: Ipv4Addr) -> Option<bool> {
        match backend {
            Backend::Builtin => {
                let node = self.node()?;
                Some(
                    node.status()
                        .peers
                        .iter()
                        // Online too: the console's node runs in its helper, so a console whose
                        // helper is down (but still home, Remote Play answering) is not taken for
                        // away.
                        .any(|p| p.ip == Some(ip) && p.online),
                )
            }
            Backend::System => {
                let probed = lock(&self.state).get(host).and_then(|s| s.tail_ok);
                let listed = sys::status().and_then(|st| {
                    st.peers
                        .iter()
                        .find(|p| p.ipv4 == Some(ip))
                        .map(|p| p.online)
                });
                listed.or(probed)
            }
            Backend::None => None,
        }
    }

    fn inputs(&self, backend: Backend, host: &str) -> Inputs {
        let ip = self.known_ip(host);
        let forced = self.forced(host);
        let mut lan_ok = lock(&self.state).get(host).and_then(|s| s.lan_ok);
        if lan_ok.is_none() && ip.is_some() && backend != Backend::None {
            // Never probed (the prober runs every few seconds): once, here.
            let ok = lan_answers(host);
            let mut st = lock(&self.state);
            let s = st.entry(host.to_string()).or_default();
            s.lan_ok = Some(ok);
            s.checked = Some(Instant::now());
            lan_ok = Some(ok);
        }
        let away = lan_ok == Some(false) || forced;
        Inputs {
            lan_ok,
            forced,
            tailnet_ip: ip,
            // Only asked when it matters: a home console never touches the peer list.
            online: match (away, ip) {
                (true, Some(ip)) => self.online(backend, host, ip),
                _ => None,
            },
        }
    }

    fn decision(&self, host: &str) -> Decision {
        let backend = self.backend();
        if backend == Backend::None {
            return Decision::Lan;
        }
        decide(backend, self.inputs(backend, host))
    }

    /// `GET /api/tailnet/route?addr=`: the decision for `host` and everything it rests on.
    pub fn explain(&self, host: &str) -> Value {
        let host = ps5upload_core::events::console_host(host);
        let backend = self.backend();
        let i = self.inputs(backend, &host);
        let d = if backend == Backend::None {
            Decision::Lan
        } else {
            decide(backend, i)
        };
        let (via, ms) = via_json(&ps5upload_core::route::via(&host));
        let st = lock(&self.state);
        let s = st.get(&host);
        let now_ms = crate::now_ms();
        let switched = s.map(|s| s.switched_ms).unwrap_or(0);
        json!({
            "host": host,
            "decision": match d { Decision::Lan => "lan", Decision::Builtin(_) => "builtin", Decision::System(_) => "system" },
            "via": via,
            "latency_ms": ms,
            "backend": backend.as_str(),
            "home": {
                "answers": i.lan_ok,
                "age_ms": s.and_then(|s| s.checked).map(|t| t.elapsed().as_millis() as u64),
            },
            "tailnet_ip": i.tailnet_ip.map(|ip| ip.to_string()),
            "tailnet_online": i.online,
            "testing": i.forced,
            "last_switch_ms": switched,
            "last_switch_ago_ms": if switched > 0 { now_ms.saturating_sub(switched) } else { 0 },
            "ports": ROUTED.iter().map(|(p, u)| json!({ "port": p, "udp": u })).collect::<Vec<_>>(),
        })
    }

    pub fn status_json(&self) -> Value {
        let s = lock(&self.settings).clone();
        let node = self.node();
        let st = node.as_ref().map(|n| n.status());
        let known = lock(&self.known).clone();
        let consoles: Vec<Value> = known
            .iter()
            .map(|k| {
                let (via, ms) = via_json(&ps5upload_core::route::via(&k.host));
                json!({ "host": k.host, "ip": k.ip.to_string(), "name": k.name, "via": via, "latency_ms": ms, "tested_at": k.tested_at })
            })
            .collect();
        let peers: Vec<Value> = st
            .as_ref()
            .map(|st| {
                st.peers
                    .iter()
                    .map(|p| {
                        json!({
                            "name": p.name,
                            "ip": p.ip.map(|i| i.to_string()).unwrap_or_default(),
                            "online": p.online,
                            "path": match p.path { tn::PathKind::Direct => "direct", tn::PathKind::Relayed => "relayed", tn::PathKind::Idle => "idle" },
                            "direct": p.direct.clone().unwrap_or_default(),
                            "derp_region": p.derp_region,
                            "latency_ms": p.latency_ms.unwrap_or(-1.0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let system = sys::detect();
        let name = st.as_ref().map(|s| s.name.clone()).unwrap_or_default();
        let asked = Self::effective_hostname(&s);
        json!({
            "ok": true,
            "enabled": s.enabled,
            "control_url": if s.control_url.is_empty() { DEFAULT_CONTROL_URL.to_string() } else { s.control_url.clone() },
            "hostname": s.hostname,
            // The name asked for when none is set: this device's own name + "-ps5u".
            "default_hostname": sys::default_device_name(),
            // Control made the name unique ("-1"): the name it gave, else empty.
            "renamed_to": if name.is_empty() { None } else { sys::renamed_by_control(&asked, &name) },
            // Signed in once and kept (off is then a pause), as against never signed in.
            "signed_in": self.dir.as_ref().is_some_and(|d| d.join("tailnet-state.json").exists()),
            "state": st.as_ref().map(|s| s.state.clone()).unwrap_or_else(|| "off".into()),
            "login_url": st.as_ref().map(|s| s.login_url.clone()).unwrap_or_default(),
            "ip": st.as_ref().and_then(|s| s.ip).map(|i| i.to_string()).unwrap_or_default(),
            "name": name,
            "home_derp": st.as_ref().map(|s| s.home_derp).unwrap_or(0),
            "derp_connected": st.as_ref().is_some_and(|s| s.derp_connected),
            "home_latency_ms": st.as_ref().and_then(|s| s.home_latency_ms).unwrap_or(-1.0),
            "key_expiry": st.as_ref().map(|s| s.key_expiry).unwrap_or(0),
            "error": lock(&self.last_error).clone(),
            "warning": lock(&self.last_warning).clone(),
            "peers": peers,
            "consoles": consoles,
            // How this device reaches the tailnet: "builtin", "system" or "none".
            "backend": self.backend().as_str(),
            // The system's Tailscale app on this device.
            "system": {
                "detected": system.detected,
                "ip": system.ipv4.map(|i| i.to_string()).unwrap_or_default(),
                "iface": system.iface,
                "choice": s.system_choice,
            },
        })
    }
}

/// ("lan" | "direct" | "relayed" | "system", latency ms or -1)
pub fn via_json(v: &Via) -> (&'static str, f64) {
    match v {
        Via::Lan => ("lan", -1.0),
        Via::TailnetDirect { latency_ms } => ("direct", latency_ms.unwrap_or(-1.0)),
        Via::TailnetRelayed => ("relayed", -1.0),
        Via::System => ("system", -1.0),
    }
}

impl Router for TailnetEngine {
    fn dial(&self, host: &str, port: u16, udp: bool) -> Option<SocketAddr> {
        if !ROUTED.contains(&(port, udp)) {
            return None;
        }
        let ip = match self.decision(host) {
            Decision::Lan => return None,
            Decision::System(ip) => return Some(SocketAddr::from((ip, port))),
            Decision::Builtin(ip) => ip,
        };
        let node = self.node()?;
        let cached = lock(&self.state)
            .get(host)
            .and_then(|s| s.maps.get(&(port, udp)).copied());
        let local = match cached {
            Some(p) => p,
            None => {
                let proto = if udp { tn::Proto::Udp } else { tn::Proto::Tcp };
                let p = node.map(proto, ip, port, 0).ok()?;
                lock(&self.state)
                    .entry(host.to_string())
                    .or_default()
                    .maps
                    .insert((port, udp), p);
                crate::log_at!(
                    Debug,
                    "tailnet",
                    "tailnet: {host}:{port} via 127.0.0.1:{p} -> {ip}"
                );
                p
            }
        };
        Some(SocketAddr::from(([127, 0, 0, 1], local)))
    }

    fn via(&self, host: &str) -> Via {
        let host = ps5upload_core::events::console_host(host);
        match self.decision(&host) {
            Decision::Lan => Via::Lan,
            Decision::System(_) => Via::System,
            Decision::Builtin(ip) => {
                let Some(node) = self.node() else {
                    return Via::Lan;
                };
                match node.status().peers.into_iter().find(|p| p.ip == Some(ip)) {
                    Some(p) if p.path == tn::PathKind::Direct => Via::TailnetDirect {
                        latency_ms: p.latency_ms,
                    },
                    _ => Via::TailnetRelayed,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_in(dir: &std::path::Path) -> TailnetEngine {
        let e = TailnetEngine::new(Some(dir.to_path_buf()));
        lock(&e.known).clear();
        e
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[test]
    fn rename_while_off_keeps_it_off_and_saves_the_name() {
        let tmp = tmp("rename");
        let e = engine_in(&tmp);
        let st = e
            .op("rename", "", "  Living room laptop  ", "", "")
            .unwrap();
        assert_eq!(st["hostname"], "Living room laptop");
        assert_eq!(st["enabled"], false);
        assert_eq!(st["signed_in"], false);
        // Kept for the next start.
        assert_eq!(
            engine_in(&tmp).settings.lock().unwrap().hostname,
            "Living room laptop"
        );
        assert!(e.op("rename", "", "   ", "", "").is_err());
        // The route has done these by the time it asks for the status (#768: "Try Tailscale
        // now" answered `unknown op "test_path"`).
        assert!(e.op("test_path", "", "", "", "").is_ok());
        assert!(e.op("remember", "", "", "", "").is_ok());
        assert!(e.op("rename", "", &"x".repeat(64), "", "").is_err());
        // A sign-in kept on disk reads as signed in (off is then a pause).
        std::fs::write(tmp.join("tailnet-state.json"), "{}").unwrap();
        assert_eq!(e.status_json()["signed_in"], true);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn pause_round_trip_survives_a_restart() {
        let tmp = tmp("pause");
        let e = engine_in(&tmp);
        // A sign-in on disk and enabled: a pause keeps the sign-in, and enabled=false is saved.
        std::fs::write(tmp.join("tailnet-state.json"), "{}").unwrap();
        lock(&e.settings).enabled = true;
        let st = e.op("disable", "", "", "", "").unwrap();
        assert_eq!(st["enabled"], false);
        assert_eq!(st["signed_in"], true);
        let again = engine_in(&tmp);
        assert!(!lock(&again.settings).enabled, "a restart stays paused");
        assert_eq!(again.status_json()["signed_in"], true);
        // Sign out forgets it.
        let st = again.op("logout", "", "", "", "").unwrap();
        assert_eq!(st["signed_in"], false);
        // Renew needs the node running.
        assert!(again.op("renew", "", "", "", "").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn system_choice_is_kept() {
        let tmp = tmp("choice");
        let e = engine_in(&tmp);
        assert!(e.op("system_choice", "", "", "", "maybe").is_err());
        let st = e.op("system_choice", "", "", "", "system").unwrap();
        assert_eq!(st["system"]["choice"], "system");
        assert_eq!(lock(&engine_in(&tmp).settings).system_choice, "system");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    const IP: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 9);

    fn at(lan: Option<bool>, ip: Option<Ipv4Addr>, online: Option<bool>) -> Inputs {
        Inputs {
            lan_ok: lan,
            forced: false,
            tailnet_ip: ip,
            online,
        }
    }

    #[test]
    fn home_first_always() {
        for b in [Backend::Builtin, Backend::System, Backend::None] {
            // Home answers: never the tailnet, whatever else is true.
            assert_eq!(
                decide(b, at(Some(true), Some(IP), Some(true))),
                Decision::Lan
            );
            // Not probed yet: home.
            assert_eq!(decide(b, at(None, Some(IP), Some(true))), Decision::Lan);
            // Away but no tailnet address known: home (nothing else to try).
            assert_eq!(decide(b, at(Some(false), None, Some(true))), Decision::Lan);
        }
    }

    #[test]
    fn away_goes_through_the_tailnet_this_device_has() {
        assert_eq!(
            decide(Backend::Builtin, at(Some(false), Some(IP), Some(true))),
            Decision::Builtin(IP)
        );
        // The built-in node needs the console online in its netmap.
        assert_eq!(
            decide(Backend::Builtin, at(Some(false), Some(IP), Some(false))),
            Decision::Lan
        );
        assert_eq!(
            decide(Backend::Builtin, at(Some(false), Some(IP), None)),
            Decision::Lan
        );
        // The system app: used unless something says the console is offline.
        assert_eq!(
            decide(Backend::System, at(Some(false), Some(IP), None)),
            Decision::System(IP)
        );
        assert_eq!(
            decide(Backend::System, at(Some(false), Some(IP), Some(false))),
            Decision::Lan
        );
        assert_eq!(
            decide(Backend::None, at(Some(false), Some(IP), Some(true))),
            Decision::Lan
        );
    }

    #[test]
    fn test_it_now_goes_through_the_tailnet_from_home() {
        let i = Inputs {
            lan_ok: Some(true),
            forced: true,
            tailnet_ip: Some(IP),
            online: Some(true),
        };
        assert_eq!(decide(Backend::Builtin, i), Decision::Builtin(IP));
        assert_eq!(decide(Backend::System, i), Decision::System(IP));
        assert_eq!(decide(Backend::None, i), Decision::Lan);
    }

    #[test]
    fn test_path_needs_a_way_and_an_address() {
        let tmp = tmp("testpath");
        let e = engine_in(&tmp);
        // Ending a test always works.
        assert!(e.test_path("192.0.2.10", 0).is_ok());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_console_added_away_then_met_at_home_is_one_known_console() {
        let tmp = tmp("placeholder");
        let e = engine_in(&tmp);
        // Added away by its tailnet address (no home address known yet).
        e.remember("100.64.0.9", IP, "den-ps5u");
        assert_eq!(e.known_hosts_for(IP), vec!["100.64.0.9"]);
        // "Try Tailscale now" worked once.
        e.note_tested("100.64.0.9");
        let tested = lock(&e.known)[0].tested_at;
        assert!(tested > 0);
        // Reached at home: the home address takes over, the name and test result kept.
        e.remember("192.168.1.20", IP, "");
        let known = lock(&e.known).clone();
        assert_eq!(known.len(), 1, "{known:?}");
        assert_eq!(known[0].host, "192.168.1.20");
        assert_eq!(known[0].name, "den-ps5u");
        assert_eq!(known[0].tested_at, tested);
        assert_eq!(e.known_name_for(IP), "den-ps5u");
        // Kept across a restart, and in the status.
        let again = TailnetEngine::new(Some(tmp.clone()));
        assert_eq!(lock(&again.known).clone(), known);
        let st = again.status_json();
        assert_eq!(st["consoles"][0]["tested_at"], tested);
        assert_eq!(st["consoles"][0]["host"], "192.168.1.20");
        // Remembered again unchanged: still one.
        again.remember("192.168.1.20", IP, "den-ps5u");
        assert_eq!(lock(&again.known).len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
