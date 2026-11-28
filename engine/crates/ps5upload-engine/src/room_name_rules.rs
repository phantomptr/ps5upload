//! Room and host names on public cards: cleaned, then checked (Play Rooms, join_room.rs).
//!
//! The same rules as the hub's `ps5upload-hub` `names.rs` (rooms-hub branch), kept as a copy until
//! that crate exports them: the engine checks a name before it ever reaches the hub, so a host is
//! told "Pick another name" at once, and guest names in a room follow the same rules.
//!
//! Cleaning drops invisible format characters (zero-width, bidirectional overrides and
//! isolates, soft hyphen, BOM, variation selectors), turns any whitespace into one space and
//! trims. Checking allows letters and digits of any script, spaces and a little punctuation (no
//! `@`, `/`, emoji or symbols), and refuses links, invite-code-looking tokens, long digit runs,
//! repeated-character spam, a small multilingual word list and the reserved words. The host
//! engine runs its own filter first; the hub is the authority for what a public card shows.

/// Why a name was refused (the `ad_refused` reason is always `name`; this is for tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameError {
    Length,
    Chars,
    Link,
    Code,
    Spam,
    Word,
    Reserved,
}

/// Punctuation a name may use.
const PUNCT: &[char] = &[
    '-', '_', '.', ',', '!', '?', '\'', '’', '&', '(', ')', '#', '+', ':', '*', '~',
];

fn invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}'
        | '\u{180B}'..='\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}'
        | '\u{FFA0}' | '\u{FFF0}'..='\u{FFFB}' | '\u{E0000}'..='\u{E007F}')
}

/// Scripts whose vowel signs and marks are not `Alphabetic` in every case (Indic, Thai, Lao,
/// Myanmar, Khmer): their whole blocks count as letters.
fn script_mark(c: char) -> bool {
    matches!(c, '\u{0900}'..='\u{0DFF}' | '\u{0E00}'..='\u{0EFF}' | '\u{1000}'..='\u{109F}'
        | '\u{1780}'..='\u{17FF}')
}

/// Latin combining diacritics: allowed, two in a row at most (no "zalgo" stacks).
fn combining(c: char) -> bool {
    matches!(c, '\u{0300}'..='\u{036F}')
}

fn allowed(c: char) -> bool {
    c == ' ' || c.is_alphanumeric() || script_mark(c) || combining(c) || PUNCT.contains(&c)
}

