//! The console's USB Ethernet adapter (EXPERIMENTAL), the engine's side: which consoles have
//! one serving, whether this computer reaches it, and whether transfers use it.
//!
//! * The helper drives the adapter and serves its AVA1 port (9120) on the adapter's own address
//!   (`ps5upload_core::usbnet`). The engine learns that address from the helper: node.status's
//!   `usbnet` brief where the helper sends one, and the `usbnet` method's status (the usbnet
//!   GET route, and a read of its own after a status check, every [`ASK_SERVING`] while the
//!   adapter serves and every [`ASK_IDLE`] while it does not; an older helper is asked again
//!   after [`ASK_OLD`]).
//! * A quick TCP connect to that address ([`PROBE_TIMEOUT`]) when it is new or changed, and on
//!   the prober's cadence ([`PROBE_EVERY`]) while the adapter serves, says whether this
//!   computer reaches it. A computer on another network, or on a network the adapter's cable
//!   does not reach, never uses it.
//! * [`decide`]: the router dials the helper's AVA1 port at the adapter while its probe passes
//!   and the console is reached at home (the tailnet's decision is the LAN): never in place of
//!   a tailnet route. Remote Play and every other port keep the console's own address, and
//!   Minion's 9180 on the adapter is for a browser, never dialled by the engine.
//! * A handshake through the adapter that fails, or a session through it that ends under its
//!   users, lets it go at once (the next attempt dials the console's own address) and holds it
//!   back for a while ([`hold_for`], doubling to [`HOLD_MAX`]); a handshake through it that
//!   works clears that. Nothing about it is ever an error the user sees: the fallback is silent.
//! * When the choice changes, the AVA1 pool drops the console's cached session once nothing
//!   uses it (`Pool::forget_idle_if_moved`), so the next handshake takes the new path; a
//!   running transfer keeps the session it has. One log line and one `usbnet` event per change.
//! * All of it is in memory: `GET /api/ps5/usbnet` adds `route` to the helper's answer.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use ps5upload_core::route::UsbLink;
use ps5upload_core::usbnet::{self as un, Brief, Seen};
use serde_json::{json, Value};

/// The one port the engine sends through the adapter: the helper's AVA1 port.
pub const ROUTED_PORT: u16 = 9120;
pub const PROBE_EVERY: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_millis(600);
/// How long an adapter that failed is held back the first time, and at most.
const HOLD_FIRST: Duration = Duration::from_secs(10);
pub const HOLD_MAX: Duration = Duration::from_secs(5 * 60);
/// How often the engine reads the helper's `usbnet` status itself, after a status check.
pub const ASK_SERVING: Duration = Duration::from_secs(15);
pub const ASK_IDLE: Duration = Duration::from_secs(60);
pub const ASK_OLD: Duration = Duration::from_secs(10 * 60);

/// What the router knows about one console's adapter at a moment: the pure input of
/// [`decide`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Inputs {
    /// The console is reached at home (the tailnet's decision is the LAN).
    pub home: bool,
    /// The adapter's AVA1 address while it serves.
    pub addr: Option<SocketAddr>,
    /// This computer's last probe of it (None: not probed yet).
    pub reachable: Option<bool>,
    /// Held back after a failure.
    pub held: bool,
}

/// The rule: the adapter while it serves, this computer reaches it, the console is at home and
/// no failure holds it back; the console's own address otherwise.
pub fn decide(i: Inputs) -> Option<SocketAddr> {
    if i.home && i.reachable == Some(true) && !i.held {
        i.addr
    } else {
        None
    }
}

/// Where to dial `port` given the decision: only the helper's AVA1 port over TCP is routed.
pub fn dial_for(port: u16, udp: bool, preferred: Option<SocketAddr>) -> Option<SocketAddr> {
    if udp || port != ROUTED_PORT {
        return None;
    }
    preferred
}

/// How long an adapter is held back after `fails` failures in a row (1 or more).
pub fn hold_for(fails: u32) -> Duration {
    let doublings = fails.saturating_sub(1).min(16);
    HOLD_FIRST.saturating_mul(1 << doublings).min(HOLD_MAX)
}

#[derive(Clone, Debug, Default)]
struct State {
    /// The adapter while it serves.
    brief: Option<Brief>,
    /// It served at least once (this run): the console has two links, so jobs say which.
    seen: bool,
    reachable: Option<bool>,
    checked: Option<Instant>,
    fails: u32,
    held_until: Option<Instant>,
    /// The choice last told (log line, event, pool).
    preferred: bool,
    /// The engine's own `usbnet` read: when, and when the next is due.
    asked: Option<Instant>,
    ask_every: Duration,
    /// node.status carries the brief: no read of its own is needed.
    in_status: bool,
}

