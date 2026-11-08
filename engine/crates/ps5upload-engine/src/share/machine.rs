//! A share's life on the sending side, without the network: the link is open until it expires,
//! is cancelled, or (one-use) someone was allowed. A friend who opened it asks for some of its
//! items; the sender allows or declines; an allowed friend gets the files and says when they
//! have them all. Every step the network does goes through here, so the order and every refusal
//! can be tested on their own.

use std::time::{Duration, Instant};

/// Friends asking at once on one share; more are told it is busy.
pub(crate) const MAX_ASKING: usize = 4;
/// Requests kept on one share (ended ones included) before the oldest ended ones go.
const MAX_REQUESTS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShareState {
    /// The link works.
    Open,
    /// One-use, and a friend was allowed: the link takes nobody new.
    Used,
    Expired,
    Cancelled,
}

impl ShareState {
    pub(crate) fn word(self) -> &'static str {
        match self {
            ShareState::Open => "open",
            ShareState::Used => "used",
            ShareState::Expired => "expired",
            ShareState::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReqState {
    /// Waiting for the sender's Allow or Decline.
    Asking,
    /// Allowed; nothing sent yet (or between connections: it can resume).
    Allowed,
    Sending,
    Done,
    Declined,
    /// The sender cancelled the share, or the friend cancelled.
    Cancelled,
    /// The friend went away before an answer, or the link expired first.
    Left,
}

impl ReqState {
    pub(crate) fn word(self) -> &'static str {
        match self {
            ReqState::Asking => "asking",
            ReqState::Allowed => "allowed",
            ReqState::Sending => "sending",
            ReqState::Done => "done",
            ReqState::Declined => "declined",
            ReqState::Cancelled => "cancelled",
            ReqState::Left => "left",
        }
    }

    pub(crate) fn ended(self) -> bool {
        matches!(
            self,
            ReqState::Done | ReqState::Declined | ReqState::Cancelled | ReqState::Left
        )
    }
}

/// One friend's request.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub rid: u64,
    pub name: String,
    /// The item ids they accepted.
    pub wants: Vec<String>,
    pub state: ReqState,
    /// Lets an allowed friend's next connection carry on (given only to them, on Allow).
    pub token: Option<String>,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub asked_at: Instant,
    pub why: Option<String>,
}

/// Why a step was refused (the API's and the friend's word).
pub(crate) type Refused = &'static str;

#[derive(Debug)]
pub(crate) struct Machine {
    pub state: ShareState,
    pub one_use: bool,
    pub expires_at: Instant,
    /// The ids of the share's items.
    pub item_ids: Vec<String>,
    pub requests: Vec<Request>,
    next_rid: u64,
}

impl Machine {
    pub(crate) fn new(now: Instant, ttl: Duration, one_use: bool, item_ids: Vec<String>) -> Self {
        Machine {
            state: ShareState::Open,
            one_use,
            expires_at: now + ttl,
            item_ids,
            requests: Vec::new(),
            next_rid: 1,
        }
    }

    /// The state now: an open (or used) link past its time is expired, and whoever was still
    /// waiting for an answer is told so.
    pub(crate) fn tick(&mut self, now: Instant) -> ShareState {
        if matches!(self.state, ShareState::Open | ShareState::Used) && now >= self.expires_at {
            self.state = ShareState::Expired;
            for r in &mut self.requests {
                if r.state == ReqState::Asking {
                    r.state = ReqState::Left;
                    r.why = Some("expired".into());
                }
            }
        }
        self.state
    }

    /// A friend opened the link: may they see what is offered?
    pub(crate) fn hello(&mut self, now: Instant) -> Result<(), Refused> {
        match self.tick(now) {
            ShareState::Open => Ok(()),
            s => Err(s.word()),
        }
    }

