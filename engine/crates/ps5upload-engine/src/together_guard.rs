//! The "friends playing" guard: one check for every engine route that would end friends' play.
//!
//! While friends are in through Play Together (`join::friends_playing`), a request that would cut
//! them off (stopping the Remote Play session, putting the console in rest mode, restarting or
//! turning it off, closing or killing the game, launching another one, replacing or stopping the
//! helper that runs their controllers) is answered 409:
//!
//! ```json
//! {"error": "friends_playing", "friends": ["Mia", "Lee"], "console": "192.168.1.20",
//!  "message": "Friends are playing on this PS5. Stop anyway?"}
//! ```
//!
//! and goes through when the caller confirms, any one of: the header `X-Confirm-Friends: 1`, the
//! query `confirm_friends=1`, or `"confirm_friends": true` in the JSON body of a route that reads
//! one. `console` is null for a request about no one console (all of them). With nobody in, nothing
//! changes. The engine's own work (a session ending on its own, a helper check) is never asked.

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

/// The confirming header.
pub(crate) const HEADER: &str = "x-confirm-friends";

/// What the person is asked.
pub(crate) const MESSAGE: &str = "Friends are playing on this PS5. Stop anyway?";

/// The refusal's `error`.
pub(crate) const CODE: &str = "friends_playing";

/// A confirming value: `1`, `true` or `yes` (any case).
pub(crate) fn says_yes(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes")
}

/// The request carries `X-Confirm-Friends: 1`.
pub(crate) fn header_confirms(headers: &HeaderMap) -> bool {
    headers
        .get(HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(says_yes)
}

/// `?confirm_friends=1`, for a route that reads nothing else from its query.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ConfirmQuery {
    /// `1` (or `true`): stop even though friends are playing.
    #[serde(default)]
    confirm_friends: Option<String>,
}

impl ConfirmQuery {
    pub(crate) fn yes(&self) -> bool {
        self.confirm_friends.as_deref().is_some_and(says_yes)
    }

    /// `?confirm_friends=1` (`yes`) or no query, for tests.
    #[cfg(test)]
    pub(crate) fn given(yes: bool) -> Self {
        Self {
            confirm_friends: yes.then(|| "1".into()),
        }
    }
}

/// `confirm_friends` in a body or a query: `true`, `1` or `"1"`/`"true"`/`"yes"` confirm; anything
/// else (or nothing) does not. Never a reason to refuse the request as unreadable.
pub(crate) fn de_confirm<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::Number(n) => n.as_u64() == Some(1),
        Value::String(s) => says_yes(&s),
        _ => false,
    })
}

/// Who is in on `console` (None: on every console).
pub(crate) fn friends(console: Option<&str>) -> Vec<String> {
    match console {
        Some(c) => crate::join::friends_playing(c),
        None => crate::join::friends_playing_any()
            .into_iter()
            .flat_map(|(_, names)| names)
            .collect(),
    }
}

/// The 409's body.
pub(crate) fn refusal(console: Option<&str>, friends: &[String]) -> Value {
    json!({
        "error": CODE,
        "code": CODE,
        "friends": friends,
        "console": console,
        "message": MESSAGE,
    })
}

/// 409 `friends_playing` when friends are in on `console` (None: on any console) and the caller did
/// not confirm (`query_confirm`: the query's or the body's `confirm_friends`, or the header).
pub(crate) fn check(
    console: Option<&str>,
    headers: &HeaderMap,
    query_confirm: bool,
) -> Option<Response> {
    let console = console.map(str::trim).filter(|c| !c.is_empty());
    let friends = friends(console);
    if friends.is_empty() {
        return None;
    }
    if query_confirm || header_confirms(headers) {
        crate::log_info!(
            "together: {} friend(s) playing on {}: the caller confirmed",
            friends.len(),
            console.unwrap_or("a console")
        );
        return None;
    }
    crate::log_info!(
        "together: {} friend(s) playing on {}: asked to confirm first",
        friends.len(),
        console.unwrap_or("a console")
    );
    Some((StatusCode::CONFLICT, Json(refusal(console, &friends))).into_response())
}

