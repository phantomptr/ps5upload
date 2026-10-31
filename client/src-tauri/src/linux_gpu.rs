//! Linux WebKitGTK white-screen rescues, chosen per graphics stack.
//!
//! WebKitGTK rendered a blank/white window on several GPU + compositor
//! combinations, chiefly the proprietary NVIDIA driver: its DMA-BUF renderer
//! (WebKitGTK 2.42+) and GPU compositing on Wayland need explicit sync, which
//! NVIDIA only gained in driver 555, and WebKitGTK 2.46 reworked that path
//! (Skia, DMA-BUF buffers handed to the compositor). Two switches rescue it:
//! `WEBKIT_DISABLE_DMABUF_RENDERER=1` and `WEBKIT_DISABLE_COMPOSITING_MODE=1`.
//! Neither is free. With compositing off the whole page renders in software:
//! the long lists scroll heavily (it was a blanket default once and was
//! reverted for that), and WebKit reads a WebGL canvas back from the GPU every
//! frame, so Remote Play's picture lags. On a current stack (NVIDIA 595 +
//! WebKitGTK 2.52, Ubuntu 26.04) both switches off rendered fine and Remote
//! Play's latency was clearly lower.
//!
//! So the rescues are matched to the stack that needs them:
//!
//! - NVIDIA on Wayland with driver 555 or later and WebKitGTK 2.46 or later,
//!   not run from the AppImage — the stack measured to work without them:
//!   neither switch.
//! - Everything else exactly as before: the DMABUF renderer off on every
//!   Linux session (AMD and Intel included: the white windows on Bazzite and
//!   SteamOS were not NVIDIA-only), and on NVIDIA + Wayland compositing off
//!   too. That includes unreadable versions, X11, and every AppImage: it
//!   carries its own WebKitGTK and Wayland client libraries, and issue #285
//!   (CachyOS, a 2026 NVIDIA driver, the AppImage) still needed the switches.
//! - Safe graphics mode (Settings, `--safe-graphics` or
//!   `PS5UPLOAD_SAFE_GRAPHICS=1` for a window too white to reach Settings):
//!   both switches on any stack.
//!
//! Everything here is only applied when the variable is unset, so a user can
//! always override in either direction.
//!
//! The decision logic is deliberately platform-independent so it is tested on
//! every CI runner, not only the Linux one — it is compiled everywhere and
//! called only on Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::PathBuf;
use std::sync::OnceLock;

/// What we could observe about the running graphics session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraphicsSession {
    pub wayland: bool,
    /// The proprietary NVIDIA driver is loaded. Nouveau does not count: it
    /// does not show this failure and would pay the scrolling cost for
    /// nothing.
    pub nvidia: bool,
    /// The NVIDIA driver's major version (595 for 595.99.02), when readable.
    pub nvidia_driver: Option<u32>,
    /// The WebKitGTK library this process runs with (major, minor).
    pub webkit: Option<(u32, u32)>,
    /// Running from the AppImage, with its own bundled libraries.
    pub appimage: bool,
}

/// NVIDIA's first driver with explicit sync on Wayland.
pub const NVIDIA_EXPLICIT_SYNC: u32 = 555;
/// The first WebKitGTK whose DMA-BUF renderer and compositing hold up there.
pub const WEBKIT_CURRENT: (u32, u32) = (2, 46);

/// Whether this process runs on the stack shown to work without the rescues:
/// NVIDIA on Wayland, driver 555+ and WebKitGTK 2.46+, outside the AppImage.
/// Unknown versions count as old; other GPUs and X11 keep the rescues.
pub fn current_stack(s: GraphicsSession) -> bool {
    s.wayland
        && s.nvidia
        && !s.appimage
        && s.nvidia_driver.is_some_and(|v| v >= NVIDIA_EXPLICIT_SYNC)
        && s.webkit.is_some_and(|v| v >= WEBKIT_CURRENT)
}

/// Whether this is a Wayland session.
///
/// `XDG_SESSION_TYPE` is authoritative when set — a `WAYLAND_DISPLAY`
/// inherited from a parent shell can outlive the session it referred to, so
/// it is only trusted when the session type says nothing.
pub fn detect_wayland(xdg_session_type: Option<&str>, wayland_display: Option<&str>) -> bool {
    if let Some(t) = xdg_session_type {
        if !t.is_empty() {
            return t.eq_ignore_ascii_case("wayland");
        }
    }
    wayland_display.is_some_and(|d| !d.is_empty())
}

