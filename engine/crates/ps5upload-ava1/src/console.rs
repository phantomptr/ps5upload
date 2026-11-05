//! Whether a console can be used at all, decided before a job starts.
//!
//! AVA1 is the only transport. A console that has no AVA1 listener (nothing sent, or an older
//! helper that only speaks the retired protocol) and a console that has not accepted this app yet
//! are both failures the person can act on, so they carry stable tokens the client keys on:
//! `helper_not_ava1` (send the helper again) and `not_paired` (open the pairing dialog).
//! There is no fallback to another protocol.

use std::time::Duration;

use crate::gen;
use ava1::Ava1Error;

use crate::pool::{pool, Pool};
use crate::upload::{terminal_connection_reason, UploadFailure};

pub use ps5upload_core::ddp::Power;
use ps5upload_core::ddp::{CODE_AWAKE, CODE_STANDBY};
pub use ps5upload_core::mgmt::{
    AVA1_UNREACHABLE, CONSOLE_NOT_ANSWERING_MESSAGE, CONSOLE_REST_MODE, CONSOLE_REST_MODE_MESSAGE,
    HELPER_NOT_AVA1, HELPER_NOT_AVA1_MESSAGE, NOT_PAIRED, NOT_PAIRED_MESSAGE,
};

/// How long one session attempt may take (nominal constant: an unreachable console must not
/// stall a job start for longer than this).
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The two actionable failures as a job failure (reason token plus message).
pub fn helper_not_ava1() -> UploadFailure {
    UploadFailure {
        reason: HELPER_NOT_AVA1.into(),
        detail: HELPER_NOT_AVA1_MESSAGE.into(),
    }
}

/// Nothing listens on the helper's port, and the console's payload loader (port 9021) does:
/// the app can send the helper right away.
pub const HELPER_DOWN_LOADER_UP_MESSAGE: &str = "The PS5 helper is not running, but the \
    console's payload loader is (port 9021), so the app can send it. Press Send helper on the \
    Connection screen, or load ps5upload.elf with your payload loader.";
/// Nothing listens on the helper's port or on the loader's: the console has no jailbreak
/// loader up, which the helper cannot be sent without.
pub const HELPER_DOWN_LOADER_DOWN_MESSAGE: &str = "Nothing on the PS5 answers: neither the \
    ps5upload helper nor a payload loader on port 9021. Run your jailbreak's loader first \
    (etaHEN, elfldr or the loader you use), then send the helper: Send helper in the app, or \
    load ps5upload.elf yourself.";
/// A helper answered, but it is too old for this app.
pub const HELPER_TOO_OLD_MESSAGE: &str = "An older ps5upload helper is running on the PS5; \
    this app needs the one that ships with it. Press Send helper on the Connection screen, or \
    load the ps5upload.elf of this version with your payload loader.";

/// `helper_not_ava1` with the message that fits what was found: see the three constants.
fn helper_not_ava1_because(detail: &'static str) -> UploadFailure {
    UploadFailure {
        reason: HELPER_NOT_AVA1.into(),
        detail: detail.into(),
    }
}

/// Whether a payload loader answers on the console's port 9021 (one connect, a second at
/// most): tells "no helper, send it" from "no jailbreak loader up at all" (#447).
fn loader_listening(addr: &str) -> bool {
    // `host_of` keeps an IPv6 literal whole (`rsplit_once(':')` cut `[fe80::1]` to `[fe80:`).
    let target = ps5upload_core::payload_lifecycle::join_host_port(
        &crate::pool::host_of(addr),
        ps5upload_core::payload_lifecycle::PS5_LOADER_PORT,
    );
    let Ok(mut it) = std::net::ToSocketAddrs::to_socket_addrs(target.as_str()) else {
        return false;
    };
    it.any(|a| std::net::TcpStream::connect_timeout(&a, Duration::from_secs(1)).is_ok())
}

pub fn not_paired() -> UploadFailure {
    UploadFailure {
        reason: NOT_PAIRED.into(),
        detail: NOT_PAIRED_MESSAGE.into(),
    }
}