/// What the Remote Play page is told instead of stopping (`{"stop": true}` with friends in and no
/// `"confirm_friends": true`).
pub(crate) fn socket_ask(friends: &[String]) -> Value {
    json!({
        "stage": "confirm_friends",
        "friends": friends,
        "message": MESSAGE,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::http::HeaderValue;

    /// The body of a guard's answer.
    pub(crate) async fn body_of(r: Response) -> (StatusCode, Value) {
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    pub(crate) fn confirm_header() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(HEADER, HeaderValue::from_static("1"));
        h
    }

    #[tokio::test]
    async fn friends_in_ask_first_and_any_confirm_lets_it_through() {
        let console = "10.0.91.1";
        assert!(check(Some(console), &HeaderMap::new(), false).is_none());
        crate::join::test_party_with_guest(console, "Mia");
        let r = check(Some(console), &HeaderMap::new(), false).expect("asked");
        let (status, v) = body_of(r).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(v["error"], "friends_playing");
        assert_eq!(v["friends"], json!(["Mia"]));
        assert_eq!(v["console"], console);
        assert_eq!(v["message"], MESSAGE);
        assert!(check(Some(console), &confirm_header(), false).is_none());
        assert!(check(Some(console), &HeaderMap::new(), true).is_none());
        // Another console: nobody there.
        assert!(check(Some("10.0.91.2"), &HeaderMap::new(), false).is_none());
        // Any console: this one counts, and `console` is null.
        let (_, v) = body_of(check(None, &HeaderMap::new(), false).unwrap()).await;
        assert!(v["console"].is_null());
        assert!(v["friends"].as_array().unwrap().contains(&json!("Mia")));
        crate::join::test_close_party(console);
        assert!(check(Some(console), &HeaderMap::new(), false).is_none());
    }

    #[test]
    fn a_nameless_guest_is_a_friend() {
        let console = "10.0.91.3";
        crate::join::test_party_with_guest(console, "  ");
        assert_eq!(crate::join::friends_playing(console), vec!["Friend"]);
        assert!(crate::join::friends_playing_any()
            .iter()
            .any(|(c, n)| c == console && n == &vec!["Friend".to_string()]));
        crate::join::test_close_party(console);
        assert!(crate::join::friends_playing(console).is_empty());
    }

    #[test]
    fn an_invite_alone_is_nobody_playing() {
        let console = "10.0.91.4";
        crate::join::test_open_party(console);
        assert!(crate::join::needs_session(console));
        assert!(crate::join::friends_playing(console).is_empty());
        assert!(check(Some(console), &HeaderMap::new(), false).is_none());
        crate::join::test_close_party(console);
    }

    #[test]
    fn confirm_reads_every_plain_form() {
        #[derive(Deserialize)]
        struct B {
            #[serde(default, deserialize_with = "de_confirm")]
            confirm_friends: bool,
        }
        let read = |v: Value| serde_json::from_value::<B>(v).unwrap().confirm_friends;
        assert!(read(json!({"confirm_friends": true})));
        assert!(read(json!({"confirm_friends": 1})));
        assert!(read(json!({"confirm_friends": "1"})));
        assert!(read(json!({"confirm_friends": "TRUE"})));
        assert!(!read(json!({"confirm_friends": false})));
        assert!(!read(json!({"confirm_friends": "no"})));
        assert!(!read(json!({"confirm_friends": null})));
        assert!(!read(json!({})));
        let q: ConfirmQuery = serde_urlencoded_like("confirm_friends=1");
        assert!(q.yes());
        let q: ConfirmQuery = serde_urlencoded_like("other=2");
        assert!(!q.yes());
        let mut h = HeaderMap::new();
        h.insert("X-Confirm-Friends", HeaderValue::from_static("true"));
        assert!(header_confirms(&h));
        h.insert("X-Confirm-Friends", HeaderValue::from_static("0"));
        assert!(!header_confirms(&h));
    }

    fn serde_urlencoded_like(q: &str) -> ConfirmQuery {
        let uri: axum::http::Uri = format!("/x?{q}").parse().unwrap();
        axum::extract::Query::<ConfirmQuery>::try_from_uri(&uri)
            .unwrap()
            .0
    }
}
