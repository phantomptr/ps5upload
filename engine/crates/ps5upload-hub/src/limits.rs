//! Per-client counters. A client is an IPv4 address or an IPv6 /64 (one home or one phone gets a
//! whole /64, so counting single IPv6 addresses would be no limit at all). Keys live in memory
//! only and are never logged.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// The rate-limit and room-count key for an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IpKey {
    V4(u32),
    V6Prefix(u64),
}

impl IpKey {
    pub fn of(ip: IpAddr) -> Self {
        match canonical_ip(ip) {
            IpAddr::V4(v4) => IpKey::V4(u32::from(v4)),
            IpAddr::V6(v6) => IpKey::V6Prefix((u128::from(v6) >> 64) as u64),
        }
    }
}

/// The IPv6 /48 an address sits in, when it is IPv6. A provider hands a business or a hosting
/// customer a whole /48: 65,536 of the /64s that [`IpKey`] counts as separate clients. Open
/// connections are counted per /48 as well, so such a block cannot fill the hub by itself.
pub fn v6_site(ip: IpAddr) -> Option<u64> {
    match canonical_ip(ip) {
        IpAddr::V4(_) => None,
        IpAddr::V6(v6) => Some((u128::from(v6) >> 80) as u64),
    }
}

/// IPv4-mapped IPv6 (`::ffff:a.b.c.d`, what a dual-stack socket reports) as plain IPv4.
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// A fixed-window counter: at most `max` events per `window`.
#[derive(Clone, Debug)]
pub struct Window {
    start: Instant,
    count: u32,
}

impl Window {
    pub fn new(now: Instant) -> Self {
        Self {
            start: now,
            count: 0,
        }
    }

    /// Counts one event; false when it is over the limit (an over-limit event is not counted).
    pub fn hit(&mut self, now: Instant, window: Duration, max: u32) -> bool {
        if now.duration_since(self.start) >= window {
            self.start = now;
            self.count = 0;
        }
        if self.count >= max {
            return false;
        }
        self.count += 1;
        true
    }

    fn expired(&self, now: Instant, window: Duration) -> bool {
        now.duration_since(self.start) >= window
    }
}

/// Windows keyed by client, bounded: when `max_keys` clients are tracked, a new client is refused
/// until [`RateTable::sweep`] drops the expired windows. Spoofed UDP sources therefore cost a
/// fixed amount of memory.
#[derive(Debug)]
pub struct RateTable {
    map: HashMap<IpKey, Window>,
    max_keys: usize,
}

impl RateTable {
    pub fn new(max_keys: usize) -> Self {
        Self {
            map: HashMap::new(),
            max_keys,
        }
    }

    pub fn hit(&mut self, key: IpKey, now: Instant, window: Duration, max: u32) -> bool {
        if !self.map.contains_key(&key) && self.map.len() >= self.max_keys {
            return false;
        }
        self.map
            .entry(key)
            .or_insert_with(|| Window::new(now))
            .hit(now, window, max)
    }

    pub fn sweep(&mut self, now: Instant, window: Duration) {
        self.map.retain(|_, w| !w.expired(now, window));
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_counts_and_resets() {
        let t0 = Instant::now();
        let w = Duration::from_secs(60);
        let mut win = Window::new(t0);
        assert!(win.hit(t0, w, 2));
        assert!(win.hit(t0, w, 2));
        assert!(!win.hit(t0, w, 2));
        assert!(win.hit(t0 + w, w, 2));
    }

    #[test]
    fn v6_keys_by_64_and_mapped_v4_is_v4() {
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(IpKey::of(a), IpKey::of(b));
        assert_ne!(IpKey::of(a), IpKey::of(c));
        let m: IpAddr = "::ffff:192.0.2.7".parse().unwrap();
        let v4: IpAddr = "192.0.2.7".parse().unwrap();
        assert_eq!(IpKey::of(m), IpKey::of(v4));
    }

    #[test]
    fn v6_site_is_the_48_and_v4_has_none() {
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:ffff::9".parse().unwrap();
        let c: IpAddr = "2001:db8:2:2::1".parse().unwrap();
        assert_eq!(v6_site(a), v6_site(b));
        assert_ne!(v6_site(a), v6_site(c));
        assert!(v6_site(a).is_some());
        assert_eq!(v6_site("192.0.2.7".parse().unwrap()), None);
        assert_eq!(v6_site("::ffff:192.0.2.7".parse().unwrap()), None);
    }

    #[test]
    fn table_is_bounded() {
        let t0 = Instant::now();
        let w = Duration::from_secs(60);
        let mut t = RateTable::new(1);
        assert!(t.hit(IpKey::V4(1), t0, w, 5));
        assert!(!t.hit(IpKey::V4(2), t0, w, 5));
        t.sweep(t0 + w, w);
        assert!(t.is_empty());
        assert!(t.hit(IpKey::V4(2), t0 + w, w, 5));
    }
}
