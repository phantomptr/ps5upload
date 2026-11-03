//! Play Together's guest controllers: the helper's `vpad.*` methods (188-194,
//! `payload/include/vpad.h`, `protocol/ps5upload-ava1/MGMT_METHODS.md`).
//!
//! Each of slots 2-4 is one virtual DualSense on the console (Player 2-4), owned by a token: the
//! same token opening a live slot again is a no-op (a guest coming back), another token is refused
//! (`vpad_busy`). The helper keeps the pad for the engine: no `vpad.state` or live `vpad.hb` for
//! [`NEUTRAL_AFTER`] and it sends a neutral state (nothing held, sticks centred); no call at all
//! for [`DELETE_AFTER`] and the pad is removed. The engine (`ps5upload-engine/src/join.rs`) sends a
//! heartbeat every [`HEARTBEAT`] per open slot.
//!
//! An older helper has none of these methods: [`status`] answers `None` and the host is told to
//! update the helper.

use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m, Method, MgmtError};

/// The first and last player a guest can be (Player 1 is the host's Remote Play pad).
pub const FIRST_SLOT: u8 = 2;
pub const LAST_SLOT: u8 = 4;
/// How often the engine keeps an open slot (`vpad.hb` when no state went out since).
pub const HEARTBEAT: Duration = Duration::from_millis(200);
/// The helper sends a neutral state after this long without a state or heartbeat.
pub const NEUTRAL_AFTER: Duration = Duration::from_millis(400);
/// The helper removes the pad after this long without a heartbeat.
pub const DELETE_AFTER: Duration = Duration::from_secs(90);
/// The deadline of one call: input that cannot get through in this long is not worth waiting for.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(2);
/// Touch points a state carries at most (the DualSense touchpad's two fingers).
pub const MAX_TOUCH: usize = 2;
/// The stick value at rest.
pub const STICK_CENTER: u8 = 128;

/// The helper's `buttons`: libScePad's bits (`payload/include/vpad.h` `VPAD_BTN_*`).
pub mod button {
    pub const CREATE: u32 = 0x1;
    pub const L3: u32 = 0x2;
    pub const R3: u32 = 0x4;
    pub const OPTIONS: u32 = 0x8;
    pub const UP: u32 = 0x10;
    pub const RIGHT: u32 = 0x20;
    pub const DOWN: u32 = 0x40;
    pub const LEFT: u32 = 0x80;
    pub const L2: u32 = 0x100;
    pub const R2: u32 = 0x200;
    pub const L1: u32 = 0x400;
    pub const R1: u32 = 0x800;
    pub const TRIANGLE: u32 = 0x1000;
    pub const CIRCLE: u32 = 0x2000;
    pub const CROSS: u32 = 0x4000;
    pub const SQUARE: u32 = 0x8000;
    pub const PS: u32 = 0x10000;
    pub const TOUCHPAD: u32 = 0x100000;
    /// What "block system buttons" takes from a guest who is playing.
    pub const SYSTEM: u32 = PS | CREATE;
}

/// The apps' pad bits (`client/src/lib/remotePlayStream.ts` `PadButton`, also Remote Play's) in
/// the order of [`APP_TO_PAD`].
pub mod app_button {
    pub const CROSS: u32 = 1;
    pub const CIRCLE: u32 = 2;
    pub const SQUARE: u32 = 4;
    pub const TRIANGLE: u32 = 8;
    pub const LEFT: u32 = 16;
    pub const RIGHT: u32 = 32;
    pub const UP: u32 = 64;
    pub const DOWN: u32 = 128;
    pub const L1: u32 = 256;
    pub const R1: u32 = 512;
    pub const L3: u32 = 1024;
    pub const R3: u32 = 2048;
    pub const OPTIONS: u32 = 4096;
    pub const SHARE: u32 = 8192;
    pub const TOUCHPAD: u32 = 16384;
    pub const PS: u32 = 32768;
}

