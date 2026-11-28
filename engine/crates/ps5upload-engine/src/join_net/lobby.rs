//! The Play Room's lobby on the hub (ps5upload-hub PROTOCOL.md, "Rooms"; design: play-rooms 9).
//!
//! A Friends or Public room (and Play again over the internet) keeps one more hub room: a bound
//! room made from the room's own `lobby_secret` (join_room.rs), kept apart from invite rooms so a
//! card never carries an invite. On it the engine publishes the room's card, answers relayed
//! previews, and runs knocks before any SDP: a guest found the room in Rooms (or holds a friend
//! pass), knocks, the host engine checks the room's rules and the password (sealed to the room
//! key: room_seal.rs), asks the host, and only an approved guest may send its offer. The
//! connection then carries today's hello on the data channel; this module remembers what the
//! knock settled (how they came, their tag, password checked) for that hello.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What a knock settled for one guest of the lobby (by hub room and gid).
#[derive(Clone, Debug)]
pub(crate) struct Approved {
    pub via: crate::join_room::Via,
    pub name: String,
    pub device: Option<String>,
    pub password_ok: Option<bool>,
    pub friend_console: Option<String>,
    pub at: Instant,
}

/// An approved gid has this long to connect and say hello (design 9.3 #5).
pub(crate) const APPROVED_FOR: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Book {
    /// The hub's per-room client id of each guest (`{"t":"guest","gid","cid"}`).
    cids: HashMap<(String, String), String>,
    approved: HashMap<(String, String), Approved>,
}

static BOOK: Mutex<Option<Book>> = Mutex::new(None);

fn with_book<R>(f: impl FnOnce(&mut Book) -> R) -> R {
    let mut g = BOOK.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Book::default))
}

/// The hub's client id for `gid` in `room` (lobby rooms only).
pub(crate) fn cid_of(room: &str, gid: &str) -> Option<String> {
    with_book(|b| b.cids.get(&(room.to_string(), gid.to_string())).cloned())
}

pub(crate) fn set_cid(room: &str, gid: &str, cid: &str) {
    if cid.len() > 16 || gid.len() > 64 {
        return;
    }
    with_book(|b| {
        if b.cids.len() > 1024 {
            b.cids.clear();
        }
        b.cids
            .insert((room.to_string(), gid.to_string()), cid.to_string());
    });
}

pub(crate) fn forget_guest(room: &str, gid: &str) {
    with_book(|b| {
        let k = (room.to_string(), gid.to_string());
        b.cids.remove(&k);
        b.approved.remove(&k);
    });
}

pub(crate) fn approve(room: &str, gid: &str, a: Approved) {
    with_book(|b| {
        let now = Instant::now();
        b.approved
            .retain(|_, x| now.saturating_duration_since(x.at) < APPROVED_FOR);
        b.approved.insert((room.to_string(), gid.to_string()), a);
    });
}

/// The approval for `gid` (taken: one hello per approval), while still fresh.
pub(crate) fn take_approved(room: &str, gid: &str) -> Option<Approved> {
    with_book(|b| {
        b.approved
            .remove(&(room.to_string(), gid.to_string()))
            .filter(|a| a.at.elapsed() < APPROVED_FOR)
    })
}

/// `gid` may send SDP/ICE in `room`: approved by a knock (lobby rooms take nothing else).
pub(crate) fn may_offer(room: &str, gid: &str) -> bool {
    with_book(|b| {
        b.approved
            .get(&(room.to_string(), gid.to_string()))
            .is_some_and(|a| a.at.elapsed() < APPROVED_FOR)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_are_taken_once_and_lapse() {
        let a = Approved {
            via: crate::join_room::Via::Rooms,
            name: "Jo".into(),
            device: None,
            password_ok: Some(true),
            friend_console: None,
            at: Instant::now(),
        };
        approve("lobbyA", "g1", a.clone());
        assert!(may_offer("lobbyA", "g1"));
        assert!(!may_offer("lobbyA", "g2"));
        assert!(take_approved("lobbyA", "g1").is_some());
        assert!(take_approved("lobbyA", "g1").is_none());
        approve(
            "lobbyA",
            "g3",
            Approved {
                at: Instant::now() - APPROVED_FOR,
                ..a
            },
        );
        assert!(!may_offer("lobbyA", "g3"));
        set_cid("lobbyA", "g1", "abcdEFGH");
        assert_eq!(cid_of("lobbyA", "g1").as_deref(), Some("abcdEFGH"));
        forget_guest("lobbyA", "g1");
        assert_eq!(cid_of("lobbyA", "g1"), None);
    }
}
