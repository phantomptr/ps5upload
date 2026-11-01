//! Wake & sign in, when the PS5 user has a login passcode (#608).
//!
//! `POST /api/ps5/power/wake-login` with `"ask_passcode": true` wakes the console and opens the
//! sign-in session as before. When the console asks for the user's passcode, the engine keeps
//! that session open (heartbeats answered, rp_login_pin) and the call answers
//! `{"state": "passcode", "id": "<token>", "retry": bool, "wait_s": 180}`.
//! `POST /api/ps5/power/wake-login/passcode` with `{"id", "passcode": "1234"}` or
//! `{"id", "cancel": true}` continues it; its answer is the final result or another
//! `"state": "passcode"` (a wrong one: `retry` true). Only the holder of the token can answer.
//! The session ends by itself 180 s after an unanswered ask, or after five wrong passcodes.
//!
//! Without `ask_passcode` (an automatic wake, e.g. for an upload), a passcode request ends the
//! sign-in at once: `{"ok": true, "signed_in": false, "passcode_needed": true}`; the console is
//! awake at its sign-in screen.
//!
//! The passcode is passed to the console and never logged or kept.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Mutex, OnceLock};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::rp_login_pin::{
    valid_passcode, Passcode, PasscodePrompt, PASSCODE_CANCELLED, PASSCODE_CONSOLE_GAVE_UP,
    PASSCODE_TIMEOUT, PASSCODE_TOO_MANY, PASSCODE_WAIT,
};
use ps5upload_core::rp_session::SignIn;
use tokio::sync::mpsc;

/// What the sign-in thread tells the waiting request.
enum Event {
    Passcode { retry: bool },
    Done(anyhow::Result<SignIn>),
}

/// A sign-in held open while the user types the passcode.
struct Held {
    answers: std_mpsc::Sender<Passcode>,
    events: tokio::sync::Mutex<mpsc::UnboundedReceiver<Event>>,
    /// The console is waiting for an answer (an answer at any other time is refused).
    asking: AtomicBool,
}

fn held() -> &'static Mutex<HashMap<String, Arc<Held>>> {
    static HELD: OnceLock<Mutex<HashMap<String, Arc<Held>>>> = OnceLock::new();
    HELD.get_or_init(Default::default)
}

fn forget(id: &str) {
    if let Ok(mut m) = held().lock() {
        m.remove(id);
    }
}

/// The sign-in thread's side: asks go out as events, answers come back over a channel.
struct ChannelPrompt {
    events: mpsc::UnboundedSender<Event>,
    answers: std_mpsc::Receiver<Passcode>,
    gone: bool,
}

impl PasscodePrompt for ChannelPrompt {
    fn passcode_needed(&mut self, retry: bool) {
        crate::log_info!("wake_login: the console asks for the user's passcode (retry {retry})");
        if self.events.send(Event::Passcode { retry }).is_err() {
            self.gone = true;
        }
    }
    fn passcode(&mut self) -> Option<Passcode> {
        match self.answers.try_recv() {
            Ok(a) => Some(a),
            Err(std_mpsc::TryRecvError::Empty) => None,
            Err(std_mpsc::TryRecvError::Disconnected) => {
                self.gone = true;
                None
            }
        }
    }
    fn stopped(&mut self) -> bool {
        self.gone
    }
}

/// The sign-in: wake + session, given who (if anyone) types the passcode.
pub(crate) type SignInJob =
    Box<dyn FnOnce(Option<&mut dyn PasscodePrompt>) -> anyhow::Result<SignIn> + Send>;

