//! The PS5 user's sign-in passcode over a Remote Play ctrl connection (#608).
//!
//! When the user a session signs in has a login passcode, the console sends `LOGIN_PIN_REQ`
//! (0x0004, empty) on ctrl and waits. The client answers `LOGIN_PIN_REP` (0x8004): the digits as
//! ASCII, encrypted on its ctrl counter like any payload. The console's verdict is `LOGIN`
//! (0x0005), one byte: 0 signed in, 1 wrong (and it asks again). See chiaki-ng `ctrl.c`
//! (`ctrl_message_received_login`) and `session.c` (`CHIAKI_EVENT_LOGIN_PIN_REQUEST`).
//!
//! Both ctrl users run the same [`PinFlow`]: starting a stream (rp_stream) and Wake & sign in
//! (rp_session). The passcode never leaves it except encrypted on the wire: not logged, not kept.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use crate::rp_session::auth_encrypt;

pub(crate) const CTRL_LOGIN_PIN_REQ: u16 = 0x0004;
pub(crate) const CTRL_LOGIN_PIN_REP: u16 = 0x8004;
pub(crate) const CTRL_LOGIN: u16 = 0x0005;
/// The state byte in a LOGIN message.
pub(crate) const LOGIN_STATE_SUCCESS: u8 = 0;
pub(crate) const LOGIN_STATE_PIN_INCORRECT: u8 = 1;

/// How long the user has to type the passcode. chiaki waits for ever; a start nobody answers
/// has to end, and the console drops ctrl on its own if it tires first (that is reported too).
pub const PASSCODE_WAIT: Duration = Duration::from_secs(180);
/// Wrong passcodes in one attempt before we stop asking (the PS5 keeps its own count too).
pub const MAX_WRONG_PASSCODES: u32 = 5;
/// The error texts the engine maps to `passcode_cancelled` / `passcode_timeout` /
/// `passcode_too_many`.
pub const PASSCODE_CANCELLED: &str = "the login passcode prompt was cancelled";
pub const PASSCODE_TIMEOUT: &str = "no login passcode was entered in time";
pub const PASSCODE_CONSOLE_GAVE_UP: &str = "the console stopped waiting for the login passcode";
pub const PASSCODE_TOO_MANY: &str = "too many wrong login passcodes";

/// The user's answer when the console asks for the passcode.
pub enum Passcode {
    /// The digits typed (a PS5 passcode is four).
    Digits(String),
    /// The user would rather not: the attempt ends.
    Cancel,
}

// Never print a passcode, not even in a debug dump.
impl std::fmt::Debug for Passcode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Passcode::Digits(_) => f.write_str("Digits(<redacted>)"),
            Passcode::Cancel => f.write_str("Cancel"),
        }
    }
}

/// A PS5 user passcode: exactly four digits.
pub fn valid_passcode(s: &str) -> bool {
    s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit())
}

/// Whoever can type the passcode: the Remote Play page, or the Wake & sign in caller.
pub trait PasscodePrompt {
    /// The console asks for the passcode; `retry`: the last one sent was wrong.
    fn passcode_needed(&mut self, retry: bool);
    /// The answer, once there is one (polled while asking).
    fn passcode(&mut self) -> Option<Passcode>;
    /// The asker has gone: the attempt ends as cancelled.
    fn stopped(&mut self) -> bool {
        false
    }
}

/// `[size BE32][kind BE16][0, 0]` then the payload, encrypted with the ctrl
/// cipher at `counter` (chiaki `ctrl_message_send`).
pub(crate) fn ctrl_message(
    kind: u16,
    payload: &[u8],
    brt: &[u8; 16],
    amb: &[u8; 16],
    counter: u64,
) -> Vec<u8> {
    let mut m = Vec::with_capacity(8 + payload.len());
    m.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    m.extend_from_slice(&kind.to_be_bytes());
    m.extend_from_slice(&[0, 0]);
    m.extend_from_slice(&auth_encrypt(brt, amb, counter, payload));
    m
}

