//! Pad states from the page, paced so the console sees every press and every release.
//!
//! The console reads the pad about once a frame: a press and its release landing in one frame
//! never register (seen: 3 quick taps moved the home screen 1 tile). So a press is held at
//! least `MIN_HOLD` before its release is applied, and (#655) a button let go stays let go for
//! `MIN_HOLD` before it is pressed again. Before, only the first rule held: a turbo (ours, or a
//! controller's own) pressing again right after the paced release had the release and the new
//! press sent together, the console saw the button held, and turbo did nothing. Each wait also
//! piled the states behind it up without end; the queue is now kept short, dropping stick- and
//! motion-only states before any press or release.
//!
//! Turbo above 10 presses a second (#671) cannot fit those holds, so the page names the buttons
//! its turbo is pressing (`ControllerState::turbo`) and those get `TURBO_HOLD`: at the top rate,
//! one frame on and one off at 60 fps. A game running slower than that misses some of them,
//! which the Controls page says. Everything else keeps `MIN_HOLD`, a tap during a turbo too.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::feedback::ControllerState;

/// The shortest a button press, or a release between two presses, lasts on the console
/// (about three frames).
pub const MIN_HOLD: Duration = Duration::from_millis(50);
/// The same for a button the page's turbo is pressing: under half a period at 30 presses a
/// second, or each wait would run past the next state and the queue would only grow.
pub const TURBO_HOLD: Duration = Duration::from_millis(16);
/// States kept waiting at most (a few holds' worth: 400 ms at `MIN_HOLD`, 130 ms at
/// `TURBO_HOLD`): older ones are dropped so the pad never lags behind the player.
const MAX_BACKLOG: usize = 8;

#[derive(Debug)]
pub struct PadPacer {
    queue: VecDeque<ControllerState>,
    applied: ControllerState,
    last_press: Option<Instant>,
    last_release: Option<Instant>,
    /// The buttons let go at `last_release`.
    released: u32,
}

impl Default for PadPacer {
    fn default() -> Self {
        Self::new()
    }
}

/// The same buttons held and the same touch (down or up): only sticks, triggers or motion differ.
fn same_keys(a: &ControllerState, b: &ControllerState) -> bool {
    a.buttons == b.buttons && a.touch.is_some() == b.touch.is_some()
}