impl State {
    fn addr(&self) -> Option<SocketAddr> {
        self.brief
            .as_ref()
            .map(|b| SocketAddr::from((b.ip, b.port)))
    }

    fn held(&self, now: Instant) -> bool {
        self.held_until.is_some_and(|t| now < t)
    }

    fn inputs(&self, home: bool, now: Instant) -> Inputs {
        Inputs {
            home,
            addr: self.addr(),
            reachable: self.reachable,
            held: self.held(now),
        }
    }
}

/// Every console's adapter, keyed by the console's bare host (`events::console_host`).
#[derive(Default)]
pub struct Table {
    m: Mutex<HashMap<String, State>>,
}

fn host_key(console: &str) -> String {
    ps5upload_core::events::console_host(console)
}

impl Table {
    /// Every console's adapter for a bug report (`/api/diag/features`): states and the chip, never
    /// a console's or the adapter's address.
    pub fn diag(&self) -> Value {
        let now = Instant::now();
        let m = self.m.lock().unwrap_or_else(|p| p.into_inner());
        let consoles: Vec<Value> = m
            .values()
            .map(|st| {
                json!({
                    "serving": st.brief.is_some(),
                    "chip": st.brief.as_ref().map(|b| b.chip.clone()),
                    "mbps": st.brief.as_ref().map(|b| b.mbps),
                    "seen": st.seen,
                    "reachable": st.reachable,
                    "fails": st.fails,
                    "held": st.held(now),
                    "preferred": st.preferred,
                })
            })
            .collect();
        json!({ "consoles": consoles })
    }

    fn with<R>(&self, host: &str, f: impl FnOnce(&mut State) -> R) -> R {
        let mut m = self.m.lock().unwrap_or_else(|p| p.into_inner());
        f(m.entry(host_key(host)).or_default())
    }

    fn get<R>(&self, host: &str, f: impl FnOnce(&State) -> R) -> Option<R> {
        let m = self.m.lock().unwrap_or_else(|p| p.into_inner());
        m.get(&host_key(host)).map(f)
    }

    /// What a status said. Returns the adapter's address when it is new or moved (to probe
    /// now); `Seen::Absent` changes nothing.
    pub fn learn(&self, host: &str, seen: &Seen) -> Option<SocketAddr> {
        let brief = match seen {
            Seen::Absent => return None,
            Seen::NotServing => None,
            Seen::Serving(b) => Some(b.clone()),
        };
        self.with(host, |s| {
            let before = s.addr();
            s.seen |= brief.is_some();
            s.brief = brief;
            let now = s.addr();
            if now != before {
                // A new address says nothing of the old one's probe or failures.
                s.reachable = None;
                s.checked = None;
                s.fails = 0;
                s.held_until = None;
            }
            now.filter(|_| now != before)
        })
    }

    /// A probe of `addr` answered or not (ignored when the adapter moved meanwhile).
    pub fn probed(&self, host: &str, addr: SocketAddr, ok: bool) {
        self.with(host, |s| {
            if s.addr() == Some(addr) {
                s.reachable = Some(ok);
                s.checked = Some(Instant::now());
            }
        });
    }

    /// A connection through `to` worked or failed (`Router::dialed`): only the adapter's own
    /// address counts. A failure lets it go now and holds it back for [`hold_for`].
    pub fn dialed(&self, host: &str, to: SocketAddr, ok: bool, now: Instant) {
        self.with(host, |s| {
            if s.addr() != Some(to) {
                return;
            }
            if ok {
                s.fails = 0;
                s.held_until = None;
            } else {
                s.fails = s.fails.saturating_add(1);
                s.reachable = Some(false);
                s.checked = Some(now);
                s.held_until = Some(now + hold_for(s.fails));
            }
        });
    }

    /// Where the router dials the console's AVA1 port now (None: its own address).
    pub fn pick(&self, host: &str, home: bool, now: Instant) -> Option<SocketAddr> {
        self.get(host, |s| decide(s.inputs(home, now))).flatten()
    }

    pub fn usb(&self, host: &str) -> Option<UsbLink> {
        self.get(host, |s| {
            s.seen.then(|| UsbLink {
                addr: s.addr(),
                mbps: s.brief.as_ref().map_or(0, |b| b.mbps),
                chip: s.brief.as_ref().map_or(String::new(), |b| b.chip.clone()),
            })
        })
        .flatten()
    }

