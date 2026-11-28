//! Play Rooms text chat: the rules a line must pass, the room's 50-line ring and the rate limit
//! (join_comms.rs carries the lines; nothing here sends anything).
//!
//! - **Quick messages** go as ids 1–16 and each client shows them in its own language
//!   (Ready? · Ready! · GG · One more? · Brb · Wait · Let's go · Thanks · Nice! · Sorry ·
//!   Pause please · Lag on my side · Can't hear you · Switch seats? · Bye · 👍).
//! - **Typed text** is 1–140 characters on one line, emoji allowed. Control and invisible
//!   formatting characters go (the emoji joiner stays), spaces collapse. Anything that looks like a
//!   link or an address keeps its words with its dots spelled "·" and `://` split, so no client
//!   can make it clickable. In a Public room invite codes and long keys become "···".
//! - **Rate:** a burst of 5, then one every 3 s (20 a minute). That never stops a person typing,
//!   only a script; past it the line is refused with "slow down" and the wait.
//! - **Repeats fold:** the same line from the same person again within 30 s, while it is still the
//!   last line, keeps its id and counts up (`x`), so clients show "GG ×3" instead of three lines.
//! - Nothing is stored or logged: the ring lives in memory for the room's life only.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The highest quick-message id.
pub(crate) const QUICK_MAX: u8 = 16;
/// The most characters (Unicode scalar values) a typed line may have.
pub(crate) const TEXT_MAX_CHARS: usize = 140;
/// Lines kept for the room's life (in memory only).
pub(crate) const RING: usize = 50;
/// Lines a newcomer gets on joining (after accepting the notice).
pub(crate) const HISTORY_ON_JOIN: usize = 20;
/// The same line from the same person within this folds into the last one.
pub(crate) const FOLD_WITHIN: Duration = Duration::from_secs(30);
/// A person's line burst, and the refill (one every 3 s).
pub(crate) const CHAT_BURST: f64 = 5.0;
pub(crate) const CHAT_PER_S: f64 = 1.0 / 3.0;

/// What a room allows in chat (host setting).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ChatMode {
    Off,
    /// Quick messages only.
    Quick,
    /// Quick messages and typed text. "text" (the design's word in the room push) is taken too.
    #[serde(alias = "text")]
    On,
}

impl ChatMode {
    pub(crate) fn word(self) -> &'static str {
        match self {
            ChatMode::Off => "off",
            ChatMode::Quick => "quick",
            ChatMode::On => "on",
        }
    }
}

/// One line's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Body {
    Quick(u8),
    Text(String),
}

/// Why a line (or a voice frame, a mic change) was refused: the `why` word clients turn into
/// words ("slow_down" carries the wait).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The responsibility notice is not accepted (the client shows it).
    Notice,
    /// The host turned chat (or voice) off.
    Off,
    /// The room takes quick messages only.
    QuickOnly,
    /// The host muted this person.
    Muted,
    /// Too many too fast: try again after this.
    SlowDown(Duration),
    Empty,
    TooLong,
    /// Not a quick-message id.
    BadQuick,
    /// Voice: the room is push-to-talk only and the key is not held.
    PttOnly,
    /// Voice: the person's mic is off.
    MicOff,
    /// Voice: paused, their connection needs it for the game.
    Poor,
    /// No room is open on this console.
    NotOpen,
}

impl Refusal {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Refusal::Notice => "notice",
            Refusal::Off => "off",
            Refusal::QuickOnly => "quick_only",
            Refusal::Muted => "muted",
            Refusal::SlowDown(_) => "slow_down",
            Refusal::Empty => "empty",
            Refusal::TooLong => "too_long",
            Refusal::BadQuick => "bad_quick",
            Refusal::PttOnly => "ptt_only",
            Refusal::MicOff => "mic_off",
            Refusal::Poor => "poor",
            Refusal::NotOpen => "not_open",
        }
    }
}

/// A token bucket: `cap` at once, refilled at `per_s`. Generous bursts, smooth refill: a person
/// never notices it, a script is slowed.
#[derive(Clone, Debug)]
pub(crate) struct Bucket {
    tokens: f64,
    at: Instant,
    cap: f64,
    per_s: f64,
}

impl Bucket {
    pub(crate) fn new(cap: f64, per_s: f64, now: Instant) -> Self {
        Bucket {
            tokens: cap,
            at: now,
            cap,
            per_s,
        }
    }

    fn refill(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = (self.tokens + dt * self.per_s).min(self.cap);
        self.at = now;
    }

