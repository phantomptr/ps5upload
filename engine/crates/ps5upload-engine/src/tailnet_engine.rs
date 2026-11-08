//! The engine's own tailnet node (Settings › Tailscale) and "use the tailnet when away".
//!
//! * Settings and keys live in `<data>/tailnet-app/` (`settings.json`, the core's
//!   `tailnet-state.json`). The auth key is used once and never stored.
//! * Consoles put on Tailscale ("Put this PS5 on Tailscale", `tailnet_api.rs`) are remembered
//!   with their tailnet address in `consoles.json`, keyed by their LAN address.
//! * A prober checks every remembered console's LAN address (TCP 9120) every few seconds while
//!   the node runs. When the LAN does not answer and the console is a peer on our tailnet,
//!   [`ps5upload_core::route`] sends the helper (9120), Remote Play (9295/TCP, 9296/UDP) and
//!   discovery (9302/UDP) through local mappings to its tailnet address.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ps5upload_core::route::{Router, Via};
use ps5upload_tailnet as tn;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_CONTROL_URL: &str = ps5upload_core::tailnet::DEFAULT_CONTROL_URL;
const PROBE_EVERY: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_millis(900);
/// The ports a console serves to the tailnet (payload/src/tailnet_node.c): (port, udp).
const ROUTED: &[(u16, bool)] = &[(9120, false), (9295, false), (9296, true), (9302, true)];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    #[serde(default)]
    pub control_url: String,
    #[serde(default)]
    pub hostname: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownConsole {
    /// The console's LAN address as the app knows it.
    pub host: String,
    pub ip: Ipv4Addr,
    #[serde(default)]
    pub name: String,
}