/// Each app bit and the helper bit it is.
pub const APP_TO_PAD: [(u32, u32); 16] = [
    (app_button::CROSS, button::CROSS),
    (app_button::CIRCLE, button::CIRCLE),
    (app_button::SQUARE, button::SQUARE),
    (app_button::TRIANGLE, button::TRIANGLE),
    (app_button::LEFT, button::LEFT),
    (app_button::RIGHT, button::RIGHT),
    (app_button::UP, button::UP),
    (app_button::DOWN, button::DOWN),
    (app_button::L1, button::L1),
    (app_button::R1, button::R1),
    (app_button::L3, button::L3),
    (app_button::R3, button::R3),
    (app_button::OPTIONS, button::OPTIONS),
    (app_button::SHARE, button::CREATE),
    (app_button::TOUCHPAD, button::TOUCHPAD),
    (app_button::PS, button::PS),
];

/// An app's buttons and triggers as the helper's buttons: each bit mapped, and L2 / R2 down while
/// their trigger is pressed at all (a DualSense sets those bits too; some games read only them).
/// Bits the apps do not define are dropped.
pub fn from_app(app: u32, l2: u8, r2: u8) -> u32 {
    let mut b = APP_TO_PAD
        .iter()
        .filter(|(a, _)| app & a != 0)
        .fold(0, |acc, (_, p)| acc | p);
    if l2 > 0 {
        b |= button::L2;
    }
    if r2 > 0 {
        b |= button::R2;
    }
    b
}

/// One finger on the touchpad (x 0-1919, y 0-1079 as the helper scales it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Touch {
    pub id: u8,
    pub x: u16,
    pub y: u16,
}

/// The motion sensors: gyro (rad/s) and accelerometer (g), x y z.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Motion {
    pub gyro: [f32; 3],
    pub accel: [f32; 3],
}

/// A guest controller's whole state at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct PadState {
    pub buttons: u32,
    pub lx: u8,
    pub ly: u8,
    pub rx: u8,
    pub ry: u8,
    pub l2: u8,
    pub r2: u8,
    pub touch: Vec<Touch>,
    pub motion: Option<Motion>,
}

impl Default for PadState {
    fn default() -> Self {
        PadState::NEUTRAL
    }
}

impl PadState {
    /// Nothing held, sticks centred, no finger, no motion.
    pub const NEUTRAL: PadState = PadState {
        buttons: 0,
        lx: STICK_CENTER,
        ly: STICK_CENTER,
        rx: STICK_CENTER,
        ry: STICK_CENTER,
        l2: 0,
        r2: 0,
        touch: Vec::new(),
        motion: None,
    };

    /// The `vpad.state` body for `slot` owned by `token`.
    pub fn body(&self, slot: u8, token: &str, seq: u32) -> serde_json::Value {
        let mut v = serde_json::json!({
            "slot": slot,
            "token": token,
            "seq": seq,
            "buttons": self.buttons,
            "lx": self.lx,
            "ly": self.ly,
            "rx": self.rx,
            "ry": self.ry,
            "l2": self.l2,
            "r2": self.r2,
        });
        if !self.touch.is_empty() {
            v["touch"] = serde_json::to_value(&self.touch).unwrap_or_default();
        }
        if let Some(m) = &self.motion {
            v["gyro"] = serde_json::json!(m.gyro);
            v["accel"] = serde_json::json!(m.accel);
        }
        v
    }
}

/// One slot's state as `vpad.states` carries it.
#[derive(Clone, Debug, PartialEq)]
pub struct StateItem {
    pub slot: u8,
    pub token: String,
    pub seq: u32,
    pub state: PadState,
}

impl StateItem {
    pub fn body(&self) -> serde_json::Value {
        self.state.body(self.slot, &self.token, self.seq)
    }
}

