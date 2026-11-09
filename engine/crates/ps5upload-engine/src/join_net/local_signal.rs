//! Signalling without the hub: `GET /api/join/signal` on this engine speaks the hub's guest side
//! (protocol v1) for this engine's own internet rooms, so a guest that can reach this computer
//! (the same Wi-Fi, or Tailscale) swaps the offer, the answer and the candidates with it directly.
//! The app joins this way when the link names this engine's page (`lan`, `ts`): no hub in the way,
//! and a game on the Wi-Fi keeps working when the internet or the hub is down.
//!
//! It is only another way to the same peer connection: the offer goes to [`super::rtc`] as one
//! from the hub would, the answer carries the same DTLS certificate (the page still checks it
//! against the link's `fp`), and the invite key still goes only inside the data channel. Nothing
//! here sees or needs the key.
//!
//! Guest → engine `{"v":1,"t":"join","room"}` → `{"t":"joined","gid"}`, or `{"t":"error","why":
//! "no_room"}` (not a room this engine has open) and the socket closes. Then `{"t":"signal","to":
//! "host","data":…}` both ways (from the engine `{"t":"signal","from":"host","data":…}`), and
//! `{"t":"leave"}`. The socket lives at most [`SIGNAL_FOR`]: once the data channel is open the
//! page leaves it, as it leaves the hub.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use tokio::sync::mpsc;

use super::rtc::Cmd;

/// The largest frame either side sends (the hub's).
pub(crate) const FRAME_MAX: usize = super::hub::FRAME_MAX;
/// The first frame (the join) must come this soon.
const HELLO_WAIT: Duration = Duration::from_secs(10);
/// A signalling socket is closed after this long (ICE has long finished or failed by then).
pub(crate) const SIGNAL_FOR: Duration = Duration::from_secs(if cfg!(test) { 20 } else { 60 });

fn frame(v: serde_json::Value) -> Message {
    Message::Text(v.to_string().into())
}

/// The room a guest's first frame asks for, if it is a well-formed join.
pub(crate) fn join_room(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("t").and_then(|t| t.as_str()) != Some("join") {
        return None;
    }
    let room = v.get("room")?.as_str()?;
    // A room id is the engine's own (random_b64(16): 22 characters).
    (!room.is_empty()
        && room.len() <= 64
        && room
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then(|| room.to_string())
}

/// One guest's signalling socket. `open` says whether a room is one of this engine's open Play
/// Together rooms.
pub(crate) async fn session(mut s: WebSocket, open: impl Fn(&str) -> bool) {
    let first = tokio::time::timeout(HELLO_WAIT, async {
        loop {
            match s.recv().await {
                Some(Ok(Message::Text(t))) => return Some(t.as_str().to_string()),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return None,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .ok()
    .flatten();
    let Some(room) = first.as_deref().and_then(join_room) else {
        return;
    };
    let no_room = || frame(serde_json::json!({"t": "error", "why": "no_room"}));
    if !open(&room) {
        let _ = s.send(no_room()).await;
        return;
    }
    let Some(net) = super::rtc::net().await else {
        let _ = s.send(no_room()).await;
        return;
    };
    // Not the hub's ids: a guest here never meets one from the hub in the driver's peers.
    let Some(gid) = super::random_b64(9).map(|r| format!("lan-{r}")) else {
        return;
    };
    if s.send(frame(serde_json::json!({"t": "joined", "gid": gid})))
        .await
        .is_err()
    {
        return;
    }
    crate::log_at!(
        Debug,
        "together",
        "join: a guest signals on the home network ({gid})"
    );
    let (sig_tx, mut sig_rx) = mpsc::unbounded_channel::<(String, serde_json::Value)>();
    let until = tokio::time::Instant::now() + SIGNAL_FOR;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(until) => break,
            m = sig_rx.recv() => {
                let Some((_, data)) = m else { break };
                if s.send(frame(serde_json::json!({"t": "signal", "from": "host", "data": data}))).await.is_err() {
                    break;
                }
            }
            m = s.recv() => {
                let t = match m {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_str()) else {
                    continue;
                };
                match v.get("t").and_then(|x| x.as_str()).unwrap_or("") {
                    "leave" => break,
                    "signal" => {
                        let data = v.get("data").cloned().unwrap_or_default();
                        let field = |k: &str| data.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                        match field("type").as_str() {
                            "offer" => {
                                let _ = net.cmd.send(Cmd::Offer {
                                    room: room.clone(),
                                    gid: gid.clone(),
                                    sdp: field("sdp"),
                                    signal: sig_tx.clone(),
                                });
                            }
                            "candidate" => {
                                let _ = net.cmd.send(Cmd::Remote {
                                    room: room.clone(),
                                    gid: gid.clone(),
                                    candidate: field("candidate"),
                                });
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    // As the hub's `left`: a peer that did not connect goes; a connected one keeps playing.
    let _ = net.cmd.send(Cmd::Left { room, gid });
    let _ = s.send(Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_well_formed_join_names_a_room() {
        let room = "AbCdEfGhIjKlMnOpQrSt_-";
        assert_eq!(
            join_room(&serde_json::json!({"v": 1, "t": "join", "room": room}).to_string())
                .as_deref(),
            Some(room)
        );
        assert_eq!(
            join_room(r#"{"t":"host","room":"abc","secret":"x"}"#),
            None,
            "never a host"
        );
        assert_eq!(join_room(r#"{"t":"join","room":""}"#), None);
        assert_eq!(join_room(r#"{"t":"join","room":"a/b"}"#), None);
        assert_eq!(join_room(r#"{"t":"join","room":7}"#), None);
        assert_eq!(
            join_room(&format!(r#"{{"t":"join","room":"{}"}}"#, "a".repeat(65))),
            None
        );
        assert_eq!(join_room("not json"), None);
    }
}
