//! Best-effort "tell the currently-running payload to exit so a fresh
//! one can take its place" RPC.
//!
//! Why this exists: the PS5 ELF loader (port 9021) is fire-and-forget —
//! when the desktop pushes new payload bytes it spawns a fresh process,
//! but the OLD payload process is unaware and keeps running. The two
//! contend for the same management port (:9114). On most firmwares the
//! new payload's bind(:9114) fails, the new process exits, and the
//! user is left with the OLD payload still answering — but now with
//! wire-protocol expectations that may not match the desktop's current
//! build. Symptoms: install RPCs that bail with "read frame header"
//! because the old payload's frame handler doesn't understand a newer
//! frame, lingering :9113 transfers, the user's "I sent the payload
//! but nothing changed" report.
//!
//! The fix is desktop-side: BEFORE pushing fresh ELF bytes to :9021,
//! send a Shutdown frame to the existing :9114. The payload's
//! shutdown handler sets a flag the main loop honours; the old
//! process exits, its ports go free, the new payload's bind succeeds.
//!
//! Best-effort by design — every error path returns Ok(false) because
//! "no old payload running" is the common case (first session boot,
//! console reboot, etc) and we don't want to block the send.

use std::time::Duration;

use ftx2_proto::FrameType;

use crate::connection::Connection;

/// Send a Shutdown frame to the payload running at `mgmt_addr`
/// (typically `<ps5-ip>:9114`).
///
/// Returns Ok(true) iff a Shutdown_Ack was received — i.e. we hit a
/// real ps5upload payload and it acknowledged. Ok(false) on any
/// failure (nothing listening, wrong process answering, ACK timeout):
/// the caller proceeds as if there were no payload to displace, which
/// is the right behaviour for the "first-send-of-the-session" path.
///
/// IO timeout is tightened from Connection's default 30 s down to
/// 2 s for both the frame send and the ACK read — the payload's
/// shutdown handler is one mutex flip and a 2-byte response; if
/// either takes longer than 2 s we'd rather give up and let the
/// new payload's bind tell the user whatever is really wrong.
pub fn shutdown_running_payload(mgmt_addr: &str) -> std::io::Result<bool> {
    let mut c = match Connection::connect(mgmt_addr) {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    // Tighten IO timeout for the small handshake. A failure here is
    // surprising (the TCP socket is freshly connected) so we surface
    // it as a real io::Error — but the caller treats anything ≠ true
    // the same way, so this still degrades gracefully.
    c.set_io_timeout(Duration::from_secs(2))?;
    if c.send_frame(FrameType::Shutdown, &[]).is_err() {
        return Ok(false);
    }
    match c.recv_frame() {
        Ok((hdr, _)) => {
            let ft = hdr.frame_type().unwrap_or(FrameType::Error);
            Ok(ft == FrameType::ShutdownAck)
        }
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// We can't easily spin up a fake payload server in a unit test
    /// (Connection wants raw FTX2 framing and we'd be re-implementing
    /// the server side just to handshake). The interesting failure
    /// mode for callers is "nothing listening on that addr" — assert
    /// that surfaces as Ok(false), not Err.
    #[test]
    fn nothing_listening_returns_ok_false() {
        // 198.51.100.0/24 is RFC 5737 TEST-NET-2; nothing should answer.
        let res = shutdown_running_payload("198.51.100.1:9114");
        match res {
            Ok(false) => {}
            other => panic!("expected Ok(false), got {other:?}"),
        }
    }
}