/// Maps a failed session attempt to the reason the person can act on. Nothing listening is
/// `helper_not_ava1`; an unpaired console (or one whose pairing window is closed) is
/// `not_paired`; anything else keeps the adapters' own reasons (`ava1_wrong_console`, and
/// `ava1_unreachable` for a timeout or other network failure).
pub fn classify(e: &Ava1Error) -> UploadFailure {
    match e {
        Ava1Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => {
            helper_not_ava1()
        }
        Ava1Error::NotPaired => not_paired(),
        Ava1Error::Refused { code, .. }
            if *code == gen::ERR_NOT_PAIRED || *code == gen::ERR_PAIRING_CLOSED =>
        {
            not_paired()
        }
        other => UploadFailure {
            reason: terminal_connection_reason(other)
                .unwrap_or("ava1_unreachable")
                .into(),
            detail: format!("could not open an AVA1 session with the console: {other}"),
        },
    }
}

/// Ok when a paired AVA1 session to `console` exists (or opens now) and its node advertises
/// every bit of `cap`; the session is kept by the pool and reused by the job that follows.
/// Blocking: call from a blocking thread.
pub fn require_in(pool: &Pool, console: &str, cap: u64) -> Result<(), UploadFailure> {
    // A failed attempt is repeated for a few seconds without dialling again: the client polls
    // many endpoints, and each fresh handshake with an unpaired console can show a pairing code
    // and count against the console's connection cap, while an unreachable one blocks for the
    // whole probe. A pairing or any successful session clears it (`Pool::clear_refusal`).
    if let Some((reason, detail)) = pool.recent_refusal(console) {
        return Err(UploadFailure { reason, detail });
    }
    let session =
        crate::block_on(async { tokio::time::timeout(PROBE_TIMEOUT, pool.session(console)).await });
    let (failure, power) = match session {
        // Nothing answered in time: rest mode with DDP up, or off / another network.
        Err(_) => silent(
            pool,
            console,
            "the console did not answer on the AVA1 port in time",
        ),
        Ok(Err(e)) if no_tcp_answer(&e) => {
            silent(pool, console, &format!("could not reach the console: {e}"))
        }
        // Nothing on the helper's port: say whether the loader is up, which decides what the
        // person does next (#447: "not running or an old version" sent people in circles).
        Ok(Err(Ava1Error::Io(io))) if io.kind() == std::io::ErrorKind::ConnectionRefused => {
            if loader_listening(&pool.addr_for(console)) {
                (
                    helper_not_ava1_because(HELPER_DOWN_LOADER_UP_MESSAGE),
                    Some(Power::Awake),
                )
            } else {
                // Both refused: a console in rest mode refuses every port. DDP tells it from
                // an awake console with no loader up.
                match pool.ddp_code(console) {
                    Some(CODE_STANDBY) => (console_rest_mode(), Some(Power::Rest)),
                    Some(CODE_AWAKE) => (
                        helper_not_ava1_because(HELPER_DOWN_LOADER_DOWN_MESSAGE),
                        Some(Power::Awake),
                    ),
                    _ => (
                        helper_not_ava1_because(HELPER_DOWN_LOADER_DOWN_MESSAGE),
                        None,
                    ),
                }
            }
        }
        Ok(Err(e)) => (classify(&e), None),
        Ok(Ok(s)) if s.peer_caps() & cap == cap => return Ok(()),
        // It answered but does not serve what this app needs: an older helper. Not cached:
        // the session is live and asking again costs nothing.
        Ok(Ok(_)) => return Err(helper_not_ava1_because(HELPER_TOO_OLD_MESSAGE)),
    };
    pool.note_verdict(console, &failure.reason, &failure.detail, power);
    Err(failure)
}

/// A session attempt that got no TCP answer at all (as opposed to a refusal): a timeout, or
/// no route to the host. The console is off, elsewhere, or asleep.
fn no_tcp_answer(e: &Ava1Error) -> bool {
    use std::io::ErrorKind::{HostUnreachable, NetworkDown, NetworkUnreachable, TimedOut};
    match e {
        Ava1Error::Timeout => true,
        Ava1Error::Io(io) => {
            matches!(
                io.kind(),
                TimedOut | HostUnreachable | NetworkUnreachable | NetworkDown
            ) || ps5upload_core::ddp::host_is_down(io)
        }
        _ => false,
    }
}