/// The environment variables to set for this session, in the order they
/// should be applied. Callers must only set a variable that is currently
/// unset, so an explicit user choice always wins. `safe`: Safe graphics mode,
/// the old rescues whatever the stack.
pub fn webkit_workarounds(
    session: GraphicsSession,
    safe: bool,
) -> Vec<(&'static str, &'static str)> {
    if !safe && current_stack(session) {
        return Vec::new();
    }
    let mut vars = vec![("WEBKIT_DISABLE_DMABUF_RENDERER", "1")];
    // Safe mode is for a machine whose window is still white, whatever it
    // was detected as: the whole set.
    if safe || (session.wayland && session.nvidia) {
        vars.push(("WEBKIT_DISABLE_COMPOSITING_MODE", "1"));
    }
    vars
}

/// The NVIDIA driver's major version from `/sys/module/nvidia/version`
/// ("595.99.02") or `/proc/driver/nvidia/version` ("NVRM version: NVIDIA UNIX
/// Open Kernel Module for x86_64  595.99.02  Release Build ...").
pub fn nvidia_major(text: &str) -> Option<u32> {
    let line = text
        .lines()
        .find(|l| l.contains("NVRM"))
        .or_else(|| text.lines().next())?;
    line.split_whitespace().find_map(|tok| {
        let mut parts = tok.split('.');
        let major = parts.next()?.parse::<u32>().ok()?;
        let minor = parts.next()?;
        (!minor.is_empty() && minor.chars().all(|c| c.is_ascii_digit())).then_some(major)
    })
}

/// How WebKitGTK reads its own switches: set to anything but "0" turns the
/// path off.
pub fn var_disables(value: Option<&str>) -> bool {
    value.is_some_and(|v| v != "0")
}

/// The launch flag for Safe graphics mode.
pub const SAFE_GRAPHICS_FLAG: &str = "--safe-graphics";

/// Safe graphics mode asked for: the `--safe-graphics` flag, then
/// `PS5UPLOAD_SAFE_GRAPHICS` (any value but "0"; "0" also overrides the saved
/// setting), then the saved setting.
pub fn safe_mode_wanted(flag: bool, env: Option<&str>, saved: bool) -> bool {
    if flag {
        return true;
    }
    match env {
        Some(v) if !v.is_empty() => v != "0",
        _ => saved,
    }
}

/// The app's identifier (tauri.conf.json), which names its config folder.
pub const APP_IDENTIFIER: &str = "com.phantomptr.ps5upload";

/// The Safe graphics mode file: present means on. In the app's config folder
/// (Tauri's app_config_dir on Linux: `$XDG_CONFIG_HOME` or `~/.config`, then
/// the identifier), read before the webview exists, so plain path rules.
pub fn safe_mode_path(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = match xdg_config_home {
        Some(d) if d.starts_with('/') => PathBuf::from(d),
        _ => PathBuf::from(home.filter(|h| !h.is_empty())?).join(".config"),
    };
    Some(base.join(APP_IDENTIFIER).join("safe-graphics"))
}

/// What this launch ended up with, for the page and Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchGraphics {
    /// WebKit's GPU compositing is off for this launch (whoever set it).
    pub compositing_off: bool,
    pub dmabuf_off: bool,
    /// Safe graphics mode was on at launch (the setting or the variable).
    pub safe_mode: bool,
    /// The stack was judged current (no rescue needed).
    pub current_stack: bool,
    /// The app set at least one of the variables itself.
    pub set_by_app: bool,
}

static LAUNCH: OnceLock<LaunchGraphics> = OnceLock::new();

/// The page's flag (an init script on the main window): whether WebGL is read
/// back each frame here, so Remote Play draws with the 2D canvas.
pub fn page_flag_script() -> String {
    let l = LAUNCH.get().copied().unwrap_or_default();
    format!(
        "window.__PS5UPLOAD_LINUX_GRAPHICS__={};",
        serde_json::json!({ "compositingOff": l.compositing_off })
    )
}

