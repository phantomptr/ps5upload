//! Remote Play full screen in the Android app: hide the status and navigation bars (true
//! immersive mode, a swipe from the edge shows them for a moment) and hold the screen
//! sideways, then give both back when the picture leaves full screen.
//!
//! The Android WebView refuses `document.requestFullscreen()`, so the page alone can fill
//! the window but not hide the system bars. This goes through Tauri's own JNI hook
//! (`PlatformWebview::jni_handle().exec`), which runs on the activity's UI thread, the only
//! thread allowed to change the window. Only framework classes are used (no androidx), so
//! nothing depends on the regenerated `gen/android` project or on R8 keeping a class: on
//! API 30+ `Window.getInsetsController()` (what `WindowInsetsControllerCompat` calls there
//! too), on older Android the `SYSTEM_UI_FLAG_IMMERSIVE_STICKY` flags.
//!
//! Every other platform: a no-op (desktop uses the window's own full screen).

/// Hide (`immersive`) or show the system bars, and lock the screen to landscape
/// (`landscape`) or give the orientation back. Fire and forget: the change is queued to the
/// UI thread and a failure there is logged, never surfaced.
#[tauri::command]
pub fn android_system_ui(
    window: tauri::WebviewWindow,
    immersive: bool,
    landscape: bool,
) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        window
            .with_webview(move |wv| {
                wv.jni_handle().exec(move |env, activity, _webview| {
                    let out = android::apply(env, activity, immersive, landscape);
                    // Never hand the UI thread back with a pending exception.
                    if env.exception_check().unwrap_or(false) {
                        let _ = env.exception_describe();
                        let _ = env.exception_clear();
                    }
                    if let Err(e) = out {
                        eprintln!("[system-ui] immersive={immersive} landscape={landscape}: {e}");
                    }
                })
            })
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (window, immersive, landscape);
        Ok(())
    }
}

/// The iOS app: make the web view the first responder. WKWebView hands controllers (Backbone,
/// Kishi, any MFi or Bluetooth pad) to the page's Gamepad API only while it is the first
/// responder (WebKit bug 269292), which it may not be after a load or another view took the
/// focus. Every other platform: a no-op.
#[tauri::command]
pub fn focus_webview(window: tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "ios")]
    {
        window
            .with_webview(|wv| {
                let view = wv.inner() as *mut objc2::runtime::AnyObject;
                if !view.is_null() {
                    // SAFETY: inner() is the live WKWebView, and with_webview runs on the main
                    // thread, the only one allowed to touch UIKit.
                    let _: bool = unsafe { objc2::msg_send![view, becomeFirstResponder] };
                }
            })
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "ios"))]
    {
        let _ = window;
        Ok(())
    }
}

/// The iOS app: hide the status bar (the clock, battery and signal) while Remote Play fills the
/// screen, and show it again on leaving (#722). The window's root view controller is tao's, whose
/// `prefersStatusBarHidden` is a property it keeps; its `setPrefersStatusBarHidden:` setter also
/// calls `setNeedsStatusBarAppearanceUpdate`. Every other platform: a no-op.
#[tauri::command]
pub fn set_status_bar_hidden(window: tauri::WebviewWindow, hidden: bool) -> Result<(), String> {
    #[cfg(target_os = "ios")]
    {
        window
            .with_webview(move |wv| {
                use objc2::runtime::{AnyObject, Bool, Sel};
                let vc = wv.view_controller() as *mut AnyObject;
                if vc.is_null() {
                    return;
                }
                let setter = Sel::register(c"setPrefersStatusBarHidden:");
                // SAFETY: view_controller() is the window's live root UIViewController, and
                // with_webview runs on the main thread, the only one allowed to touch UIKit. The
                // setter is checked for before it is sent (tao's controller has it).
                unsafe {
                    let has: bool = objc2::msg_send![vc, respondsToSelector: setter];
                    if has {
                        let _: () =
                            objc2::msg_send![vc, setPrefersStatusBarHidden: Bool::new(hidden)];
                    } else {
                        eprintln!("[system-ui] the root view controller has no status bar setter");
                    }
                }
            })
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "ios"))]
    {
        let _ = (window, hidden);
        Ok(())
    }
}