    /// The consoles whose adapter serves: (host, its address).
    fn serving(&self) -> Vec<(String, SocketAddr)> {
        let m = self.m.lock().unwrap_or_else(|p| p.into_inner());
        m.iter()
            .filter_map(|(h, s)| s.addr().map(|a| (h.clone(), a)))
            .collect()
    }

    /// The consoles that ever had one (the pool may hold a session through it).
    fn seen(&self) -> Vec<String> {
        let m = self.m.lock().unwrap_or_else(|p| p.into_inner());
        m.iter()
            .filter(|(_, s)| s.seen)
            .map(|(h, _)| h.clone())
            .collect()
    }

    /// Records the choice now; `Some(new)` when it changed.
    fn settle(&self, host: &str, home: bool, now: Instant) -> Option<bool> {
        self.with(host, |s| {
            let p = decide(s.inputs(home, now)).is_some();
            (p != s.preferred).then(|| {
                s.preferred = p;
                p
            })
        })
    }

    /// The engine's own `usbnet` read is due (and is marked as started).
    fn ask_due(&self, host: &str, now: Instant) -> bool {
        self.with(host, |s| {
            let due = !s.in_status && s.asked.is_none_or(|t| now >= t + s.ask_every);
            if due {
                s.asked = Some(now);
                s.ask_every = ASK_IDLE;
            }
            due
        })
    }

    fn asked(&self, host: &str, every: Duration) {
        self.with(host, |s| s.ask_every = every);
    }

    /// The engine's view for the usbnet route: `{reachable, preferred, checked_ms_ago}`.
    pub fn route_json(&self, host: &str, home: bool, now: Instant) -> Value {
        let s = self.get(host, Clone::clone).unwrap_or_default();
        json!({
            "reachable": s.reachable == Some(true),
            "preferred": decide(s.inputs(home, now)).is_some(),
            "checked_ms_ago": s.checked.map(|t| now.saturating_duration_since(t).as_millis() as u64),
        })
    }
}

pub fn table() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(Table::default)
}

fn home(host: &str) -> bool {
    crate::tailnet_engine::engine().decision(&host_key(host))
        == crate::tailnet_engine::Decision::Lan
}

/// The router's question (`TailnetEngine::dial` while the console is at home).
pub fn dial(host: &str, port: u16, udp: bool) -> Option<SocketAddr> {
    dial_for(port, udp, table().pick(host, true, Instant::now()))
}

fn probe(addr: SocketAddr) -> bool {
    TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok()
}

/// The engine's runtime, for the pool calls the prober's threads make (its sessions' tasks run
/// there).
static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    match RUNTIME.get() {
        Some(h) => h.block_on(f),
        None => ps5upload_ava1::block_on(f),
    }
}

/// At engine start, on its runtime (after the router is installed, which
/// `tailnet_engine::boot` does whether Tailscale is on or not): the prober.
pub fn boot() {
    if let Ok(h) = tokio::runtime::Handle::try_current() {
        let _ = RUNTIME.set(h);
    }
    let _ = std::thread::Builder::new()
        .name("usbnet-prober".into())
        .spawn(|| loop {
            probe_all();
            std::thread::sleep(PROBE_EVERY);
        });
}

fn probe_all() {
    let t = table();
    for (host, addr) in t.serving() {
        t.probed(&host, addr, probe(addr));
    }
    for host in t.seen() {
        settle(&host);
    }
}

/// The choice for `host` now: one log line and one event when it changed, and the pool's
/// cached session dropped when it was made on the other path and nothing uses it (tried again
/// every tick until it can be).
fn settle(host: &str) {
    let t = table();
    if let Some(on) = t.settle(host, home(host), Instant::now()) {
        let u = t.usb(host);
        let (addr, chip, mbps) = u
            .as_ref()
            .map(|u| (u.addr, u.chip.clone(), u.mbps))
            .unwrap_or_default();
        if on {
            crate::log_at!(
                Info,
                "usbnet",
                "usbnet: {host}: transfers go through its USB adapter at {} ({chip}, {mbps} Mb/s)",
                addr.map(|a| a.to_string()).unwrap_or_default()
            );
        } else {
            crate::log_at!(
                Info,
                "usbnet",
                "usbnet: {host}: transfers use its own address"
            );
        }
        crate::event_bus::bus().publish_json(
            "usbnet",
            &json!({ "host": host, "link": if on { "usb" } else { "console" }, "mbps": if on { mbps } else { 0 } }),
        );
    }
    let host = host.to_string();
    block_on(async move {
        ps5upload_ava1::pool::pool()
            .forget_idle_if_moved(&host)
            .await
    });
}