/// Decide and set the WebKit variables for this launch. Single-threaded at
/// this point (no Tauri runtime, no engine thread yet), so set_var is sound.
#[cfg(target_os = "linux")]
pub fn apply() {
    let session = probe();
    let saved = saved_path().is_some_and(|p| p.exists());
    let flag = std::env::args_os().skip(1).any(|a| a == SAFE_GRAPHICS_FLAG);
    let safe = safe_mode_wanted(
        flag,
        std::env::var("PS5UPLOAD_SAFE_GRAPHICS").ok().as_deref(),
        saved,
    );
    let mut set_by_app = false;
    let mut done = Vec::new();
    for (key, value) in webkit_workarounds(session, safe) {
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, value);
            set_by_app = true;
            done.push(format!("{key}={value}"));
        } else {
            done.push(format!("{key} left as set"));
        }
    }
    let env = |k: &str| std::env::var(k).ok();
    let launch = LaunchGraphics {
        compositing_off: var_disables(env("WEBKIT_DISABLE_COMPOSITING_MODE").as_deref()),
        dmabuf_off: var_disables(env("WEBKIT_DISABLE_DMABUF_RENDERER").as_deref()),
        safe_mode: safe,
        current_stack: current_stack(session),
        set_by_app,
    };
    // In every log and bug report: what was read and what was decided.
    eprintln!(
        "[linux] graphics: webkitgtk {}, nvidia {} (driver {}), {}, appimage {}, safe mode {} \
         (flag {flag}, saved {saved}) -> {} [{}]; WEBKIT_DISABLE_COMPOSITING_MODE={:?} \
         WEBKIT_DISABLE_DMABUF_RENDERER={:?}",
        session
            .webkit
            .map_or("unknown".into(), |(a, b)| format!("{a}.{b}")),
        session.nvidia,
        session
            .nvidia_driver
            .map_or("unknown".into(), |v| v.to_string()),
        if session.wayland { "wayland" } else { "x11" },
        session.appimage,
        safe,
        if launch.current_stack && !safe {
            "current stack, no rescues"
        } else {
            "rescues"
        },
        done.join(", "),
        env("WEBKIT_DISABLE_COMPOSITING_MODE"),
        env("WEBKIT_DISABLE_DMABUF_RENDERER"),
    );
    let _ = LAUNCH.set(launch);
}

/// Settings › Graphics: what this launch uses, and the saved switch.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphicsStatus {
    /// The Linux app (the only one with these switches).
    pub linux: bool,
    /// Safe graphics mode is saved on (takes effect at the next launch).
    pub saved: bool,
    pub launch: LaunchGraphics,
}

/// Where Safe graphics mode is saved for this user.
fn saved_path() -> Option<PathBuf> {
    safe_mode_path(
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

#[tauri::command]
pub fn linux_graphics_get() -> GraphicsStatus {
    GraphicsStatus {
        linux: cfg!(target_os = "linux"),
        saved: saved_path().is_some_and(|p| p.exists()),
        launch: LAUNCH.get().copied().unwrap_or_default(),
    }
}

/// Save Safe graphics mode on or off. Read at the next launch: the variables
/// must be set before the webview starts.
#[tauri::command]
pub fn linux_graphics_safe_mode_set(on: bool) -> Result<GraphicsStatus, String> {
    if !cfg!(target_os = "linux") {
        return Err("Safe graphics mode is for the Linux app".into());
    }
    let path = saved_path().ok_or("no home folder")?;
    if on {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&path, b"1\n").map_err(|e| format!("{}: {e}", path.display()))?;
    } else if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(format!("{}: {e}", path.display()));
        }
    }
    Ok(linux_graphics_get())
}

/// The environment variable a re-exec carries so it never happens twice.
pub const PRELOADED_MARK: &str = "PS5UPLOAD_WAYLAND_PRELOADED";

/// The system's `libwayland-client.so.0` from `ldconfig -p` output, or None.
pub fn system_wayland_from_ldconfig(output: &str) -> Option<String> {
    output
        .lines()
        .find(|l| l.contains("libwayland-client.so.0"))
        .and_then(|l| l.rsplit_once("=> "))
        .map(|(_, p)| p.trim().to_string())
        .filter(|p| p.starts_with('/'))
}