#[derive(Default)]
struct ConsoleState {
    /// None: not probed yet.
    lan_ok: Option<bool>,
    checked: Option<Instant>,
    maps: HashMap<(u16, bool), u16>,
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
        let e = TailnetEngine {
            settings: Mutex::new(load(&dir, "settings.json").unwrap_or_default()),
            known: Mutex::new(load(&dir, "consoles.json").unwrap_or_default()),
            dir,
            node: Mutex::new(None),
            state: Mutex::new(HashMap::new()),
            last_error: Mutex::new(String::new()),
            last_warning: Mutex::new(String::new()),
        };
        Arc::new(e)
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

fn lan_answers(host: &str) -> bool {
    let Ok(addrs) = (host, 9120).to_socket_addrs() else {
        return false;
    };
    addrs
        .into_iter()
        .any(|a| TcpStream::connect_timeout(&a, PROBE_TIMEOUT).is_ok())
}

impl TailnetEngine {
    fn node(&self) -> Option<Arc<tn::Node>> {
        self.node.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn start(&self, authkey: &str) -> anyhow::Result<()> {
        let mut slot = self.node.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_some() {
            return Ok(());
        }
        let Some(dir) = self.dir.clone() else {
            anyhow::bail!("no data folder for the tailnet keys; set PS5UPLOAD_DATA_DIR");
        };
        let s = self
            .settings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let cfg = tn::Config {
            state_dir: dir,
            control_url: if s.control_url.is_empty() {
                DEFAULT_CONTROL_URL.to_string()
            } else {
                s.control_url.clone()
            },
            auth_key: authkey.to_string(),
            hostname: s.hostname.clone(),
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
                    engine()
                        .last_error
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clear();
                }
            }
            tn::Event::Path(p) => crate::log_at!(Info, "tailnet", "tailnet: {p}"),
            tn::Event::Warning(w) => {
                crate::log_at!(Warn, "tailnet", "tailnet: {w}");
                *engine()
                    .last_warning
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = w;
            }
            tn::Event::Error(w) => {
                crate::log_at!(Warn, "tailnet", "tailnet: {w}");
                *engine()
                    .last_error
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = w;
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
        self.last_error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        self.last_warning
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    fn stop(&self) {
        let node = self.node.lock().unwrap_or_else(|p| p.into_inner()).take();
        drop(node);
        for st in self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values_mut()
        {
            st.maps.clear();
        }
    }

    /// enable / disable / logout / rename from Settings › Tailscale.
    pub fn op(
        &self,
        op: &str,
        control_url: &str,
        hostname: &str,
        authkey: &str,
    ) -> anyhow::Result<Value> {
        match op {
            "enable" => {
                let changed = {
                    let mut s = self.settings.lock().unwrap_or_else(|p| p.into_inner());
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
                    save(&self.dir, "settings.json", &*s);
                    changed
                };
                if changed || !authkey.is_empty() {
                    self.stop();
                }
                self.start(authkey.trim())?;
            }
            "disable" | "logout" => {
                self.stop();
                let mut s = self.settings.lock().unwrap_or_else(|p| p.into_inner());
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
                    let mut s = self.settings.lock().unwrap_or_else(|p| p.into_inner());
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
            "status" | "" => {}
            "remember" => {}
            other => anyhow::bail!("unknown op {other:?}"),
        }
        Ok(self.status_json())
    }

    /// Where the engine would dial `host:port` now: a local mapping when away, else None.
    pub fn route(&self, host: &str, port: u16, udp: bool) -> Option<SocketAddr> {
        Router::dial(self, &ps5upload_core::events::console_host(host), port, udp)
    }

    /// A console put on Tailscale: remember its tailnet address for when it is away.
    pub fn remember(&self, host: &str, ip: Ipv4Addr, name: &str) {
        let host = ps5upload_core::events::console_host(host);
        let mut known = self.known.lock().unwrap_or_else(|p| p.into_inner());
        if known
            .iter()
            .any(|k| k.host == host && k.ip == ip && k.name == name)
        {
            return;
        }
        let moved = known.iter().any(|k| k.host == host && k.ip != ip);
        known.retain(|k| k.host != host);
        known.push(KnownConsole {
            host: host.clone(),
            ip,
            name: name.to_string(),
        });
        save(&self.dir, "consoles.json", &*known);
        drop(known);
        if moved {
            // A new tailnet address: the mappings made for the old one would dial nobody.
            let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(s) = st.get_mut(&host) {
                s.maps.clear();
            }
        }
    }

    fn known_ip(&self, host: &str) -> Option<Ipv4Addr> {
        let known = self.known.lock().unwrap_or_else(|p| p.into_inner());
        known.iter().find(|k| k.host == host).map(|k| k.ip)
    }

    fn probe_all(&self) {
        if self.node().is_none() {
            return;
        }
        let hosts: Vec<String> = self
            .known
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|k| k.host.clone())
            .collect();
        for h in hosts {
            let ok = lan_answers(&h);
            let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let s = st.entry(h.clone()).or_default();
            if s.lan_ok != Some(ok) {
                crate::log_at!(
                    Info,
                    "tailnet",
                    "tailnet: {h} {}",
                    if ok {
                        "answers on the LAN"
                    } else {
                        "is away: using the tailnet"
                    }
                );
            }
            s.lan_ok = Some(ok);
            s.checked = Some(Instant::now());
        }
    }

    /// Is `host` away (its LAN address silent) and on our tailnet? Its tailnet address.
    fn away(&self, host: &str) -> Option<(Arc<tn::Node>, Ipv4Addr)> {
        let node = self.node()?;
        let ip = self.known_ip(host)?;
        let probed = {
            let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            st.get(host).and_then(|s| s.lan_ok)
        };
        let lan_ok = match probed {
            Some(ok) => ok,
            None => {
                // Never probed (the prober runs every few seconds): once, here.
                let ok = lan_answers(host);
                let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let s = st.entry(host.to_string()).or_default();
                s.lan_ok = Some(ok);
                s.checked = Some(Instant::now());
                ok
            }
        };
        if lan_ok {
            return None;
        }
        let status = node.status();
        status
            .peers
            .iter()
            // Online too: the console's node runs in its helper, so a console whose helper is down
            // (but still home, its Remote Play answering) is not taken for away.
            .any(|p| p.ip == Some(ip) && p.online)
            .then_some((node, ip))
    }

    pub fn status_json(&self) -> Value {
        let s = self
            .settings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let node = self.node();
        let st = node.as_ref().map(|n| n.status());
        let known = self.known.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let consoles: Vec<Value> = known
            .iter()
            .map(|k| {
                let (via, ms) = via_json(&ps5upload_core::route::via(&k.host));
                json!({ "host": k.host, "ip": k.ip.to_string(), "name": k.name, "via": via, "latency_ms": ms })
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
        json!({
            "ok": true,
            "enabled": s.enabled,
            "control_url": if s.control_url.is_empty() { DEFAULT_CONTROL_URL.to_string() } else { s.control_url.clone() },
            "hostname": s.hostname,
            // Signed in once and kept (off is then a pause), as against never signed in.
            "signed_in": self.dir.as_ref().is_some_and(|d| d.join("tailnet-state.json").exists()),
            "state": st.as_ref().map(|s| s.state.clone()).unwrap_or_else(|| "off".into()),
            "login_url": st.as_ref().map(|s| s.login_url.clone()).unwrap_or_default(),
            "ip": st.as_ref().and_then(|s| s.ip).map(|i| i.to_string()).unwrap_or_default(),
            "name": st.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
            "home_derp": st.as_ref().map(|s| s.home_derp).unwrap_or(0),
            "derp_connected": st.as_ref().is_some_and(|s| s.derp_connected),
            "home_latency_ms": st.as_ref().and_then(|s| s.home_latency_ms).unwrap_or(-1.0),
            "key_expiry": st.as_ref().map(|s| s.key_expiry).unwrap_or(0),
            "error": self.last_error.lock().unwrap_or_else(|p| p.into_inner()).clone(),
            "warning": self.last_warning.lock().unwrap_or_else(|p| p.into_inner()).clone(),
            "peers": peers,
            "consoles": consoles,
        })
    }
}

/// ("lan" | "direct" | "relayed", latency ms or -1)
pub fn via_json(v: &Via) -> (&'static str, f64) {
    match v {
        Via::Lan => ("lan", -1.0),
        Via::TailnetDirect { latency_ms } => ("direct", latency_ms.unwrap_or(-1.0)),
        Via::TailnetRelayed => ("relayed", -1.0),
    }
}

impl Router for TailnetEngine {
    fn dial(&self, host: &str, port: u16, udp: bool) -> Option<SocketAddr> {
        if !ROUTED.contains(&(port, udp)) {
            return None;
        }
        let (node, ip) = self.away(host)?;
        let cached = {
            let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            st.get(host).and_then(|s| s.maps.get(&(port, udp)).copied())
        };
        let local = match cached {
            Some(p) => p,
            None => {
                let proto = if udp { tn::Proto::Udp } else { tn::Proto::Tcp };
                let p = node.map(proto, ip, port, 0).ok()?;
                let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
                st.entry(host.to_string())
                    .or_default()
                    .maps
                    .insert((port, udp), p);
                crate::log_at!(
                    Info,
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
        let Some((node, ip)) = self.away(&host) else {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_in(dir: &std::path::Path) -> TailnetEngine {
        let dir = Some(dir.to_path_buf());
        TailnetEngine {
            settings: Mutex::new(load(&dir, "settings.json").unwrap_or_default()),
            known: Mutex::new(Vec::new()),
            dir,
            node: Mutex::new(None),
            state: Mutex::new(HashMap::new()),
            last_error: Mutex::new(String::new()),
            last_warning: Mutex::new(String::new()),
        }
    }

    #[test]
    fn rename_while_off_keeps_it_off_and_saves_the_name() {
        let tmp = std::env::temp_dir().join(format!("tn-rename-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let e = engine_in(&tmp);
        let st = e.op("rename", "", "  Living room laptop  ", "").unwrap();
        assert_eq!(st["hostname"], "Living room laptop");
        assert_eq!(st["enabled"], false);
        assert_eq!(st["signed_in"], false);
        // Kept for the next start.
        assert_eq!(
            engine_in(&tmp).settings.lock().unwrap().hostname,
            "Living room laptop"
        );
        assert!(e.op("rename", "", "   ", "").is_err());
        assert!(e.op("rename", "", &"x".repeat(64), "").is_err());
        // A sign-in kept on disk reads as signed in (off is then a pause).
        std::fs::write(tmp.join("tailnet-state.json"), "{}").unwrap();
        assert_eq!(e.status_json()["signed_in"], true);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
