//! How each console's connection has been holding up (`GET /api/ps5/link-history`).
//!
//! The status route sees every check the app makes, so it is where a drop is noticed: a check
//! that fails after one that answered. This keeps, per console and in memory only, a short record
//! of those drops (when, and whether the console stopped answering or its helper's port refused),
//! the times it came back, a helper that started over (another instance answering), and USB
//! drives appearing or going in the volume lists the app reads anyway. No addresses, names or
//! file paths beyond a drive's mount point (`/mnt/usb0`) are kept.
//!
//! From that it says when the connection "keeps dropping" (a few drops close together, lately),
//! and whether the drops started around when a USB drive appeared on a console that is on Wi-Fi:
//! USB 3 drives and cables give off noise in the 2.4 GHz band, and a drive next to the console
//! can make its Wi-Fi drop in and out. The app shows a hint for either; the bug report carries
//! the record.
//!
//! The decisions are free functions over plain inputs, tested without a console.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use serde::Serialize;

use crate::net_doctor::LinkKind;

/// Events kept per console (the oldest go first).
const MAX_EVENTS: usize = 64;
/// Events older than this are dropped.
const KEEP_MS: u64 = 24 * 60 * 60_000;
/// Drops further apart than this are separate episodes.
pub(crate) const STREAK_GAP_MS: u64 = 10 * 60_000;
/// Drops in one episode before the connection "keeps dropping".
pub(crate) const HINT_MIN_DROPS: usize = 3;
/// The episode's last drop is at most this old for the hint to show.
pub(crate) const HINT_RECENT_MS: u64 = 15 * 60_000;
/// A USB drive that appeared within this of the first drop is the likely trigger.
pub(crate) const DRIVE_NEAR_MS: u64 = 10 * 60_000;
/// After the console comes back, the volume list is read again at most this often (it tells
/// whether a drive appeared). Only while it drops: a steady console is never asked for it here.
const VOLUMES_EVERY_MS: u64 = 60_000;
/// How often the console's link (cable or Wi-Fi) may be asked for while it drops.
const LINK_EVERY_MS: u64 = 5 * 60_000;
/// Consoles remembered.
const MAX_CONSOLES: usize = 64;

/// Why a check failed, as far as it counts as a drop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DropWhy {
    /// The console stopped answering (no reply in time, its port did not answer).
    Timeout,
    /// The console answered, but nothing listened on the helper's port.
    Refused,
    /// Any other failure of the connection.
    Other,
}

/// One thing that happened to a console's connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LinkEvent {
    /// A check failed after one that answered.
    Drop { at_ms: u64, why: DropWhy },
    /// A check answered again, `after_ms` after the drop.
    Back { at_ms: u64, after_ms: u64 },
    /// Another helper instance answers: the helper started over (sent again, or restarted).
    /// `prior` is how the new one says the previous one ended (`replaced`, `clean`, ...).
    HelperRestarted {
        at_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        prior: Option<String>,
    },
    /// A drive appeared in the console's volume list. `usb`: a USB drive (not the M.2 slot).
    /// `absent_at_ms`: the last list it was not in.
    DriveAdded {
        at_ms: u64,
        path: String,
        usb: bool,
        absent_at_ms: u64,
    },
    /// A drive went from the volume list.
    DriveRemoved { at_ms: u64, path: String },
}

impl LinkEvent {
    fn at(&self) -> u64 {
        match self {
            LinkEvent::Drop { at_ms, .. }
            | LinkEvent::Back { at_ms, .. }
            | LinkEvent::HelperRestarted { at_ms, .. }
            | LinkEvent::DriveAdded { at_ms, .. }
            | LinkEvent::DriveRemoved { at_ms, .. } => *at_ms,
        }
    }
}

/// The console's own link, as its helper reported it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ConsoleLink {
    pub kind: LinkKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iface: Option<String>,
    pub at_ms: u64,
}