/// Something new about `host`'s adapter: probe a new address at once (off the caller's
/// thread), then settle the choice.
fn learned(host: &str, seen: &Seen) {
    let t = table();
    let before = t.get(host, State::addr).flatten();
    let fresh = t.learn(host, seen);
    if t.get(host, State::addr).flatten() == before {
        // Nothing moved: the prober's next tick keeps it fresh.
        return;
    }
    let host = host_key(host);
    let _ = std::thread::Builder::new()
        .name("usbnet-probe".into())
        .spawn(move || {
            if let Some(addr) = fresh {
                table().probed(&host, addr, probe(addr));
            }
            settle(&host);
        });
}

/// The helper's `usbnet` answer for `host` (the route's, or the engine's own read).
pub fn note_answer(host: &str, answer: &Value) {
    let seen = un::from_status(answer);
    let every = if matches!(seen, Seen::Serving(_)) {
        ASK_SERVING
    } else {
        ASK_IDLE
    };
    table().asked(host, every);
    learned(host, &seen);
}

/// After a status check that answered (`lib.rs::ps5_status`): node.status's brief where the
/// helper sends one; else the engine's own `usbnet` read when one is due (in the background).
pub fn after_status(console: &str, status: &Value) {
    let seen = un::from_node_status(status);
    if seen != Seen::Absent {
        table().with(console, |s| s.in_status = true);
        learned(console, &seen);
        return;
    }
    if !table().ask_due(console, Instant::now()) {
        return;
    }
    let console = console.to_string();
    let _ = std::thread::Builder::new()
        .name("usbnet-ask".into())
        .spawn(move || match un::console(&console, &un::Op::Status) {
            Ok(Some(v)) => note_answer(&console, &v),
            // An older helper: ask again much later (it may be replaced by then).
            Ok(None) => table().asked(&console, ASK_OLD),
            Err(e) => crate::log_at!(
                Debug,
                "usbnet",
                "usbnet: {console}: status read failed ({e:#})"
            ),
        });
}

