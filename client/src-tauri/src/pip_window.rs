//! The main window, built here rather than from tauri.conf.json (`"create": false`) so it can
//! answer `window.open`: Remote Play's picture in picture is an always-on-top popup that shares
//! the page's JavaScript (the page moves the stream's canvas into it), the desktop app's
//! counterpart of the browser's Document Picture-in-Picture.
//!
//! Only a blank popup (`window.open("", …)`) becomes that window. Any other address a page tries
//! to open goes to the system browser, http(s) only, and never into a webview.

#[cfg(desktop)]
use std::sync::atomic::{AtomicU32, Ordering};

use tauri::{AppHandle, Runtime};

/// Labels of the picture-in-picture windows: `rp-pip-<n>`.
#[cfg(desktop)]
pub const PIP_PREFIX: &str = "rp-pip-";

/// Sent to the main window when a picture-in-picture window is gone (closed from its title bar).
#[cfg(desktop)]
pub const PIP_CLOSED_EVENT: &str = "rp-pip-closed";

#[cfg(desktop)]
static NEXT: AtomicU32 = AtomicU32::new(1);

/// Create the main window from its tauri.conf.json entry (the one Tauri would have created).
pub fn build_main<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let Some(cfg) = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .cloned()
    else {
        return Ok(());
    };
    let builder = tauri::WebviewWindowBuilder::from_config(app, &cfg)?;
    #[cfg(desktop)]
    let builder = {
        let handle = app.clone();
        builder.on_new_window(move |url, features| open_popup(&handle, url, features))
    };
    // WebKitGTK refuses a page's microphone unless the app answers its permission request, so
    // Remote Play's mic (talking in party chat) failed on Linux with NotAllowedError. The
    // other platforms ask the user through the system as before.
    #[cfg(target_os = "linux")]
    let builder = builder.on_permission_request(|_, kind| match kind {
        tauri::webview::PermissionKind::Microphone => tauri::webview::PermissionResponse::Allow,
        _ => tauri::webview::PermissionResponse::Default,
    });
    // Whether WebKit's compositing is off this launch (linux_gpu.rs): Remote Play then draws
    // with the 2D canvas, as WebGL is read back from the GPU every frame there.
    #[cfg(target_os = "linux")]
    let builder = builder.initialization_script(crate::linux_gpu::page_flag_script());
    builder.build()?;
    Ok(())
}

#[cfg(desktop)]
fn open_popup<R: Runtime>(
    app: &AppHandle<R>,
    url: tauri::Url,
    features: tauri::webview::NewWindowFeatures,
) -> tauri::webview::NewWindowResponse<R> {
    use tauri::webview::NewWindowResponse;
    if url.as_str() != "about:blank" {
        if matches!(url.scheme(), "http" | "https") {
            use tauri_plugin_opener::OpenerExt;
            let _ = app.opener().open_url(url.as_str(), None::<&str>);
        }
        return NewWindowResponse::Deny;
    }
    let label = format!("{PIP_PREFIX}{}", NEXT.fetch_add(1, Ordering::SeqCst));
    let built = tauri::WebviewWindowBuilder::new(
        app,
        label,
        tauri::WebviewUrl::External("about:blank".parse().expect("about:blank")),
    )
    // Size (from the page) and, per platform, the opener's webview configuration, so the
    // page gets this window back from window.open and can draw in it.
    .window_features(features)
    .title("Remote Play")
    .always_on_top(true)
    .resizable(true)
    .min_inner_size(240.0, 135.0)
    .on_document_title_changed(|w, title| {
        let _ = w.set_title(&title);
    })
    .build();
    match built {
        Ok(window) => NewWindowResponse::Create { window },
        Err(e) => {
            eprintln!("[tauri] picture in picture window failed: {e}");
            NewWindowResponse::Deny
        }
    }
}

/// Window events for picture in picture: tell the page when its popup is gone, and close the
/// popups with the main window (their page lives in the main window).
#[cfg(desktop)]
pub fn on_window_event<R: Runtime>(window: &tauri::Window<R>, event: &tauri::WindowEvent) {
    use tauri::{Emitter, Manager};
    if !matches!(event, tauri::WindowEvent::Destroyed) {
        return;
    }
    let label = window.label();
    let app = window.app_handle();
    if label.starts_with(PIP_PREFIX) {
        let _ = app.emit_to("main", PIP_CLOSED_EVENT, label);
    } else if label == "main" {
        for (l, w) in app.webview_windows() {
            if l.starts_with(PIP_PREFIX) {
                let _ = w.close();
            }
        }
    }
}