    /// Takes one; Err: how long until one is there.
    pub(crate) fn take(&mut self, now: Instant) -> Result<(), Duration> {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - self.tokens) / self.per_s))
        }
    }
}

/// Is this a character nobody sees (or one that turns text around)? Zero-width spaces and marks,
/// bidi overrides and isolates, word joiners, the BOM, the soft hyphen. The zero-width joiner
/// (U+200D) is kept: emoji sequences need it.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{180E}'
            | '\u{200B}'
            | '\u{200C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// The invite-code alphabet (join.rs `CODE_ALPHABET`), either case.
fn code_char(c: char) -> bool {
    c.is_ascii() && crate::join::CODE_ALPHABET.contains(&(c.to_ascii_uppercase() as u8))
}

/// A token's core without the punctuation around it.
fn core(t: &str) -> &str {
    t.trim_matches(|c: char| {
        matches!(
            c,
            '(' | ')' | '[' | ']' | '"' | '\'' | ',' | '!' | '?' | ';'
        )
    })
    .trim_end_matches(['.', ':'])
}

/// Looks like a link or an address: a scheme, `www.`, a dotted name ending in a 2–24 letter word,
/// or four dotted numbers (an IP address).
pub(crate) fn looks_like_link(token: &str) -> bool {
    let t = core(token);
    let lower = t.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("www.") {
        return true;
    }
    // The host part: up to a path, port or query.
    let host = t.split(['/', '?', '#']).next().unwrap_or(t);
    let host = match host.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host,
    };
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() < 2 || parts.iter().any(|p| p.is_empty()) {
        return false;
    }
    if parts.len() == 4
        && parts
            .iter()
            .all(|p| p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    let tld = parts[parts.len() - 1];
    (2..=24).contains(&tld.chars().count())
        && tld.chars().all(|c| c.is_alphabetic())
        && parts[..parts.len() - 1]
            .iter()
            .all(|p| p.chars().any(|c| c.is_alphanumeric()))
}

/// An invite code (`ABC-DEF`, or six code characters with a digit in them), or a long key
/// (16+ letters, digits, `-`, `_` with both letters and digits).
pub(crate) fn looks_like_code(token: &str) -> bool {
    let t = core(token);
    let n = t.chars().count();
    if n == 7 && t.is_ascii() {
        let (a, b) = t.split_at(3);
        if let Some(b) = b.strip_prefix('-') {
            return a.chars().all(code_char) && b.chars().all(code_char) && b.len() == 3;
        }
    }
    if n == 6 && t.chars().all(code_char) && t.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    n >= 16
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars().any(|c| c.is_ascii_alphabetic())
}

/// A typed line as everyone gets it, or why not. `public`: invite codes and keys go too.
pub(crate) fn clean_text(raw: &str, public: bool) -> Result<String, Refusal> {
    // One line: control characters become spaces, invisible ones go, spaces collapse.
    let mut flat = String::with_capacity(raw.len());
    for c in raw.chars() {
        if invisible(c) {
            continue;
        }
        flat.push(if c.is_control() || c.is_whitespace() {
            ' '
        } else {
            c
        });
    }
    let mut out: Vec<String> = Vec::new();
    for token in flat.split(' ').filter(|t| !t.is_empty()) {
        if public && looks_like_code(token) {
            out.push("···".into());
        } else if looks_like_link(token) {
            out.push(token.replace("://", ": //").replace('.', "·"));
        } else {
            out.push(token.to_string());
        }
    }
    let line = out.join(" ");
    match line.chars().count() {
        0 => Err(Refusal::Empty),
        n if n > TEXT_MAX_CHARS => Err(Refusal::TooLong),
        _ => Ok(line),
    }
}

/// What the room's chat mode allows for this body (before the text is cleaned).
pub(crate) fn allowed(mode: ChatMode, body: &Body) -> Result<(), Refusal> {
    match (mode, body) {
        (ChatMode::Off, _) => Err(Refusal::Off),
        (_, Body::Quick(n)) if !(1..=QUICK_MAX).contains(n) => Err(Refusal::BadQuick),
        (ChatMode::Quick, Body::Text(_)) => Err(Refusal::QuickOnly),
        _ => Ok(()),
    }
}

