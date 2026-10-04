//! Whether a console can be used at all, decided before a job starts.
//!
//! AVA1 is the only transport. A console that has no AVA1 listener (nothing sent, or an older
//! helper that only speaks the retired protocol) and a console that has not accepted this app yet
//! are both failures the person can act on, so they carry stable tokens the client keys on:
//! `helper_not_ava1` (send the helper again) and `not_paired` (open the pairing dialog).
//! There is no fallback to another protocol.

use std::time::Duration;

use ava1::gen;
use ava1::Ava1Error;

use crate::pool::{pool, Pool};
use crate::upload::{terminal_connection_reason, UploadFailure};

pub use ps5upload_core::mgmt::{
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
    let failure = match session {
        Err(_) => UploadFailure {
            reason: "ava1_unreachable".into(),
            detail: "the console did not answer on the AVA1 port in time".into(),
        },
        Ok(Err(e)) => classify(&e),
        Ok(Ok(s)) if s.peer_caps() & cap == cap => return Ok(()),
        // It answered but does not serve what this app needs: an older helper. Not cached:
        // the session is live and asking again costs nothing.
        Ok(Ok(_)) => return Err(helper_not_ava1()),
    };
    pool.note_refusal(console, &failure.reason, &failure.detail);
    Err(failure)
}

/// [`require_in`] on the process's pool, for the data plane (uploads and downloads).
pub fn require_ava1(console: &str) -> Result<(), UploadFailure> {
    require_in(pool(), console, gen::CAP_DATA_PLANE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_listening_is_helper_not_ava1_with_the_exact_message() {
        let d = std::env::temp_dir().join(format!("p5a-console-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        // Port 1 is never a listener: the attempt is refused at once.
        let p = Pool::new(d.join("ava")).with_addr("127.0.0.1:1");
        let f = require_in(&p, "c", gen::CAP_DATA_PLANE).unwrap_err();
        assert_eq!(f.reason, "helper_not_ava1");
        assert_eq!(
            f.detail,
            "The PS5 helper is not running or is an old version. Desktop app: send it from the Connection screen, or click Update helper. Web UI: start the ps5upload payload on the console with your payload loader, or click Update helper."
        );
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
}
