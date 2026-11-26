//! A stream install needs the console to reach this engine (#942).
//!
//! The console fetches the package from `http://<this computer>:<port>/pkg-host/…`. When it
//! cannot (a firewall dropping the connection is the usual cause), Sony's install call on some
//! firmwares does not come back with an error for many minutes: the installer daemon stays in
//! the call, this engine waited up to its 15-minute timeout, and the app showed "0%" with no
//! word of why. Three things here end that:
//!
//! - **The address** ([`choose_reachable`], [`alternatives`]). The engine tells the console its
//!   own address as the routing table gives it for the console. On a computer with two adapters
//!   on the console's network (a cable and Wi-Fi, one of them a Public network Windows blocks)
//!   that can be the one the console cannot get in on. The helper is asked to connect to it;
//!   if it cannot, the computer's other addresses on the console's network are tried and the
//!   first the console reaches is the one it is given.
//! - **The check** (the same probe). When the console reaches none of them, the install fails
//!   at once with the reason, before Sony is handed anything: nothing is left stuck on the
//!   console and Upload & install works right away. An answer that cannot be had (an older
//!   helper, a call that fails) never blocks an install.
//! - **The watchdog** ([`watch_handover`]). Once the link is handed over, the console asks for
//!   the package within seconds. When no request has come after [`NO_FETCH_GIVE_UP`], waiting
//!   stops and the job fails with the reason. The call left behind on the console is followed
//!   to its end in the background ([`AbandonedCalls`]): the next install to that console waits
//!   for it, since the daemon runs one Sony call at a time, and the daemon is recycled when
//!   Sony's answer says its network state is spoilt.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ps5upload_core::diagnostics::NetReach;
use ps5upload_core::installer_client as ic;
use serde::{Deserialize, Serialize};

/// How long the console's connect to the address it would be given may take.
pub const REACH_TIMEOUT_MS: u32 = 4000;
/// The same for each other address tried after the first failed.
pub const ALTERNATIVE_TIMEOUT_MS: u32 = 2500;
/// Other addresses of this computer tried at most.
pub const MAX_ALTERNATIVES: usize = 2;

/// No request from the console this long after it was handed the link: it is not going to fetch.
/// Past Sony's own connect timeout (30 s where it has been measured), so a firmware that does
/// answer by itself is heard first. Two minutes, not one: a healthy install on a busy console
/// must never be stopped by this (a pre-check that gave up early was removed in 6.2.0 for that),
/// and how soon every firmware makes its first request has not been measured.
pub const NO_FETCH_GIVE_UP: Duration = Duration::from_secs(120);

/// What a job is waiting for before the console has moved a byte, for the app to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Waiting {
    /// The call an earlier install left on the console has to end first.
    PreviousInstall,
    /// The helper is being asked whether the console reaches this engine.
    ReachCheck,
    /// The console has the link and has not asked for the package yet.
    ConsoleFetch,
}

/// Where a stream that never started was found out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReachStage {
    /// The check before the link was handed over.
    Check,
    /// The console was handed the link and never asked for the package.
    NoFetch,
}

/// How the console's connect ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReachCause {
    /// Nothing answered: packets dropped on the way (a firewall, an isolating Wi-Fi).
    TimedOut,
    /// Something answered "no": nothing listens there for the console, or a rule rejects it.
    Refused,
    /// Another network error.
    Other,
    /// Not measured (the watchdog only saw that no request came).
    Unknown,
}

/// A stream the console could not start, in facts: the app words it in the user's language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachFail {
    pub stage: ReachStage,
    /// `http://host:port` the console was (or would have been) sent to.
    pub origin: String,
    pub cause: ReachCause,
    /// Seconds waited for a request (`NoFetch`), else 0.
    pub waited_s: u64,
    /// The install's source kind (`host_file`, `remote`, …): only a file on this computer can
    /// be uploaded instead.
    pub source: String,
    /// What is known beyond the usual cause: Windows' own account of the network, or that the
    /// engine runs in a container. English.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub fn cause_of(r: &NetReach) -> ReachCause {
    if r.timed_out {
        ReachCause::TimedOut
    } else if r.errno == 61 || r.errno == 111 || r.err.to_ascii_lowercase().contains("refused") {
        ReachCause::Refused
    } else {
        ReachCause::Other
    }
}