/// The iOS app: whether the controllers' Home button (the PS button) is the app's or the system's.
/// By default iOS opens its Games overlay and the page never sees the press (#675). iOS only honours
/// this when the user's in-app Home button setting defers to the app. Every other platform: a no-op.
#[tauri::command]
pub fn hold_controller_home(window: tauri::WebviewWindow, on: bool) -> Result<(), String> {
    #[cfg(target_os = "ios")]
    {
        window
            .with_webview(move |_| ios_home::set(on))
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "ios"))]
    {
        let _ = (window, on);
        Ok(())
    }
}

#[cfg(target_os = "ios")]
mod ios_home {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use std::os::raw::c_char;

    // Loaded for GCController: WebKit only pulls the framework in once a page asks for gamepads.
    #[link(name = "GameController", kind = "framework")]
    extern "C" {}

    /// `GCSystemGestureStateEnabled`.
    const GESTURE_ENABLED: isize = 0;
    /// `GCSystemGestureStateDisabled`: "input is passed directly to the app".
    const GESTURE_DISABLED: isize = 2;

    /// Runs on the main thread (`with_webview`). Objects come back autoreleased.
    pub fn set(on: bool) {
        let (Some(controllers), Some(strings)) =
            (AnyClass::get(c"GCController"), AnyClass::get(c"NSString"))
        else {
            return;
        };
        let state = if on {
            GESTURE_DISABLED
        } else {
            GESTURE_ENABLED
        };
        // SAFETY: documented GameController and Foundation selectors, on the main thread, each
        // result checked for nil before use. `GCInputButtonHome` is the string "Button Home".
        unsafe {
            let name: *mut AnyObject = msg_send![
                strings,
                stringWithUTF8String: c"Button Home".as_ptr() as *const c_char
            ];
            let list: *mut AnyObject = msg_send![controllers, controllers];
            if list.is_null() || name.is_null() {
                return;
            }
            let count: usize = msg_send![list, count];
            for i in 0..count {
                let controller: *mut AnyObject = msg_send![list, objectAtIndex: i];
                let profile: *mut AnyObject = msg_send![controller, physicalInputProfile];
                if profile.is_null() {
                    continue;
                }
                let buttons: *mut AnyObject = msg_send![profile, buttons];
                if buttons.is_null() {
                    continue;
                }
                let home: *mut AnyObject = msg_send![buttons, objectForKey: name];
                if !home.is_null() {
                    let _: () = msg_send![home, setPreferredSystemGestureState: state];
                }
            }
        }
    }
}

#[cfg(target_os = "android")]
mod android {
    use jni::objects::{JObject, JValue};
    use jni::JNIEnv;
    use std::sync::atomic::{AtomicI32, Ordering};

    /// `WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE`.
    const BEHAVIOR_TRANSIENT_BY_SWIPE: i32 = 2;
    /// `WindowInsetsController.BEHAVIOR_DEFAULT` (API 31; on API 30 the same value is
    /// `BEHAVIOR_SHOW_BARS_BY_SWIPE`, the old default).
    const BEHAVIOR_DEFAULT: i32 = 1;
    /// `View.SYSTEM_UI_FLAG_HIDE_NAVIGATION | SYSTEM_UI_FLAG_FULLSCREEN |
    /// SYSTEM_UI_FLAG_IMMERSIVE_STICKY` (API < 30).
    const LEGACY_IMMERSIVE: i32 = 0x2 | 0x4 | 0x1000;
    /// `SYSTEM_UI_FLAG_LAYOUT_STABLE | _LAYOUT_HIDE_NAVIGATION | _LAYOUT_FULLSCREEN`: the
    /// edge-to-edge layout the activity already draws (set again so the page does not jump).
    const LEGACY_LAYOUT: i32 = 0x100 | 0x200 | 0x400;
    /// `ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE` (either way round, by the sensor:
    /// what Chrome's `screen.orientation.lock("landscape")` uses).
    const SENSOR_LANDSCAPE: i32 = 6;
    /// No orientation saved (the activity's own one is restored on unlock).
    const NONE_SAVED: i32 = i32::MIN;