/// Run a Wake & sign in; with `ask`, a passcode request is handed to the caller (see the
/// module doc) instead of ending the sign-in.
pub(crate) async fn start(job: SignInJob, ask: bool) -> Response {
    if !ask {
        let r = tokio::task::spawn_blocking(move || job(None))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
        return finished(r);
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let (etx, erx) = mpsc::unbounded_channel();
    let (atx, arx) = std_mpsc::channel();
    let h = Arc::new(Held {
        answers: atx,
        events: tokio::sync::Mutex::new(erx),
        asking: AtomicBool::new(false),
    });
    if let Ok(mut m) = held().lock() {
        m.insert(id.clone(), h.clone());
    }
    let thread_id = id.clone();
    tokio::task::spawn_blocking(move || {
        let mut prompt = ChannelPrompt {
            events: etx.clone(),
            answers: arx,
            gone: false,
        };
        let r = job(Some(&mut prompt));
        // Gone from the table first: a late answer is "expired", not "not asking".
        forget(&thread_id);
        let _ = etx.send(Event::Done(r));
    });
    next(&h, &id).await
}

/// Wait for the sign-in's next step and answer with it.
async fn next(h: &Held, id: &str) -> Response {
    let ev = h.events.lock().await.recv().await;
    match ev {
        Some(Event::Passcode { retry }) => {
            h.asking.store(true, Ordering::SeqCst);
            Json(serde_json::json!({
                "state": "passcode",
                "id": id,
                "retry": retry,
                "wait_s": PASSCODE_WAIT.as_secs(),
            }))
            .into_response()
        }
        Some(Event::Done(r)) => {
            forget(id);
            finished(r)
        }
        None => {
            forget(id);
            finished(Err(anyhow::anyhow!("the sign-in ended unexpectedly")))
        }
    }
}

/// The stable code the client turns into words, for the passcode endings.
pub(crate) fn passcode_error_code(text: &str) -> Option<&'static str> {
    if text.contains(PASSCODE_CANCELLED) {
        Some("passcode_cancelled")
    } else if text.contains(PASSCODE_TOO_MANY) {
        Some("passcode_too_many")
    } else if text.contains(PASSCODE_TIMEOUT) || text.contains(PASSCODE_CONSOLE_GAVE_UP) {
        Some("passcode_timeout")
    } else {
        None
    }
}

fn finished(r: anyhow::Result<SignIn>) -> Response {
    match r {
        Ok(SignIn::SignedIn) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "signed_in": true })),
        )
            .into_response(),
        Ok(SignIn::PasscodeNeeded) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "signed_in": false, "passcode_needed": true })),
        )
            .into_response(),
        Err(e) => {
            let text = format!("{e:#}");
            let mut body = serde_json::json!({ "error": text });
            if let Some(code) = passcode_error_code(&text) {
                body["code"] = code.into();
            }
            (StatusCode::BAD_GATEWAY, Json(body)).into_response()
        }
    }
}

/// A passcode as the client sent it: never printed, not even in a debug dump.
#[derive(serde::Deserialize)]
#[serde(transparent)]
pub(crate) struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct PasscodeReq {
    id: String,
    #[serde(default)]
    passcode: Option<Secret>,
    #[serde(default)]
    cancel: bool,
}

