//! `/api/ps5/hearbridge/*` — the console's HearBridge (Bluetooth headset audio, HTTP on
//! :8090..8095) as the app sees it: its status, its saved and scanned headsets, and the
//! few changes the app makes. Every call is a plain HTTP request to the console
//! (`ps5upload_core::hearbridge`); its token never leaves the engine.

use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use ps5upload_core::hearbridge as hb;

/// The routes, for any router state (the handlers need none).
pub(crate) fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/api/ps5/hearbridge/status", get(status))
        .route("/api/ps5/hearbridge/saved", get(saved))
        .route("/api/ps5/hearbridge/devices", get(devices))
        .route("/api/ps5/hearbridge/action", post(action))
}

#[derive(Deserialize)]
pub(crate) struct HostParams {
    #[serde(alias = "addr")]
    #[serde(alias = "console")]
    host: String,
}

fn bad_gateway(e: String) -> Response {
    crate::json_err(StatusCode::BAD_GATEWAY, e).into_response()
}

/// The console's address, or a 400 for something that isn't one (whitespace, control
/// characters, URL syntax), refused before anything is sent anywhere.
#[allow(clippy::result_large_err)]
fn host(h: &str) -> Result<String, Response> {
    let h = h.trim();
    hb::check_host(h).map_err(|e| crate::json_err(StatusCode::BAD_REQUEST, e).into_response())?;
    Ok(ps5upload_core::events::console_host(h))
}

/// GET /api/ps5/hearbridge/status?host= — `{ok, present:true, port, status}`, or
/// `{ok, present:false, error}` when nothing answers on 8090..8095.
pub(crate) async fn status(Query(q): Query<HostParams>) -> Response {
    let h = match host(&q.host) {
        Ok(h) => h,
        Err(r) => return r,
    };
    match tokio::task::spawn_blocking(move || hb::status(&h)).await {
        Ok(Ok(f)) => Json(serde_json::json!({
            "ok": true,
            "present": true,
            "port": f.port,
            "status": f.status,
        }))
        .into_response(),
        Ok(Err(e)) => {
            Json(serde_json::json!({ "ok": true, "present": false, "error": e })).into_response()
        }
        Err(e) => bad_gateway(format!("{e}")),
    }
}

async fn list(q: HostParams, which: hb::DeviceList) -> Response {
    let h = match host(&q.host) {
        Ok(h) => h,
        Err(r) => return r,
    };
    match tokio::task::spawn_blocking(move || hb::devices(&h, which)).await {
        Ok(Ok(v)) => {
            Json(serde_json::json!({ "ok": true, "devices": v["devices"] })).into_response()
        }
        Ok(Err(e)) => bad_gateway(e),
        Err(e) => bad_gateway(format!("{e}")),
    }
}

/// GET /api/ps5/hearbridge/saved?host= — the paired headsets `{ok, devices}`.
pub(crate) async fn saved(Query(q): Query<HostParams>) -> Response {
    list(q, hb::DeviceList::Saved).await
}

/// GET /api/ps5/hearbridge/devices?host= — the last scan's results `{ok, devices}`.
pub(crate) async fn devices(Query(q): Query<HostParams>) -> Response {
    list(q, hb::DeviceList::Scanned).await
}

#[derive(Deserialize)]
pub(crate) struct ActionReq {
    /// The console (`addr` here is the headset's Bluetooth address, see `ActionParams`, so it is
    /// no name for the console on this route).
    #[serde(alias = "console")]
    host: String,
    action: String,
    #[serde(flatten)]
    params: hb::ActionParams,
}

/// POST /api/ps5/hearbridge/action `{host, action, pct?|on?|index?|addr?|auto?|ms?}` —
/// one of volume, mute, reconnect, select, scan, night, latency, stop. Answers
/// `{ok, result}` with HearBridge's reply (its new status, token removed).
pub(crate) async fn action(Json(req): Json<ActionReq>) -> Response {
    let a = match hb::Action::parse(&req.action, &req.params) {
        Ok(a) => a,
        Err(e) => return crate::json_err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let h = match host(&req.host) {
        Ok(h) => h,
        Err(r) => return r,
    };
    crate::log_info!("hearbridge {}: host={h}", req.action);
    match tokio::task::spawn_blocking(move || hb::act(&h, &a)).await {
        Ok(Ok(v)) => Json(serde_json::json!({ "ok": true, "result": v })).into_response(),
        Ok(Err(e)) => bad_gateway(e),
        Err(e) => bad_gateway(format!("{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serve the routes on an ephemeral port and send one raw request; the status code and
    /// body come back.
    async fn call(method: &str, path: &str, body: Option<&str>) -> (u16, String) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(l, routes::<()>()).await.unwrap();
        });
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let body = body.unwrap_or("");
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        let text = String::from_utf8_lossy(&out).to_string();
        let code = text
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        (code, body)
    }

    #[tokio::test]
    async fn an_unknown_action_is_refused_before_anything_is_sent() {
        // A blackholed console: any request to it would take seconds, so a fast 400 shows
        // nothing was tried.
        let t = Instant::now();
        let (code, body) = call(
            "POST",
            "/api/ps5/hearbridge/action",
            Some(r#"{"host":"10.255.255.1","action":"forget","addr":"AA:BB:CC:DD:EE:FF"}"#),
        )
        .await;
        assert_eq!(code, 400, "{body}");
        assert!(body.contains("unknown HearBridge action"), "{body}");
        let (code, body) = call(
            "POST",
            "/api/ps5/hearbridge/action",
            Some(r#"{"host":"10.255.255.1","action":"volume","pct":9000}"#),
        )
        .await;
        assert_eq!(code, 400, "{body}");
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
    }

    #[tokio::test]
    async fn a_host_that_is_not_an_address_is_refused() {
        let (code, _) = call(
            "POST",
            "/api/ps5/hearbridge/action",
            Some(r#"{"host":"1.2.3.4/x","action":"stop"}"#),
        )
        .await;
        assert_eq!(code, 400);
        // `addr` is accepted for `host` on the reads, and checked the same way.
        let (code, body) = call("GET", "/api/ps5/hearbridge/status?addr=1.2.3.4%20x", None).await;
        assert_eq!(code, 400, "{body}");
        let (code, _) = call("GET", "/api/ps5/hearbridge/saved?host=a%0d%0ab", None).await;
        assert_eq!(code, 400);
    }

    #[tokio::test]
    async fn status_by_addr_reports_an_absent_hearbridge() {
        let (code, body) = call("GET", "/api/ps5/hearbridge/status?addr=127.0.0.1", None).await;
        assert_eq!(code, 200, "{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["ok"], true);
        // Something may listen on 8090 on a dev machine; either way no token is handed out.
        assert!(v.get("token").is_none() && v["status"].get("token").is_none());
    }
}
