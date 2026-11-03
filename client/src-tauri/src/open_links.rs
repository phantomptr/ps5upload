//! `ps5upload://` links opened with the app (tauri-plugin-deep-link), on every platform.
//!
//! A Play Together invite page offers "Open in the PS5Upload app", a
//! `ps5upload://join?link=<the invite>` link. The system hands it to the app: at launch
//! (`get_current`) or while it runs (`on_open_url`, which on Windows and Linux arrives through
//! the single-instance plugin's `deep-link` feature). Each link is kept here until the page
//! takes it with `open_links_take`, and `ps5upload-open-link` tells a page that is already
//! listening; the page subscribes first and then takes, so nothing is lost before it loads.
//! The page checks what a link asks for; nothing happens here beyond keeping the text.

use std::sync::Mutex;

/// Event the page listens for; the links themselves come from `open_links_take`.
pub const EVENT: &str = "ps5upload-open-link";

/// Our scheme; anything else the system hands over is ignored.
const SCHEME: &str = "ps5upload";
/// Longer than any invite link; a bigger one is not ours.
const MAX_LEN: usize = 4096;
/// Links held for a page that hasn't loaded yet.
const MAX_HELD: usize = 8;

static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn pending() -> std::sync::MutexGuard<'static, Vec<String>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keeps our links among `urls` for the page. True when there was anything new.
pub fn push<'a>(urls: impl IntoIterator<Item = &'a tauri::Url>) -> bool {
    let mut held = pending();
    let mut added = false;
    for u in urls {
        let text = u.as_str();
        if u.scheme() != SCHEME || text.len() > MAX_LEN || held.iter().any(|h| h == text) {
            continue;
        }
        if held.len() >= MAX_HELD {
            held.remove(0);
        }
        held.push(text.to_string());
        added = true;
    }
    added
}

/// Hands the page the links that arrived since it last asked, once. The first time it also asks
/// the plugin for the launch link again: on Android the start-up `get_current` can run before the
/// activity's intent has reached the plugin (a cold start from a link landed on Home).
#[tauri::command]
pub fn open_links_take<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Vec<String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tauri_plugin_deep_link::DeepLinkExt;
    static ASKED: AtomicBool = AtomicBool::new(false);
    if !ASKED.swap(true, Ordering::Relaxed) {
        if let Ok(Some(urls)) = app.deep_link().get_current() {
            push(&urls);
        }
    }
    take()
}

fn take() -> Vec<String> {
    std::mem::take(&mut *pending())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> tauri::Url {
        tauri::Url::parse(s).unwrap()
    }

    #[test]
    fn keeps_our_links_once_and_hands_them_over_once() {
        let _ = take();
        let a =
            url("ps5upload://join?link=http%3A%2F%2F192.168.1.20%3A19113%2Fjoin%23i%3Dabcdefgh");
        let other = url("https://example.com/join");
        assert!(push([&a, &other, &a]));
        assert!(!push([&a]));
        assert_eq!(take(), vec![a.as_str().to_string()]);
        assert!(take().is_empty());
        let long = url(&format!("ps5upload://join?link={}", "x".repeat(MAX_LEN)));
        assert!(!push([&long]));
        for i in 0..MAX_HELD + 2 {
            push([&url(&format!("ps5upload://join?n={i}"))]);
        }
        let held = take();
        assert_eq!(held.len(), MAX_HELD);
        assert!(held[0].ends_with("n=2"));
    }
}