/// The answer to `vpad.open`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opened {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub slot: u8,
    /// The pad's handle: a different one after a reopen means a new pad (the console asks
    /// "Who's using this controller?" again).
    #[serde(default)]
    pub handle: i64,
    /// The console user the pad started with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<u32>,
    /// The same token reopened a live pad (no new pad; its `seq` starts again at 0). False: a new
    /// pad (the console asks "Who's using this controller?").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
}

/// One slot `vpad.states` could not apply.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotFailure {
    #[serde(default)]
    pub slot: u8,
    #[serde(default)]
    pub err: String,
}

/// The answer to `vpad.state`, `vpad.states`, `vpad.hb` and `vpad.close`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    #[serde(default)]
    pub ok: bool,
    /// States applied (`vpad.state` says true or false, read as 1 or 0; a `seq` not above the
    /// last is not applied).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "count_or_bool"
    )]
    pub applied: Option<u32>,
    /// `vpad.states`: the slots that failed (`vpad_not_open`, `vpad_not_owner`, `vpad_failed`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<SlotFailure>,
    /// `vpad.hb`: the pad is neutral (its watchdog let go).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neutral: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
}

/// `vpad.states` counts what it applied, `vpad.state` says true or false.
fn count_or_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum N {
        Count(u32),
        Flag(bool),
    }
    Ok(Option::<N>::deserialize(d)?.map(|n| match n {
        N::Count(c) => c,
        N::Flag(b) => u32::from(b),
    }))
}

/// The answer to `vpad.output`: what the game asks of Player N's controller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    /// Rumble, left (strong) and right (weak), 0-255.
    #[serde(default)]
    pub motors: [u8; 2],
    /// The light bar.
    #[serde(default)]
    pub color: [u8; 3],
}

/// The refusal `err` for a slot another token owns.
pub const ERR_BUSY: &str = "vpad_busy";

/// A refusal that is an answer all the same (the call's error cause is a JSON body or a bare
/// error token such as `vpad_busy`).
trait Refusal: Sized {
    fn refused(err: String) -> Self;
}
impl Refusal for Opened {
    fn refused(err: String) -> Self {
        Opened {
            err: Some(err),
            ..Default::default()
        }
    }
}
impl Refusal for Ack {
    fn refused(err: String) -> Self {
        Ack {
            ok: false,
            err: Some(err),
            ..Default::default()
        }
    }
}

/// True when `e` is the helper saying it has no such method (an older helper).
pub fn is_unsupported(e: &anyhow::Error) -> bool {
    crate::vinput::is_unsupported(e)
}

/// The methods that drive a pad: never through the generic passthrough (mgmt_proxy.rs).
pub fn is_input_method(id: u16) -> bool {
    [
        m::VPAD_OPEN,
        m::VPAD_STATE,
        m::VPAD_STATES,
        m::VPAD_HB,
        m::VPAD_CLOSE,
    ]
    .iter()
    .any(|x| x.id == id)
}

fn answer<T: serde::de::DeserializeOwned + Refusal>(r: Result<Vec<u8>>) -> Result<T> {
    match r {
        Ok(b) => Ok(serde_json::from_slice(&b)?),
        Err(e) => {
            if let Some(me) = e.downcast_ref::<MgmtError>() {
                if me.status != crate::console_files::ERR_UNKNOWN_METHOD && me.status != 0 {
                    let cause = me.cause.trim();
                    if cause.starts_with('{') {
                        if let Ok(v) = serde_json::from_str::<T>(cause) {
                            return Ok(v);
                        }
                    } else if !cause.is_empty()
                        && cause.len() <= 64
                        && cause
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    {
                        return Ok(T::refused(cause.to_string()));
                    }
                }
            }
            Err(e)
        }
    }
}

