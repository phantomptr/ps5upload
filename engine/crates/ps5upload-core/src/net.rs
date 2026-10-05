//! Socket helpers shared by the host-side clients: resolving an address to the
//! sockets to try, and telling a transient local resource error from a dead peer.

use std::io;
use std::net::{SocketAddr, ToSocketAddrs};

use anyhow::{bail, Context, Result};

/// True when an `io::Error` is a *transient local* network-stack resource
/// exhaustion — the host kernel momentarily lacked socket-buffer space or a
/// free ephemeral port — as opposed to a problem with the peer.
///
/// The motivating case is Windows `WSAENOBUFS` (os error 10055) under
/// multi-stream upload churn: 4 concurrent transfer connections plus
/// resume-driven reconnects accumulate sockets in TIME_WAIT, exhausting the
/// nonpaged pool / dynamic-port range, and a fresh `connect()` (or a
/// mid-upload reconnect) fails. It is fully recoverable: the resource frees
/// within a few hundred ms, so the right response is a short backoff-and-retry,
/// never failing the whole transfer. Pre-fix this surfaced as
/// `ErrorKind::Other`, which `is_retryable_transfer_error` treats as fatal, so
/// one spike aborted the upload ("too much buffer error middle of upload").
///
/// Exposed `pub(crate)` so `transfer::is_retryable_transfer_error` can use the same
/// classification.
pub(crate) fn is_transient_local_resource_error(e: &io::Error) -> bool {
    match e.raw_os_error() {
        Some(code) => transient_resource_code(code),
        None => false,
    }
}

/// Windows variant: classify WSA error codes. `raw_os_error()` on Windows
/// sockets carries the `WSAGetLastError` value (e.g. the 10055 the user sees in
/// `(os error 10055)`), so we match the WSA code space directly.
#[cfg(windows)]
fn transient_resource_code(code: i32) -> bool {
    matches!(
        code,
        10055 /* WSAENOBUFS  — no buffer space available */
            | 10048 /* WSAEADDRINUSE — ephemeral port exhaustion under churn */
    )
}

/// POSIX variant: ENOBUFS / EADDRNOTAVAIL / EADDRINUSE map to the same
/// "host stack momentarily out of resources" condition (rare on Unix at our
/// connection rate, but the retry is harmless and keeps the two platforms'
/// behaviour aligned). Uses `libc` constants rather than hard-coded numbers
/// because ENOBUFS differs across Unixes (macOS/BSD 55, Linux 105).
#[cfg(unix)]
fn transient_resource_code(code: i32) -> bool {
    code == libc::ENOBUFS || code == libc::EADDRINUSE || code == libc::EADDRNOTAVAIL
}

#[cfg(not(any(windows, unix)))]
fn transient_resource_code(_code: i32) -> bool {
    false
}

/// Resolve a `host:port` string into the concrete socket addresses to try.
///
/// `SocketAddr`'s `FromStr` is a pure literal parser (it never consults the resolver), so a
/// console entered by DNS name (`ps5.lan:9020`) needs this: it is issue #272. The address must
/// carry a port; a bare host is an error (callers join one with the port they mean).
///
/// Candidates are returned IPv4-first. The PS5's LAN listeners are IPv4 in practice, and a
/// name that also carries an unreachable AAAA would otherwise burn a full connect timeout on
/// IPv6 before falling back. The IP-literal fast path stays ahead of `to_socket_addrs` so the
/// common case (the address the app stores after discovery) never touches the resolver.
pub(crate) fn resolve_connect_targets(addr: &str) -> Result<Vec<SocketAddr>> {
    if let Ok(sa) = addr.parse::<SocketAddr>() {
        return Ok(vec![sa]);
    }
    let mut out: Vec<SocketAddr> = addr
        .to_socket_addrs()
        .with_context(|| format!("resolve addr: {addr}"))?
        .collect();
    if out.is_empty() {
        bail!("resolve addr: {addr} resolved to no addresses");
    }
    out.sort_by_key(|sa| !sa.is_ipv4());
    Ok(out)
}