/// The failure for a console that gave no TCP answer, explained by DDP: rest mode on 620,
/// "not answering" on silence, and the plain `ava1_unreachable` (with `detail`) when DDP says
/// it is awake (something else is wrong on the way).
fn silent(pool: &Pool, console: &str, detail: &str) -> (UploadFailure, Option<Power>) {
    match pool.ddp_code(console) {
        Some(CODE_STANDBY) => (console_rest_mode(), Some(Power::Rest)),
        Some(CODE_AWAKE) => (
            UploadFailure {
                reason: AVA1_UNREACHABLE.into(),
                detail: detail.into(),
            },
            Some(Power::Awake),
        ),
        Some(_) => (
            UploadFailure {
                reason: AVA1_UNREACHABLE.into(),
                detail: detail.into(),
            },
            None,
        ),
        None => (
            UploadFailure {
                reason: AVA1_UNREACHABLE.into(),
                detail: CONSOLE_NOT_ANSWERING_MESSAGE.into(),
            },
            Some(Power::Unreachable),
        ),
    }
}

/// The console is in rest mode (DDP answered 620): not a fault, waking it is the fix.
pub fn console_rest_mode() -> UploadFailure {
    UploadFailure {
        reason: CONSOLE_REST_MODE.into(),
        detail: CONSOLE_REST_MODE_MESSAGE.into(),
    }
}

/// What DDP said about `console`'s power when its last session attempt on the process's pool
/// failed, while that failure is remembered (`None`: not asked, or no fresh failure).
pub fn recent_power(console: &str) -> Option<Power> {
    pool().recent_power(console)
}