fn supported<T>(r: Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if is_unsupported(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

fn call(addr: &str, method: Method, body: &serde_json::Value) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(body)?;
    mgmt::call_with(addr, method, method.label, &body, Some(CALL_TIMEOUT))
}

/// Opens (or keeps) Player `slot`'s pad for `token`; `user` the console user it starts with (the
/// helper picks the foreground user when None). `None` from a helper without the method.
pub fn open(addr: &str, slot: u8, token: &str, user: Option<u32>) -> Result<Option<Opened>> {
    let mut body = serde_json::json!({"slot": slot, "token": token});
    if let Some(u) = user {
        body["user"] = u.into();
    }
    supported(answer(call(addr, m::VPAD_OPEN, &body)))
}

/// One slot's state.
pub fn state(addr: &str, item: &StateItem) -> Result<Option<Ack>> {
    supported(answer(call(addr, m::VPAD_STATE, &item.body())))
}

/// Several slots' states in one call (one is sent as `vpad.state`).
pub fn states(addr: &str, items: &[StateItem]) -> Result<Option<Ack>> {
    match items {
        [] => Ok(Some(Ack {
            ok: true,
            ..Default::default()
        })),
        [one] => state(addr, one),
        _ => {
            let body =
                serde_json::json!({"items": items.iter().map(StateItem::body).collect::<Vec<_>>()});
            supported(answer(call(addr, m::VPAD_STATES, &body)))
        }
    }
}

/// Keeps `slot` (and its signed-in guest) without changing its state. `live`: the guest is
/// connected and sending, so the helper's 400 ms neutral watchdog is reset too (a held stick
/// stays held); plain (`live` false, a guest reconnecting) keeps the pad alive but lets it go
/// neutral.
pub fn hb(addr: &str, slot: u8, token: &str, live: bool) -> Result<Option<Ack>> {
    let mut body = serde_json::json!({"slot": slot, "token": token});
    if live {
        body["live"] = true.into();
    }
    supported(answer(call(addr, m::VPAD_HB, &body)))
}

/// Removes `slot`'s pad.
pub fn close(addr: &str, slot: u8, token: &str) -> Result<Option<Ack>> {
    supported(answer(call(
        addr,
        m::VPAD_CLOSE,
        &serde_json::json!({"slot": slot, "token": token}),
    )))
}

/// The rumble and light bar the game asks of `slot`.
pub fn output(addr: &str, slot: u8) -> Result<Option<Output>> {
    supported(
        call(addr, m::VPAD_OUTPUT, &serde_json::json!({ "slot": slot }))
            .and_then(|b| Ok(serde_json::from_slice(&b)?)),
    )
}

/// The helper's slots as it reports them; `None` from a helper without Play Together.
pub fn status(addr: &str) -> Result<Option<serde_json::Value>> {
    supported(
        call(addr, m::VPAD_STATUS, &serde_json::json!({}))
            .and_then(|b| Ok(serde_json::from_slice(&b)?)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mgmt::{scoped_transport, MgmtTransport};
    use std::sync::{Arc, Mutex};

    type Reply = Box<dyn Fn(Method, &serde_json::Value) -> Result<Option<Vec<u8>>> + Send + Sync>;

    struct Fake {
        seen: Mutex<Vec<(u16, serde_json::Value)>>,
        reply: Reply,
    }

    impl MgmtTransport for Fake {
        fn call(
            &self,
            _addr: &str,
            method: Method,
            _label: &str,
            body: &[u8],
            timeout: Duration,
        ) -> Result<Option<Vec<u8>>> {
            assert_eq!(timeout, CALL_TIMEOUT);
            let v: serde_json::Value = serde_json::from_slice(body).unwrap();
            self.seen.lock().unwrap().push((method.id, v.clone()));
            (self.reply)(method, &v)
        }
    }

    fn fake(
        f: impl Fn(Method, &serde_json::Value) -> Result<Option<Vec<u8>>> + Send + Sync + 'static,
    ) -> Arc<Fake> {
        Arc::new(Fake {
            seen: Mutex::default(),
            reply: Box::new(f),
        })
    }

    fn refusal(status: u16, cause: &str) -> anyhow::Error {
        anyhow::Error::new(MgmtError {
            label: "VPAD".into(),
            status,
            cause: cause.into(),
        })
    }

    #[test]
    fn open_carries_slot_token_and_user_and_reads_the_handle() {
        let f = fake(|m, b| {
            assert_eq!(m.id, 188);
            Ok(Some(
                format!(r#"{{"ok":true,"slot":{},"handle":7}}"#, b["slot"]).into_bytes(),
            ))
        });
        let _g = scoped_transport(f.clone());
        let o = open("c", 3, "ab", Some(0x1000_0001)).unwrap().unwrap();
        assert!(o.ok);
        assert_eq!((o.slot, o.handle), (3, 7));
        let seen = f.seen.lock().unwrap();
        assert_eq!(
            seen[0].1,
            serde_json::json!({"slot":3,"token":"ab","user":0x1000_0001u32})
        );
    }

    #[test]
    fn a_busy_slot_is_an_answer_not_an_error() {
        let _g = scoped_transport(fake(|_, _| Err(refusal(7, "vpad_busy"))));
        let o = open("c", 2, "t", None).unwrap().unwrap();
        assert!(!o.ok);
        assert_eq!(o.err.as_deref(), Some(ERR_BUSY));
        let _g = scoped_transport(fake(|_, _| {
            Err(refusal(7, r#"{"ok":false,"err":"vpad_no_slot"}"#))
        }));
        let a = hb("c", 2, "t", false).unwrap().unwrap();
        assert_eq!(a.err.as_deref(), Some("vpad_no_slot"));
        // a transport failure stays an error
        let _g = scoped_transport(fake(|_, _| Err(anyhow::anyhow!("connection reset"))));
        assert!(hb("c", 2, "t", false).is_err());
        // a cause that is prose (not a token) stays an error too
        let _g = scoped_transport(fake(|_, _| Err(refusal(7, "the helper is busy now"))));
        assert!(hb("c", 2, "t", false).is_err());
    }

    #[test]
    fn an_older_helper_is_none() {
        let _g = scoped_transport(fake(|_, _| {
            Err(refusal(
                crate::console_files::ERR_UNKNOWN_METHOD,
                "unknown method",
            ))
        }));
        assert_eq!(status("c").unwrap(), None);
        assert_eq!(open("c", 2, "t", None).unwrap(), None);
        assert_eq!(output("c", 2).unwrap(), None);
    }

    #[test]
    fn a_state_body_is_the_helpers_contract() {
        let s = PadState {
            buttons: button::PS | button::RIGHT,
            lx: 0,
            ly: 255,
            touch: vec![Touch {
                id: 1,
                x: 100,
                y: 200,
            }],
            motion: Some(Motion {
                gyro: [0.5, 0.0, -1.0],
                accel: [0.0, 1.0, 0.0],
            }),
            ..PadState::NEUTRAL
        };
        assert_eq!(
            s.body(2, "tok", 9),
            serde_json::json!({
                "slot": 2, "token": "tok", "seq": 9, "buttons": 0x10020,
                "lx": 0, "ly": 255, "rx": 128, "ry": 128, "l2": 0, "r2": 0,
                "touch": [{"id": 1, "x": 100, "y": 200}],
                "gyro": [0.5, 0.0, -1.0], "accel": [0.0, 1.0, 0.0],
            })
        );
        // neutral carries no touch or motion
        let n = PadState::NEUTRAL.body(4, "t", 1);
        assert!(n.get("touch").is_none() && n.get("gyro").is_none());
    }

    #[test]
    fn several_states_go_in_one_call_and_one_as_vpad_state() {
        let f = fake(|_, _| Ok(Some(br#"{"ok":true}"#.to_vec())));
        let _g = scoped_transport(f.clone());
        let item = |slot| StateItem {
            slot,
            token: "t".into(),
            seq: 1,
            state: PadState::NEUTRAL,
        };
        assert!(states("c", &[]).unwrap().unwrap().ok);
        assert!(states("c", &[item(2)]).unwrap().unwrap().ok);
        assert!(states("c", &[item(2), item(3)]).unwrap().unwrap().ok);
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, m::VPAD_STATE.id);
        assert_eq!(seen[1].0, m::VPAD_STATES.id);
        assert_eq!(seen[1].1["items"].as_array().unwrap().len(), 2);
        assert_eq!(seen[1].1["items"][1]["slot"], 3);
    }

    #[test]
    fn the_helpers_answers_decode() {
        let o: Opened = serde_json::from_str(
            r#"{"ok":true,"slot":2,"handle":5,"user":268435457,"resumed":true}"#,
        )
        .unwrap();
        assert_eq!(o.resumed, Some(true));
        assert_eq!(o.user, Some(268435457));
        let a: Ack = serde_json::from_str(
            r#"{"ok":true,"applied":1,"stale":0,"failed":[{"slot":3,"err":"vpad_not_open"}]}"#,
        )
        .unwrap();
        assert_eq!(a.failed[0].slot, 3);
        assert_eq!(a.failed[0].err, "vpad_not_open");
        let a: Ack = serde_json::from_str(r#"{"ok":true,"neutral":true}"#).unwrap();
        assert_eq!(a.neutral, Some(true));
        // vpad.state answers a bool
        let a: Ack = serde_json::from_str(r#"{"ok":true,"applied":true}"#).unwrap();
        assert_eq!((a.ok, a.applied), (true, Some(1)));
        let a: Ack = serde_json::from_str(r#"{"ok":true,"applied":false}"#).unwrap();
        assert_eq!(a.applied, Some(0));
    }

    #[test]
    fn a_live_heartbeat_says_so() {
        let f = fake(|_, _| Ok(Some(br#"{"ok":true,"neutral":false}"#.to_vec())));
        let _g = scoped_transport(f.clone());
        hb("c", 2, "t", true).unwrap();
        hb("c", 2, "t", false).unwrap();
        let seen = f.seen.lock().unwrap();
        assert_eq!(
            seen[0].1,
            serde_json::json!({"slot":2,"token":"t","live":true})
        );
        assert_eq!(seen[1].1, serde_json::json!({"slot":2,"token":"t"}));
    }

    #[test]
    fn app_buttons_become_the_helpers() {
        assert_eq!(from_app(app_button::CROSS, 0, 0), button::CROSS);
        assert_eq!(
            from_app(app_button::SHARE | app_button::PS, 0, 0),
            button::SYSTEM
        );
        assert_eq!(from_app(0, 1, 0), button::L2);
        assert_eq!(from_app(0, 0, 255), button::R2);
        assert_eq!(from_app(1 << 20, 0, 0), 0, "unknown bits are dropped");
        // every app bit maps to a distinct helper bit
        let all = APP_TO_PAD.iter().fold(0, |a, (x, _)| a | x);
        assert_eq!(from_app(all, 0, 0).count_ones(), 16);
    }

    #[test]
    fn output_reads_motors_and_color() {
        let _g = scoped_transport(fake(|m, b| {
            assert_eq!(m.id, m::VPAD_OUTPUT.id);
            assert_eq!(b["slot"], 3);
            Ok(Some(
                br#"{"ok":true,"slot":3,"motors":[10,200],"color":[0,0,255]}"#.to_vec(),
            ))
        }));
        let o = output("c", 3).unwrap().unwrap();
        assert_eq!(o.motors, [10, 200]);
        assert_eq!(o.color, [0, 0, 255]);
    }

    #[test]
    fn only_the_reads_may_go_through_the_passthrough() {
        assert!(is_input_method(m::VPAD_STATE.id));
        assert!(is_input_method(m::VPAD_OPEN.id));
        assert!(!is_input_method(m::VPAD_OUTPUT.id));
        assert!(!is_input_method(m::VPAD_STATUS.id));
    }
}
