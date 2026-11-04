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

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::feedback::ControllerState;

/// The shortest a button press, or a release between two presses, lasts on the console
/// (about three frames).
pub const MIN_HOLD: Duration = Duration::from_millis(50);
/// States kept waiting at most (a few holds' worth): older ones are dropped so the pad never
/// lags behind the player.
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
        let held =
            |t: Option<Instant>| t.is_some_and(|t| now.saturating_duration_since(t) < MIN_HOLD);
        let let_go = self.applied.buttons & !next.buttons;
        let pressed = next.buttons & !self.applied.buttons;
        let releases = let_go != 0 || (self.applied.touch.is_some() && next.touch.is_none());
        let presses = pressed != 0 || (self.applied.touch.is_none() && next.touch.is_some());
        if releases && held(self.last_press) {
            return None;
        }
        if pressed & self.released != 0 && held(self.last_release) {
            return None;
        }
        if presses {
            self.last_press = Some(now);
        }
        if let_go != 0 {
            self.released = if held(self.last_release) {
                self.released | let_go
            } else {
                let_go
            };
            self.last_release = Some(now);
        }
        self.applied = next;
        self.queue.pop_front();
        Some(next)
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
}