#[cfg(test)]
mod transient_resource_error_tests {
    //! Pin the WSAENOBUFS-class classification. The bug this guards against:
    //! a Windows host under multi-stream upload churn returns WSAENOBUFS
    //! (os error 10055) from `connect()`, which maps to `ErrorKind::Other`.
    //! Before the fix `is_retryable_transfer_error` treated `Other` as fatal,
    //! so one transient buffer spike aborted the whole upload. These tests
    //! lock in that the OS-code classifier recognises the transient codes and
    //! ignores unrelated ones.
    use super::*;

    #[test]
    #[cfg(windows)]
    fn wsaenobufs_is_transient() {
        let e = io::Error::from_raw_os_error(10055);
        assert!(is_transient_local_resource_error(&e));
    }

    #[test]
    #[cfg(windows)]
    fn wsaeaddrinuse_is_transient() {
        let e = io::Error::from_raw_os_error(10048);
        assert!(is_transient_local_resource_error(&e));
    }

    #[test]
    #[cfg(windows)]
    fn connection_refused_is_not_transient() {
        // WSAECONNREFUSED (10061) is a dead/closed peer — must fast-fail,
        // not retry, so a genuinely offline PS5 surfaces immediately.
        let e = io::Error::from_raw_os_error(10061);
        assert!(!is_transient_local_resource_error(&e));
    }

    #[test]
    #[cfg(unix)]
    fn enobufs_is_transient() {
        let e = io::Error::from_raw_os_error(libc::ENOBUFS);
        assert!(is_transient_local_resource_error(&e));
    }

    #[test]
    #[cfg(unix)]
    fn connection_refused_is_not_transient() {
        let e = io::Error::from_raw_os_error(libc::ECONNREFUSED);
        assert!(!is_transient_local_resource_error(&e));
    }

    #[test]
    fn non_os_error_is_not_transient() {
        // An error with no raw OS code (e.g. a synthesised ErrorKind) must
        // not be misclassified as a recoverable resource spike.
        let e = io::Error::other("synthetic");
        assert!(!is_transient_local_resource_error(&e));
    }
}

#[cfg(test)]
mod addr_resolution_tests {
    //! Hostname support: `SocketAddr`'s `FromStr` never consults the resolver (#272).
    use super::*;

    #[test]
    fn literal_socket_addrs_resolve_to_themselves() {
        let out = resolve_connect_targets("192.168.1.131:9020").unwrap();
        assert_eq!(
            out,
            vec!["192.168.1.131:9020".parse::<SocketAddr>().unwrap()]
        );
    }

    #[test]
    fn bracketed_ipv6_literals_still_parse() {
        let out = resolve_connect_targets("[::1]:9020").unwrap();
        assert_eq!(out, vec!["[::1]:9020".parse::<SocketAddr>().unwrap()]);
    }

    #[test]
    fn a_bare_host_is_refused_not_given_a_guessed_port() {
        assert!(resolve_connect_targets("192.168.1.131").is_err());
    }

    #[test]
    fn hostnames_are_resolved() {
        // `localhost` is the one name every CI box resolves. It may map to
        // both ::1 and 127.0.0.1; we only assert that resolution happened
        // and produced something usable.
        let out = resolve_connect_targets("localhost:9020").unwrap();
        assert!(
            !out.is_empty(),
            "localhost must resolve to at least one addr"
        );
        assert!(out.iter().all(|sa| sa.port() == 9020));
    }

    #[test]
    fn ipv4_candidates_are_tried_first() {
        let mixed = resolve_connect_targets("localhost:9020").unwrap();
        if mixed.iter().any(|sa| sa.is_ipv4()) && mixed.iter().any(|sa| sa.is_ipv6()) {
            assert!(mixed[0].is_ipv4(), "IPv4 must sort first: {mixed:?}");
        }
    }

    #[test]
    fn unresolvable_names_report_the_name() {
        let err = resolve_connect_targets("no-such-host.invalid:9020").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no-such-host.invalid:9020"),
            "error must name the address the user typed: {msg}"
        );
    }
}