/// What asking the console to connect came to.
#[derive(Debug, Clone)]
pub enum ReachOutcome {
    /// The console connected to this address: the one to give it.
    Reachable(IpAddr),
    /// The console could not be asked (an older helper, a failed call). Nothing is known, and
    /// an install that might work is never stopped on that.
    Unknown,
    /// The console reached none of this computer's addresses; the first one's answer.
    Unreachable(NetReach),
}

#[cfg(test)]
impl ReachOutcome {
    /// The address the console reached, when it reached one.
    pub fn reached(&self) -> Option<IpAddr> {
        match self {
            ReachOutcome::Reachable(addr) => Some(*addr),
            _ => None,
        }
    }
}

/// Asks the console to connect to `first` (the address it would be given), then to each of
/// `others`, and says which one it reaches. `probe(addr, timeout_ms)` is the helper's answer;
/// `Err` means it could not be asked.
///
/// One dropped packet must not fail an install: before "unreachable" the first address is
/// asked once more.
pub fn choose_reachable(
    first: IpAddr,
    others: &[IpAddr],
    probe: &mut dyn FnMut(IpAddr, u32) -> Result<NetReach, String>,
) -> ReachOutcome {
    let Ok(answer) = probe(first, REACH_TIMEOUT_MS) else {
        return ReachOutcome::Unknown;
    };
    if answer.ok {
        return ReachOutcome::Reachable(first);
    }
    for &other in others {
        if probe(other, ALTERNATIVE_TIMEOUT_MS).is_ok_and(|r| r.ok) {
            return ReachOutcome::Reachable(other);
        }
    }
    match probe(first, REACH_TIMEOUT_MS) {
        Ok(again) if again.ok => ReachOutcome::Reachable(first),
        Ok(again) => ReachOutcome::Unreachable(again),
        Err(_) => ReachOutcome::Unknown,
    }
}

/// One of this computer's addresses, with its network's prefix length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalAddr {
    pub ip: Ipv4Addr,
    pub prefix: u8,
}

fn same_network(a: Ipv4Addr, b: Ipv4Addr, prefix: u8) -> bool {
    let mask = match prefix {
        0 => 0,
        p if p >= 32 => u32::MAX,
        p => u32::MAX << (32 - p),
    };
    (u32::from(a) & mask) == (u32::from(b) & mask)
}

/// This computer's other addresses worth offering the console when it cannot reach `first`:
/// those on the console's own network (a second adapter plugged into the same LAN). An address
/// on any other network (a VPN, a virtual switch, a container bridge) is one the console has no
/// way to, so asking about it would only add seconds before the answer.
pub fn alternatives(first: IpAddr, console: IpAddr, locals: &[LocalAddr]) -> Vec<IpAddr> {
    let IpAddr::V4(console) = console else {
        return Vec::new();
    };
    let mut out: Vec<IpAddr> = Vec::new();
    for l in locals {
        let ip = IpAddr::V4(l.ip);
        if ip == first || l.ip.is_loopback() || l.ip.is_link_local() || l.ip.is_unspecified() {
            continue;
        }
        if !same_network(l.ip, console, l.prefix) || out.contains(&ip) {
            continue;
        }
        out.push(ip);
        if out.len() == MAX_ALTERNATIVES {
            break;
        }
    }
    out
}

/// This computer's IPv4 addresses.
pub fn local_addrs() -> Vec<LocalAddr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(a) => Some(LocalAddr {
                ip: a.ip,
                prefix: a.prefixlen,
            }),
            if_addrs::IfAddr::V6(_) => None,
        })
        .collect()
}

/// `host` and `port` of an `http://host:port` origin; an IPv6 host comes without its brackets.
pub fn split_origin(origin: &str) -> Option<(String, u16)> {
    let authority = origin.split_once("://")?.1;
    let (host, port) = authority.rsplit_once(':')?;
    let host = host.trim_matches(|c| c == '[' || c == ']');
    (!host.is_empty()).then_some(())?;
    Some((host.to_string(), port.parse().ok()?))
}

/// `http://host:port` for an address, brackets and all.
pub fn origin_for(host: IpAddr, port: u16) -> String {
    match host {
        IpAddr::V6(v6) => format!("http://[{v6}]:{port}"),
        IpAddr::V4(v4) => format!("http://{v4}:{port}"),
    }
}

// ─── the watchdog ────────────────────────────────────────────────────────

