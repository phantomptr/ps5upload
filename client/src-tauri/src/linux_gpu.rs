//! Linux WebKitGTK white-screen rescues, chosen per graphics stack.
//!
//! WebKitGTK renders a blank/white window on several GPU + compositor
//! combinations. There is no single switch that fixes all of them, and the
//! switches are not free: disabling accelerated compositing drops the whole
//! page to software rendering, which this app feels acutely on scrolling
//! because its main screens are long lists (a library of 200 game rows, a
//! file browser of thousands of entries). That was shipped as a blanket
//! default once and had to be reverted after Linux users reported sluggish
//! scrolling.
//!
//! So the rescues are matched to the stack that needs them:
//!
//! - Every Linux session: disable the DMABUF renderer. Fixes the common case
//!   at no perceptible cost.
//! - NVIDIA on Wayland: also disable accelerated compositing. Issue #285
//!   (CachyOS) — DMABUF alone left the window white. Narrow enough that
//!   nobody else pays the scrolling cost.
//!
//! Everything here is only applied when the variable is unset, so a user can
//! always override in either direction.
//!
//! The decision logic is deliberately platform-independent so it is tested on
//! every CI runner, not only the Linux one — it is compiled everywhere and
//! called only on Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// What we could observe about the running graphics session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsSession {
    pub wayland: bool,
    /// The proprietary NVIDIA driver is loaded. Nouveau does not count: it
    /// does not show this failure and would pay the scrolling cost for
    /// nothing.
    pub nvidia: bool,
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
/// unset, so an explicit user choice always wins.
pub fn webkit_workarounds(session: GraphicsSession) -> Vec<(&'static str, &'static str)> {
    let mut vars = vec![("WEBKIT_DISABLE_DMABUF_RENDERER", "1")];
    if session.wayland && session.nvidia {
        vars.push(("WEBKIT_DISABLE_COMPOSITING_MODE", "1"));
    }
    vars
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
    }
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

    #[test]
    fn every_linux_session_disables_the_dmabuf_renderer() {
        // The existing default, kept for all stacks: it fixes the common
        // white window and costs nothing perceptible.
        for &(wayland, nvidia) in &[(false, false), (true, false), (false, true), (true, true)] {
            let vars = webkit_workarounds(GraphicsSession { wayland, nvidia });
            assert!(
                vars.iter()
                    .any(|&(k, v)| k == "WEBKIT_DISABLE_DMABUF_RENDERER" && v == "1"),
                "missing DMABUF rescue for wayland={wayland} nvidia={nvidia}"
            );
        }
    }

    #[test]
    fn nvidia_on_wayland_also_disables_compositing() {
        // Issue #285: DMABUF alone was not enough on CachyOS + NVIDIA +
        // Wayland; the window stayed white until compositing was off too.
        let vars = webkit_workarounds(GraphicsSession {
            wayland: true,
            nvidia: true,
        });
        assert!(vars
            .iter()
            .any(|&(k, v)| k == "WEBKIT_DISABLE_COMPOSITING_MODE" && v == "1"));
    }

    #[test]
    fn compositing_stays_on_for_everyone_else() {
        // Disabling it drops the whole page to software rendering, which
        // this app feels on scrolling — its main screens are long lists.
        // That regression is why it is not a blanket default, so it must
        // stay off for stacks that do not need it.
        for &(wayland, nvidia) in &[(false, false), (true, false), (false, true)] {
            let vars = webkit_workarounds(GraphicsSession { wayland, nvidia });
            assert!(
                !vars
                    .iter()
                    .any(|&(k, _)| k == "WEBKIT_DISABLE_COMPOSITING_MODE"),
                "compositing must not be disabled for wayland={wayland} nvidia={nvidia}"
            );
        }
    }
}
