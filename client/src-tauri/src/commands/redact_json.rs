//! Replacing the value of a named key in JSON text, for report redaction (bug_report.rs).
//!
//! Works on text, not parsed JSON: a log is a mix of JSON lines, half lines and plain words. Each
//! `"key"` is found where it stands, never by pairing quotes from the start of the text: one
//! escaped quote earlier in a line (`"a 5\" drive"`) used to shift the pairing and leave every
//! secret after it in place.
//!
//! No dependencies, so this file can be compiled and tested by itself. The same rules are in
//! client/src/lib/redaction.ts (`replaceJsonValues`), and both run redaction.vectors.json.

/// At `at`: optional ASCII whitespace, `:`, optional whitespace. The index after them.
fn after_colon(b: &[u8], mut at: usize) -> Option<usize> {
    while at < b.len() && b[at].is_ascii_whitespace() {
        at += 1;
    }
    if at >= b.len() || b[at] != b':' {
        return None;
    }
    at += 1;
    while at < b.len() && b[at].is_ascii_whitespace() {
        at += 1;
    }
    Some(at)
}

/// `key` (any letter case) at `at`. The index after it.
fn key_at(b: &[u8], at: usize, keys: &[&str]) -> Option<usize> {
    keys.iter().find_map(|k| {
        let end = at.checked_add(k.len())?;
        (end <= b.len() && b[at..end].eq_ignore_ascii_case(k.as_bytes())).then_some(end)
    })
}

/// Where a JSON string value that starts at `from` ends: its first quote that no backslash
/// escapes (`\"` stays inside, `\\"` ends it). A value cut short ends at the line's end.
fn plain_value_end(b: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'"' | b'\n' => return i,
            b'\\' => i += 2,
            _ => i += 1,
        }
    }
    b.len()
}

/// The same for a value inside JSON that is itself stored in a JSON string, where the inner
/// text's `"` reads `\"` and its `\` reads `\\`: the value ends at the first `\"` that the inner
/// text does not escape (`\\\"` stays inside, `\\\\\"` ends it). A bare `"` ends the outer
/// string, and so the value; a value cut short ends at the line's end.
fn escaped_value_end(b: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'"' | b'\n' => return i,
            b'\\' => match b.get(i + 1) {
                // The inner text's closing quote.
                Some(b'"') => return i,
                // One backslash of the inner text: it escapes what follows there.
                Some(b'\\') => match (b.get(i + 2), b.get(i + 3)) {
                    (Some(b'\\'), Some(b'\\' | b'"')) => i += 4,
                    (Some(b'"' | b'\n'), _) => return i + 2,
                    (Some(_), _) => i += 3,
                    (None, _) => return b.len(),
                },
                // An escape of the outer string (`\n`, `é`).
                _ => i += 2,
            },
            _ => i += 1,
        }
    }
    b.len()
}

fn replace_values(
    s: &str,
    keys: &[&str],
    with: &str,
    quote: &[u8],
    value_end: fn(&[u8], usize) -> usize,
) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut cursor = 0;
    let mut i = 0;
    while i < b.len() {
        let value = b[i..]
            .starts_with(quote)
            .then(|| key_at(b, i + quote.len(), keys))
            .flatten()
            .filter(|end| b[*end..].starts_with(quote))
            .and_then(|end| after_colon(b, end + quote.len()))
            .filter(|at| b[*at..].starts_with(quote))
            .map(|at| at + quote.len());
        let Some(start) = value else {
            i += 1;
            continue;
        };
        // Every index here sits on an ASCII byte (a quote, a backslash, a line end) or the
        // text's end, so the slices fall on character boundaries.
        let end = value_end(b, start).min(b.len());
        out.push_str(&s[cursor..start]);
        out.push_str(with);
        cursor = end;
        i = end.max(i + 1);
    }
    out.push_str(&s[cursor..]);
    out
}

/// `"key": "value"` with `key` in `keys` (case-insensitive): the value becomes `with`.
pub(crate) fn replace_json_values(s: &str, keys: &[&str], with: &str) -> String {
    replace_values(s, keys, with, b"\"", plain_value_end)
}