    /// The activity's orientation before we locked it.
    static SAVED_ORIENTATION: AtomicI32 = AtomicI32::new(NONE_SAVED);

    pub fn apply(
        env: &mut JNIEnv,
        activity: &JObject,
        immersive: bool,
        landscape: bool,
    ) -> jni::errors::Result<()> {
        bars(env, activity, immersive)?;
        orientation(env, activity, landscape)
    }

    fn bars(env: &mut JNIEnv, activity: &JObject, immersive: bool) -> jni::errors::Result<()> {
        let window = env
            .call_method(activity, "getWindow", "()Landroid/view/Window;", &[])?
            .l()?;
        if window.is_null() {
            return Ok(());
        }
        let sdk = env
            .get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?
            .i()?;
        if sdk >= 30 {
            let ctl = env
                .call_method(
                    &window,
                    "getInsetsController",
                    "()Landroid/view/WindowInsetsController;",
                    &[],
                )?
                .l()?;
            if ctl.is_null() {
                return Ok(());
            }
            let types = env
                .call_static_method("android/view/WindowInsets$Type", "systemBars", "()I", &[])?
                .i()?;
            if immersive {
                env.call_method(
                    &ctl,
                    "setSystemBarsBehavior",
                    "(I)V",
                    &[JValue::Int(BEHAVIOR_TRANSIENT_BY_SWIPE)],
                )?;
                env.call_method(&ctl, "hide", "(I)V", &[JValue::Int(types)])?;
            } else {
                env.call_method(&ctl, "show", "(I)V", &[JValue::Int(types)])?;
                env.call_method(
                    &ctl,
                    "setSystemBarsBehavior",
                    "(I)V",
                    &[JValue::Int(BEHAVIOR_DEFAULT)],
                )?;
            }
        } else {
            let decor = env
                .call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?
                .l()?;
            if decor.is_null() {
                return Ok(());
            }
            let now = env
                .call_method(&decor, "getSystemUiVisibility", "()I", &[])?
                .i()?;
            let next = if immersive {
                now | LEGACY_IMMERSIVE | LEGACY_LAYOUT
            } else {
                now & !LEGACY_IMMERSIVE
            };
            env.call_method(
                &decor,
                "setSystemUiVisibility",
                "(I)V",
                &[JValue::Int(next)],
            )?;
        }
        Ok(())
    }

    fn orientation(
        env: &mut JNIEnv,
        activity: &JObject,
        landscape: bool,
    ) -> jni::errors::Result<()> {
        if landscape {
            let now = env
                .call_method(activity, "getRequestedOrientation", "()I", &[])?
                .i()?;
            // Only the first lock saves: locking twice must not save our own landscape.
            let _ = SAVED_ORIENTATION.compare_exchange(
                NONE_SAVED,
                now,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            if now != SENSOR_LANDSCAPE {
                env.call_method(
                    activity,
                    "setRequestedOrientation",
                    "(I)V",
                    &[JValue::Int(SENSOR_LANDSCAPE)],
                )?;
            }
        } else {
            let saved = SAVED_ORIENTATION.swap(NONE_SAVED, Ordering::SeqCst);
            if saved != NONE_SAVED {
                env.call_method(
                    activity,
                    "setRequestedOrientation",
                    "(I)V",
                    &[JValue::Int(saved)],
                )?;
            }
        }
        Ok(())
    }
}