/// One ctrl connection's passcode exchange. Feed it every ctrl message ([`PinFlow::on_message`])
/// and poll it between reads ([`PinFlow::poll`]); while it is asking, the caller's own
/// "the console went quiet" deadline is suspended (the user is typing).
pub(crate) struct PinFlow {
    /// Since when the user has been asked, while there is no answer yet.
    asking: Option<Instant>,
    /// Passcodes sent: a request after one means it was wrong.
    sent: u32,
    wait: Duration,
}

impl PinFlow {
    pub(crate) fn new(wait: Duration) -> Self {
        PinFlow {
            asking: None,
            sent: 0,
            wait,
        }
    }

    /// The user is being asked.
    pub(crate) fn asking(&self) -> bool {
        self.asking.is_some()
    }

    /// A passcode exchange has happened (the console asked at least once).
    pub(crate) fn involved(&self) -> bool {
        self.asking.is_some() || self.sent > 0
    }

    /// A ctrl message (payload decrypted). Returns true when it was part of the passcode
    /// exchange (a request, or a "wrong" verdict for a passcode we sent) and is handled.
    pub(crate) fn on_message(
        &mut self,
        kind: u16,
        payload: &[u8],
        prompt: &mut dyn PasscodePrompt,
    ) -> Result<bool> {
        let ask = match kind {
            CTRL_LOGIN_PIN_REQ => true,
            // chiaki re-requests only after one was asked for; success needs nothing here.
            CTRL_LOGIN => payload.first() == Some(&LOGIN_STATE_PIN_INCORRECT) && self.sent > 0,
            _ => false,
        };
        // One prompt at a time: the console may both say "incorrect" and ask again.
        if ask && self.asking.is_none() {
            // Every passcode sent before this request was wrong.
            if self.sent >= MAX_WRONG_PASSCODES {
                bail!(PASSCODE_TOO_MANY);
            }
            self.asking = Some(Instant::now());
            prompt.passcode_needed(self.sent > 0);
        }
        Ok(ask)
    }