/// Whether to restart this process once with the system's libwayland-client preloaded, and
/// with which library. The AppImage carries its own copy; on Wayland + NVIDIA one older than the
/// compositor leaves the window white (issue #285). The Linux .zip's PS5Upload.sh does this
/// before launch; doing it here makes the bare .AppImage (and launchers that run it directly,
/// such as Shelly) the same. Never when the user set LD_PRELOAD, and never twice.
pub fn wayland_preload(
    session: GraphicsSession,
    in_appimage: bool,
    ld_preload_set: bool,
    already: bool,
    system_lib: Option<&str>,
) -> Option<String> {
    if !(session.wayland && session.nvidia && in_appimage) || ld_preload_set || already {
        return None;
    }
    system_lib.map(str::to_string)
}

/// Restart once with the system's libwayland-client preloaded, when `wayland_preload` says so
/// (the same process: exec replaces it, so the AppImage runtime still waits for it). Returns only
/// when nothing needed doing or the exec failed.
#[cfg(target_os = "linux")]
pub fn reexec_with_system_wayland() {
    use std::os::unix::process::CommandExt;
    let ld_set = std::env::var_os("LD_PRELOAD").is_some_and(|v| !v.is_empty());
    let already = std::env::var_os(PRELOADED_MARK).is_some();
    let in_appimage = std::env::var_os("APPIMAGE").is_some();
    let session = probe();
    if !(session.wayland && session.nvidia && in_appimage) || ld_set || already {
        return;
    }
    let lib = ["ldconfig", "/sbin/ldconfig", "/usr/sbin/ldconfig"]
        .iter()
        .find_map(|ldc| {
            std::process::Command::new(ldc)
                .arg("-p")
                .output()
                .ok()
                .and_then(|o| system_wayland_from_ldconfig(&String::from_utf8_lossy(&o.stdout)))
        });
    let Some(lib) = wayland_preload(session, in_appimage, ld_set, already, lib.as_deref()) else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let err = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env("LD_PRELOAD", &lib)
        .env(PRELOADED_MARK, "1")
        .exec();
    eprintln!("[linux] could not restart with {lib} preloaded: {err}");
}

/// Probe the real session. Kept apart from the decision logic above so the
/// decisions stay testable without a Linux box, an NVIDIA card and a
/// compositor.
#[cfg(target_os = "linux")]
pub fn probe() -> GraphicsSession {
    GraphicsSession {
        wayland: detect_wayland(
            std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
            std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        ),
        nvidia: nvidia_driver_loaded(),
        nvidia_driver: ["/sys/module/nvidia/version", "/proc/driver/nvidia/version"]
            .iter()
            .find_map(|p| {
                std::fs::read_to_string(p)
                    .ok()
                    .as_deref()
                    .and_then(nvidia_major)
            }),
        webkit: webkit_version(),
        appimage: std::env::var_os("APPIMAGE").is_some(),
    }
}

/// The WebKitGTK this process is linked with, asked of the library itself (so
/// the AppImage's bundled copy reports its own version). Needs no GTK init.
#[cfg(target_os = "linux")]
fn webkit_version() -> Option<(u32, u32)> {
    // SAFETY: plain getters of constants in the already-loaded library.
    let (major, minor) = unsafe {
        (
            webkit2gtk_sys::webkit_get_major_version(),
            webkit2gtk_sys::webkit_get_minor_version(),
        )
    };
    (major > 0).then_some((major, minor))
}

