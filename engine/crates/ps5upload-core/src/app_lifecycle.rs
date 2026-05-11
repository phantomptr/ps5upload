//! App lifecycle (suspend / resume / kill / list) + rich toast.
//!
//! Talks to the payload's APP_LIFECYCLE and TOAST_SEND frames. Both
//! are non-destructive RPCs — the kill path is the closest to
//! destructive (terminates a running app) but doesn't affect system
//! state beyond that.

use anyhow::{bail, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppAction {
    Suspend,
    Resume,
    Kill,
    /// Enumerate currently-running apps. `app_id` is ignored.
    List,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningApp {
    pub app_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppLifecycleAck {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub app_id: Option<u32>,
    /// Sony API return code (0 = success).
    #[serde(default)]
    pub code: Option<i32>,
    /// Error string when `ok=false`.
    #[serde(default)]
    pub err: Option<String>,
    /// Populated only for `action="list"`.
    #[serde(default)]
    pub apps: Vec<RunningApp>,
}

pub fn app_lifecycle(addr: &str, action: AppAction, app_id: u32) -> Result<AppLifecycleAck> {
    let body = serde_json::json!({
        "action": match action {
            AppAction::Suspend => "suspend",
            AppAction::Resume => "resume",
            AppAction::Kill => "kill",
            AppAction::List => "list",
        },
        "app_id": app_id,
    });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::AppLifecycle, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected APP_LIFECYCLE: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::AppLifecycleAck {
        bail!("expected APP_LIFECYCLE_ACK, got {ft:?}");
    }
    let parsed: AppLifecycleAck = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "APP_LIFECYCLE failed: {}",
            parsed.err.as_deref().unwrap_or("payload returned ok=false")
        );
    }
    Ok(parsed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToastRequest {
    /// Title line of the toast (bold, top of bubble).
    pub title: String,
    /// Optional body line below the title.
    #[serde(default)]
    pub subtitle: String,
    /// Optional URL or local-resource path for the icon. Sony's
    /// notification daemon falls back to a default icon when absent
    /// or unreachable.
    #[serde(default)]
    pub icon: String,
    /// Optional deep-link URL the user taps to act on the toast
    /// (e.g. `pssettings://`). Empty = no action button.
    #[serde(default)]
    pub action_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToastSendAck {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub err: Option<String>,
}

/// Push a styled toast to the PS5. Builds a minimal Sony notification
/// JSON template and forwards it through the payload. Best-effort —
/// Sony's daemon silently drops malformed templates, so the success
/// signal is "no error code" rather than "user actually saw it."
pub fn toast_send(addr: &str, req: &ToastRequest) -> Result<ToastSendAck> {
    /* Minimal but well-formed template. Field set discovered from
     * the payload SDK's samples/notify/main.c — the keys Sony's
     * daemon recognises. */
    let body = serde_json::json!({
        "messageType": 0,
        "summary": req.title,
        "messageBody": req.subtitle,
        "imageUri": req.icon,
        "actionUrl": req.action_url,
        "useIconImageUri": !req.icon.is_empty(),
    });
    let body = serde_json::to_vec(&body)?;
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::ToastSend, &body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected TOAST_SEND: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::ToastSendAck {
        bail!("expected TOAST_SEND_ACK, got {ft:?}");
    }
    let parsed: ToastSendAck = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "TOAST_SEND failed: {}",
            parsed.err.as_deref().unwrap_or("payload returned ok=false")
        );
    }
    Ok(parsed)
}