/// The same as [`replace_json_values`] for JSON stored inside a JSON string (localStorage
/// values): `\"key\": \"value\"`.
pub(crate) fn replace_escaped_json_values(s: &str, keys: &[&str], with: &str) -> String {
    replace_values(s, keys, with, b"\\\"", escaped_value_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: &[&str] = &["token", "password"];

    fn plain(s: &str) -> String {
        replace_json_values(s, KEYS, "<removed>")
    }

    fn escaped(s: &str) -> String {
        replace_escaped_json_values(s, KEYS, "<removed>")
    }

    #[test]
    fn an_escaped_quote_earlier_in_the_line_does_not_hide_a_secret() {
        assert_eq!(
            plain(r#"{"message":"a 5\" drive","detail":{"token":"SECRET2"}}"#),
            r#"{"message":"a 5\" drive","detail":{"token":"<removed>"}}"#
        );
        // An odd number of quotes before the key, in plain words.
        assert_eq!(
            plain(r#"it said "no, "token": "abc""#),
            r#"it said "no, "token": "<removed>""#
        );
    }

    #[test]
    fn a_value_ends_at_its_first_unescaped_quote() {
        assert_eq!(
            plain(r#"{"token":"ab\"cd","x":"y"}"#),
            r#"{"token":"<removed>","x":"y"}"#
        );
        // `\\"`: an escaped backslash, then the closing quote.
        assert_eq!(
            plain(r#"{"token":"ab\\","x":"y"}"#),
            r#"{"token":"<removed>","x":"y"}"#
        );
        assert_eq!(
            plain(r#"{"token":"","x":"y"}"#),
            r#"{"token":"<removed>","x":"y"}"#
        );
    }

    #[test]
    fn keys_match_whole_in_any_case_and_only_string_values() {
        assert_eq!(plain(r#""TOKEN" : "a""#), r#""TOKEN" : "<removed>""#);
        assert_eq!(plain(r#""token_count":"7""#), r#""token_count":"7""#);
        assert_eq!(plain(r#""my token":"7""#), r#""my token":"7""#);
        assert_eq!(plain(r#""token": 7, "a":"b""#), r#""token": 7, "a":"b""#);
        assert_eq!(
            plain(r#""token":"a" and "password":"b""#),
            r#""token":"<removed>" and "password":"<removed>""#
        );
    }

    #[test]
    fn a_value_cut_short_goes_to_the_end_of_its_line() {
        assert_eq!(plain("\"token\":\"abc"), "\"token\":\"<removed>");
        assert_eq!(
            plain("\"token\":\"abc\nnext \"token\":\"d\""),
            "\"token\":\"<removed>\nnext \"token\":\"<removed>\""
        );
    }

    #[test]
    fn text_around_it_keeps_its_characters() {
        assert_eq!(
            plain("é \"token\":\"páss\\\"wörd\" 多"),
            "é \"token\":\"<removed>\" 多"
        );
        assert_eq!(
            escaped("é \\\"token\\\":\\\"p\\\\é\\\\\\\"x\\\" 多"),
            "é \\\"token\\\":\\\"<removed>\\\" 多"
        );
    }

    #[test]
    fn escaped_json_with_a_quote_inside_an_earlier_value() {
        // {"note":"a 5\" drive","token":"SECRET"} stored inside a JSON string.
        assert_eq!(
            escaped(r#""k": "{\"note\":\"a 5\\\" drive\",\"token\":\"SECRET\"}""#),
            r#""k": "{\"note\":\"a 5\\\" drive\",\"token\":\"<removed>\"}""#
        );
    }

    #[test]
    fn an_escaped_value_ends_at_its_first_unescaped_quote() {
        // The value is  ab\"cd  in the inner text.
        assert_eq!(
            escaped(r#"{\"token\":\"ab\\\"cd\",\"x\":\"y\"}"#),
            r#"{\"token\":\"<removed>\",\"x\":\"y\"}"#
        );
        // The value is  ab\\  in the inner text: an escaped backslash, then the closing quote.
        assert_eq!(
            escaped(r#"{\"token\":\"ab\\\\\",\"x\":\"y\"}"#),
            r#"{\"token\":\"<removed>\",\"x\":\"y\"}"#
        );
        // An escape that is not a quote (\n in the inner text).
        assert_eq!(
            escaped(r#"{\"token\":\"a\\nb\",\"x\":\"y\"}"#),
            r#"{\"token\":\"<removed>\",\"x\":\"y\"}"#
        );
        assert_eq!(
            escaped(r#"\"Password\" : \"a\" \"token_x\":\"b\""#),
            r#"\"Password\" : \"<removed>\" \"token_x\":\"b\""#
        );
    }

    #[test]
    fn an_escaped_value_stops_where_the_outer_string_does() {
        assert_eq!(
            escaped(r#""k": "{\"token\":\"abc", "n": 1"#),
            r#""k": "{\"token\":\"<removed>", "n": 1"#
        );
        assert_eq!(escaped(r#"\"token\":\"abc"#), r#"\"token\":\"<removed>"#);
    }

    #[test]
    fn the_plain_form_leaves_the_escaped_one_alone_and_back() {
        let both = r#"{"token":"a","store":"{\"token\":\"b\"}"}"#;
        assert_eq!(
            plain(both),
            r#"{"token":"<removed>","store":"{\"token\":\"b\"}"}"#
        );
        assert_eq!(
            escaped(both),
            r#"{"token":"a","store":"{\"token\":\"<removed>\"}"}"#
        );
    }
}
