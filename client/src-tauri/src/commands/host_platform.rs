//! The OS and CPU this app was built for, which the web view cannot tell (an Intel Mac and an Apple
//! Silicon Mac send the same user agent). The bug report names the issue form's platform from it.
use serde::Serialize;

#[derive(Serialize, PartialEq, Debug)]
pub struct HostPlatform {
    pub os: &'static str,
    pub arch: &'static str,
}

pub(crate) fn current() -> HostPlatform {
    HostPlatform {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    }
}

#[tauri::command]
pub fn host_platform() -> HostPlatform {
    current()
}

/// This computer for a bug report: the OS's real name and version (WebKit's user agent is frozen
/// at "Mac OS X 10_15_7", so it comes from the OS: sysctl, the registry, /etc/os-release,
/// getprop), the machine's model, CPU, memory and uptime, and the web view's own version
/// (WebKit, WebView2 or WebKitGTK). Never a name, an address or a serial.
#[derive(Serialize, Debug)]
pub struct HostSystemInfo {
    #[serde(flatten)]
    pub host: ps5upload_core::host_info::HostInfo,
    pub webview_version: Option<String>,
    /// The app's own uptime: since this process started, in seconds.
    pub app_uptime_s: u64,
}

fn process_started() -> &'static std::time::Instant {
    static T: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    T.get_or_init(std::time::Instant::now)
}

/// Note when the app started (its uptime counts from here). Called once at startup.
pub(crate) fn mark_started() {
    let _ = process_started();
}

#[tauri::command]
pub async fn host_system_info() -> HostSystemInfo {
    // Windows reads its memory through PowerShell: off the main thread.
    let host = tokio::task::spawn_blocking(ps5upload_core::host_info::host_info)
        .await
        .unwrap_or_default();
    HostSystemInfo {
        host,
        webview_version: tauri::webview_version().ok(),
        app_uptime_s: process_started().elapsed().as_secs(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_the_build_target() {
        let p = super::current();
        assert_eq!(p.os, std::env::consts::OS);
        assert!(!p.arch.is_empty());
    }
}