    /// While asking: pass the user's answer on. Returns true when a passcode went out (the
    /// caller restarts its wait for the console's verdict).
    pub(crate) fn poll(
        &mut self,
        w: &mut dyn Write,
        brt: &[u8; 16],
        amb: &[u8; 16],
        counter_local: &mut u64,
        prompt: &mut dyn PasscodePrompt,
    ) -> Result<bool> {
        let Some(since) = self.asking else {
            return Ok(false);
        };
        if prompt.stopped() {
            bail!(PASSCODE_CANCELLED);
        }
        match prompt.passcode() {
            Some(Passcode::Digits(d)) => {
                let m = ctrl_message(CTRL_LOGIN_PIN_REP, d.as_bytes(), brt, amb, *counter_local);
                *counter_local += 1;
                w.write_all(&m)?;
                w.flush().ok();
                self.sent += 1;
                self.asking = None;
                Ok(true)
            }
            Some(Passcode::Cancel) => bail!(PASSCODE_CANCELLED),
            None if since.elapsed() > self.wait => bail!(PASSCODE_TIMEOUT),
            None => Ok(false),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! A fake console's side of ctrl, shared by the stream and the sign-in tests.
    use super::*;
    use crate::rp_session::auth_decrypt;
    use std::collections::VecDeque;
    use std::io::Read;
    use std::net::TcpStream;

    pub const BRT: [u8; 16] = [7u8; 16];
    pub const AMB: [u8; 16] = [9u8; 16];

    /// A console's ctrl message: its payload encrypted on the console's counter.
    pub fn console_msg(kind: u16, payload: &[u8], counter: &mut u64) -> Vec<u8> {
        let mut m = Vec::new();
        m.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        m.extend_from_slice(&kind.to_be_bytes());
        m.extend_from_slice(&[0, 0]);
        if !payload.is_empty() {
            m.extend_from_slice(&auth_encrypt(&BRT, &AMB, *counter, payload));
            *counter += 1;
        }
        m
    }

    pub fn read_msg(c: &mut TcpStream) -> (u16, Vec<u8>) {
        let mut h = [0u8; 8];
        c.read_exact(&mut h).unwrap();
        let size = u32::from_be_bytes(h[0..4].try_into().unwrap()) as usize;
        let mut p = vec![0u8; size];
        c.read_exact(&mut p).unwrap();
        (u16::from_be_bytes([h[4], h[5]]), p)
    }

    /// Read our next LOGIN_PIN_REP (skipping heartbeat replies) and decrypt it at `counter`.
    pub fn read_pin(c: &mut TcpStream, counter: u64) -> Vec<u8> {
        loop {
            match read_msg(c) {
                (CTRL_LOGIN_PIN_REP, p) => return auth_decrypt(&BRT, &AMB, counter, &p),
                (0x01fe, _) => continue,
                (k, _) => panic!("unexpected ctrl message {k:#06x}"),
            }
        }
    }

    /// A user who answers from a list (None: still typing).
    #[derive(Default)]
    pub struct Typist {
        pub asked: Vec<bool>,
        pub answers: VecDeque<Option<Passcode>>,
    }

    impl Typist {
        pub fn new(answers: Vec<Option<Passcode>>) -> Self {
            Typist {
                asked: vec![],
                answers: answers.into(),
            }
        }
    }

    impl PasscodePrompt for Typist {
        fn passcode_needed(&mut self, retry: bool) {
            self.asked.push(retry);
        }
        fn passcode(&mut self) -> Option<Passcode> {
            self.answers.pop_front().flatten()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn passcodes_are_four_digits_and_never_printed() {
        assert!(valid_passcode("0042"));
        assert!(!valid_passcode("123"));
        assert!(!valid_passcode("12345"));
        assert!(!valid_passcode("12a4"));
        assert!(!format!("{:?}", Passcode::Digits("1234".into())).contains("1234"));
    }

    #[test]
    fn five_wrong_passcodes_end_the_asking() {
        let mut flow = PinFlow::new(PASSCODE_WAIT);
        let mut user = Typist::new(
            (0..6)
                .map(|_| Some(Passcode::Digits("0000".into())))
                .collect(),
        );
        let mut out = Vec::new();
        let mut counter = 5;
        assert!(flow.on_message(CTRL_LOGIN_PIN_REQ, &[], &mut user).unwrap());
        for i in 1..=5u64 {
            assert!(flow.asking());
            assert!(flow
                .poll(&mut out, &BRT, &AMB, &mut counter, &mut user)
                .unwrap());
            assert_eq!(counter, 5 + i);
            let verdict = flow.on_message(CTRL_LOGIN, &[LOGIN_STATE_PIN_INCORRECT], &mut user);
            if i < 5 {
                assert!(verdict.unwrap());
            } else {
                // The fifth "wrong" is the last: no sixth prompt.
                assert!(format!("{:#}", verdict.unwrap_err()).contains(PASSCODE_TOO_MANY));
            }
        }
        assert_eq!(user.asked, vec![false, true, true, true, true]);
    }

    #[test]
    fn an_unanswered_prompt_times_out_and_a_cancel_ends_it() {
        let mut flow = PinFlow::new(Duration::from_millis(30));
        let mut user = Typist::default();
        let mut out = Vec::new();
        let mut counter = 5;
        flow.on_message(CTRL_LOGIN_PIN_REQ, &[], &mut user).unwrap();
        assert!(!flow
            .poll(&mut out, &BRT, &AMB, &mut counter, &mut user)
            .unwrap());
        std::thread::sleep(Duration::from_millis(50));
        let e = flow.poll(&mut out, &BRT, &AMB, &mut counter, &mut user);
        assert!(format!("{:#}", e.unwrap_err()).contains(PASSCODE_TIMEOUT));

        let mut flow = PinFlow::new(PASSCODE_WAIT);
        let mut user = Typist::new(vec![Some(Passcode::Cancel)]);
        flow.on_message(CTRL_LOGIN_PIN_REQ, &[], &mut user).unwrap();
        let e = flow.poll(&mut out, &BRT, &AMB, &mut counter, &mut user);
        assert!(format!("{:#}", e.unwrap_err()).contains(PASSCODE_CANCELLED));
        assert!(out.is_empty(), "nothing was sent");
        assert_eq!(counter, 5);
    }
}