/// One line in the ring.
#[derive(Clone, Debug)]
pub(crate) struct Line {
    pub(crate) id: u64,
    /// Who said it: their person id (never sent), speaker id, name, player, host or not.
    pub(crate) pid: u64,
    pub(crate) sid: u8,
    pub(crate) from: String,
    pub(crate) player: Option<u8>,
    pub(crate) host: bool,
    pub(crate) body: Body,
    /// Times said in a row (folded repeats).
    pub(crate) x: u32,
    pub(crate) at: Instant,
    /// When the room's chat started, for `t` (seconds into the room).
    pub(crate) t_s: u64,
}

impl Line {
    /// The line as clients get it (`{"chat": …}` wraps it).
    pub(crate) fn json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "id": self.id,
            "sid": self.sid,
            "from": self.from,
            "player": self.player,
            "host": self.host,
            "t": self.t_s,
        });
        match &self.body {
            Body::Quick(n) => v["quick"] = (*n).into(),
            Body::Text(t) => v["text"] = t.clone().into(),
        }
        if self.x > 1 {
            v["x"] = self.x.into();
        }
        v
    }
}

/// The room's last [`RING`] lines.
#[derive(Debug)]
pub(crate) struct Ring {
    lines: VecDeque<Line>,
    next_id: u64,
    started: Instant,
}

/// Who says a line.
pub(crate) struct Speaker<'a> {
    pub(crate) pid: u64,
    pub(crate) sid: u8,
    pub(crate) name: &'a str,
    pub(crate) player: Option<u8>,
    pub(crate) host: bool,
}

impl Ring {
    pub(crate) fn new(now: Instant) -> Self {
        Ring {
            lines: VecDeque::new(),
            next_id: 1,
            started: now,
        }
    }

    /// Adds a line (or folds it into the last one): the line as it now is.
    pub(crate) fn push(&mut self, who: Speaker<'_>, body: Body, now: Instant) -> &Line {
        let fold = self.lines.back().is_some_and(|l| {
            l.pid == who.pid && l.body == body && now.saturating_duration_since(l.at) <= FOLD_WITHIN
        });
        if fold {
            let l = self.lines.back_mut().expect("checked above");
            l.x = l.x.saturating_add(1);
            l.at = now;
            return l;
        }
        if self.lines.len() >= RING {
            self.lines.pop_front();
        }
        let id = self.next_id;
        self.next_id += 1;
        self.lines.push_back(Line {
            id,
            pid: who.pid,
            sid: who.sid,
            from: who.name.to_string(),
            player: who.player,
            host: who.host,
            body,
            x: 1,
            at: now,
            t_s: now.saturating_duration_since(self.started).as_secs(),
        });
        self.lines.back().expect("just pushed")
    }