/// [`require_in`] on the process's pool, for the data plane (uploads and downloads).
pub fn require_ava1(console: &str) -> Result<(), UploadFailure> {
    require_in(pool(), console, gen::CAP_DATA_PLANE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn host_is_down_counts_as_no_tcp_answer() {
        let e = Ava1Error::Io(std::io::Error::from_raw_os_error(libc::EHOSTDOWN));
        assert!(no_tcp_answer(&e));
        let refused = Ava1Error::Io(std::io::ErrorKind::ConnectionRefused.into());
        assert!(!no_tcp_answer(&refused));
    }

    #[test]
    fn nothing_listening_is_helper_not_ava1_with_the_exact_message() {
        let d = std::env::temp_dir().join(format!("p5a-console-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        // Port 1 is never a listener: the attempt is refused at once.
        let p = Pool::new(d.join("ava")).with_addr("127.0.0.1:1");
        let f = require_in(&p, "c", gen::CAP_DATA_PLANE).unwrap_err();
        assert_eq!(f.reason, "helper_not_ava1");
        // No loader listens on this machine's 9021 either (the test would otherwise say the
        // app can send the helper, which is the other true answer).
        if loader_listening("127.0.0.1:1") {
            assert_eq!(f.detail, HELPER_DOWN_LOADER_UP_MESSAGE);
        } else {
            assert_eq!(f.detail, HELPER_DOWN_LOADER_DOWN_MESSAGE);
        }
        assert!(f.detail.contains("port 9021"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unpaired_console_is_not_paired() {
        assert_eq!(classify(&Ava1Error::NotPaired).reason, "not_paired");
        for code in [gen::ERR_NOT_PAIRED, gen::ERR_PAIRING_CLOSED] {
            let e = Ava1Error::Refused {
                code,
                message: String::new(),
            };
            assert_eq!(classify(&e).reason, "not_paired");
        }
    }

    #[test]
    fn a_missing_identity_is_unreachable_not_a_pairing_prompt() {
        let f = require_in(&Pool::unavailable(), "c", gen::CAP_DATA_PLANE).unwrap_err();
        assert_eq!(f.reason, "ava1_unreachable");
    }

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A pool whose every console refuses at once (port 1) and whose DDP answers `code`;
    /// the counter is how many times DDP was asked.
    fn refusing(tag: &str, code: Option<u16>) -> (Pool, Arc<AtomicUsize>, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("p5a-rest-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let asked = Arc::new(AtomicUsize::new(0));
        let n = asked.clone();
        let p = Pool::new(d.join("ava"))
            .with_addr("127.0.0.1:1")
            .with_power_probe(move |_| {
                n.fetch_add(1, Ordering::SeqCst);
                code
            });
        (p, asked, d)
    }

    /// A listener that never accepts: the TCP connect completes from the backlog and the
    /// handshake is never answered, so the session attempt times out (`PROBE_TIMEOUT`).
    fn silent_listener() -> (std::net::TcpListener, String) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap().to_string();
        (l, a)
    }

    #[test]
    fn refused_everywhere_and_ddp_620_is_rest_mode() {
        let (p, asked, d) = refusing("620", Some(620));
        // A loader on this machine's 9021 would make it the "send the helper" answer instead.
        if loader_listening("127.0.0.1:1") {
            return;
        }
        let f = require_in(&p, "c", gen::CAP_DATA_PLANE).unwrap_err();
        assert_eq!(f.reason, "console_rest_mode");
        assert_eq!(
            f.detail,
            "The PS5 is in rest mode. Wake it, and the helper can be sent again."
        );
        assert_eq!(p.recent_power("c"), Some(Power::Rest));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ddp_200_or_silence_keeps_the_old_refusal() {
        if loader_listening("127.0.0.1:1") {
            return;
        }
        for (tag, code, power) in [("200", Some(200), Some(Power::Awake)), ("none", None, None)] {
            let (p, _, d) = refusing(tag, code);
            let f = require_in(&p, "c", gen::CAP_DATA_PLANE).unwrap_err();
            assert_eq!(f.reason, "helper_not_ava1", "{tag}");
            assert_eq!(f.detail, HELPER_DOWN_LOADER_DOWN_MESSAGE, "{tag}");
            assert_eq!(p.recent_power("c"), power, "{tag}");
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn a_timeout_with_no_ddp_reply_is_unreachable_in_plain_words() {
        let (_l, addr) = silent_listener();
        let d = std::env::temp_dir().join(format!("p5a-rest-timeout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let p = Pool::new(d.join("ava"))
            .with_addr(addr)
            .with_power_probe(|_| None);
        let f = require_in(&p, "c", gen::CAP_DATA_PLANE).unwrap_err();
        assert_eq!(f.reason, "ava1_unreachable");
        assert_eq!(
            f.detail,
            "The PS5 isn't answering: it's off, on another network, or in rest mode with its network features off."
        );
        assert_eq!(p.recent_power("c"), Some(Power::Unreachable));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_tcp_answer_is_explained_by_ddp() {
        let (p, _, d) = refusing("silent", Some(620));
        assert_eq!(
            silent(&p, "c", "x").0.reason,
            "console_rest_mode",
            "a 620 is rest mode"
        );
        let (p, _, d2) = refusing("silent200", Some(200));
        let (f, power) = silent(&p, "c", "the console did not answer in time");
        assert_eq!(f.reason, "ava1_unreachable");
        assert_eq!(f.detail, "the console did not answer in time", "old text");
        assert_eq!(power, Some(Power::Awake));
        assert!(no_tcp_answer(&Ava1Error::Timeout));
        assert!(no_tcp_answer(&Ava1Error::Io(std::io::Error::from(
            std::io::ErrorKind::TimedOut
        ))));
        assert!(!no_tcp_answer(&Ava1Error::Io(std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused
        ))));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&d2);
    }

    #[test]
    fn a_rest_verdict_is_remembered_past_the_client_poll() {
        if loader_listening("127.0.0.1:1") {
            return;
        }
        assert_eq!(crate::pool::PROBED_REFUSAL_TTL, Duration::from_secs(12));
        assert!(crate::pool::PROBED_REFUSAL_TTL > Duration::from_secs(10));
        // The plain refusal TTL is zero here: only the probed TTL can hold the answer.
        let (p, asked, d) = refusing("cache", Some(620));
        let p = p.with_refusal_ttl(Duration::ZERO);
        for _ in 0..10 {
            let f = require_in(&p, "c", gen::CAP_DATA_PLANE).unwrap_err();
            assert_eq!(f.reason, "console_rest_mode");
        }
        assert_eq!(p.attempts(), 1, "ten calls, one dial");
        assert_eq!(asked.load(Ordering::SeqCst), 1, "ten calls, one DDP probe");
        // Once the probed TTL is over, the console is asked again.
        let p = p.with_probed_refusal_ttl(Duration::ZERO);
        require_in(&p, "c", gen::CAP_DATA_PLANE).unwrap_err();
        assert_eq!(p.attempts(), 2);
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        // A refusal DDP did not explain keeps the plain TTL.
        let (q, _, d2) = refusing("cache-none", None);
        let q = q.with_refusal_ttl(Duration::ZERO);
        require_in(&q, "c", gen::CAP_DATA_PLANE).unwrap_err();
        require_in(&q, "c", gen::CAP_DATA_PLANE).unwrap_err();
        assert_eq!(q.attempts(), 2, "an unexplained refusal is not held longer");
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&d2);
    }
}