/// How a handed-over link ended for the job that was waiting on it.
#[derive(Debug)]
pub enum Handover<T> {
    /// The daemon answered.
    Answered(T),
    /// The user stopped the job; the call is still in flight.
    Stopped(tokio::task::JoinHandle<T>),
    /// No request came from the console in time; the call is still in flight.
    NeverAsked(tokio::task::JoinHandle<T>),
}

/// Waits for `call` (the daemon's answer to a handed-over link), unless `stop` ends first or the
/// console has made no request by `give_up`. `asked` is read every `tick`; once it says the
/// console has asked, only the answer or a stop ends the wait.
pub async fn watch_handover<T>(
    mut call: tokio::task::JoinHandle<T>,
    stop: impl std::future::Future<Output = ()>,
    mut asked: impl FnMut() -> bool,
    give_up: Duration,
    tick: Duration,
) -> Result<Handover<T>, tokio::task::JoinError> {
    tokio::pin!(stop);
    let started = tokio::time::Instant::now();
    let mut console_asked = false;
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => return Ok(Handover::Stopped(call)),
            out = &mut call => return out.map(Handover::Answered),
            _ = tokio::time::sleep(tick), if !console_asked => {
                if asked() {
                    console_asked = true;
                } else if started.elapsed() >= give_up {
                    return Ok(Handover::NeverAsked(call));
                }
            }
        }
    }
}

/// What to do about the daemon once a call nobody waits for any more has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterAbandoned {
    /// The daemon is as good as before.
    Leave,
    /// Sony refused on the network: its library keeps that for the life of the process, and
    /// every later link would fail the same way. Stop the daemon so the next install sends a
    /// fresh one.
    Recycle,
    /// No answer came back, so the call may still be running: stop the daemon as soon as it
    /// lets go (or its own watchdog ends it).
    RecycleWhenFree,
}

pub fn after_abandoned(reply: &Result<ic::InstallReply, String>) -> AfterAbandoned {
    match reply {
        Ok(ic::InstallReply::Sony { code, .. }) if super::is_network_class(*code) => {
            AfterAbandoned::Recycle
        }
        Err(_) | Ok(ic::InstallReply::Unknown(_)) => AfterAbandoned::RecycleWhenFree,
        Ok(_) => AfterAbandoned::Leave,
    }
}

/// Calls left in flight on a console's installer daemon: a job stopped waiting (the watchdog, or
/// the user's Stop) while Sony's call had not returned. The daemon runs one Sony call at a time,
/// so the next install to that console would queue behind it with nothing to show; it waits here
/// instead, where it can say so, until the call has ended and the daemon is fit to use.
#[derive(Default)]
pub struct AbandonedCalls {
    inner: Arc<Mutex<AbandonedInner>>,
}

#[derive(Default)]
struct AbandonedInner {
    seq: u64,
    pending: HashMap<String, (u64, tokio::sync::watch::Receiver<()>)>,
}

/// Held while an abandoned call is followed to its end; dropping it lets the console's next
/// install go on.
pub struct AbandonedGuard {
    inner: Arc<Mutex<AbandonedInner>>,
    console: String,
    id: u64,
    _done: tokio::sync::watch::Sender<()>,
}

impl Drop for AbandonedGuard {
    fn drop(&mut self) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.pending
            .get(&self.console)
            .is_some_and(|(id, _)| *id == self.id)
        {
            g.pending.remove(&self.console);
        }
    }
}

impl AbandonedCalls {
    /// Marks a call to `console`'s daemon as left in flight.
    pub fn begin(&self, console: &str) -> AbandonedGuard {
        let (tx, rx) = tokio::sync::watch::channel(());
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.seq += 1;
        let id = g.seq;
        g.pending.insert(console.to_string(), (id, rx));
        AbandonedGuard {
            inner: self.inner.clone(),
            console: console.to_string(),
            id,
            _done: tx,
        }
    }

    pub fn pending(&self, console: &str) -> bool {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.pending.contains_key(console)
    }