/// What the app should say about a console's connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HintKind {
    /// It keeps dropping since a USB drive appeared, and the console is on Wi-Fi.
    UsbWifi,
    /// It keeps dropping.
    KeepsDropping,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Hint {
    pub kind: HintKind,
    /// Drops in this episode.
    pub drops: usize,
    /// The episode's first drop: what a dismissal is keyed on, so a new episode shows again.
    pub since_ms: u64,
    /// The episode's last drop.
    pub last_ms: u64,
    /// When the USB drive was first seen (for `usb_wifi`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drive_at_ms: Option<u64>,
}

/// The record as the route answers it.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct LinkReport {
    /// Oldest first.
    pub events: Vec<LinkEvent>,
    /// The console's link, when it was read (only while it drops, or when asked).
    pub link: Option<ConsoleLink>,
    pub hint: Option<Hint>,
    /// Whether the console's last check answered (`None`: no check yet).
    pub up: Option<bool>,
}

/// What the status route should read next, in the background, after a check.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FollowUp {
    pub volumes: bool,
    pub link: bool,
}

#[derive(Debug, Default)]
struct Console {
    events: VecDeque<LinkEvent>,
    up: Option<bool>,
    down_since: Option<u64>,
    instance_id: Option<u64>,
    /// The drives in the last volume list read, and when.
    drives: Option<(u64, Vec<Drive>)>,
    link: Option<ConsoleLink>,
    volumes_asked_ms: u64,
    link_asked_ms: u64,
    touched_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Drive {
    pub path: String,
    pub usb: bool,
}

impl Console {
    fn push(&mut self, e: LinkEvent) {
        let now = e.at();
        self.events.push_back(e);
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        while self
            .events
            .front()
            .is_some_and(|f| now.saturating_sub(f.at()) > KEEP_MS)
        {
            self.events.pop_front();
        }
    }

