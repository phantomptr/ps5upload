//! Diagnostics: the "Detailed logs" switch and the feature summary a bug report carries.
//!
//! * `GET  /api/diag/log-level` -> `{detailed, until_ms, max_hours}`
//! * `POST /api/diag/log-level` `{detailed, until_ms?}` -> the same. On turns extra debug lines on
//!   (`log_detail!`, `core_debug!`: the Remote Play handshake, Play Together) until `until_ms`
//!   (Unix ms), at most [`MAX_DETAILED`] from now and a day when it is left out, so nobody stays
//!   verbose by accident. The engine keeps it in memory; the app keeps the time and sets it again
//!   when the engine restarts.
//! * `GET  /api/diag/features` -> what each feature is doing now, as counts and states: Play
//!   Together, phone controllers and the watch party, Go live, Remote Play (the running session
//!   and the last one's start and end), saved remote connections, the mesh. Built to be posted in
//!   public: never a stream, pairing, invite or resume key, a passcode, a password, a user name,
//!   or the full address of a stream destination.

use std::time::Duration;

use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

/// The longest detailed logs stay on at a time.
pub(crate) const MAX_DETAILED: Duration = Duration::from_secs(24 * 3600);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn level_json() -> Value {
    let until = ps5upload_core::log::verbose_until();
    json!({
        "detailed": until.is_some(),
        "until_ms": until,
        "max_hours": MAX_DETAILED.as_secs() / 3600,
    })
}

/// When detailed logs asked to run until `asked` (Unix ms) turn off, given the time now: never
/// later than [`MAX_DETAILED`] from now; the full day when no time was given.
pub(crate) fn clamp_until(asked: Option<u64>, now: u64) -> u64 {
    let max = now + MAX_DETAILED.as_millis() as u64;
    asked.unwrap_or(max).min(max)
}

pub async fn log_level_get() -> Json<Value> {
    Json(level_json())
}

#[derive(Deserialize)]
pub struct LogLevelReq {
    detailed: bool,
    #[serde(default)]
    until_ms: Option<u64>,
}

pub async fn log_level_set(Json(req): Json<LogLevelReq>) -> Response {
    let was = ps5upload_core::log::verbose();
    if req.detailed {
        let now = now_ms();
        let until = clamp_until(req.until_ms, now);
        ps5upload_core::log::set_verbose_until(until);
        if until > now && !was {
            crate::log_info!(
                "detailed logs on for {} min (Remote Play handshake, Play Together)",
                (until - now) / 60_000
            );
        }
    } else {
        ps5upload_core::log::set_verbose_until(0);
        if was {
            crate::log_info!("detailed logs off");
        }
    }
    Json(level_json()).into_response()
}

/// The feature summary (see the module docs).
pub(crate) fn features_json() -> Value {
    json!({
        "engine": {
            "version": env!("CARGO_PKG_VERSION"),
            "kind": crate::mesh_api::kind(),
        },
        "detailed_logs": level_json(),
        "remote_play": crate::rp_stream_api::diag_summary(),
        "play_together": crate::join::diag_summary(),
        "pad_link": crate::pad_link::diag_summary(),
        "go_live": crate::rp_golive::diag_summary(),
        "remote_connections": crate::remote::api::diag_summary(),
        "mesh": crate::mesh_api::diag_summary(),
    })
}

pub async fn features() -> Response {
    // The Tailscale check and the stores read the disk and the network stack: off the reactor.
    match tokio::task::spawn_blocking(features_json).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => crate::json_err(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("feature summary failed: {e}"),
        )
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detailed_logs_never_run_past_a_day() {
        let now = 1_800_000_000_000;
        let day = MAX_DETAILED.as_millis() as u64;
        assert_eq!(clamp_until(None, now), now + day);
        assert_eq!(clamp_until(Some(now + 3_600_000), now), now + 3_600_000);
        assert_eq!(clamp_until(Some(now + 30 * day), now), now + day);
    }

    /// Detail lines reach the log (at debug) only while the switch is on.
    #[test]
    fn detail_lines_follow_the_switch() {
        let id = std::process::id();
        let off = format!("detail-off-{id}");
        let on = format!("detail-on-{id}");
        ps5upload_core::log::set_verbose_until(0);
        crate::log_detail!("{off}");
        ps5upload_core::log::set_verbose_until(now_ms() + 60_000);
        // A test flooding the debug ring in parallel could push one line out: a few tries.
        let seen = (0..3).any(|_| {
            crate::log_detail!("{on}");
            crate::engine_log::tail_since(0)
                .iter()
                .any(|e| e.msg == on && e.level == "debug")
        });
        ps5upload_core::log::set_verbose_until(0);
        assert!(seen);
        assert!(!crate::engine_log::tail_since(0)
            .iter()
            .any(|e| e.msg == off));
    }

    #[test]
    fn a_custom_server_shows_only_its_host() {
        use crate::rp_golive::server_host;
        assert_eq!(
            server_host("rtmp://live.example.com:1935/app/sk_live_SECRETKEY?x=1").as_deref(),
            Some("live.example.com:1935")
        );
        assert_eq!(
            server_host("srt://user:hunter2@ingest.example.net:9000?streamid=SECRET").as_deref(),
            Some("ingest.example.net:9000")
        );
        assert_eq!(server_host("").as_deref(), None);
    }

    /// The summary as a whole carries no secret: a go-live destination with a key, an open
    /// phone link, Play Together with an invite and a guest are all reduced to counts.
    #[test]
    fn the_feature_summary_carries_no_keys() {
        let text = features_json().to_string();
        for k in [
            "\"key\"",
            "\"token\"",
            "\"pad_token\"",
            "\"invite\"",
            "\"code\"",
            "\"password\"",
            "\"url\"",
        ] {
            assert!(!text.contains(k), "{k} in {text}");
        }
        // Every section is there.
        let v: Value = serde_json::from_str(&text).unwrap();
        for s in [
            "engine",
            "detailed_logs",
            "remote_play",
            "play_together",
            "pad_link",
            "go_live",
            "remote_connections",
            "mesh",
        ] {
            assert!(v.get(s).is_some(), "{s} missing");
        }
    }
}
