//! Binary messages on the "join" data channel. The guest page implements the same framing.
//!
//! Text messages go as data-channel string messages, whole and unchanged (they are JSON, well
//! under 16 KiB). Every binary data-channel message, both ways, starts with one header byte:
//!
//! | byte | meaning |
//! |------|---------|
//! | `0x00` | the last (or only) piece of a message: append the rest, the message is complete |
//! | `0x01` | a piece with more to follow: append the rest, wait for the next binary message |
//! | `0x02` | ping (engine → page): 8 bytes follow; answer `0x03` with the same 8 bytes at once |
//! | `0x03` | pong (page → engine) |
//!
//! A data-channel message is at most [`DC_MAX`] bytes (header included), the size every browser
//! takes without fragmenting. The channel is reliable and ordered, so pieces arrive in order and
//! never interleave with another binary message's. What the pieces make up is exactly the
//! WebSocket message `/api/join/ws` would carry (the picture and sound's tagged binary, the guest's
//! pad frames). The ping is the WebSocket ping's stand-in: its answer gives the guest's round trip.

/// The largest data-channel message either side sends.
pub(crate) const DC_MAX: usize = 16 * 1024;
/// The last piece of a message.
pub(crate) const LAST: u8 = 0x00;
/// A piece with more to follow.
pub(crate) const MORE: u8 = 0x01;
pub(crate) const PING: u8 = 0x02;
pub(crate) const PONG: u8 = 0x03;

/// `msg` as data-channel messages (at least one: an empty message is one `[0x00]`).
pub(crate) fn split(msg: &[u8]) -> Vec<Vec<u8>> {
    let room = DC_MAX - 1;
    let n = msg.len().div_ceil(room).max(1);
    (0..n)
        .map(|i| {
            let part = &msg[(i * room).min(msg.len())..((i + 1) * room).min(msg.len())];
            let mut v = Vec::with_capacity(part.len() + 1);
            v.push(if i + 1 == n { LAST } else { MORE });
            v.extend_from_slice(part);
            v
        })
        .collect()
}

/// What one binary data-channel message added up to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    /// Not complete yet.
    Partial,
    /// A whole message.
    Message(Vec<u8>),
    Ping([u8; 8]),
    Pong([u8; 8]),
}

/// Puts pieces back together, refusing a message over `max` bytes.
#[derive(Debug)]
pub(crate) struct Reassembler {
    buf: Vec<u8>,
    max: usize,
}

impl Reassembler {
    pub(crate) fn new(max: usize) -> Self {
        Reassembler {
            buf: Vec::new(),
            max,
        }
    }

    /// Err: not this framing, or a message over the limit (the peer is broken or hostile).
    pub(crate) fn push(&mut self, dc: &[u8]) -> Result<Piece, &'static str> {
        let (&head, rest) = dc.split_first().ok_or("empty")?;
        match head {
            LAST | MORE => {
                if self.buf.len() + rest.len() > self.max {
                    self.buf = Vec::new();
                    return Err("too_big");
                }
                self.buf.extend_from_slice(rest);
                if head == MORE {
                    return Ok(Piece::Partial);
                }
                Ok(Piece::Message(std::mem::take(&mut self.buf)))
            }
            PING | PONG => {
                let b: [u8; 8] = rest.try_into().map_err(|_| "ping_size")?;
                Ok(if head == PING {
                    Piece::Ping(b)
                } else {
                    Piece::Pong(b)
                })
            }
            _ => Err("header"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_of_every_size_come_back_whole() {
        for len in [
            0,
            1,
            DC_MAX - 2,
            DC_MAX - 1,
            DC_MAX,
            3 * (DC_MAX - 1),
            3 * (DC_MAX - 1) + 1,
            200_000,
        ] {
            let msg: Vec<u8> = (0..len).map(|i| (i * 7 + len) as u8).collect();
            let parts = split(&msg);
            assert_eq!(parts.len(), len.div_ceil(DC_MAX - 1).max(1), "{len}");
            assert!(parts.iter().all(|p| p.len() <= DC_MAX));
            assert!(parts[..parts.len() - 1].iter().all(|p| p[0] == MORE));
            assert_eq!(parts.last().unwrap()[0], LAST);
            let mut r = Reassembler::new(1 << 20);
            let mut got = None;
            for (i, p) in parts.iter().enumerate() {
                match r.push(p).unwrap() {
                    Piece::Partial => assert!(i + 1 < parts.len()),
                    Piece::Message(m) => got = Some(m),
                    other => panic!("{other:?}"),
                }
            }
            assert_eq!(got.unwrap(), msg, "{len}");
        }
    }

    #[test]
    fn a_message_over_the_limit_or_a_bad_header_is_refused() {
        let mut r = Reassembler::new(100);
        let parts = split(&[1u8; 101]);
        assert_eq!(r.push(&parts[0]), Err("too_big"));
        // after the refusal it starts over clean
        assert_eq!(r.push(&[LAST, 9]), Ok(Piece::Message(vec![9])));
        assert_eq!(r.push(&[0x7f, 1]), Err("header"));
        assert_eq!(r.push(&[]), Err("empty"));
        let mut long = vec![MORE];
        long.extend_from_slice(&[0u8; 60]);
        assert_eq!(r.push(&long), Ok(Piece::Partial));
        assert_eq!(r.push(&long), Err("too_big"));
    }

    #[test]
    fn pings_and_pongs_carry_eight_bytes() {
        let mut r = Reassembler::new(10);
        let mut p = vec![PING];
        p.extend_from_slice(&42u64.to_le_bytes());
        assert_eq!(r.push(&p), Ok(Piece::Ping(42u64.to_le_bytes())));
        p[0] = PONG;
        assert_eq!(r.push(&p), Ok(Piece::Pong(42u64.to_le_bytes())));
        assert_eq!(r.push(&[PONG, 1, 2]), Err("ping_size"));
    }
}