/// The cleaned name, or why it is refused. `min..=max` counts characters. `extra_words` are
/// words added at run time (the control file's `word` lines), matched as whole words.
pub(crate) fn clean_name(
    raw: &str,
    min: usize,
    max: usize,
    extra_words: &[String],
) -> Result<String, NameError> {
    // Bound the work before anything else: a name is short.
    if raw.len() > max * 8 + 64 {
        return Err(NameError::Length);
    }
    let mut out = String::with_capacity(raw.len());
    let mut space = false;
    for c in raw.chars() {
        if invisible(c) {
            continue;
        }
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    let n = out.chars().count();
    if n < min || n > max {
        return Err(NameError::Length);
    }
    if !out.chars().all(allowed) {
        return Err(NameError::Chars);
    }
    check_spam(&out)?;
    let lower = out.to_lowercase();
    if is_link(&lower) {
        return Err(NameError::Link);
    }
    if looks_like_code(&out) {
        return Err(NameError::Code);
    }
    check_words(&lower, extra_words)?;
    Ok(out)
}

fn check_spam(s: &str) -> Result<(), NameError> {
    let mut run = 0;
    let mut marks = 0;
    let mut prev = None;
    let mut punct = 0;
    let mut total = 0;
    for c in s.chars() {
        total += 1;
        if combining(c) {
            marks += 1;
            if marks > 2 {
                return Err(NameError::Spam);
            }
        } else {
            marks = 0;
        }
        if PUNCT.contains(&c) {
            punct += 1;
        }
        run = if Some(c) == prev { run + 1 } else { 1 };
        prev = Some(c);
        if run >= 5 {
            return Err(NameError::Spam);
        }
    }
    // Mostly punctuation ("!!??!!..") is not a name.
    if punct * 2 > total {
        return Err(NameError::Spam);
    }
    Ok(())
}

/// TLDs that make "word.tld" a link even without a scheme.
const TLDS: &[&str] = &[
    "com", "net", "org", "gg", "io", "me", "ly", "tv", "xyz", "co", "app", "link", "site",
    "online", "info", "biz", "ru", "de", "uk", "fr", "es", "it", "br", "jp", "cn", "in", "us",
    "eu", "to", "cc", "ws", "club", "live", "shop", "store", "top", "icu", "fun", "pro", "dev",
    "page",
];

/// Contact or invite services: a name that names one is an advert.
const SERVICES: &[&str] = &[
    "discord", "telegram", "whatsapp", "wechat", "snapchat", "onlyfans", "t.me", "wa.me", "bit.ly",
];

fn is_link(lower: &str) -> bool {
    if lower.contains("://") || lower.contains("www") || lower.contains("http") {
        return true;
    }
    if SERVICES.iter().any(|s| lower.contains(s)) {
        return true;
    }
    // "word.tld" or "word . tld" with a known TLD (spaces next to a dot dropped).
    let chars: Vec<char> = lower.chars().collect();
    let squashed: String = chars
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            **c != ' '
                || !(chars.get(i + 1) == Some(&'.') || (*i > 0 && chars.get(i - 1) == Some(&'.')))
        })
        .map(|(_, c)| *c)
        .collect();
    for (i, _) in squashed.match_indices('.') {
        let before = squashed[..i].chars().next_back();
        let after: String = squashed[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        if before.is_some_and(|c| c.is_alphanumeric()) && TLDS.contains(&after.as_str()) {
            return true;
        }
    }
    false
}

/// An invite code or a contact number in a name: a mixed letter/digit token that switches
/// between letters and digits four times or more ("K7P2Q9"; game titles like "NBA2K25" switch
/// three times), or six digits in a row.
fn looks_like_code(s: &str) -> bool {
    for token in s.split(|c: char| !c.is_ascii_alphanumeric()) {
        if token.len() < 5 {
            continue;
        }
        let mut switches = 0;
        let mut digits_run = 0;
        let mut prev_digit = None;
        for c in token.chars() {
            let d = c.is_ascii_digit();
            if prev_digit.is_some_and(|p| p != d) {
                switches += 1;
            }
            prev_digit = Some(d);
            digits_run = if d { digits_run + 1 } else { 0 };
            if digits_run >= 6 {
                return true;
            }
        }
        if switches >= 4 {
            return true;
        }
    }
    false
}

/// Matched anywhere in the squashed name (letters only, look-alike digits mapped, runs of three
/// or more of one letter collapsed): long enough that innocent words do not contain them.
const WORDS_ANYWHERE: &[&str] = &[
    // English
    "fuck",
    "nigger",
    "nigga",
    "faggot",
    "whore",
    "cocksuck",
    "pedophile",
    "paedophile",
    // Spanish / Portuguese / Italian / French / German
    "mierda",
    "pendejo",
    "maricon",
    "caralho",
    "buceta",
    "vaffanculo",
    "stronzo",
    "cazzo",
    "putain",
    "salope",
    "connard",
    "encule",
    "hurensohn",
    "wichser",
    "fotze",
    "schlampe",
    // Polish / Turkish / Hindi-Urdu (romanised) / Indonesian
    "kurwa",
    "pierdol",
    "orospu",
    "siktir",
    "madarchod",
    "bhenchod",
    "chutiya",
    "kontol",
    "memek",
    // Russian / Ukrainian
    "хуй",
    "пизд",
    "ебат",
    "ебан",
    "бляд",
    "блять",
    "пидор",
    // Chinese / Japanese / Korean
    "操你",
    "傻逼",
    "肏",
    "屄",
    "まんこ",
    "ちんこ",
    "씨발",
    "병신",
    // Arabic / Persian / Thai
    "شرموط",
    "جنده",
    "เหี้ย",
    // Hate
    "hitler",
    "1488",
];

