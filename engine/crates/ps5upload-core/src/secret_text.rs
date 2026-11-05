//! Secrets out of text that is kept or shown: log lines, journal events, error messages. An
//! error from a library can quote the URL or the request it was given, and a line can quote what
//! the console said, so the lines themselves are checked where they are written, whatever put
//! them together.
//!
//! This goes by shape: a password in a URL, a secret-named `name=value` or JSON field, a bearer
//! token, the key at the end of an `rtmp://` address. A bare key with no name beside it can't be
//! told from any other hex, so `core_debug!`'s rule stands: never put one in a line.
use std::borrow::Cow;

/// What stands in for a secret, as in Go live's own scrubbing (`golive_ffmpeg::redact`).
const MARK: &str = "••••";

/// This name holds a secret (`stream_key`, `X-Api-Key`, `access_token`, `password`...).
pub fn is_secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "key" | "pass" | "pwd" | "auth" | "sig" | "streamid" | "invite"
    ) || [
        "_key",
        "-key",
        "apikey",
        "token",
        "password",
        "passwd",
        "passcode",
        "secret",
        "credential",
        "signature",
        "authorization",
    ]
    .iter()
    .any(|end| name.ends_with(end))
}

fn in_name(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// The name that ends right before `end`.
fn name_before(text: &str, end: usize) -> &str {
    let b = text.as_bytes();
    let mut start = end;
    while start > 0 && in_name(b[start - 1]) {
        start -= 1;
    }
    &text[start..end]
}

/// `user:password@` out of every URL, and the last part of an `rtmp://` path, which is the key.
fn in_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (before, after) = (&rest[..at], &rest[at + 3..]);
        out.push_str(before);
        out.push_str("://");
        let scheme = name_before(before, before.len()).to_ascii_lowercase();
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
            .unwrap_or(after.len());
        let (url, tail) = after.split_at(end);
        let authority_end = url.find(['/', '?', '#']).unwrap_or(url.len());
        let (authority, path) = url.split_at(authority_end);
        match authority.rfind('@') {
            Some(i) => {
                out.push_str(MARK);
                out.push_str(&authority[i..]);
            }
            None => out.push_str(authority),
        }
        let path_end = path.find(['?', '#']).unwrap_or(path.len());
        let (only_path, query) = path.split_at(path_end);
        match only_path.rfind('/').filter(|i| {
            matches!(scheme.as_str(), "rtmp" | "rtmps") && *i > 0 && only_path.len() > *i + 1
        }) {
            Some(i) => {
                out.push_str(&only_path[..=i]);
                out.push_str(MARK);
            }
            None => out.push_str(only_path),
        }
        out.push_str(query);
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// Where a `name=value` value ends, from its first byte. A quoted one goes to its closing quote.
fn value_end(text: &str, start: usize) -> usize {
    let b = text.as_bytes();
    if let Some(q @ (b'"' | b'\'')) = b.get(start).copied() {
        if let Some(n) = text[start + 1..].find(q as char) {
            return start + 1 + n + 1;
        }
    }
    let mut i = start;
    while i < b.len() {
        match b[i] {
            b' ' | b'\t' | b'\r' | b'\n' | b'&' | b',' | b';' | b'"' | b'\'' | b')' | b']'
            | b'}' | b'<' | b'>' => break,
            _ => i += 1,
        }
    }
    i
}

/// A value that says something about a secret without being one (`password=true`).
fn says_nothing(value: &str) -> bool {
    value.is_empty()
        || ["true", "false", "null", "none", "yes", "no", "some"]
            .iter()
            .any(|w| value.eq_ignore_ascii_case(w))
        || value == MARK
}

/// After a JSON field's closing quote at `quote` (with its backslash when the JSON is itself
/// inside a string): where its string value's text starts and ends.
fn json_value(text: &str, quote: usize) -> Option<(usize, usize)> {
    let b = text.as_bytes();
    let escaped = quote > 0 && b[quote - 1] == b'\\';
    let mut i = quote + 1;
    while b.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    if b.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    while b.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    if escaped {
        if b.get(i) != Some(&b'\\') || b.get(i + 1) != Some(&b'"') {
            return None;
        }
        i += 2;
    } else if b.get(i) != Some(&b'"') {
        return None;
    } else {
        i += 1;
    }
    let start = i;
    while i < b.len() {
        match b[i] {
            b'\\' if escaped => return Some((start, i)),
            b'\\' => i += 2,
            b'"' => return Some((start, i)),
            _ => i += 1,
        }
    }
    Some((start, b.len()))
}

/// `name=value`, `"name":"value"` and `Bearer value`, for a secret name.
fn in_fields(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut i) = (0, 0);
    while i < b.len() {
        let found = match b[i] {
            b'=' if is_secret_name(name_before(text, i)) => {
                let end = value_end(text, i + 1);
                (!says_nothing(text[i + 1..end].trim_matches(['"', '\'']))).then_some((i + 1, end))
            }
            b'"' => {
                let name_end = if i > 0 && b[i - 1] == b'\\' { i - 1 } else { i };
                if is_secret_name(name_before(text, name_end)) {
                    json_value(text, i).filter(|(s, e)| !says_nothing(&text[*s..*e]))
                } else {
                    None
                }
            }
            // A struct printed with `{:?}`: `regist_key: "abc"`.
            b':' if b.get(i + 1) == Some(&b' ')
                && b.get(i + 2) == Some(&b'"')
                && is_secret_name(name_before(text, i)) =>
            {
                let end = value_end(text, i + 2);
                (!says_nothing(text[i + 2..end].trim_matches('"'))).then_some((i + 2, end))
            }
            // Long enough to be a token, not "the bearer of bad news".
            b' ' if name_before(text, i).eq_ignore_ascii_case("bearer") => {
                let end = value_end(text, i + 1);
                (end - (i + 1) >= 8).then_some((i + 1, end))
            }
            _ => None,
        };
        match found {
            Some((start, end)) => {
                out.push_str(&text[copied..start]);
                out.push_str(MARK);
                copied = end;
                i = end.max(i + 1);
            }
            None => i += 1,
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// `text` with what looks like a secret replaced. Borrowed when there is nothing to replace, which
/// is nearly always: this runs on every log line.
pub fn without_secrets(text: &str) -> Cow<'_, str> {
    let cleaned = in_fields(&in_urls(text));
    if cleaned == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(cleaned)
    }
}

/// [`without_secrets`] on every string of a JSON value, and a secret-named string field goes
/// whatever it holds.
pub fn value_without_secrets(value: &mut serde_json::Value) -> bool {
    use serde_json::Value;
    let mut any = false;
    match value {
        Value::String(text) => {
            if let Cow::Owned(clean) = without_secrets(text) {
                *text = clean;
                any = true;
            }
        }
        Value::Array(items) => {
            for v in items {
                any |= value_without_secrets(v);
            }
        }
        Value::Object(map) => {
            for (name, v) in map.iter_mut() {
                if is_secret_name(name) && v.is_string() {
                    if v.as_str().is_some_and(|s| !says_nothing(s)) {
                        *v = Value::String(MARK.to_string());
                        any = true;
                    }
                } else {
                    any |= value_without_secrets(v);
                }
            }
        }
        _ => {}
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = "hunter2-abcdef";

    fn gone(text: &str) -> String {
        let out = without_secrets(text).into_owned();
        assert!(!out.contains(S), "{out}");
        assert!(out.contains(MARK), "{out}");
        out
    }

    #[test]
    fn passwords_in_urls() {
        assert_eq!(
            gone(&format!("fetch https://me:{S}@nas.local:8080/a.pkg failed")),
            "fetch https://••••@nas.local:8080/a.pkg failed"
        );
        gone(&format!("smb://{S}@nas/share"));
        gone(&format!("\"ftp://u:{S}@h/\""));
    }

    #[test]
    fn query_and_named_fields() {
        for name in [
            "key",
            "stream_key",
            "X-Api-Key",
            "token",
            "access_token",
            "password",
            "PASSWD",
            "credential",
            "wake_credential",
            "regist_key",
            "rp_key",
            "passcode",
            "client_secret",
            "X-Amz-Signature",
            "streamid",
        ] {
            let out = gone(&format!("GET /a?x=1&{name}={S}&y=2 -> 500"));
            assert!(out.contains("x=1&") && out.contains("&y=2 -> 500"), "{out}");
            gone(&format!(r#"{{"{name}":"{S}","ok":true}}"#));
            gone(&format!(r#"{{"{name}": "{S}"}}"#));
            gone(&format!(r#"body: "{{\"{name}\":\"{S}\"}}""#));
            gone(&format!(r#"{name}="{S} and more""#));
        }
    }

    #[test]
    fn a_struct_printed_for_debugging() {
        let out = gone(&format!(
            r#"Registration {{ regist_key: "{S}", rp_key: "{S}", n: 1 }}"#
        ));
        assert!(out.ends_with(", n: 1 }"), "{out}");
    }

    #[test]
    fn a_json_value_with_a_quote_in_it_goes_whole() {
        let out = gone(&format!(r#"{{"password":"a\"{S}","ok":1}}"#));
        assert_eq!(out, r#"{"password":"••••","ok":1}"#);
    }

    #[test]
    fn bearer_tokens_and_rtmp_keys() {
        gone(&format!("Authorization: Bearer {S}"));
        assert_eq!(
            gone(&format!("connecting rtmps://live.example/app/{S} now")),
            "connecting rtmps://live.example/app/•••• now"
        );
        // Nothing after the app name: no key to take.
        assert!(matches!(
            without_secrets("rtmp://live.example/app"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn ordinary_lines_stay_as_they_are() {
        for line in [
            "fs_extract: addr=10.0.0.2 path=/a password=true op_id=4",
            "password=false key=",
            "GET /api/jobs -> 200 (3ms)",
            "monkey=banana keyboard=open tokens=3 authority=x",
            "https://example.com/a/b/c?x=1#y",
            "http://[fe80::1]:80/a",
            "「日本語」 = 値, ключ=значение",
            "the bearer of bad news",
            "a = b",
            "trailing=",
            "=",
            "\"",
            "://",
        ] {
            assert!(
                matches!(without_secrets(line), Cow::Borrowed(_)),
                "{line} -> {}",
                without_secrets(line)
            );
        }
    }

    #[test]
    fn stops_safely_inside_other_alphabets() {
        let out = gone(&format!("鍵 token={S}。 続き"));
        assert!(out.starts_with("鍵 token=••••"), "{out}");
        gone(&format!("日本語 Bearer {S}"));
    }

    #[test]
    fn json_values_go_by_name_and_by_text() {
        let mut v = serde_json::json!({
            "error": format!("GET https://u:{S}@h/x failed"),
            "password": S,
            "has_password": true,
            "nested": [{"note": format!("token={S}")}],
            "count": 3,
            "plain": "fine",
        });
        assert!(value_without_secrets(&mut v));
        let text = v.to_string();
        assert!(!text.contains(S), "{text}");
        assert_eq!(v["has_password"], true);
        assert_eq!(v["plain"], "fine");
        assert_eq!(v["count"], 3);
        assert!(!value_without_secrets(&mut v));
    }
}