/// POST /api/ps5/power/wake-login/passcode — the user's answer for a held sign-in.
pub(crate) async fn answer(Json(req): Json<PasscodeReq>) -> Response {
    let h = held().lock().ok().and_then(|m| m.get(&req.id).cloned());
    let Some(h) = h else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "this sign-in has ended; press Wake & sign in again",
                "code": "passcode_expired",
            })),
        )
            .into_response();
    };
    let answer = if req.cancel {
        Passcode::Cancel
    } else {
        match req.passcode {
            Some(Secret(p)) if valid_passcode(&p) => Passcode::Digits(p),
            _ => {
                return crate::json_err(StatusCode::BAD_REQUEST, "a passcode is four digits")
                    .into_response()
            }
        }
    };
    if !h.asking.swap(false, Ordering::SeqCst) {
        return crate::json_err(
            StatusCode::CONFLICT,
            "the console is not waiting for a passcode right now",
        )
        .into_response();
    }
    if h.answers.send(answer).is_err() {
        forget(&req.id);
        return finished(Err(anyhow::anyhow!(PASSCODE_CONSOLE_GAVE_UP)));
    }
    next(&h, &req.id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    async fn body(r: Response) -> (StatusCode, serde_json::Value) {
        let status = r.status();
        let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&b).unwrap())
    }

    async fn send(id: &str, v: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let mut v = v;
        v["id"] = id.into();
        let req: PasscodeReq = serde_json::from_value(v).unwrap();
        body(answer(Json(req)).await).await
    }

    /// A console that wants `good`, asking up to `asks` times, like rp_session does.
    fn console(good: &'static str, asks: usize) -> SignInJob {
        Box::new(move |p: Option<&mut dyn PasscodePrompt>| {
            let Some(p) = p else {
                return Ok(SignIn::PasscodeNeeded);
            };
            for i in 0..asks {
                p.passcode_needed(i > 0);
                let t0 = Instant::now();
                let got = loop {
                    if p.stopped() {
                        anyhow::bail!(PASSCODE_CANCELLED);
                    }
                    if let Some(a) = p.passcode() {
                        break a;
                    }
                    if t0.elapsed() > Duration::from_millis(500) {
                        anyhow::bail!(PASSCODE_TIMEOUT);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                };
                match got {
                    Passcode::Cancel => anyhow::bail!(PASSCODE_CANCELLED),
                    Passcode::Digits(d) if d == good => return Ok(SignIn::SignedIn),
                    Passcode::Digits(_) => {}
                }
            }
            anyhow::bail!(PASSCODE_TOO_MANY)
        })
    }

    #[tokio::test]
    async fn a_passcode_round_trip_signs_in_and_a_wrong_one_asks_again() {
        let (st, v) = body(start(console("1234", 5), true).await).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], "passcode");
        assert_eq!(v["retry"], false);
        assert_eq!(v["wait_s"], 180);
        let id = v["id"].as_str().unwrap().to_string();
        // Not four digits: refused, and the console still waits.
        let (st, _) = send(&id, serde_json::json!({ "passcode": "12" })).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        // Another token cannot answer.
        let (st, v) = send("nope", serde_json::json!({ "passcode": "1234" })).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert_eq!(v["code"], "passcode_expired");
        let (st, v) = send(&id, serde_json::json!({ "passcode": "0000" })).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["state"], "passcode");
        assert_eq!(v["retry"], true);
        assert_eq!(v["id"], id);
        let (st, v) = send(&id, serde_json::json!({ "passcode": "1234" })).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["signed_in"], true);
        // Done: the token is gone.
        let (st, _) = send(&id, serde_json::json!({ "passcode": "1234" })).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cancel_ends_the_sign_in() {
        let (_, v) = body(start(console("1234", 5), true).await).await;
        let id = v["id"].as_str().unwrap().to_string();
        let (st, v) = send(&id, serde_json::json!({ "cancel": true })).await;
        assert_eq!(st, StatusCode::BAD_GATEWAY);
        assert_eq!(v["code"], "passcode_cancelled");
    }

    #[tokio::test]
    async fn an_unanswered_prompt_expires() {
        let (_, v) = body(start(console("1234", 5), true).await).await;
        let id = v["id"].as_str().unwrap().to_string();
        tokio::time::sleep(Duration::from_millis(900)).await;
        let (st, v) = send(&id, serde_json::json!({ "passcode": "1234" })).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert_eq!(v["code"], "passcode_expired");
    }

    #[tokio::test]
    async fn five_wrong_passcodes_say_so() {
        let (_, mut v) = body(start(console("1234", 5), true).await).await;
        let id = v["id"].as_str().unwrap().to_string();
        for _ in 0..5 {
            assert_eq!(v["state"], "passcode");
            v = send(&id, serde_json::json!({ "passcode": "0000" })).await.1;
        }
        assert_eq!(v["code"], "passcode_too_many");
    }

    #[tokio::test]
    async fn an_automatic_wake_does_not_ask() {
        let (st, v) = body(start(console("1234", 5), false).await).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["signed_in"], false);
        assert_eq!(v["passcode_needed"], true);
        assert!(v.get("id").is_none());
    }

    #[test]
    fn a_passcode_is_never_printed() {
        let req: PasscodeReq =
            serde_json::from_value(serde_json::json!({ "id": "x", "passcode": "1234" })).unwrap();
        assert!(!format!("{req:?}").contains("1234"));
    }
}