/// Whether the proprietary NVIDIA kernel module is loaded.
///
/// Both paths are created by the proprietary driver and by nothing else, and
/// reading them needs no libraries, no X/Wayland connection and no GPU query
/// — this runs before the window exists.
#[cfg(target_os = "linux")]
fn nvidia_driver_loaded() -> bool {
    std::path::Path::new("/proc/driver/nvidia/version").exists()
        || std::path::Path::new("/sys/module/nvidia").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_appimage_preloads_the_system_wayland_only_where_it_must() {
        let nv_wl = GraphicsSession {
            wayland: true,
            nvidia: true,
            ..Default::default()
        };
        let lib = Some("/usr/lib/libwayland-client.so.0");
        assert_eq!(
            wayland_preload(nv_wl, true, false, false, lib).as_deref(),
            lib
        );
        // Not an AppImage (deb, rpm): its own libraries are the system's.
        assert_eq!(wayland_preload(nv_wl, false, false, false, lib), None);
        // The user's own LD_PRELOAD wins; and never twice.
        assert_eq!(wayland_preload(nv_wl, true, true, false, lib), None);
        assert_eq!(wayland_preload(nv_wl, true, false, true, lib), None);
        // Other stacks, or no system copy found.
        assert_eq!(
            wayland_preload(
                GraphicsSession {
                    wayland: true,
                    nvidia: false,
                    ..Default::default()
                },
                true,
                false,
                false,
                lib
            ),
            None
        );
        assert_eq!(
            wayland_preload(
                GraphicsSession {
                    wayland: false,
                    nvidia: true,
                    ..Default::default()
                },
                true,
                false,
                false,
                lib
            ),
            None
        );
        assert_eq!(wayland_preload(nv_wl, true, false, false, None), None);
    }

    #[test]
    fn the_system_wayland_is_read_from_ldconfig() {
        let out = "\t1234 libs found in cache `/etc/ld.so.cache'\n\
            \tlibwayland-cursor.so.0 (libc6,x86-64) => /usr/lib/libwayland-cursor.so.0\n\
            \tlibwayland-client.so.0 (libc6,x86-64) => /usr/lib/libwayland-client.so.0\n";
        assert_eq!(
            system_wayland_from_ldconfig(out).as_deref(),
            Some("/usr/lib/libwayland-client.so.0")
        );
        assert_eq!(system_wayland_from_ldconfig("nothing here"), None);
    }

    #[test]
    fn wayland_detected_from_session_type() {
        assert!(detect_wayland(Some("wayland"), None));
        assert!(!detect_wayland(Some("x11"), None));
    }

    #[test]
    fn wayland_session_type_is_case_insensitive() {
        // Set by the display manager; casing is not guaranteed.
        assert!(detect_wayland(Some("Wayland"), None));
    }

    #[test]
    fn wayland_detected_from_display_socket_alone() {
        // A session started outside a display manager may leave
        // XDG_SESSION_TYPE unset while WAYLAND_DISPLAY is present.
        assert!(detect_wayland(None, Some("wayland-0")));
    }

    #[test]
    fn empty_wayland_display_does_not_count() {
        // An exported-but-empty var means no socket, not a Wayland session.
        assert!(!detect_wayland(None, Some("")));
    }

    #[test]
    fn no_wayland_signals_means_x11() {
        assert!(!detect_wayland(None, None));
    }

    #[test]
    fn session_type_x11_wins_over_a_stale_wayland_display() {
        // XDG_SESSION_TYPE is the authoritative answer when present; a
        // leftover WAYLAND_DISPLAY inherited from a parent shell is not.
        assert!(!detect_wayland(Some("x11"), Some("wayland-0")));
    }

    /// An old stack: the WebKitGTK and NVIDIA versions unknown.
    fn old(wayland: bool, nvidia: bool) -> GraphicsSession {
        GraphicsSession {
            wayland,
            nvidia,
            ..Default::default()
        }
    }

    /// The user's Ubuntu 26.04 PC: NVIDIA 595 on Wayland, WebKitGTK 2.52, from the .deb.
    fn current_nvidia_wayland() -> GraphicsSession {
        GraphicsSession {
            wayland: true,
            nvidia: true,
            nvidia_driver: Some(595),
            webkit: Some((2, 52)),
            appimage: false,
        }
    }

    #[test]
    fn an_old_or_unknown_stack_disables_the_dmabuf_renderer() {
        // The old default, kept wherever the stack is not known to be current.
        for &(wayland, nvidia) in &[(false, false), (true, false), (false, true), (true, true)] {
            let vars = webkit_workarounds(old(wayland, nvidia), false);
            assert!(
                vars.iter()
                    .any(|&(k, v)| k == "WEBKIT_DISABLE_DMABUF_RENDERER" && v == "1"),
                "missing DMABUF rescue for wayland={wayland} nvidia={nvidia}"
            );
        }
    }

    #[test]
    fn old_nvidia_on_wayland_also_disables_compositing() {
        // Issue #285: DMABUF alone was not enough on CachyOS + NVIDIA +
        // Wayland; the window stayed white until compositing was off too.
        let vars = webkit_workarounds(old(true, true), false);
        assert!(vars
            .iter()
            .any(|&(k, v)| k == "WEBKIT_DISABLE_COMPOSITING_MODE" && v == "1"));
        // A driver before explicit sync on a current WebKitGTK: still old.
        let s = GraphicsSession {
            nvidia_driver: Some(550),
            ..current_nvidia_wayland()
        };
        assert_eq!(webkit_workarounds(s, false).len(), 2);
        // A current driver on a WebKitGTK before 2.46: still old.
        let s = GraphicsSession {
            webkit: Some((2, 44)),
            ..current_nvidia_wayland()
        };
        assert_eq!(webkit_workarounds(s, false).len(), 2);
    }

    #[test]
    fn compositing_stays_on_for_everyone_else() {
        // Disabling it drops the whole page to software rendering, which
        // this app feels on scrolling — its main screens are long lists.
        // That regression is why it is not a blanket default, so it must
        // stay off for stacks that do not need it.
        for &(wayland, nvidia) in &[(false, false), (true, false), (false, true)] {
            let vars = webkit_workarounds(old(wayland, nvidia), false);
            assert!(
                !vars
                    .iter()
                    .any(|&(k, _)| k == "WEBKIT_DISABLE_COMPOSITING_MODE"),
                "compositing must not be disabled for wayland={wayland} nvidia={nvidia}"
            );
        }
    }

    const DMABUF: (&str, &str) = ("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    const COMPOSITING: (&str, &str) = ("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    type Vars = Vec<(&'static str, &'static str)>;

    #[test]
    fn a_current_stack_needs_no_rescue() {
        assert!(webkit_workarounds(current_nvidia_wayland(), false).is_empty());
        // Exactly the thresholds.
        let s = GraphicsSession {
            nvidia_driver: Some(NVIDIA_EXPLICIT_SYNC),
            webkit: Some(WEBKIT_CURRENT),
            ..current_nvidia_wayland()
        };
        assert!(webkit_workarounds(s, false).is_empty());
    }

    #[test]
    fn only_the_measured_stack_changes_the_old_default() {
        // (session, expected) for every stack the app meets. Only NVIDIA
        // 555+ on Wayland with WebKitGTK 2.46+ outside the AppImage was shown
        // to work without the rescues; everything else keeps them exactly.
        let new_webkit = Some((2, 52));
        let old_webkit = Some((2, 44));
        let s = |wayland, nvidia, nvidia_driver, webkit| GraphicsSession {
            wayland,
            nvidia,
            nvidia_driver,
            webkit,
            appimage: false,
        };
        let matrix: Vec<(&str, GraphicsSession, Vars)> = vec![
            (
                "nvidia new, wayland",
                s(true, true, Some(595), new_webkit),
                vec![],
            ),
            (
                "nvidia 555, wayland",
                s(true, true, Some(555), new_webkit),
                vec![],
            ),
            (
                "nvidia 550, wayland",
                s(true, true, Some(550), new_webkit),
                vec![DMABUF, COMPOSITING],
            ),
            (
                "nvidia old webkit",
                s(true, true, Some(595), old_webkit),
                vec![DMABUF, COMPOSITING],
            ),
            (
                "nvidia driver unread",
                s(true, true, None, new_webkit),
                vec![DMABUF, COMPOSITING],
            ),
            (
                "nvidia webkit unread",
                s(true, true, Some(595), None),
                vec![DMABUF, COMPOSITING],
            ),
            (
                "nvidia new, x11",
                s(false, true, Some(595), new_webkit),
                vec![DMABUF],
            ),
            (
                "nvidia old, x11",
                s(false, true, Some(535), old_webkit),
                vec![DMABUF],
            ),
            // AMD / Intel (Mesa): no NVIDIA driver, whatever the WebKitGTK.
            (
                "mesa wayland, new webkit",
                s(true, false, None, new_webkit),
                vec![DMABUF],
            ),
            (
                "mesa x11, new webkit",
                s(false, false, None, new_webkit),
                vec![DMABUF],
            ),
            (
                "mesa wayland, old webkit",
                s(true, false, None, old_webkit),
                vec![DMABUF],
            ),
            (
                "mesa, nothing read",
                s(true, false, None, None),
                vec![DMABUF],
            ),
        ];
        for (name, session, want) in matrix {
            assert_eq!(webkit_workarounds(session, false), want, "{name}");
            assert_eq!(current_stack(session), want.is_empty(), "{name}");
            // Safe graphics mode: both, on every one of them.
            assert_eq!(
                webkit_workarounds(session, true),
                vec![DMABUF, COMPOSITING],
                "{name} safe"
            );
        }
    }

    #[test]
    fn the_appimage_keeps_the_rescues() {
        // Its bundled libraries: #285 needed them on a current NVIDIA driver.
        let s = GraphicsSession {
            appimage: true,
            ..current_nvidia_wayland()
        };
        assert_eq!(
            webkit_workarounds(s, false),
            vec![
                ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
                ("WEBKIT_DISABLE_COMPOSITING_MODE", "1")
            ]
        );
    }

    #[test]
    fn safe_mode_sets_both_on_any_stack() {
        let both = vec![
            ("WEBKIT_DISABLE_DMABUF_RENDERER", "1"),
            ("WEBKIT_DISABLE_COMPOSITING_MODE", "1"),
        ];
        assert_eq!(webkit_workarounds(current_nvidia_wayland(), true), both);
        assert_eq!(webkit_workarounds(old(false, false), true), both);
    }

    #[test]
    fn safe_mode_from_the_flag_the_variable_or_the_setting() {
        assert!(!safe_mode_wanted(false, None, false));
        assert!(safe_mode_wanted(false, None, true));
        assert!(safe_mode_wanted(false, Some("1"), false));
        // "0" turns a saved setting off for this launch; empty says nothing.
        assert!(!safe_mode_wanted(false, Some("0"), true));
        assert!(safe_mode_wanted(false, Some(""), true));
        // The flag always turns it on.
        assert!(safe_mode_wanted(true, None, false));
        assert!(safe_mode_wanted(true, Some("0"), false));
    }

    #[test]
    fn webkit_reads_anything_but_zero_as_disable() {
        assert!(!var_disables(None));
        assert!(!var_disables(Some("0")));
        assert!(var_disables(Some("1")));
        assert!(var_disables(Some("")));
    }

    #[test]
    fn nvidia_version_is_read_from_either_file() {
        assert_eq!(nvidia_major("595.99.02\n"), Some(595));
        let proc_open = "NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  595.99.02  Release Build  (dvs-builder@U22-I3-AE18-23-3)  Thu Oct  2 2026\nGCC version:  gcc version 15.2.0 (Ubuntu 15.2.0-4ubuntu4)\n";
        assert_eq!(nvidia_major(proc_open), Some(595));
        let proc_old = "NVRM version: NVIDIA UNIX x86_64 Kernel Module  535.183.01  Sun May 12 19:39:15 UTC 2024\nGCC version:  gcc version 12.3.0\n";
        assert_eq!(nvidia_major(proc_old), Some(535));
        assert_eq!(nvidia_major(""), None);
        assert_eq!(nvidia_major("NVRM version: garbage x86_64"), None);
    }

    #[test]
    fn safe_mode_file_lives_in_the_app_config_folder() {
        assert_eq!(
            safe_mode_path(None, Some("/home/u")),
            Some(PathBuf::from(
                "/home/u/.config/com.phantomptr.ps5upload/safe-graphics"
            ))
        );
        assert_eq!(
            safe_mode_path(Some("/x/cfg"), Some("/home/u")),
            Some(PathBuf::from(
                "/x/cfg/com.phantomptr.ps5upload/safe-graphics"
            ))
        );
        // A relative XDG_CONFIG_HOME is invalid by the spec: ignored.
        assert_eq!(
            safe_mode_path(Some("cfg"), Some("/home/u")),
            Some(PathBuf::from(
                "/home/u/.config/com.phantomptr.ps5upload/safe-graphics"
            ))
        );
        assert_eq!(safe_mode_path(None, None), None);
        // The folder is named after the app's identifier.
        assert!(include_str!("../tauri.conf.json")
            .contains(&format!("\"identifier\": \"{APP_IDENTIFIER}\"")));
    }
}