    /// The last `n` lines, oldest first.
    pub(crate) fn last(&self, n: usize) -> Vec<serde_json::Value> {
        let skip = self.lines.len().saturating_sub(n);
        self.lines.iter().skip(skip).map(Line::json).collect()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lines.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_is_one_clean_line_of_at_most_140_characters() {
        assert_eq!(clean_text("  gg\n\twp  ", false).unwrap(), "gg wp");
        assert_eq!(clean_text("", false), Err(Refusal::Empty));
        assert_eq!(clean_text(" \u{200B}\u{202E} ", false), Err(Refusal::Empty));
        // Invisible characters and bidi overrides go; the emoji joiner stays.
        assert_eq!(clean_text("a\u{200B}b\u{202E}c", false).unwrap(), "abc");
        let family = "👨\u{200D}👩\u{200D}👧";
        assert_eq!(clean_text(family, false).unwrap(), family);
        // 140 characters (emoji count as one each) pass, 141 do not.
        assert!(clean_text(&"😀".repeat(140), false).is_ok());
        assert_eq!(clean_text(&"x".repeat(141), false), Err(Refusal::TooLong));
        // Accents and other scripts are fine.
        assert_eq!(
            clean_text("¡Buen juego! ナイス", false).unwrap(),
            "¡Buen juego! ナイス"
        );
    }

    #[test]
    fn links_and_addresses_never_stay_links() {
        for (raw, want) in [
            (
                "go to https://evil.example/x now",
                "go to https: //evil·example/x now",
            ),
            ("www.example.com", "www·example·com"),
            ("see example.com.", "see example·com·"),
            (
                "join me at 192.168.1.20:8080",
                "join me at 192·168·1·20:8080",
            ),
            ("ps5upload://join?k=1", "ps5upload: //join?k=1"),
            ("mail me a@b.io", "mail me a@b·io"),
        ] {
            assert_eq!(clean_text(raw, false).unwrap(), want, "{raw}");
        }
        // Ordinary text with dots is left alone.
        for raw in ["1.5 seconds", "e.g. this", "wait...", "v2.0", "ok."] {
            assert_eq!(clean_text(raw, false).unwrap(), raw, "{raw}");
        }
    }

    #[test]
    fn invite_codes_and_keys_are_stripped_in_public_rooms_only() {
        assert_eq!(clean_text("code ABC-DEF", true).unwrap(), "code ···");
        assert_eq!(clean_text("code abc-def!", true).unwrap(), "code ···");
        assert_eq!(clean_text("code K7M2PQ", true).unwrap(), "code ···");
        assert_eq!(
            clean_text("key Zx81_kq-93HHaa77bb", true).unwrap(),
            "key ···"
        );
        // Shouting is not a code.
        assert_eq!(clean_text("THANKS GG", true).unwrap(), "THANKS GG");
        // Private: the host's friends may share them.
        assert_eq!(clean_text("code ABC-DEF", false).unwrap(), "code ABC-DEF");
    }

    #[test]
    fn the_rooms_chat_mode_decides_what_goes() {
        assert_eq!(allowed(ChatMode::Off, &Body::Quick(1)), Err(Refusal::Off));
        assert_eq!(
            allowed(ChatMode::Quick, &Body::Text("hi".into())),
            Err(Refusal::QuickOnly)
        );
        assert_eq!(allowed(ChatMode::Quick, &Body::Quick(16)), Ok(()));
        assert_eq!(
            allowed(ChatMode::On, &Body::Quick(0)),
            Err(Refusal::BadQuick)
        );
        assert_eq!(
            allowed(ChatMode::On, &Body::Quick(17)),
            Err(Refusal::BadQuick)
        );
        assert_eq!(allowed(ChatMode::On, &Body::Text("hi".into())), Ok(()));
        let m: ChatMode = serde_json::from_str("\"text\"").unwrap();
        assert_eq!(m, ChatMode::On);
    }

    #[test]
    fn the_rate_limit_lets_a_person_type_and_slows_a_script() {
        let t0 = Instant::now();
        let mut b = Bucket::new(CHAT_BURST, CHAT_PER_S, t0);
        // A quick exchange: five lines at once go.
        for _ in 0..5 {
            assert!(b.take(t0).is_ok());
        }
        // The sixth waits about 3 s, and says so.
        let wait = b.take(t0).unwrap_err();
        assert!(wait > Duration::from_millis(2900) && wait <= Duration::from_secs(3));
        // A person chatting at a line every 3 s is never refused.
        let mut t = t0;
        for _ in 0..40 {
            t += Duration::from_secs(3);
            assert!(b.take(t).is_ok());
        }
        // A script sending 10 a second gets about one in 30 through.
        let mut ok = 0;
        for i in 0..300 {
            if b.take(t + Duration::from_millis(100 * i)).is_ok() {
                ok += 1;
            }
        }
        assert!(ok <= 12, "{ok}");
    }

    #[test]
    fn the_ring_keeps_50_lines_and_folds_repeats() {
        let t0 = Instant::now();
        let mut r = Ring::new(t0);
        let sam = |pid| Speaker {
            pid,
            sid: pid as u8,
            name: "Sam",
            player: Some(2),
            host: false,
        };
        let id = r.push(sam(1), Body::Quick(3), t0).id;
        let l = r.push(sam(1), Body::Quick(3), t0 + Duration::from_secs(1));
        assert_eq!((l.id, l.x), (id, 2));
        assert_eq!(l.json()["x"], 2);
        // Someone else's line in between: a new line.
        r.push(sam(2), Body::Quick(3), t0 + Duration::from_secs(2));
        let id3 = r
            .push(sam(1), Body::Quick(3), t0 + Duration::from_secs(3))
            .id;
        assert_ne!(id3, id);
        // Too late to fold.
        let id4 = r
            .push(sam(1), Body::Quick(3), t0 + Duration::from_secs(40))
            .id;
        assert_ne!(id4, id3);
        for i in 0..100 {
            r.push(sam(1), Body::Text(format!("line {i}")), t0);
        }
        assert_eq!(r.len(), RING);
        let last = r.last(HISTORY_ON_JOIN);
        assert_eq!(last.len(), HISTORY_ON_JOIN);
        assert_eq!(last.last().unwrap()["text"], "line 99");
        assert!(last[0].get("pid").is_none(), "the person id never leaves");
    }
}
