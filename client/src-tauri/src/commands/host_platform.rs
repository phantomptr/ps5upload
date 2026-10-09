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

#[cfg(test)]
mod tests {
    #[test]
    fn reports_the_build_target() {
        let p = super::current();
        assert_eq!(p.os, std::env::consts::OS);
        assert!(!p.arch.is_empty());
    }
}
