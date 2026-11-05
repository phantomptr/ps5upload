//! Minimize / close to the system tray (#516). Desktop only, and off until the person turns it
//! on in Settings: the renderer calls `tray_configure` with the two switches (and the menu's
//! words in the app's language) at start and whenever they change.
//!
//! * Windows: the notification area. Linux: the AppIndicator tray (needs
//!   libayatana-appindicator3 or libappindicator3; without it there is no tray, and closing or
//!   minimizing does what it always did: a window is never hidden where it cannot be found).
//! * macOS: the menu bar. Closing hides the window and its Dock icon; the menu bar icon, the app
//!   launched again, or Show brings it back. Minimizing stays the system's (to the Dock).
//!
//! Quit (the tray menu, Cmd+Q, the app's own exit) still quits: the exit path is the bounded one
//! in exit_guard.rs, so the engine stops with the app.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime, Window, WindowEvent};

const TRAY_ID: &str = "ps5upload-main";

static MINIMIZE: AtomicBool = AtomicBool::new(false);
static CLOSE: AtomicBool = AtomicBool::new(false);
/// The tray icon exists (created, not removed).
static SHOWN: AtomicBool = AtomicBool::new(false);
/// The words the menu was built with: rebuilt when the language changes.
static LABELS: Mutex<Option<Labels>> = Mutex::new(None);

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Labels {
    pub show: String,
    pub quit: String,
    pub tooltip: String,
}

#[derive(Debug, serde::Serialize)]
pub struct TrayStatus {
    /// This system can show a tray icon at all.
    pub available: bool,
    /// The icon is up now (one of the switches is on).
    pub active: bool,
}

/// Whether this system can show a tray icon. Windows and macOS always can; Linux needs the
/// AppIndicator library, which the tray code loads at run time (and panics without).
pub fn available() -> bool {
    #[cfg(target_os = "linux")]
    {
        const LIBS: [&str; 4] = [
            "libayatana-appindicator3.so.1",
            "libayatana-appindicator3.so",
            "libappindicator3.so.1",
            "libappindicator3.so",
        ];
        LIBS.iter().any(|name| {
            let Ok(c) = std::ffi::CString::new(*name) else {
                return false;
            };
            // SAFETY: dlopen/dlclose with a valid C string; the handle is only closed again.
            unsafe {
                let h = libc::dlopen(c.as_ptr(), libc::RTLD_LAZY);
                if h.is_null() {
                    false
                } else {
                    libc::dlclose(h);
                    true
                }
            }
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Brings the main window back (from the tray, a second launch, the Dock).
pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    #[cfg(target_os = "macos")]
    let _ = app.set_dock_visibility(true);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn hide_main<R: Runtime>(window: &Window<R>) {
    let _ = window.hide();
    #[cfg(target_os = "macos")]
    let _ = window.app_handle().set_dock_visibility(false);
}

fn build<R: Runtime>(app: &AppHandle<R>, labels: &Labels) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "tray-show", &labels.show, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray-quit", &labels.quit, true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut b = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(&labels.tooltip)
        .menu(&menu)
        // Windows / Linux: a click shows the window, the menu is on right-click. macOS: a click
        // opens the menu, as menu bar icons do.
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| match event.id().as_ref() {
            "tray-show" => show_main(app),
            "tray-quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if !cfg!(target_os = "macos") {
                    show_main(tray.app_handle());
                }
            }
        });
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.build(app)?;
    Ok(())
}

/// Turns the two switches on or off. The icon is up while either is on.
#[tauri::command]
pub fn tray_configure<R: Runtime>(
    app: AppHandle<R>,
    minimize: bool,
    close: bool,
    labels: Labels,
) -> Result<TrayStatus, String> {
    let available = available();
    let want = available && (minimize || close);
    let mut current = LABELS.lock().unwrap_or_else(|e| e.into_inner());
    if want && (!SHOWN.load(Ordering::SeqCst) || current.as_ref() != Some(&labels)) {
        let _ = app.remove_tray_by_id(TRAY_ID);
        // A tray library that loads but fails (some Linux desktops) must not take the app down.
        let made = crate::crash_hook::optional("the tray icon", || build(&app, &labels));
        match made {
            Some(Ok(())) => {
                SHOWN.store(true, Ordering::SeqCst);
                *current = Some(labels);
            }
            Some(Err(e)) => {
                SHOWN.store(false, Ordering::SeqCst);
                return Err(format!("could not add the tray icon: {e}"));
            }
            None => {
                SHOWN.store(false, Ordering::SeqCst);
                return Err("could not add the tray icon".into());
            }
        }
    } else if !want && SHOWN.swap(false, Ordering::SeqCst) {
        let _ = app.remove_tray_by_id(TRAY_ID);
        *current = None;
        // Nothing to come back from: make sure the window is not left hidden.
        show_main(&app);
    }
    MINIMIZE.store(minimize, Ordering::SeqCst);
    CLOSE.store(close, Ordering::SeqCst);
    Ok(TrayStatus {
        available,
        active: SHOWN.load(Ordering::SeqCst),
    })
}

/// The main window's events: close and minimize go to the tray when asked and the icon is up.
pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if window.label() != "main" || !SHOWN.load(Ordering::SeqCst) {
        return;
    }
    match event {
        WindowEvent::CloseRequested { api, .. } if CLOSE.load(Ordering::SeqCst) => {
            api.prevent_close();
            hide_main(window);
            // The app keeps running, so an update armed to install on quit waits for Quit: the
            // renderer says so once (state/update.ts).
            let _ = window.emit("tray-closed", ());
        }
        // Minimizing shows up as a resize on Windows and Linux; on macOS it stays the Dock's.
        WindowEvent::Resized(_)
            if !cfg!(target_os = "macos")
                && MINIMIZE.load(Ordering::SeqCst)
                && window.is_minimized().unwrap_or(false) =>
        {
            hide_main(window);
        }
        _ => {}
    }
}
