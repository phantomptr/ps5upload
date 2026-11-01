// SPDX-License-Identifier: AGPL-3.0-only
// Ported from chiaki-ng: Copyright (C) the Chiaki and chiaki-ng authors. Rust port for
// ps5upload. Licensed under the GNU AGPL v3.0 (LICENSES/AGPL-3.0.txt).
//! The console's on-screen keyboard, typed into from the page.
//!
//! Ported from chiaki-ng (AGPL-3.0, `lib/src/ctrl.c`: `ctrl_enable_features`,
//! `chiaki_ctrl_keyboard_set_text` / `_accept` / `_reject`, and
//! `ctrl_message_received_keyboard_open` / `_close` / `_text_change`). All on ctrl:
//!
//! * At session start, with the feature asked for: ENABLE (0x0d, a 16-byte "signature"
//!   chiaki itself marks as not understood) then ENABLE_TOGGLE (0x20, `[1]`).
//! * The console says when its keyboard opens (0x21, with the text already in the field),
//!   when the text changed (0x24, the answer to each of ours) and when it closed on its
//!   own (0x22).
//! * We send the whole text (0x23, with a counter that goes up by one each time), and
//!   close it: 0x25 with `[0,0,0,0]` to accept, `[0,0,0,1]` to cancel.
//!
//! The text is raw bytes in both directions: chiaki passes a C string through untouched,
//! so UTF-8 is assumed. Experimental: chiaki-ng's own GUI never switches this on, and it
//! has not been tried on a console here yet.

pub const CTRL_KEYBOARD_ENABLE: u16 = 0x0d;
pub const CTRL_KEYBOARD_ENABLE_TOGGLE: u16 = 0x20;
pub const CTRL_KEYBOARD_OPEN: u16 = 0x21;
pub const CTRL_KEYBOARD_CLOSE_REMOTE: u16 = 0x22;
pub const CTRL_KEYBOARD_TEXT_CHANGE_REQ: u16 = 0x23;
pub const CTRL_KEYBOARD_TEXT_CHANGE_RES: u16 = 0x24;
pub const CTRL_KEYBOARD_CLOSE_REQ: u16 = 0x25;

/// `CtrlKeyboardOpenMessage`: 0x1c unknown bytes, the text length (BE32), the text.
const OPEN_HEADER: usize = 0x20;
/// `CtrlKeyboardTextRequestMessage`: counter, length, 8 + 16 unknown bytes, length again.
const REQUEST_HEADER: usize = 0x24;
/// `CtrlKeyboardTextResponseMessage`: counter, ?, length (at 8), ?, 16 + 4 unknown bytes,
/// a second length.
const RESPONSE_HEADER: usize = 0x28;

/// The longest text we send, in bytes: the console's fields are short, and ctrl drops a
/// message over 8 KiB as out of step.
pub const MAX_TEXT: usize = 2048;

/// What the console's keyboard did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyboardEvent {
    /// It opened, with this text already in the field.
    Open(String),
    /// Its text is now this (the console's answer to our text).
    Text(String),
    /// It closed on its own (the user closed it on the console, or the game did).
    Closed,
}

/// What the page asks of the console's keyboard, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyboardInput {
    /// The whole text of the field.
    Text(String),
    /// Done: keep the text (Enter).
    Accept,
    /// Cancel (Esc).
    Reject,
}

/// The two messages that switch the feature on (chiaki `ctrl_enable_features`).
pub fn enable_messages() -> Vec<(u16, Vec<u8>)> {
    vec![
        (
            CTRL_KEYBOARD_ENABLE,
            vec![
                0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x05, 0xAE, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00,
            ],
        ),
        (CTRL_KEYBOARD_ENABLE_TOGGLE, vec![1]),
    ]
}