/// `GET /api/ps5/usbnet`'s `route`.
pub fn route_json(host: &str) -> Value {
    table().route_json(host, home(host), Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: &str = "192.0.2.40";

    fn brief(ip: &str) -> Seen {
        Seen::Serving(Brief {
            ip: ip.parse().unwrap(),
            port: 9120,
            mbps: 2500,
            chip: "RTL8156B".into(),
        })
    }

    fn adapter() -> SocketAddr {
        "198.51.100.20:9120".parse().unwrap()
    }

    #[test]
    fn the_adapter_is_used_only_at_home_while_it_answers() {
        let base = Inputs {
            home: true,
            addr: Some(adapter()),
            reachable: Some(true),
            held: false,
        };
        assert_eq!(decide(base), Some(adapter()));
        // Not reached from this computer, or not probed yet.
        for reachable in [Some(false), None] {
            assert_eq!(decide(Inputs { reachable, ..base }), None);
        }
        // Away: the tailnet route stands, never the adapter.
        assert_eq!(
            decide(Inputs {
                home: false,
                ..base
            }),
            None
        );
        // Held back after a failure.
        assert_eq!(decide(Inputs { held: true, ..base }), None);
        // Not serving.
        assert_eq!(decide(Inputs { addr: None, ..base }), None);
    }

    #[test]
    fn only_the_helpers_ava1_port_is_routed() {
        let p = Some(adapter());
        assert_eq!(dial_for(9120, false, p), p);
        // Minion on the adapter is a browser's; Remote Play and UDP keep the console's address.
        assert_eq!(dial_for(9180, false, p), None);
        assert_eq!(dial_for(9295, false, p), None);
        assert_eq!(dial_for(9120, true, p), None);
        assert_eq!(dial_for(9120, false, None), None);
    }

    #[test]
    fn a_failure_falls_back_at_once_and_holds_it_back_for_longer_each_time() {
        let t = Table::default();
        let now = Instant::now();
        assert_eq!(t.learn(AT, &brief("198.51.100.20")), Some(adapter()));
        // Not probed yet: the console's own address.
        assert_eq!(t.pick(AT, true, now), None);
        t.probed(AT, adapter(), true);
        assert_eq!(t.pick(AT, true, now), Some(adapter()));
        assert_eq!(t.pick(AT, false, now), None, "away");
        // A dial elsewhere (the console's own address) says nothing about the adapter.
        t.dialed(AT, "192.0.2.40:9120".parse().unwrap(), false, now);
        assert_eq!(t.pick(AT, true, now), Some(adapter()));
        // A handshake through it failed: the next attempt uses the console's address...
        t.dialed(AT, adapter(), false, now);
        assert_eq!(t.pick(AT, true, now), None);
        // ...even once a probe answers again, until the hold ends.
        t.probed(AT, adapter(), true);
        assert_eq!(t.pick(AT, true, now), None);
        assert_eq!(t.pick(AT, true, now + hold_for(1)), Some(adapter()));
        // A second failure in a row holds it back twice as long; one that works clears it.
        t.dialed(AT, adapter(), false, now);
        t.probed(AT, adapter(), true);
        assert_eq!(t.pick(AT, true, now + hold_for(1)), None);
        assert_eq!(t.pick(AT, true, now + hold_for(2)), Some(adapter()));
        t.dialed(AT, adapter(), true, now);
        assert_eq!(t.pick(AT, true, now), Some(adapter()));
        assert_eq!(hold_for(1), HOLD_FIRST);
        assert_eq!(hold_for(2), HOLD_FIRST * 2);
        assert_eq!(hold_for(40), HOLD_MAX);
    }

    #[test]
    fn what_the_helper_says_moves_the_choice() {
        let t = Table::default();
        let now = Instant::now();
        // An older helper says nothing: nothing is known, and jobs show no link.
        assert_eq!(t.learn(AT, &Seen::Absent), None);
        assert_eq!(t.usb(AT), None);
        t.learn(AT, &brief("198.51.100.20"));
        t.probed(AT, adapter(), true);
        assert_eq!(t.pick(AT, true, now), Some(adapter()));
        let u = t.usb(AT).unwrap();
        assert_eq!(
            (u.addr, u.mbps, u.chip.as_str()),
            (Some(adapter()), 2500, "RTL8156B")
        );
        // The same brief again changes nothing (no new probe).
        assert_eq!(t.learn(AT, &brief("198.51.100.20")), None);
        assert_eq!(t.pick(AT, true, now), Some(adapter()));
        // Unplugged: not used, but the console still has two links to tell apart.
        t.learn(AT, &Seen::NotServing);
        assert_eq!(t.pick(AT, true, now), None);
        assert_eq!(t.usb(AT).unwrap().addr, None);
        // A new address is probed afresh; a probe of the old one counts for nothing.
        let moved: SocketAddr = "198.51.100.21:9120".parse().unwrap();
        assert_eq!(t.learn(AT, &brief("198.51.100.21")), Some(moved));
        t.probed(AT, adapter(), true);
        assert_eq!(t.pick(AT, true, now), None);
        t.probed(AT, moved, true);
        assert_eq!(t.pick(AT, true, now), Some(moved));
        // The host is the console's bare host, whatever form the address came in.
        assert_eq!(t.pick("192.0.2.40:9120", true, now), Some(moved));
    }

    #[test]
    fn the_choice_is_told_once_per_change() {
        let t = Table::default();
        let now = Instant::now();
        t.learn(AT, &brief("198.51.100.20"));
        assert_eq!(t.settle(AT, true, now), None, "not probed: unchanged");
        t.probed(AT, adapter(), true);
        assert_eq!(t.settle(AT, true, now), Some(true));
        assert_eq!(t.settle(AT, true, now), None);
        assert_eq!(t.settle(AT, false, now), Some(false));
        let v = t.route_json(AT, true, now);
        assert_eq!(v["reachable"], true);
        assert_eq!(v["preferred"], true);
        assert!(v["checked_ms_ago"].is_u64());
        let none = t.route_json("192.0.2.41", true, now);
        assert_eq!(none["reachable"], false);
        assert!(none["checked_ms_ago"].is_null());
    }

    #[test]
    fn the_engine_asks_the_helper_itself_only_when_due() {
        let t = Table::default();
        let now = Instant::now();
        assert!(t.ask_due(AT, now));
        assert!(!t.ask_due(AT, now + Duration::from_secs(1)));
        t.asked(AT, ASK_SERVING);
        assert!(t.ask_due(AT, now + ASK_SERVING));
        t.asked(AT, ASK_OLD);
        assert!(!t.ask_due(AT, now + ASK_SERVING + ASK_IDLE));
        // A helper whose node.status carries the brief is never asked.
        t.with(AT, |s| s.in_status = true);
        assert!(!t.ask_due(AT, now + ASK_OLD * 3));
    }
}