impl PadPacer {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            applied: ControllerState::default(),
            last_press: None,
            last_release: None,
            released: 0,
        }
    }

    /// One state from the page, in order.
    pub fn push(&mut self, s: ControllerState) {
        self.queue.push_back(s);
        while self.queue.len() > MAX_BACKLOG {
            // A state the next one replaces without a press or release between them goes
            // first; with none, the oldest.
            let i = (0..self.queue.len() - 1)
                .find(|&i| same_keys(&self.queue[i], &self.queue[i + 1]))
                .unwrap_or(0);
            self.queue.remove(i);
        }
    }

    /// The next state to apply now, or None (nothing waiting, or the next must wait).
    pub fn next(&mut self, now: Instant) -> Option<ControllerState> {
        let next = *self.queue.front()?;
        let held = |t: Option<Instant>, hold: Duration| {
            t.is_some_and(|t| now.saturating_duration_since(t) < hold)
        };
        // Only when every button that changes is the turbo's.
        let hold_for = |changed: u32| {
            if changed != 0 && changed & !next.turbo == 0 {
                TURBO_HOLD
            } else {
                MIN_HOLD
            }
        };
        let let_go = self.applied.buttons & !next.buttons;
        let pressed = next.buttons & !self.applied.buttons;
        let finger_up = self.applied.touch.is_some() && next.touch.is_none();
        let releases = let_go != 0 || finger_up;
        let presses = pressed != 0 || (self.applied.touch.is_none() && next.touch.is_some());
        let release_hold = if finger_up {
            MIN_HOLD
        } else {
            hold_for(let_go)
        };
        if releases && held(self.last_press, release_hold) {
            return None;
        }
        let again = pressed & self.released;
        if again != 0 && held(self.last_release, hold_for(again)) {
            return None;
        }
        if presses {
            self.last_press = Some(now);
        }
        if let_go != 0 {
            self.released = if held(self.last_release, MIN_HOLD) {
                self.released | let_go
            } else {
                let_go
            };
            self.last_release = Some(now);
        }
        self.applied = next;
        self.queue.pop_front();
        Some(ControllerState { turbo: 0, ..next })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(buttons: u32) -> ControllerState {
        ControllerState {
            buttons,
            ..Default::default()
        }
    }

    /// Every state applied from `t0 + at` on, until the pacer waits.
    fn drain(p: &mut PadPacer, t: Instant) -> Vec<u32> {
        let mut out = Vec::new();
        while let Some(s) = p.next(t) {
            out.push(s.buttons);
        }
        out
    }

    #[test]
    fn a_quick_tap_is_held_before_its_release() {
        let t0 = Instant::now();
        let mut p = PadPacer::new();
        p.push(pad(1));
        p.push(pad(0));
        assert_eq!(drain(&mut p, t0), vec![1]);
        assert_eq!(
            drain(&mut p, t0 + Duration::from_millis(20)),
            Vec::<u32>::new()
        );
        assert_eq!(drain(&mut p, t0 + MIN_HOLD), vec![0]);
    }

    #[test]
    fn a_release_lasts_before_the_same_button_is_pressed_again() {
        // A turbo: press, release, press, release, faster than the holds. Each release must be
        // applied alone, not with the next press in the same moment (#655).
        let t0 = Instant::now();
        let mut p = PadPacer::new();
        for b in [1, 0, 1, 0, 1] {
            p.push(pad(b));
        }
        let ms = |n| t0 + Duration::from_millis(n);
        assert_eq!(drain(&mut p, ms(0)), vec![1]);
        assert_eq!(drain(&mut p, ms(50)), vec![0]);
        assert_eq!(drain(&mut p, ms(60)), Vec::<u32>::new());
        assert_eq!(drain(&mut p, ms(100)), vec![1]);
        assert_eq!(drain(&mut p, ms(150)), vec![0]);
        assert_eq!(drain(&mut p, ms(200)), vec![1]);
    }

    #[test]
    fn another_button_is_not_held_back_by_a_release() {
        let t0 = Instant::now();
        let mut p = PadPacer::new();
        p.push(pad(1));
        p.push(pad(0));
        p.push(pad(2));
        assert_eq!(drain(&mut p, t0), vec![1]);
        assert_eq!(drain(&mut p, t0 + MIN_HOLD), vec![0, 2]);
    }

    #[test]
    fn the_backlog_stays_short_and_keeps_presses_and_releases() {
        let t0 = Instant::now();
        let mut p = PadPacer::new();
        p.push(pad(1));
        assert_eq!(drain(&mut p, t0), vec![1]);
        // Held back by the press: a release, a press, then many stick moves.
        p.push(pad(0));
        p.push(pad(1));
        for i in 0..20 {
            p.push(ControllerState {
                buttons: 1,
                lx: i,
                ..Default::default()
            });
        }
        assert!(p.queue.len() <= MAX_BACKLOG);
        // The release and the press are still there, in order; the last stick position too.
        let buttons: Vec<u32> = p.queue.iter().map(|s| s.buttons).collect();
        assert_eq!(&buttons[..2], &[0, 1]);
        assert_eq!(p.queue.back().map(|s| s.lx), Some(19));
    }

    fn turbo(buttons: u32) -> ControllerState {
        ControllerState {
            buttons,
            turbo: 1,
            ..Default::default()
        }
    }

    #[test]
    fn a_turbo_button_is_held_a_frame_not_three() {
        let t0 = Instant::now();
        let mut p = PadPacer::new();
        for b in [1, 0, 1, 0] {
            p.push(turbo(b));
        }
        let ms = |n| t0 + Duration::from_millis(n);
        assert_eq!(drain(&mut p, ms(0)), vec![1]);
        assert_eq!(drain(&mut p, ms(15)), Vec::<u32>::new());
        assert_eq!(drain(&mut p, ms(16)), vec![0]);
        assert_eq!(drain(&mut p, ms(31)), Vec::<u32>::new());
        assert_eq!(drain(&mut p, ms(32)), vec![1]);
        assert_eq!(drain(&mut p, ms(48)), vec![0]);
    }

    #[test]
    fn the_console_never_sees_the_turbo_mark() {
        let mut p = PadPacer::new();
        p.push(turbo(1));
        assert_eq!(p.next(Instant::now()).map(|s| s.turbo), Some(0));
    }

    #[test]
    fn a_tap_on_another_button_during_a_turbo_keeps_its_full_hold() {
        let t0 = Instant::now();
        let mut p = PadPacer::new();
        // Button 1 is the turbo's; button 2 is an ordinary tap.
        p.push(turbo(1 | 2));
        p.push(turbo(1));
        assert_eq!(drain(&mut p, t0), vec![3]);
        assert_eq!(
            drain(&mut p, t0 + Duration::from_millis(16)),
            Vec::<u32>::new()
        );
        assert_eq!(drain(&mut p, t0 + MIN_HOLD), vec![1]);
    }

    #[test]
    fn a_finger_lifted_during_a_turbo_keeps_its_full_hold() {
        let t0 = Instant::now();
        let mut p = PadPacer::new();
        p.push(ControllerState {
            touch: Some([10, 10]),
            turbo: 1,
            ..Default::default()
        });
        p.push(turbo(0));
        assert!(p.next(t0).is_some());
        assert!(p.next(t0 + TURBO_HOLD).is_none());
        assert!(p.next(t0 + MIN_HOLD).is_some());
    }

    /// `hz` presses a second for a second, read by the stream loop every `tick_ms`: how many
    /// presses were applied, and the longest the queue got.
    fn run_turbo(hz: u64, tick_ms: u64) -> (u32, usize) {
        let t0 = Instant::now();
        let half_us = 500_000 / hz;
        let mut p = PadPacer::new();
        let (mut presses, mut last, mut longest) = (0, 0, 0);
        let mut next_state = 0;
        let mut at_us = 0;
        while at_us < 1_000_000 {
            while next_state * half_us <= at_us {
                p.push(turbo(u32::from(next_state % 2 == 0)));
                next_state += 1;
            }
            longest = longest.max(p.queue.len());
            for b in drain(&mut p, t0 + Duration::from_micros(at_us)) {
                if b == 1 && last == 0 {
                    presses += 1;
                }
                last = b;
            }
            at_us += tick_ms * 1000;
        }
        (presses, longest)
    }

    #[test]
    fn thirty_presses_a_second_all_get_through_without_the_queue_growing() {
        // The stream loop reads every 4 ms at the slowest (`takion.poll`).
        for tick_ms in [1, 4] {
            let (presses, longest) = run_turbo(30, tick_ms);
            assert!((29..=30).contains(&presses), "{presses} at {tick_ms} ms");
            assert!(longest <= 3, "queue reached {longest} at {tick_ms} ms");
        }
        assert_eq!(run_turbo(20, 4).0, 20);
    }

    #[test]
    fn after_a_stall_at_the_top_rate_the_backlog_stays_bound_and_the_last_state_arrives() {
        let t0 = Instant::now();
        let mut p = PadPacer::new();
        // Half a second with nothing read: 30 presses and releases pile up.
        for i in 0..31 {
            p.push(turbo(u32::from(i % 2 == 0)));
        }
        assert!(p.queue.len() <= MAX_BACKLOG);
        let mut applied = Vec::new();
        for n in 0..=(MAX_BACKLOG as u64 + 1) {
            applied.extend(drain(&mut p, t0 + TURBO_HOLD * n as u32));
        }
        assert!(p.queue.is_empty(), "drained within the backlog's holds");
        // The last one sent was a press: never dropped.
        assert_eq!(applied.last(), Some(&1));
    }

    #[test]
    fn old_pages_send_no_turbo_field() {
        let s: ControllerState = serde_json::from_str(r#"{"buttons":1}"#).unwrap();
        assert_eq!(s.turbo, 0);
        let t: ControllerState = serde_json::from_str(r#"{"buttons":1,"turbo":1}"#).unwrap();
        assert_eq!(t.turbo, 1);
    }
}