    fn drops_lately(&self, now: u64) -> usize {
        self.events
            .iter()
            .filter(|e| matches!(e, LinkEvent::Drop { at_ms, .. } if now.saturating_sub(*at_ms) <= HINT_RECENT_MS))
            .count()
    }
}

/// Whether a failed check is a drop, and of what kind. `token` is the failure's reason token
/// (`ps5upload_core::mgmt::error_token`), `text` its message. Rest mode, a helper that is only
/// slow and a console waiting to be paired are not drops.
pub(crate) fn classify(token: Option<&str>, text: &str) -> Option<DropWhy> {
    use ps5upload_core::mgmt::{AVA1_UNREACHABLE, CONSOLE_REST_MODE, HELPER_NOT_AVA1, NOT_PAIRED};
    match token {
        Some(t) if t == CONSOLE_REST_MODE || t == NOT_PAIRED => return None,
        Some(t) if t == ps5upload_ava1::mgmt::STATUS_SLOW => return None,
        Some(t) if t == AVA1_UNREACHABLE => return Some(DropWhy::Timeout),
        Some(t) if t == HELPER_NOT_AVA1 => return Some(DropWhy::Refused),
        _ => {}
    }
    let t = text.to_ascii_lowercase();
    if t.contains("refused") {
        Some(DropWhy::Refused)
    } else if t.contains("timed out") || t.contains("timeout") {
        Some(DropWhy::Timeout)
    } else {
        Some(DropWhy::Other)
    }
}

/// The drives in a volume list: the external slots (`/mnt/usbN`, `/mnt/extN`). USB is anything
/// at `/mnt/usbN`, or an extended-storage drive on a USB device (`/dev/da*`); the M.2 slot
/// (`/dev/nvme*`) is not.
pub(crate) fn drives_of<'a>(vols: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<Drive> {
    let mut out: Vec<Drive> = vols
        .into_iter()
        .filter(|(path, _)| path.starts_with("/mnt/usb") || path.starts_with("/mnt/ext"))
        .map(|(path, from)| Drive {
            path: path.to_string(),
            usb: path.starts_with("/mnt/usb") || from.starts_with("/dev/da"),
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// The hint for a console's record, or `None` when the connection is fine.
///
/// It keeps dropping: [`HINT_MIN_DROPS`] drops with no gap over [`STREAK_GAP_MS`], the last one
/// at most [`HINT_RECENT_MS`] ago. It is the USB drive's Wi-Fi noise as well when the console is
/// on Wi-Fi and a USB drive (still there) appeared within [`DRIVE_NEAR_MS`] of the first drop,
/// after a time it was known to be absent that comes before the first drop (so the drops did not
/// start before the drive came).
pub(crate) fn hint(events: &[LinkEvent], link: Option<&ConsoleLink>, now: u64) -> Option<Hint> {
    let drops: Vec<u64> = events
        .iter()
        .filter_map(|e| match e {
            LinkEvent::Drop { at_ms, .. } => Some(*at_ms),
            _ => None,
        })
        .collect();
    let last = *drops.last()?;
    if now.saturating_sub(last) > HINT_RECENT_MS {
        return None;
    }
    // Walk back from the last drop while the gaps stay small.
    let mut first = last;
    let mut count = 1;
    for w in drops.windows(2).rev() {
        if w[1].saturating_sub(w[0]) > STREAK_GAP_MS {
            break;
        }
        first = w[0];
        count += 1;
    }
    if count < HINT_MIN_DROPS {
        return None;
    }
    let wifi = link.is_some_and(|l| l.kind == LinkKind::Wifi);
    let drive_at = wifi
        .then(|| {
            events.iter().find_map(|e| match e {
                LinkEvent::DriveAdded {
                    at_ms,
                    path,
                    usb: true,
                    absent_at_ms,
                } if *absent_at_ms < first
                    && at_ms.abs_diff(first) <= DRIVE_NEAR_MS
                    && !events.iter().any(|r| {
                        matches!(r, LinkEvent::DriveRemoved { at_ms: gone, path: p }
                            if p == path && gone > at_ms)
                    }) =>
                {
                    Some(*at_ms)
                }
                _ => None,
            })
        })
        .flatten();
    Some(Hint {
        kind: if drive_at.is_some() {
            HintKind::UsbWifi
        } else {
            HintKind::KeepsDropping
        },
        drops: count,
        since_ms: first,
        last_ms: last,
        drive_at_ms: drive_at,
    })
}

/// Every console's record.
#[derive(Debug, Default)]
pub(crate) struct Links {
    consoles: HashMap<String, Console>,
}

impl Links {
    fn console(&mut self, key: &str, now: u64) -> &mut Console {
        if !self.consoles.contains_key(key) && self.consoles.len() >= MAX_CONSOLES {
            if let Some(oldest) = self
                .consoles
                .iter()
                .min_by_key(|(_, c)| c.touched_ms)
                .map(|(k, _)| k.clone())
            {
                self.consoles.remove(&oldest);
            }
        }
        let c = self.consoles.entry(key.to_string()).or_default();
        c.touched_ms = now;
        c
    }

    /// A check that answered. `instance_id` is the helper's (absent from a reply that lacks it),
    /// `prior` how the helper says its predecessor ended.
    pub(crate) fn status_ok(
        &mut self,
        key: &str,
        now: u64,
        instance_id: Option<u64>,
        prior: Option<&str>,
    ) -> FollowUp {
        let c = self.console(key, now);
        let mut follow = FollowUp::default();
        let was_down = c.up == Some(false);
        if let Some(since) = c.down_since.take() {
            c.push(LinkEvent::Back {
                at_ms: now,
                after_ms: now.saturating_sub(since),
            });
        }
        c.up = Some(true);
        if let Some(id) = instance_id.filter(|id| *id != 0) {
            if c.instance_id.is_some_and(|old| old != id) {
                c.push(LinkEvent::HelperRestarted {
                    at_ms: now,
                    prior: prior.filter(|p| !p.is_empty()).map(str::to_string),
                });
                // Another helper: its link may differ (it is read again when needed).
                c.link = None;
            }
            c.instance_id = Some(id);
        }
        // The first answer gives the volume list to compare later ones with; coming back from
        // a drop reads it again (a drive that just appeared shows there).
        let want_volumes = c.drives.is_none() || was_down;
        if want_volumes && now.saturating_sub(c.volumes_asked_ms) >= VOLUMES_EVERY_MS {
            c.volumes_asked_ms = now;
            follow.volumes = true;
        }
        // Wired or Wi-Fi decides the hint: asked only while it drops.
        if c.link.is_none()
            && c.drops_lately(now) >= 2
            && now.saturating_sub(c.link_asked_ms) >= LINK_EVERY_MS
        {
            c.link_asked_ms = now;
            follow.link = true;
        }
        follow
    }

    /// A check that failed (see [`classify`]).
    pub(crate) fn status_failed(&mut self, key: &str, now: u64, why: Option<DropWhy>) {
        let c = self.console(key, now);
        let Some(why) = why else { return };
        if c.up == Some(true) {
            c.push(LinkEvent::Drop { at_ms: now, why });
            c.down_since = Some(now);
        }
        c.up = Some(false);
    }

    /// The drives of a volume list just read.
    pub(crate) fn volumes(&mut self, key: &str, now: u64, drives: Vec<Drive>) {
        let c = self.console(key, now);
        if let Some((then, before)) = c.drives.take() {
            for d in drives
                .iter()
                .filter(|d| !before.iter().any(|b| b.path == d.path))
            {
                c.push(LinkEvent::DriveAdded {
                    at_ms: now,
                    path: d.path.clone(),
                    usb: d.usb,
                    absent_at_ms: then,
                });
            }
            for b in before
                .iter()
                .filter(|b| !drives.iter().any(|d| d.path == b.path))
            {
                c.push(LinkEvent::DriveRemoved {
                    at_ms: now,
                    path: b.path.clone(),
                });
            }
        }
        c.drives = Some((now, drives));
    }

    /// The console's link, as its helper reported it.
    pub(crate) fn link(&mut self, key: &str, now: u64, kind: LinkKind, iface: Option<String>) {
        let c = self.console(key, now);
        c.link = Some(ConsoleLink {
            kind,
            iface,
            at_ms: now,
        });
    }

    pub(crate) fn has_link(&self, key: &str) -> bool {
        self.consoles.get(key).is_some_and(|c| c.link.is_some())
    }

    pub(crate) fn report(&self, key: &str, now: u64) -> LinkReport {
        let Some(c) = self.consoles.get(key) else {
            return LinkReport {
                events: Vec::new(),
                link: None,
                hint: None,
                up: None,
            };
        };
        let events: Vec<LinkEvent> = c.events.iter().cloned().collect();
        let hint = hint(&events, c.link.as_ref(), now);
        LinkReport {
            events,
            link: c.link.clone(),
            hint,
            up: c.up,
        }
    }
}

// ── The engine's one record ────────────────────────────────────────────────────────────────

static LINKS: Mutex<Option<Links>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Links) -> T) -> T {
    let mut g = LINKS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Links::default))
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The record's key for a console address (`192.0.2.1:9120` and `192.0.2.1` are one console).
pub(crate) fn key(addr: &str) -> String {
    ps5upload_core::events::console_host(addr).to_ascii_lowercase()
}