    /// A friend asks for `wants` (some of the items). The request's id.
    pub(crate) fn ask(
        &mut self,
        now: Instant,
        name: &str,
        wants: &[String],
    ) -> Result<u64, Refused> {
        self.hello(now)?;
        let mut w: Vec<String> = Vec::new();
        for id in wants {
            if !self.item_ids.contains(id) {
                return Err("unknown_item");
            }
            if !w.contains(id) {
                w.push(id.clone());
            }
        }
        if w.is_empty() {
            return Err("nothing_asked");
        }
        if self
            .requests
            .iter()
            .filter(|r| r.state == ReqState::Asking)
            .count()
            >= MAX_ASKING
        {
            return Err("busy");
        }
        if self.requests.len() >= MAX_REQUESTS {
            if let Some(i) = self.requests.iter().position(|r| r.state.ended()) {
                self.requests.remove(i);
            } else {
                return Err("busy");
            }
        }
        let rid = self.next_rid;
        self.next_rid += 1;
        self.requests.push(Request {
            rid,
            name: name.to_string(),
            wants: w,
            state: ReqState::Asking,
            token: None,
            bytes_done: 0,
            bytes_total: 0,
            asked_at: now,
            why: None,
        });
        Ok(rid)
    }

    pub(crate) fn request(&self, rid: u64) -> Option<&Request> {
        self.requests.iter().find(|r| r.rid == rid)
    }

    fn request_mut(&mut self, rid: u64) -> Option<&mut Request> {
        self.requests.iter_mut().find(|r| r.rid == rid)
    }

    /// The sender's answer. `token` is the allowed friend's resume token. A one-use share takes
    /// one Allow: the others still asking are declined with it.
    pub(crate) fn decide(
        &mut self,
        now: Instant,
        rid: u64,
        allow: bool,
        token: String,
    ) -> Result<ReqState, Refused> {
        let state = self.tick(now);
        let one_use = self.one_use;
        let r = self.request_mut(rid).ok_or("no_request")?;
        if r.state != ReqState::Asking {
            return Err("answered");
        }
        if !allow {
            r.state = ReqState::Declined;
            return Ok(ReqState::Declined);
        }
        if state != ShareState::Open {
            return Err(state.word());
        }
        r.state = ReqState::Allowed;
        r.token = Some(token);
        if one_use {
            self.state = ShareState::Used;
            for o in &mut self.requests {
                if o.rid != rid && o.state == ReqState::Asking {
                    o.state = ReqState::Declined;
                    o.why = Some("used".into());
                }
            }
        }
        Ok(ReqState::Allowed)
    }

    /// An allowed friend connects again with their token: their request, to carry on. Refused
    /// once the share was cancelled or expired, or the request ended.
    pub(crate) fn resume(&mut self, now: Instant, token: &str) -> Result<u64, Refused> {
        let state = self.tick(now);
        if matches!(state, ShareState::Cancelled | ShareState::Expired) {
            return Err(state.word());
        }
        let r = self
            .requests
            .iter_mut()
            .find(|r| r.token.as_deref() == Some(token) && !token.is_empty())
            .ok_or("no_request")?;
        if !matches!(r.state, ReqState::Allowed | ReqState::Sending) {
            return Err(r.state.word());
        }
        r.state = ReqState::Allowed;
        Ok(r.rid)
    }

    /// Files start going to an allowed friend (`total`: the bytes of what they accepted).
    pub(crate) fn sending(&mut self, rid: u64, total: u64) -> Result<(), Refused> {
        if self.state == ShareState::Cancelled {
            return Err("cancelled");
        }
        let r = self.request_mut(rid).ok_or("no_request")?;
        if !matches!(r.state, ReqState::Allowed | ReqState::Sending) {
            return Err(r.state.word());
        }
        r.state = ReqState::Sending;
        r.bytes_total = total;
        Ok(())
    }

    pub(crate) fn progress(&mut self, rid: u64, done: u64) {
        if let Some(r) = self.request_mut(rid) {
            r.bytes_done = done.min(r.bytes_total.max(done));
        }
    }

    /// The friend has everything.
    pub(crate) fn done(&mut self, rid: u64) -> Result<(), Refused> {
        let r = self.request_mut(rid).ok_or("no_request")?;
        if r.state != ReqState::Sending {
            return Err(r.state.word());
        }
        r.state = ReqState::Done;
        r.bytes_done = r.bytes_total;
        Ok(())
    }

