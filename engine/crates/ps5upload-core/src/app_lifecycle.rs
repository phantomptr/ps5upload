//! App lifecycle (suspend / resume / kill / list) + rich toast.
//!
//! Talks to the payload's APP_LIFECYCLE and TOAST_SEND frames. Both
//! are non-destructive RPCs — the kill path is the closest to
//! destructive (terminates a running app) but doesn't affect system
//! state beyond that.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::mgmt::{self, m};

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
    // A refused action is `{"ok":false,"action":..,"code":..}`; over AVA1 it travels as the error's cause
    // and `call_legacy_body` returns it, so the Sony return code below still reaches the user.
    let resp = mgmt::call_legacy_body(addr, m::APP_LIFECYCLE, "APP_LIFECYCLE", &body)?;
    let parsed: AppLifecycleAck = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        // Include the Sony return code. Without it every failure reads
        // "payload returned ok=false", which is untraceable — the code is the
        // only thing that says WHY the console refused (title not running,
        // wrong app id, API unavailable on this firmware).
        match (parsed.err.as_deref(), parsed.code) {
            (Some(e), Some(code)) => bail!("APP_LIFECYCLE failed: {e} (code {code:#010x})"),
            (Some(e), None) => bail!("APP_LIFECYCLE failed: {e}"),
            (None, Some(code)) => {
                bail!("APP_LIFECYCLE failed: console returned {code:#010x}")
            }
            (None, None) => bail!("APP_LIFECYCLE failed: payload returned ok=false"),
        }
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
    // toast.send: over 4 KiB the payload answers ERR_PROTOCOL "body_too_large" (a refusal, an error here);
    // `{"ok":false,"code":N}` (daemon offline) comes back as the body, as the handler built it.
    let resp = mgmt::call_keep(addr, m::TOAST_SEND, "TOAST_SEND", &body)?;
    let parsed: ToastSendAck = serde_json::from_slice(&resp)?;
    if !parsed.ok {
        bail!(
            "TOAST_SEND failed: {}",
            parsed.err.as_deref().unwrap_or("payload returned ok=false")
        );
    }
    Ok(parsed)
}
