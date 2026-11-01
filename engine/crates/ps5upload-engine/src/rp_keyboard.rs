//! Typing into the console's on-screen keyboard from the Remote Play page (see
//! `ps5upload_core::rp_stream::keyboard`).
//!
//! Only for a session started with `"keyboard": true`. Engine → page:
//! `{"keyboard": {"open": true, "text": "…"}}` when the console's keyboard opens (with the
//! text already in its field), `{"keyboard": {"text": "…"}}` when the console reports the
//! text changed (its answer to ours), `{"keyboard": {"open": false}}` when it closed on its
//! own. Page → engine: `{"keyboard_text": "…"}` (the whole text), `{"keyboard_accept": true}`
//! (Enter), `{"keyboard_reject": true}` (cancel). A page that attaches while the keyboard is
//! open is told so at once.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use ps5upload_core::rp_stream::{KeyboardEvent, KeyboardInput};

/// Page messages waiting for the stream thread; typing is coalesced, so this only fills if
/// the thread stalls.
const INPUT_QUEUE: usize = 64;

/// What a page message may carry for the keyboard (flattened into the page's message).
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct PageKeyboard {
    #[serde(default)]
    keyboard_text: Option<String>,
    #[serde(default)]
    keyboard_accept: bool,
    #[serde(default)]
    keyboard_reject: bool,
}

#[derive(Default)]
pub(crate) struct KeyboardLink {
    /// The session was set up for the keyboard; if not, the page's typing is ignored.
    pub(crate) enabled: AtomicBool,
    /// The console's keyboard is open, with this text as last known.
    open: Mutex<Option<String>>,
    input: Mutex<VecDeque<KeyboardInput>>,
}

impl KeyboardLink {
    /// The console's keyboard did something: remember it, and say it to the page.
    pub(crate) fn event(&self, ev: KeyboardEvent) -> serde_json::Value {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        match ev {
            KeyboardEvent::Open(t) => {
                *open = Some(t.clone());
                serde_json::json!({"keyboard": {"open": true, "text": t}})
            }
            KeyboardEvent::Text(t) => {
                if let Some(o) = open.as_mut() {
                    o.clone_from(&t);
                }
                serde_json::json!({"keyboard": {"text": t}})
            }
            KeyboardEvent::Closed => {
                *open = None;
                serde_json::json!({"keyboard": {"open": false}})
            }
        }
    }

    /// For a page that just attached: the keyboard, if it is open.
    pub(crate) fn on_attach(&self) -> Option<serde_json::Value> {
        let open = self.open.lock().ok()?;
        let t = open.as_ref()?;
        Some(serde_json::json!({"keyboard": {"open": true, "text": t}}))
    }

    /// The attached page's typing. Text replaces text still waiting (only the latest
    /// matters); accept and cancel close the keyboard here too.
    pub(crate) fn page_typed(&self, m: &PageKeyboard) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut q) = self.input.lock() else {
            return;
        };
        if let Some(t) = &m.keyboard_text {
            if let Some(KeyboardInput::Text(last)) = q.back_mut() {
                last.clone_from(t);
            } else if q.len() < INPUT_QUEUE {
                q.push_back(KeyboardInput::Text(t.clone()));
            }
            if let Ok(mut o) = self.open.lock() {
                if let Some(o) = o.as_mut() {
                    o.clone_from(t);
                }
            }
        }
        if (m.keyboard_accept || m.keyboard_reject) && q.len() < INPUT_QUEUE {
            q.push_back(if m.keyboard_accept {
                KeyboardInput::Accept
            } else {
                KeyboardInput::Reject
            });
            if let Ok(mut o) = self.open.lock() {
                *o = None;
            }
        }
    }

    pub(crate) fn next(&self) -> Option<KeyboardInput> {
        self.input.lock().ok()?.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(json: &str) -> PageKeyboard {
        serde_json::from_str(json).unwrap()
    }

    fn on() -> KeyboardLink {
        let k = KeyboardLink::default();
        k.enabled.store(true, Ordering::Relaxed);
        k
    }

    #[test]
    fn console_events_become_page_json() {
        let k = on();
        assert_eq!(k.on_attach(), None);
        let v = k.event(KeyboardEvent::Open("héllo 日本".into()));
        assert_eq!(v["keyboard"]["open"], true);
        assert_eq!(v["keyboard"]["text"], "héllo 日本");
        assert_eq!(k.on_attach(), Some(v));
        let v = k.event(KeyboardEvent::Text("abc".into()));
        assert_eq!(v, serde_json::json!({"keyboard": {"text": "abc"}}));
        assert_eq!(k.on_attach().unwrap()["keyboard"]["text"], "abc");
        let v = k.event(KeyboardEvent::Closed);
        assert_eq!(v, serde_json::json!({"keyboard": {"open": false}}));
        assert_eq!(k.on_attach(), None);
    }

    #[test]
    fn typing_is_coalesced_then_accepted() {
        let k = on();
        k.event(KeyboardEvent::Open(String::new()));
        k.page_typed(&page(r#"{"keyboard_text": "a"}"#));
        k.page_typed(&page(r#"{"keyboard_text": "ab"}"#));
        k.page_typed(&page(
            r#"{"keyboard_text": "abç", "keyboard_accept": true}"#,
        ));
        assert_eq!(k.next(), Some(KeyboardInput::Text("abç".into())));
        assert_eq!(k.next(), Some(KeyboardInput::Accept));
        assert_eq!(k.next(), None);
        assert_eq!(k.on_attach(), None, "accepting closes it");
    }

    #[test]
    fn cancel_and_a_session_without_the_keyboard() {
        let k = on();
        k.page_typed(&page(r#"{"keyboard_reject": true}"#));
        assert_eq!(k.next(), Some(KeyboardInput::Reject));
        let off = KeyboardLink::default();
        off.page_typed(&page(r#"{"keyboard_text": "x", "keyboard_accept": true}"#));
        assert_eq!(off.next(), None);
        // Other page messages carry nothing for it.
        k.page_typed(&page(r#"{"pad": {}}"#));
        assert_eq!(k.next(), None);
    }

    #[test]
    fn a_stalled_stream_holds_a_bounded_queue() {
        let k = on();
        for _ in 0..INPUT_QUEUE * 2 {
            k.page_typed(&page(r#"{"keyboard_reject": true}"#));
        }
        k.page_typed(&page(r#"{"keyboard_text": "x"}"#));
        let mut n = 0;
        while k.next().is_some() {
            n += 1;
        }
        assert_eq!(n, INPUT_QUEUE);
    }
}