    /// The friend's connection ended. Asking: they left. Sending or allowed: kept, so they can
    /// resume with their token, unless `cancelled` (they said so).
    pub(crate) fn gone(&mut self, rid: u64, cancelled: bool, why: Option<&str>) {
        let Some(r) = self.request_mut(rid) else {
            return;
        };
        match r.state {
            ReqState::Asking => {
                r.state = if cancelled {
                    ReqState::Cancelled
                } else {
                    ReqState::Left
                };
            }
            ReqState::Allowed | ReqState::Sending if cancelled => {
                r.state = ReqState::Cancelled;
            }
            ReqState::Sending => r.state = ReqState::Allowed,
            _ => return,
        }
        if let Some(w) = why {
            r.why = Some(w.to_string());
        }
    }

    /// The sender cancels: the link stops, and every request not ended is cancelled.
    pub(crate) fn cancel(&mut self) {
        self.state = ShareState::Cancelled;
        for r in &mut self.requests {
            if !r.state.ended() {
                r.state = ReqState::Cancelled;
                r.why = Some("sender".into());
            }
        }
    }

    /// Nothing more can happen: cancelled or expired with nothing going, or used and every
    /// request ended. The share's files can go.
    pub(crate) fn finished(&mut self, now: Instant) -> bool {
        let s = self.tick(now);
        let busy = self
            .requests
            .iter()
            .any(|r| matches!(r.state, ReqState::Sending));
        match s {
            ShareState::Cancelled => true,
            ShareState::Expired => !busy,
            ShareState::Used => self.requests.iter().all(|r| r.state.ended()),
            ShareState::Open => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 3600);

    fn m(one_use: bool) -> (Machine, Instant) {
        let now = Instant::now();
        (
            Machine::new(now, DAY, one_use, vec!["a".into(), "b".into()]),
            now,
        )
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn invite_accept_allow_send_done() {
        let (mut s, now) = m(true);
        assert_eq!(s.hello(now), Ok(()));
        let rid = s.ask(now, "Alex", &ids(&["a"])).unwrap();
        assert_eq!(s.request(rid).unwrap().state, ReqState::Asking);
        // Nothing goes before the Allow.
        assert_eq!(s.sending(rid, 10), Err("asking"));
        assert_eq!(
            s.decide(now, rid, true, "tok".into()),
            Ok(ReqState::Allowed)
        );
        assert_eq!(s.state, ShareState::Used);
        s.sending(rid, 10).unwrap();
        s.progress(rid, 4);
        assert_eq!(s.request(rid).unwrap().bytes_done, 4);
        s.done(rid).unwrap();
        assert_eq!(s.request(rid).unwrap().state, ReqState::Done);
        assert!(s.finished(now));
    }

    #[test]
    fn a_friend_asks_only_for_what_is_offered() {
        let (mut s, now) = m(true);
        assert_eq!(s.ask(now, "A", &ids(&["zzz"])), Err("unknown_item"));
        assert_eq!(s.ask(now, "A", &[]), Err("nothing_asked"));
        let rid = s.ask(now, "A", &ids(&["b", "b", "a"])).unwrap();
        assert_eq!(s.request(rid).unwrap().wants, ids(&["b", "a"]));
    }

    #[test]
    fn decline_ends_the_request_and_the_link_stays_open() {
        let (mut s, now) = m(true);
        let rid = s.ask(now, "A", &ids(&["a"])).unwrap();
        assert_eq!(
            s.decide(now, rid, false, String::new()),
            Ok(ReqState::Declined)
        );
        assert_eq!(s.state, ShareState::Open);
        assert_eq!(s.decide(now, rid, true, "t".into()), Err("answered"));
        assert_eq!(s.sending(rid, 1), Err("declined"));
        assert_eq!(s.hello(now), Ok(()), "someone else may still ask");
    }

    #[test]
    fn one_use_takes_one_allow_and_turns_the_others_away() {
        let (mut s, now) = m(true);
        let r1 = s.ask(now, "A", &ids(&["a"])).unwrap();
        let r2 = s.ask(now, "B", &ids(&["a"])).unwrap();
        s.decide(now, r1, true, "t1".into()).unwrap();
        assert_eq!(s.request(r2).unwrap().state, ReqState::Declined);
        assert_eq!(s.hello(now), Err("used"));
        assert_eq!(s.ask(now, "C", &ids(&["a"])), Err("used"));
        // The allowed friend can still carry on.
        assert_eq!(s.resume(now, "t1"), Ok(r1));
        assert_eq!(s.resume(now, "nope"), Err("no_request"));
        assert_eq!(s.resume(now, ""), Err("no_request"));
    }

    #[test]
    fn a_link_for_many_takes_several_friends() {
        let (mut s, now) = m(false);
        let r1 = s.ask(now, "A", &ids(&["a"])).unwrap();
        let r2 = s.ask(now, "B", &ids(&["b"])).unwrap();
        s.decide(now, r1, true, "t1".into()).unwrap();
        s.decide(now, r2, true, "t2".into()).unwrap();
        assert_eq!(s.state, ShareState::Open);
        assert_eq!(s.hello(now), Ok(()));
    }

    #[test]
    fn expiry_stops_the_link_and_whoever_was_waiting() {
        let (mut s, now) = m(true);
        let rid = s.ask(now, "A", &ids(&["a"])).unwrap();
        let later = now + DAY + Duration::from_secs(1);
        assert_eq!(s.hello(later), Err("expired"));
        assert_eq!(s.request(rid).unwrap().state, ReqState::Left);
        assert_eq!(s.decide(later, rid, true, "t".into()), Err("answered"));
        assert!(s.finished(later));
    }

    #[test]
    fn an_allow_after_expiry_is_refused() {
        let (mut s, now) = m(true);
        let rid = s.ask(now, "A", &ids(&["a"])).unwrap();
        // Expired, but not yet noticed by anything but the decide itself.
        s.expires_at = now;
        assert_eq!(s.decide(now, rid, true, "t".into()), Err("answered"));
        assert_eq!(s.state, ShareState::Expired);
    }

    #[test]
    fn a_resume_after_expiry_is_refused_but_a_running_transfer_finishes() {
        let (mut s, now) = m(true);
        let rid = s.ask(now, "A", &ids(&["a"])).unwrap();
        s.decide(now, rid, true, "t".into()).unwrap();
        s.sending(rid, 5).unwrap();
        let later = now + DAY * 2;
        assert!(!s.finished(later), "still sending");
        s.done(rid).unwrap();
        assert!(s.finished(later));
        assert_eq!(s.resume(later, "t"), Err("expired"));
    }

    #[test]
    fn cancel_stops_everything_at_once() {
        let (mut s, now) = m(false);
        let r1 = s.ask(now, "A", &ids(&["a"])).unwrap();
        let r2 = s.ask(now, "B", &ids(&["a"])).unwrap();
        s.decide(now, r2, true, "t".into()).unwrap();
        s.sending(r2, 9).unwrap();
        s.cancel();
        assert_eq!(s.state, ShareState::Cancelled);
        assert_eq!(s.request(r1).unwrap().state, ReqState::Cancelled);
        assert_eq!(s.request(r2).unwrap().state, ReqState::Cancelled);
        assert_eq!(s.hello(now), Err("cancelled"));
        assert_eq!(s.sending(r2, 9), Err("cancelled"));
        assert_eq!(s.resume(now, "t"), Err("cancelled"));
        assert!(s.finished(now));
    }

    #[test]
    fn the_friend_leaving_or_cancelling() {
        let (mut s, now) = m(false);
        let r1 = s.ask(now, "A", &ids(&["a"])).unwrap();
        s.gone(r1, false, None);
        assert_eq!(s.request(r1).unwrap().state, ReqState::Left);
        // An allowed friend whose connection dropped mid-way can resume…
        let r2 = s.ask(now, "B", &ids(&["a"])).unwrap();
        s.decide(now, r2, true, "t2".into()).unwrap();
        s.sending(r2, 9).unwrap();
        s.gone(r2, false, Some("lost"));
        assert_eq!(s.request(r2).unwrap().state, ReqState::Allowed);
        assert_eq!(s.resume(now, "t2"), Ok(r2));
        // …one who cancelled can't.
        s.sending(r2, 9).unwrap();
        s.gone(r2, true, None);
        assert_eq!(s.request(r2).unwrap().state, ReqState::Cancelled);
        assert_eq!(s.resume(now, "t2"), Err("cancelled"));
    }

    #[test]
    fn too_many_asking_at_once_is_busy() {
        let (mut s, now) = m(false);
        for i in 0..MAX_ASKING {
            s.ask(now, &format!("F{i}"), &ids(&["a"])).unwrap();
        }
        assert_eq!(s.ask(now, "late", &ids(&["a"])), Err("busy"));
    }
}