/// Matched only as a whole word (short, or inside innocent words otherwise: "therapist",
/// "spice", "torpedo", "disputa", "Scunthorpe", "đụng").
const WORDS_WHOLE: &[&str] = &[
    "ass",
    "cunt",
    "dick",
    "cock",
    "fag",
    "nazi",
    "slut",
    "bitch",
    "sex",
    "porn",
    "xxx",
    "cum",
    "tits",
    "shit",
    "rape",
    "rapist",
    "pedo",
    "retard",
    "spic",
    "kike",
    "chink",
    "tranny",
    "heil",
    "puta",
    "puto",
    "porra",
    "coño",
    "merde",
    "nique",
    "fick",
    "arsch",
    "chuj",
    "amk",
    "anjing",
    "bangsat",
    "сука",
    "كس",
    "کیر",
    "ควย",
    "địt",
    "đụ",
    "좆",
];

/// Reserved: nobody's room may look official. The first two anywhere, the rest as words.
const RESERVED_ANYWHERE: &[&str] = &["ps5upload", "phantomptr"];
const RESERVED_WHOLE: &[&str] = &[
    "official",
    "admin",
    "administrator",
    "support",
    "moderator",
    "staff",
];

/// Lowercase letters only, look-alike digits and symbols mapped to the letters they imitate,
/// runs of three or more of one letter collapsed to one ("F u u u c k", "f.u.c.k" and "fuuuck"
/// all squash to "fuck"; "Niger" stays "niger", never "nigger").
fn squash(lower: &str) -> String {
    let mapped: Vec<char> = lower
        .chars()
        .map(|c| match c {
            '0' => 'o',
            '1' | '!' => 'i',
            '3' => 'e',
            '4' => 'a',
            '5' | '$' => 's',
            '7' => 't',
            '8' => 'b',
            c => c,
        })
        .filter(|c| c.is_alphanumeric() || script_mark(*c))
        .collect();
    let mut s = String::with_capacity(lower.len());
    let mut i = 0;
    while i < mapped.len() {
        let c = mapped[i];
        let mut j = i;
        while j < mapped.len() && mapped[j] == c {
            j += 1;
        }
        let n = if j - i >= 3 { 1 } else { j - i };
        for _ in 0..n {
            s.push(c);
        }
        i = j;
    }
    s
}