/// After a status check answered: notes it, and reads in the background what the record needs
/// (the volume list after a drop, the console's link while it drops). Never blocks the caller.
pub(crate) fn after_status_ok(addr: &str, json: &serde_json::Value) {
    let k = key(addr);
    let instance = json.get("instance_id").and_then(serde_json::Value::as_u64);
    let prior = json
        .get("prior_instance")
        .and_then(serde_json::Value::as_str);
    let follow = with(|l| l.status_ok(&k, now_ms(), instance, prior));
    if follow.volumes {
        let addr = addr.to_string();
        tokio::task::spawn_blocking(move || {
            if let Ok(v) = ps5upload_core::volumes::list_volumes(&addr) {
                note_volumes(&addr, &v);
            }
        });
    }
    if follow.link {
        let addr = addr.to_string();
        tokio::task::spawn_blocking(move || read_link(&addr));
    }
}

/// After a status check failed.
pub(crate) fn after_status_failed(addr: &str, e: &anyhow::Error) {
    let why = classify(
        ps5upload_core::mgmt::error_token(e).as_deref(),
        &format!("{e:#}"),
    );
    with(|l| l.status_failed(&key(addr), now_ms(), why));
}

/// A volume list read for this console (by the app's screens or by the record itself).
pub(crate) fn note_volumes(addr: &str, list: &ps5upload_core::volumes::VolumeList) {
    let drives = drives_of(
        list.volumes
            .iter()
            .map(|v| (v.path.as_str(), v.mount_from.as_str())),
    );
    with(|l| l.volumes(&key(addr), now_ms(), drives));
}