    /// Resolves once no abandoned call is in flight on `console`.
    pub async fn settled(&self, console: &str) {
        loop {
            let rx = {
                let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                g.pending.get(console).map(|(_, rx)| rx.clone())
            };
            let Some(mut rx) = rx else { return };
            // Ends with an error when the guard (and its sender) is dropped: what is waited for.
            let _ = rx.changed().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn reached() -> Result<NetReach, String> {
        Ok(NetReach {
            ok: true,
            ms: 1,
            ..Default::default()
        })
    }

    fn dropped() -> Result<NetReach, String> {
        Ok(NetReach {
            ok: false,
            timed_out: true,
            err: "timed out".into(),
            ms: 4000,
            ..Default::default()
        })
    }

    #[test]
    fn a_console_that_reaches_the_first_address_is_given_it_after_one_question() {
        let mut asked = Vec::new();
        let out = choose_reachable(v4("192.168.1.3"), &[v4("192.168.1.7")], &mut |a, t| {
            asked.push((a, t));
            reached()
        });
        assert_eq!(out.reached(), Some(v4("192.168.1.3")));
        assert_eq!(asked, vec![(v4("192.168.1.3"), REACH_TIMEOUT_MS)]);
    }

    /// #942's kind of computer: a cable on a network Windows calls Public (blocked) and Wi-Fi on
    /// the same LAN as a Private one. The routing table names the cable.
    #[test]
    fn a_second_adapter_the_console_does_reach_is_the_address_it_is_given() {
        let out = choose_reachable(v4("192.168.1.3"), &[v4("192.168.1.7")], &mut |a, _| {
            if a == v4("192.168.1.7") {
                reached()
            } else {
                dropped()
            }
        });
        assert_eq!(out.reached(), Some(v4("192.168.1.7")));
    }

    #[test]
    fn unreachable_is_said_only_after_every_address_and_a_second_try_of_the_first() {
        let mut asked = Vec::new();
        let out = choose_reachable(v4("192.168.1.3"), &[v4("192.168.1.7")], &mut |a, _| {
            asked.push(a);
            dropped()
        });
        assert!(matches!(out, ReachOutcome::Unreachable(r) if r.timed_out));
        assert_eq!(
            asked,
            vec![v4("192.168.1.3"), v4("192.168.1.7"), v4("192.168.1.3")]
        );
    }

    #[test]
    fn one_lost_answer_does_not_fail_the_install() {
        let mut n = 0;
        let out = choose_reachable(v4("192.168.1.3"), &[], &mut |_, _| {
            n += 1;
            if n == 1 {
                dropped()
            } else {
                reached()
            }
        });
        assert_eq!(out.reached(), Some(v4("192.168.1.3")));
    }

    /// An older helper has no such question, and a busy one may not answer: neither says the
    /// console cannot reach us.
    #[test]
    fn a_console_that_cannot_be_asked_is_never_called_unreachable() {
        let out = choose_reachable(v4("192.168.1.3"), &[v4("192.168.1.7")], &mut |_, _| {
            Err("unknown frame".into())
        });
        assert!(matches!(out, ReachOutcome::Unknown));
        // Asked once, answered "no", then the helper went away: still nothing known.
        let mut n = 0;
        let out = choose_reachable(v4("192.168.1.3"), &[], &mut |_, _| {
            n += 1;
            if n == 1 {
                dropped()
            } else {
                Err("timed out talking to the helper".into())
            }
        });
        assert!(matches!(out, ReachOutcome::Unknown));
    }

    #[test]
    fn only_addresses_on_the_consoles_own_network_are_offered_instead() {
        let locals = [
            LocalAddr {
                ip: "192.168.1.3".parse().unwrap(),
                prefix: 24,
            },
            LocalAddr {
                ip: "192.168.1.7".parse().unwrap(),
                prefix: 24,
            },
            // A VPN, a virtual switch, a container bridge, a host-only adapter, a link-local one.
            LocalAddr {
                ip: "10.8.0.2".parse().unwrap(),
                prefix: 24,
            },
            LocalAddr {
                ip: "172.28.112.1".parse().unwrap(),
                prefix: 20,
            },
            LocalAddr {
                ip: "172.17.0.1".parse().unwrap(),
                prefix: 16,
            },
            LocalAddr {
                ip: "192.168.56.1".parse().unwrap(),
                prefix: 24,
            },
            LocalAddr {
                ip: "169.254.10.4".parse().unwrap(),
                prefix: 16,
            },
            LocalAddr {
                ip: "127.0.0.1".parse().unwrap(),
                prefix: 8,
            },
        ];
        assert_eq!(
            alternatives(v4("192.168.1.3"), v4("192.168.1.25"), &locals),
            vec![v4("192.168.1.7")]
        );
        // The routing table chose the VPN's address (the case PS5UPLOAD_PKG_HOST_IP exists for):
        // both LAN addresses are offered.
        assert_eq!(
            alternatives(v4("10.8.0.2"), v4("192.168.1.25"), &locals),
            vec![v4("192.168.1.3"), v4("192.168.1.7")]
        );
        // A console on another network than every adapter: nothing to offer.
        assert!(alternatives(v4("192.168.1.3"), v4("192.168.50.9"), &locals).is_empty());
        // An IPv6 console is asked about the first address only.
        assert!(alternatives(v4("192.168.1.3"), "fe80::1".parse().unwrap(), &locals).is_empty());
    }

    #[test]
    fn no_more_than_two_other_addresses_are_tried() {
        let locals: Vec<LocalAddr> = (10..20)
            .map(|n| LocalAddr {
                ip: Ipv4Addr::new(192, 168, 1, n),
                prefix: 24,
            })
            .collect();
        assert_eq!(
            alternatives(v4("192.168.1.3"), v4("192.168.1.25"), &locals).len(),
            MAX_ALTERNATIVES
        );
    }

    #[test]
    fn networks_are_compared_by_their_own_prefix() {
        let a: Ipv4Addr = "10.0.5.2".parse().unwrap();
        assert!(same_network(a, "10.0.200.9".parse().unwrap(), 16));
        assert!(!same_network(a, "10.0.200.9".parse().unwrap(), 24));
        assert!(same_network(a, "203.0.113.1".parse().unwrap(), 0));
        assert!(!same_network(a, "10.0.5.3".parse().unwrap(), 32));
        assert!(same_network(a, a, 32));
    }

    #[test]
    fn an_origin_splits_into_its_host_and_port() {
        assert_eq!(
            split_origin("http://192.168.1.3:19113"),
            Some(("192.168.1.3".into(), 19113))
        );
        assert_eq!(
            split_origin("http://[fd00::3]:8080"),
            Some(("fd00::3".into(), 8080))
        );
        assert_eq!(split_origin("http://192.168.1.3"), None);
        assert_eq!(split_origin("192.168.1.3:19113"), None);
        assert_eq!(
            origin_for(v4("192.168.1.7"), 19113),
            "http://192.168.1.7:19113"
        );
        assert_eq!(
            origin_for("fd00::3".parse().unwrap(), 19113),
            "http://[fd00::3]:19113"
        );
    }

    #[test]
    fn the_cause_is_read_off_the_helpers_answer() {
        assert_eq!(cause_of(&dropped().unwrap()), ReachCause::TimedOut);
        let refused = NetReach {
            errno: 61,
            err: "Connection refused".into(),
            ..Default::default()
        };
        assert_eq!(cause_of(&refused), ReachCause::Refused);
        let linux = NetReach {
            errno: 111,
            ..Default::default()
        };
        assert_eq!(cause_of(&linux), ReachCause::Refused);
        let other = NetReach {
            errno: 65,
            err: "No route to host".into(),
            ..Default::default()
        };
        assert_eq!(cause_of(&other), ReachCause::Other);
    }

    #[test]
    fn the_facts_go_to_the_app_in_snake_case_without_an_empty_detail() {
        let f = ReachFail {
            stage: ReachStage::NoFetch,
            origin: "http://192.168.1.3:19113".into(),
            cause: ReachCause::Unknown,
            waited_s: 60,
            source: "host_file".into(),
            detail: None,
        };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["stage"], "no_fetch");
        assert_eq!(v["cause"], "unknown");
        assert_eq!(v["waited_s"], 60);
        assert!(v.get("detail").is_none());
        assert_eq!(
            serde_json::to_value(Waiting::PreviousInstall).unwrap(),
            "previous_install"
        );
        assert_eq!(serde_json::to_value(ReachStage::Check).unwrap(), "check");
        assert_eq!(
            serde_json::to_value(ReachCause::TimedOut).unwrap(),
            "timed_out"
        );
    }

    // ── the watchdog ──

    const GIVE_UP: Duration = Duration::from_secs(60);
    const TICK: Duration = Duration::from_secs(1);

    /// #942: Sony's call never comes back and the console never asks for the package.
    #[tokio::test(start_paused = true)]
    async fn a_console_that_never_asks_ends_the_wait_after_the_give_up_time() {
        let call = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(900)).await;
            "the daemon's answer"
        });
        let t0 = tokio::time::Instant::now();
        let out = watch_handover(call, std::future::pending(), || false, GIVE_UP, TICK)
            .await
            .unwrap();
        let Handover::NeverAsked(call) = out else {
            panic!("expected NeverAsked, got {out:?}");
        };
        let waited = t0.elapsed();
        assert!(
            waited >= GIVE_UP && waited < GIVE_UP + 2 * TICK,
            "{waited:?}"
        );
        // The call was not cut off: whoever follows it still gets its answer.
        assert_eq!(call.await.unwrap(), "the daemon's answer");
    }

    #[tokio::test(start_paused = true)]
    async fn an_answer_before_the_give_up_time_is_the_answer() {
        let call = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            7
        });
        let out = watch_handover(call, std::future::pending(), || false, GIVE_UP, TICK)
            .await
            .unwrap();
        assert!(matches!(out, Handover::Answered(7)));
    }

    /// A console that has asked for the package is fetching: a long install call (Sony reads the
    /// header first) is waited for as long as it takes.
    #[tokio::test(start_paused = true)]
    async fn once_the_console_has_asked_the_wait_has_no_give_up_time() {
        let call = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(600)).await;
            "accepted"
        });
        let t0 = tokio::time::Instant::now();
        let mut polls = 0;
        let out = watch_handover(
            call,
            std::future::pending(),
            || {
                polls += 1;
                // The first request arrives 5 s in.
                tokio::time::Instant::now() - t0 >= Duration::from_secs(5)
            },
            GIVE_UP,
            TICK,
        )
        .await
        .unwrap();
        assert!(matches!(out, Handover::Answered("accepted")));
        // Not asked again once it has said yes.
        assert_eq!(polls, 5);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_ends_the_wait_at_once_and_hands_back_the_call() {
        let call = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(900)).await;
            1
        });
        let stop = tokio::time::sleep(Duration::from_secs(10));
        let t0 = tokio::time::Instant::now();
        let out = watch_handover(call, stop, || false, GIVE_UP, TICK)
            .await
            .unwrap();
        assert!(matches!(out, Handover::Stopped(_)));
        assert_eq!(t0.elapsed(), Duration::from_secs(10));
    }

    #[test]
    fn what_becomes_of_the_daemon_after_a_call_nobody_waited_for() {
        use ic::InstallReply as R;
        // Sony gave up on the network: the process keeps that, so it is replaced.
        for code in [0x8043_1068u32, 0x8043_1064] {
            assert_eq!(
                after_abandoned(&Ok(R::Sony { code, hint: None })),
                AfterAbandoned::Recycle
            );
        }
        // No answer: the call may still be running.
        assert_eq!(
            after_abandoned(&Err("read: timed out".into())),
            AfterAbandoned::RecycleWhenFree
        );
        assert_eq!(
            after_abandoned(&Ok(R::Unknown("?".into()))),
            AfterAbandoned::RecycleWhenFree
        );
        // A verdict on the package, a late acceptance, another job's turn: the daemon is fine.
        assert_eq!(
            after_abandoned(&Ok(R::Sony {
                code: 0x80B2_116F,
                hint: None
            })),
            AfterAbandoned::Leave
        );
        assert_eq!(
            after_abandoned(&Ok(R::Accepted {
                job: "1-1".into(),
                via: "url".into()
            })),
            AfterAbandoned::Leave
        );
        assert_eq!(
            after_abandoned(&Ok(R::Busy { job: "1-1".into() })),
            AfterAbandoned::Leave
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_next_install_waits_until_the_abandoned_call_has_ended() {
        let calls = Arc::new(AbandonedCalls::default());
        assert!(!calls.pending("192.168.1.25"));
        // Nothing pending: no wait.
        calls.settled("192.168.1.25").await;

        let guard = calls.begin("192.168.1.25");
        assert!(calls.pending("192.168.1.25"));
        // Another console is not held up.
        calls.settled("192.168.1.26").await;

        let waiter = {
            let calls = calls.clone();
            tokio::spawn(async move {
                let t0 = tokio::time::Instant::now();
                calls.settled("192.168.1.25").await;
                t0.elapsed()
            })
        };
        tokio::time::sleep(Duration::from_secs(300)).await;
        assert!(!waiter.is_finished());
        drop(guard);
        assert_eq!(waiter.await.unwrap(), Duration::from_secs(300));
        assert!(!calls.pending("192.168.1.25"));
    }

    #[tokio::test]
    async fn an_older_guard_ending_does_not_free_a_newer_abandoned_call() {
        let calls = AbandonedCalls::default();
        let first = calls.begin("192.168.1.25");
        let second = calls.begin("192.168.1.25");
        drop(first);
        assert!(calls.pending("192.168.1.25"));
        drop(second);
        assert!(!calls.pending("192.168.1.25"));
    }
}