fn check_words(lower: &str, extra: &[String]) -> Result<(), NameError> {
    let squashed = squash(lower);
    // "1488" stays digits in the plain text.
    let digits: String = lower.chars().filter(|c| c.is_ascii_digit()).collect();
    for w in RESERVED_ANYWHERE {
        if squashed.contains(&squash(w)) {
            return Err(NameError::Reserved);
        }
    }
    // Words, not numbers: "Room 455" is a number, never "ass".
    let tokens: Vec<String> = lower
        .split(|c: char| !(c.is_alphanumeric() || script_mark(c) || c == '$' || c == '!'))
        .filter(|t| !t.is_empty() && !t.chars().all(|c| c.is_ascii_digit()))
        .map(squash)
        .collect();
    for w in RESERVED_WHOLE {
        let w = squash(w);
        if tokens.contains(&w) {
            return Err(NameError::Reserved);
        }
    }
    for w in WORDS_ANYWHERE {
        if w.chars().all(|c| c.is_ascii_digit()) {
            if digits.contains(w) {
                return Err(NameError::Word);
            }
            continue;
        }
        if squashed.contains(&squash(w)) {
            return Err(NameError::Word);
        }
    }
    let extra = extra.iter().map(|w| w.to_lowercase());
    for w in WORDS_WHOLE.iter().map(|w| w.to_string()).chain(extra) {
        let w = squash(&w);
        if !w.is_empty() && tokens.contains(&w) {
            return Err(NameError::Word);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(s: &str) -> Result<String, NameError> {
        clean_name(s, 3, 32, &[])
    }

    #[test]
    fn good_names_pass_in_many_languages() {
        for ok in [
            "Couch chaos",
            "Sam's room",
            "It Takes Two (co-op)",
            "NBA2K25 night",
            "FIFA 23 cup!",
            "Café crème",
            "Ночные гонки",
            "週末の協力プレイ",
            "周末一起玩",
            "주말 협동",
            "خانه بازی",
            "عکس های بازی",
            "हम साथ खेलें",
            "เล่นด้วยกัน",
            "Mr. Bean fans",
            "Scunthorpe United",
            "Badminton club",
            "Assassin's Creed",
            "Class of 2024",
            "Therapist meetup",
            "Spice girls",
            "Torpedo run",
            "Reputación alta",
            "đụng độ",
            "Niger fans",
            "Couch night 455",
            "Room 455 and 5",
        ] {
            assert!(room(ok).is_ok(), "{ok}: {:?}", room(ok));
        }
    }

    #[test]
    fn cleaning_drops_invisibles_and_extra_space() {
        assert_eq!(room("  Couch \t  chaos \n").unwrap(), "Couch chaos");
        assert_eq!(room("Co\u{200B}uch\u{202E} chaos").unwrap(), "Couch chaos");
        // Invisibles cannot hide a word either.
        assert_eq!(room("f\u{200B}u\u{200D}ck it"), Err(NameError::Word));
        assert_eq!(room("\u{200B}\u{200B}ab"), Err(NameError::Length));
    }

    #[test]
    fn bad_names_are_refused_for_the_right_reason() {
        let long = "x".repeat(33);
        for (bad, why) in [
            ("ab", NameError::Length),
            (long.as_str(), NameError::Length),
            ("mail me @sam", NameError::Chars),
            ("go to x/y", NameError::Chars),
            ("Party 🎉🎉🎉", NameError::Chars),
            ("<script>", NameError::Chars),
            ("visit https x", NameError::Link),
            ("join www site", NameError::Link),
            ("ps5games.com night", NameError::Link),
            ("rooms . gg here", NameError::Link),
            ("our discord crew", NameError::Link),
            ("code K7P2Q9", NameError::Code),
            ("call 5551234567", NameError::Code),
            ("Wooooooo", NameError::Spam),
            ("!!??!!", NameError::Spam),
            ("a\u{0301}\u{0301}\u{0301}bc", NameError::Spam),
            ("FUCK this", NameError::Word),
            ("f u c k", NameError::Word),
            ("f.u.c.k off", NameError::Word),
            ("fuuuck", NameError::Word),
            ("sh1t ass room", NameError::Word),
            ("Hurensohn", NameError::Word),
            ("сука блять", NameError::Word),
            ("傻逼房间", NameError::Word),
            ("Hitler youth", NameError::Word),
            ("PS5Upload night", NameError::Reserved),
            ("Official room", NameError::Reserved),
            ("Admin room", NameError::Reserved),
            ("support desk", NameError::Reserved),
            ("p h a n t o m p t r", NameError::Reserved),
        ] {
            assert_eq!(room(bad), Err(why), "{bad}");
        }
        // Words added at run time (control file `word ...`).
        assert_eq!(
            clean_name("big meanie", 3, 32, &["Meanie".into()]),
            Err(NameError::Word)
        );
        // Host names: 1..=24.
        assert!(clean_name("Sam", 1, 24, &[]).is_ok());
        assert_eq!(clean_name("", 1, 24, &[]), Err(NameError::Length));
    }
}