/// Asks the helper for the console's interfaces and notes which one carries this connection.
/// Blocking. Leaves the link unknown when the helper cannot say.
pub(crate) fn read_link(addr: &str) {
    let Ok(list) = ps5upload_core::diagnostics::net_interfaces(addr) else {
        return;
    };
    let ip = ps5upload_core::events::console_host(addr)
        .parse::<std::net::Ipv4Addr>()
        .ok();
    let l = crate::net_doctor::console_link(&list.interfaces, ip);
    with(|r| r.link(&key(addr), now_ms(), l.kind, l.iface));
}

/// The console's record. With `read_link`, its link is asked for first when it is not known
/// (a bug report wants it even when nothing dropped). Blocking then.
pub(crate) fn report(addr: &str, read_link_now: bool) -> LinkReport {
    if read_link_now && !with(|l| l.has_link(&key(addr))) {
        read_link(addr);
    }
    with(|l| l.report(&key(addr), now_ms()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60_000;
    const T0: u64 = 1_800_000_000_000;

    fn wifi(at: u64) -> (LinkKind, Option<String>, u64) {
        (LinkKind::Wifi, Some("wlan0".into()), at)
    }

    /// A console that answered, then drops `n` times a minute apart (each followed by a recovery).
    fn flapping(l: &mut Links, k: &str, start: u64, n: usize) {
        for i in 0..n as u64 {
            l.status_ok(k, start + i * MIN, Some(7), None);
            l.status_failed(k, start + i * MIN + 10_000, Some(DropWhy::Timeout));
        }
        l.status_ok(k, start + n as u64 * MIN, Some(7), None);
    }

    #[test]
    fn a_failure_is_a_drop_only_after_an_answer_and_only_once() {
        let mut l = Links::default();
        // Not seen up yet: nothing dropped.
        l.status_failed("a", T0, Some(DropWhy::Timeout));
        l.status_ok("a", T0 + 1_000, Some(1), None);
        l.status_failed("a", T0 + 2_000, Some(DropWhy::Timeout));
        // The confirming checks that follow are the same drop.
        l.status_failed("a", T0 + 3_000, Some(DropWhy::Refused));
        l.status_ok("a", T0 + 9_000, Some(1), None);
        let r = l.report("a", T0 + 9_000);
        assert_eq!(
            r.events,
            vec![
                LinkEvent::Drop {
                    at_ms: T0 + 2_000,
                    why: DropWhy::Timeout
                },
                LinkEvent::Back {
                    at_ms: T0 + 9_000,
                    after_ms: 7_000
                },
            ]
        );
        assert_eq!(r.up, Some(true));
        assert_eq!(r.hint, None);
    }

    #[test]
    fn rest_mode_a_slow_helper_and_pairing_are_not_drops() {
        use ps5upload_core::mgmt::{CONSOLE_REST_MODE, NOT_PAIRED};
        assert_eq!(classify(Some(CONSOLE_REST_MODE), ""), None);
        assert_eq!(classify(Some(NOT_PAIRED), ""), None);
        assert_eq!(classify(Some(ps5upload_ava1::mgmt::STATUS_SLOW), ""), None);
        assert_eq!(
            classify(Some("ava1_unreachable"), ""),
            Some(DropWhy::Timeout)
        );
        assert_eq!(
            classify(Some("helper_not_ava1"), ""),
            Some(DropWhy::Refused)
        );
        assert_eq!(
            classify(
                None,
                "management call STATUS failed: Connection refused (os error 61)"
            ),
            Some(DropWhy::Refused)
        );
        assert_eq!(
            classify(None, "STATUS: timed out after 2s"),
            Some(DropWhy::Timeout)
        );
        assert_eq!(classify(None, "broken pipe"), Some(DropWhy::Other));

        let mut l = Links::default();
        l.status_ok("a", T0, Some(1), None);
        l.status_failed("a", T0 + 1, None);
        assert!(l.report("a", T0 + 2).events.is_empty());
    }

    #[test]
    fn a_new_helper_instance_is_a_restart() {
        let mut l = Links::default();
        l.status_ok("a", T0, Some(1), None);
        l.status_ok("a", T0 + 1, Some(1), None);
        l.status_ok("a", T0 + 2, Some(2), Some("replaced"));
        // A reply without an instance (older helper) says nothing.
        l.status_ok("a", T0 + 3, None, None);
        assert_eq!(
            l.report("a", T0 + 3).events,
            vec![LinkEvent::HelperRestarted {
                at_ms: T0 + 2,
                prior: Some("replaced".into())
            }]
        );
    }

    #[test]
    fn three_drops_close_together_keep_dropping_and_old_ones_do_not() {
        let mut l = Links::default();
        flapping(&mut l, "a", T0, 2);
        assert_eq!(l.report("a", T0 + 3 * MIN).hint, None, "two is not yet");
        flapping(&mut l, "a", T0 + 3 * MIN, 1);
        let h = l.report("a", T0 + 4 * MIN).hint.unwrap();
        assert_eq!(h.kind, HintKind::KeepsDropping);
        assert_eq!(h.drops, 3);
        assert_eq!(h.since_ms, T0 + 10_000);
        // Quiet for a while: gone.
        assert_eq!(
            l.report("a", T0 + 3 * MIN + HINT_RECENT_MS + MIN).hint,
            None
        );

        // Spread over hours: never an episode.
        let mut l = Links::default();
        for i in 0..5 {
            flapping(&mut l, "b", T0 + i * 30 * MIN, 1);
        }
        assert_eq!(l.report("b", T0 + 4 * 30 * MIN + MIN).hint, None);
    }

    #[test]
    fn a_usb_drive_before_the_drops_on_wifi_is_the_usb_hint() {
        let mut l = Links::default();
        l.status_ok("a", T0, Some(7), None);
        l.volumes("a", T0, drives_of([("/data", "/dev/ssd0.user")]));
        l.volumes(
            "a",
            T0 + 2 * MIN,
            drives_of([("/data", "/dev/ssd0.user"), ("/mnt/ext1", "/dev/da0s1")]),
        );
        flapping(&mut l, "a", T0 + 3 * MIN, 3);
        let (kind, iface, at) = wifi(T0 + 5 * MIN);
        l.link("a", at, kind, iface);
        let h = l.report("a", T0 + 7 * MIN).hint.unwrap();
        assert_eq!(h.kind, HintKind::UsbWifi);
        assert_eq!(h.drive_at_ms, Some(T0 + 2 * MIN));

        // The same on a cable: only "keeps dropping".
        l.link("a", T0 + 6 * MIN, LinkKind::Wired, Some("eth0".into()));
        assert_eq!(
            l.report("a", T0 + 7 * MIN).hint.unwrap().kind,
            HintKind::KeepsDropping
        );
        // And when the link is not known.
        let mut l2 = Links::default();
        l2.volumes("a", T0, Vec::new());
        l2.volumes("a", T0 + MIN, drives_of([("/mnt/usb0", "/dev/da0s1")]));
        flapping(&mut l2, "a", T0 + 2 * MIN, 3);
        assert_eq!(
            l2.report("a", T0 + 6 * MIN).hint.unwrap().kind,
            HintKind::KeepsDropping
        );
    }

    #[test]
    fn a_drive_seen_after_drops_already_started_or_long_before_is_not_the_cause() {
        // Dropping before the drive came (its last absence is after the first drop).
        let mut l = Links::default();
        flapping(&mut l, "a", T0, 2);
        l.volumes("a", T0 + 2 * MIN, Vec::new());
        l.volumes("a", T0 + 3 * MIN, drives_of([("/mnt/usb0", "/dev/da0s1")]));
        flapping(&mut l, "a", T0 + 3 * MIN, 2);
        let (k, i, at) = wifi(T0);
        l.link("a", at, k, i);
        assert_eq!(
            l.report("a", T0 + 6 * MIN).hint.unwrap().kind,
            HintKind::KeepsDropping
        );

        // A drive there for an hour before anything dropped.
        let mut l = Links::default();
        l.volumes("a", T0, Vec::new());
        l.volumes("a", T0 + MIN, drives_of([("/mnt/usb0", "/dev/da0s1")]));
        flapping(&mut l, "a", T0 + 60 * MIN, 3);
        l.link("a", T0, LinkKind::Wifi, None);
        assert_eq!(
            l.report("a", T0 + 64 * MIN).hint.unwrap().kind,
            HintKind::KeepsDropping
        );

        // A drive that went again.
        let mut l = Links::default();
        l.volumes("a", T0, Vec::new());
        l.volumes("a", T0 + MIN, drives_of([("/mnt/usb0", "/dev/da0s1")]));
        l.volumes("a", T0 + 2 * MIN, Vec::new());
        flapping(&mut l, "a", T0 + 3 * MIN, 3);
        l.link("a", T0, LinkKind::Wifi, None);
        assert_eq!(
            l.report("a", T0 + 7 * MIN).hint.unwrap().kind,
            HintKind::KeepsDropping
        );
    }

    #[test]
    fn the_m2_slot_is_not_usb_and_only_external_slots_count() {
        let d = drives_of([
            ("/data", "/dev/ssd0.user"),
            ("/mnt/ext0", "/dev/nvme0"),
            ("/mnt/ext1", "/dev/da0s1"),
            ("/mnt/usb0", "/dev/da1s1"),
            ("/mnt/ps5upload/img", "/dev/lvd2"),
        ]);
        assert_eq!(
            d,
            vec![
                Drive {
                    path: "/mnt/ext0".into(),
                    usb: false
                },
                Drive {
                    path: "/mnt/ext1".into(),
                    usb: true
                },
                Drive {
                    path: "/mnt/usb0".into(),
                    usb: true
                },
            ]
        );
    }

    #[test]
    fn volumes_are_read_first_and_after_a_drop_and_the_link_only_while_it_drops() {
        let mut l = Links::default();
        // First answer: read the volumes once, not the link.
        assert_eq!(
            l.status_ok("a", T0, Some(1), None),
            FollowUp {
                volumes: true,
                link: false
            }
        );
        l.volumes("a", T0, Vec::new());
        // A steady console reads nothing more.
        assert_eq!(
            l.status_ok("a", T0 + 10_000, Some(1), None),
            FollowUp::default()
        );
        assert_eq!(
            l.status_ok("a", T0 + 5 * MIN, Some(1), None),
            FollowUp::default()
        );
        // Back from a drop: the volumes again.
        l.status_failed("a", T0 + 6 * MIN, Some(DropWhy::Timeout));
        assert!(l.status_ok("a", T0 + 7 * MIN, Some(1), None).volumes);
        // Back again 20 s later: not again yet.
        l.status_failed("a", T0 + 7 * MIN + 10_000, Some(DropWhy::Timeout));
        let f = l.status_ok("a", T0 + 7 * MIN + 20_000, Some(1), None);
        assert!(!f.volumes);
        // Two recent drops: the link is asked for, once per few minutes.
        assert!(f.link);
        l.status_failed("a", T0 + 7 * MIN + 30_000, Some(DropWhy::Timeout));
        assert!(!l.status_ok("a", T0 + 7 * MIN + 40_000, Some(1), None).link);
    }

    #[test]
    fn the_record_is_bounded_and_says_no_address() {
        let mut l = Links::default();
        flapping(&mut l, "192.0.2.10", T0, 100);
        let r = l.report("192.0.2.10", T0 + 101 * MIN);
        assert!(r.events.len() <= MAX_EVENTS);
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("192.0.2.10"), "{json}");
        // Old events fall out.
        l.status_failed(
            "192.0.2.10",
            T0 + KEEP_MS + 200 * MIN,
            Some(DropWhy::Timeout),
        );
        let r = l.report("192.0.2.10", T0 + KEEP_MS + 200 * MIN);
        assert!(r.events.iter().all(|e| e.at() >= T0 + 200 * MIN), "{r:?}");
        assert_eq!(key("192.0.2.10:9120"), "192.0.2.10");
    }
}