/// Cut `text` to at most MAX_TEXT bytes, on a character boundary.
fn capped(text: &str) -> &str {
    if text.len() <= MAX_TEXT {
        return text;
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The TEXT_CHANGE_REQ payload for `text` (chiaki `chiaki_ctrl_keyboard_set_text`);
/// `counter` is this message's number, 1 for the first of the session.
pub fn set_text_payload(counter: u32, text: &str) -> Vec<u8> {
    let t = capped(text).as_bytes();
    let len = (t.len() as u32).to_be_bytes();
    let mut p = vec![0u8; REQUEST_HEADER + t.len()];
    p[0..4].copy_from_slice(&counter.to_be_bytes());
    p[4..8].copy_from_slice(&len);
    p[0x20..0x24].copy_from_slice(&len);
    p[REQUEST_HEADER..].copy_from_slice(t);
    p
}

/// The CLOSE_REQ payload: accept keeps the text, otherwise the console drops it.
pub fn close_payload(accept: bool) -> [u8; 4] {
    [0, 0, 0, if accept { 0 } else { 1 }]
}

/// The text after a header whose length field is at `len_at`, as chiaki reads it. Too short
/// or a length past the end: None (chiaki asserts the length fits exactly).
fn text_after(p: &[u8], header: usize, len_at: usize) -> Option<String> {
    if p.len() < header {
        return None;
    }
    let len = u32::from_be_bytes(p[len_at..len_at + 4].try_into().ok()?) as usize;
    let t = p.get(header..header.checked_add(len)?)?;
    Some(String::from_utf8_lossy(t).into_owned())
}

/// A decrypted ctrl message from the console, if it is about the keyboard.
pub fn parse(kind: u16, payload: &[u8]) -> Option<KeyboardEvent> {
    match kind {
        CTRL_KEYBOARD_OPEN => text_after(payload, OPEN_HEADER, 0x1c).map(KeyboardEvent::Open),
        CTRL_KEYBOARD_TEXT_CHANGE_RES => {
            text_after(payload, RESPONSE_HEADER, 8).map(KeyboardEvent::Text)
        }
        CTRL_KEYBOARD_CLOSE_REMOTE => Some(KeyboardEvent::Closed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_matches_chiaki() {
        let m = enable_messages();
        assert_eq!(m[0].0, 0x0d);
        assert_eq!(m[0].1.len(), 0x10);
        assert_eq!(&m[0].1[..8], &[0, 0, 0, 1, 0, 0, 0x05, 0xAE]);
        assert_eq!(m[1], (0x20, vec![1]));
    }

    #[test]
    fn set_text_layout_matches_chiaki() {
        let p = set_text_payload(3, "hi");
        assert_eq!(p.len(), 0x24 + 2);
        assert_eq!(&p[0..4], &[0, 0, 0, 3]);
        assert_eq!(&p[4..8], &[0, 0, 0, 2]);
        assert!(p[8..0x20].iter().all(|&b| b == 0));
        assert_eq!(&p[0x20..0x24], &[0, 0, 0, 2]);
        assert_eq!(&p[0x24..], b"hi");
    }

    #[test]
    fn non_ascii_text_is_utf8_bytes() {
        let s = "café 日本 🎮";
        let p = set_text_payload(1, s);
        let n = s.len() as u32;
        assert_eq!(&p[4..8], &n.to_be_bytes());
        assert_eq!(&p[0x24..], s.as_bytes());
        // The console's answer, the same text back.
        let mut r = vec![0u8; 0x28];
        r[8..12].copy_from_slice(&n.to_be_bytes());
        r.extend_from_slice(s.as_bytes());
        assert_eq!(
            parse(CTRL_KEYBOARD_TEXT_CHANGE_RES, &r),
            Some(KeyboardEvent::Text(s.into()))
        );
    }

    #[test]
    fn long_text_is_cut_on_a_character() {
        let s = "é".repeat(MAX_TEXT); // 2 bytes each
        let p = set_text_payload(1, &s);
        assert_eq!(p.len() - 0x24, MAX_TEXT);
        assert!(std::str::from_utf8(&p[0x24..]).is_ok());
        let s = format!("a{}", "é".repeat(MAX_TEXT));
        let p = set_text_payload(1, &s);
        assert_eq!(p.len() - 0x24, MAX_TEXT - 1);
    }

    #[test]
    fn close_accept_and_reject() {
        assert_eq!(close_payload(true), [0, 0, 0, 0]);
        assert_eq!(close_payload(false), [0, 0, 0, 1]);
    }

    #[test]
    fn open_with_and_without_text() {
        let mut p = vec![0xAAu8; 0x1c];
        p.extend_from_slice(&5u32.to_be_bytes());
        p.extend_from_slice(b"hello");
        assert_eq!(
            parse(CTRL_KEYBOARD_OPEN, &p),
            Some(KeyboardEvent::Open("hello".into()))
        );
        let mut e = vec![0u8; 0x1c];
        e.extend_from_slice(&0u32.to_be_bytes());
        assert_eq!(
            parse(CTRL_KEYBOARD_OPEN, &e),
            Some(KeyboardEvent::Open(String::new()))
        );
    }

    #[test]
    fn short_or_overlong_messages_are_ignored() {
        assert_eq!(parse(CTRL_KEYBOARD_OPEN, &[0; 0x1f]), None);
        assert_eq!(parse(CTRL_KEYBOARD_TEXT_CHANGE_RES, &[0; 0x27]), None);
        let mut p = vec![0u8; 0x1c];
        p.extend_from_slice(&100u32.to_be_bytes());
        p.extend_from_slice(b"short");
        assert_eq!(parse(CTRL_KEYBOARD_OPEN, &p), None);
        let mut p = vec![0u8; 0x1c];
        p.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(parse(CTRL_KEYBOARD_OPEN, &p), None);
    }

    #[test]
    fn remote_close_and_other_kinds() {
        assert_eq!(
            parse(CTRL_KEYBOARD_CLOSE_REMOTE, &[]),
            Some(KeyboardEvent::Closed)
        );
        assert_eq!(parse(0xfe, &[]), None);
        assert_eq!(parse(CTRL_KEYBOARD_TEXT_CHANGE_REQ, &[0; 0x30]), None);
    }
}
