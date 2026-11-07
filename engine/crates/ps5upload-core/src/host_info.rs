//! This computer, for a bug report: the OS's name and real version (a web view's user agent
//! can't say: WebKit's is frozen at "Mac OS X 10_15_7"), the machine's model, CPU and memory, and
//! how long it has been up. Read from the OS itself (sysctl on Apple systems, /proc and
//! /etc/os-release on Linux, getprop on Android, the registry on Windows). Every value is
//! best-effort: None when it can't be read. Nothing here names the user, the computer or an
//! address.

use std::sync::OnceLock;

use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct HostInfo {
    /// `macos`, `windows`, `linux`, `android`, `ios`.
    pub os: &'static str,
    pub arch: &'static str,
    /// The marketing name: "macOS", "Windows 11 Pro", "Ubuntu 24.04.1 LTS", "Android".
    pub os_name: Option<String>,
    /// The version people say: "15.6.1", "24H2 (26100)", "14".
    pub os_version: Option<String>,
    /// The kernel's own: Darwin "24.6.0", Linux "6.8.0-45-generic", an OS build.
    pub kernel: Option<String>,
    /// "MacBookPro18,3", "iPhone15,2", "Dell Inc. XPS 13 9310", "Google Pixel 8".
    pub machine_model: Option<String>,
    pub cpu_model: Option<String>,
    pub cpu_threads: Option<usize>,
    pub memory_bytes: Option<u64>,
    /// Since the computer started.
    pub system_uptime_s: Option<u64>,
}

/// Everything but the uptime is read once: it does not change while the app runs.
pub fn host_info() -> HostInfo {
    static STATIC: OnceLock<HostInfo> = OnceLock::new();
    let mut h = STATIC.get_or_init(read_static).clone();
    h.system_uptime_s = system_uptime_s();
    h
}

fn read_static() -> HostInfo {
    let (os_name, os_version, kernel) = os_version();
    HostInfo {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        os_name,
        os_version,
        kernel,
        machine_model: machine_model(),
        cpu_model: cpu_model(),
        cpu_threads: std::thread::available_parallelism().map(|n| n.get()).ok(),
        memory_bytes: memory_bytes(),
        system_uptime_s: None,
    }
}

fn clean(s: impl AsRef<str>) -> Option<String> {
    let s = s.as_ref().trim().trim_matches('\0').trim();
    (!s.is_empty()).then(|| s.split_whitespace().collect::<Vec<_>>().join(" "))
}

// ---- Apple ----

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn sysctl_bytes(name: &str) -> Option<Vec<u8>> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut len: libc::size_t = 0;
    // SAFETY: a size query with a null buffer, then a read into a buffer of that size.
    unsafe {
        if libc::sysctlbyname(
            cname.as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        ) != 0
            || len == 0
        {
            return None;
        }
        let mut buf = vec![0u8; len];
        if libc::sysctlbyname(
            cname.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
        buf.truncate(len);
        Some(buf)
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn sysctl_string(name: &str) -> Option<String> {
    clean(String::from_utf8_lossy(&sysctl_bytes(name)?))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn os_version() -> (Option<String>, Option<String>, Option<String>) {
    let name = if cfg!(target_os = "ios") {
        "iOS"
    } else {
        "macOS"
    };
    let version =
        sysctl_string("kern.osproductversion").map(|v| match sysctl_string("kern.osversion") {
            Some(build) => format!("{v} ({build})"),
            None => v,
        });
    (
        Some(name.to_string()),
        version,
        sysctl_string("kern.osrelease").map(|r| format!("Darwin {r}")),
    )
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn machine_model() -> Option<String> {
    // On a Mac hw.model is "MacBookPro18,3"; on an iPhone it is the board, and hw.machine the model.
    if cfg!(target_os = "ios") {
        sysctl_string("hw.machine")
    } else {
        sysctl_string("hw.model")
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn cpu_model() -> Option<String> {
    sysctl_string("machdep.cpu.brand_string")
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn memory_bytes() -> Option<u64> {
    let b = sysctl_bytes("hw.memsize")?;
    Some(u64::from_ne_bytes(b.get(..8)?.try_into().ok()?))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn system_uptime_s() -> Option<u64> {
    let b = sysctl_bytes("kern.boottime")?;
    // struct timeval: the seconds come first, as a time_t (8 bytes on every Apple target).
    let secs = i64::from_ne_bytes(b.get(..8)?.try_into().ok()?);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    (secs > 0 && now >= secs).then(|| (now - secs) as u64)
}

// ---- Linux and Android ----

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(target_os = "android")]
fn getprop(name: &str) -> Option<String> {
    let out = std::process::Command::new("getprop")
        .arg(name)
        .output()
        .ok()?;
    clean(String::from_utf8_lossy(&out.stdout))
}

/// `PRETTY_NAME` (else `NAME` + `VERSION`) out of /etc/os-release's text.
pub fn os_release_name(text: &str) -> Option<String> {
    let field = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .map(|v| v.trim().trim_matches('"').to_string())
            .and_then(clean)
    };
    field("PRETTY_NAME").or_else(|| match (field("NAME"), field("VERSION")) {
        (Some(n), Some(v)) => Some(format!("{n} {v}")),
        (n, _) => n,
    })
}

#[cfg(target_os = "linux")]
fn os_version() -> (Option<String>, Option<String>, Option<String>) {
    let text = read("/etc/os-release").or_else(|| read("/usr/lib/os-release"));
    let version = text.as_deref().and_then(|t| {
        t.lines()
            .find_map(|l| l.strip_prefix("VERSION_ID="))
            .and_then(|v| clean(v.trim_matches('"')))
    });
    (
        text.as_deref().and_then(os_release_name),
        version,
        read("/proc/sys/kernel/osrelease")
            .and_then(clean)
            .map(|k| format!("Linux {k}")),
    )
}

#[cfg(target_os = "android")]
fn os_version() -> (Option<String>, Option<String>, Option<String>) {
    let version =
        getprop("ro.build.version.release").map(|v| match getprop("ro.build.version.sdk") {
            Some(sdk) => format!("{v} (API {sdk})"),
            None => v,
        });
    (
        Some("Android".to_string()),
        version,
        read("/proc/sys/kernel/osrelease")
            .and_then(clean)
            .map(|k| format!("Linux {k}")),
    )
}

#[cfg(target_os = "linux")]
fn machine_model() -> Option<String> {
    let dmi = |f: &str| read(&format!("/sys/devices/virtual/dmi/id/{f}")).and_then(clean);
    match (dmi("sys_vendor"), dmi("product_name")) {
        (Some(v), Some(p)) if !p.starts_with(&v) => Some(format!("{v} {p}")),
        (_, Some(p)) => Some(p),
        _ => read("/proc/device-tree/model").and_then(clean),
    }
}

#[cfg(target_os = "android")]
fn machine_model() -> Option<String> {
    match (
        getprop("ro.product.manufacturer"),
        getprop("ro.product.model"),
    ) {
        (Some(m), Some(p)) if !p.to_lowercase().starts_with(&m.to_lowercase()) => {
            Some(format!("{m} {p}"))
        }
        (_, p) => p,
    }
}

/// The CPU's name in /proc/cpuinfo's text (`model name` on x86, `Hardware` or `Model` on ARM).
pub fn cpuinfo_model(text: &str) -> Option<String> {
    ["model name", "Hardware", "Model", "cpu model", "Processor"]
        .iter()
        .find_map(|k| {
            text.lines().find_map(|l| {
                let (key, v) = l.split_once(':')?;
                (key.trim() == *k).then(|| clean(v)).flatten()
            })
        })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn cpu_model() -> Option<String> {
    read("/proc/cpuinfo").as_deref().and_then(cpuinfo_model)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn memory_bytes() -> Option<u64> {
    let text = read("/proc/meminfo")?;
    let kb: u64 = text
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kb * 1024)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn system_uptime_s() -> Option<u64> {
    let text = read("/proc/uptime")?;
    let secs: f64 = text.split_whitespace().next()?.parse().ok()?;
    Some(secs as u64)
}

// ---- Windows ----

#[cfg(windows)]
fn reg_value(key: &str, name: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("reg")
        .args(["query", key, "/v", name])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    reg_query_value(&String::from_utf8_lossy(&out.stdout), name)
}

/// The value of `name` in `reg query`'s output: `    ProductName    REG_SZ    Windows 10 Pro`.
pub fn reg_query_value(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix(name)?.trim_start();
        let (_ty, value) = rest.split_once(char::is_whitespace)?;
        clean(value)
    })
}

#[cfg(windows)]
fn os_version() -> (Option<String>, Option<String>, Option<String>) {
    const NT: &str = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build = reg_value(NT, "CurrentBuild");
    let ubr = reg_value(NT, "UBR").and_then(|v| {
        // REG_DWORD prints as 0x...
        u64::from_str_radix(v.trim_start_matches("0x"), 16)
            .ok()
            .map(|n| n.to_string())
    });
    // Windows 11 still says "Windows 10" in ProductName: the build number tells them apart.
    let mut name = reg_value(NT, "ProductName");
    if let (Some(n), Some(b)) = (&name, build.as_deref().and_then(|b| b.parse::<u32>().ok())) {
        if b >= 22000 && n.contains("Windows 10") {
            name = Some(n.replace("Windows 10", "Windows 11"));
        }
    }
    let display = reg_value(NT, "DisplayVersion");
    let version = match (display, &build) {
        (Some(d), Some(b)) => Some(format!("{d} ({b})")),
        (None, Some(b)) => Some(b.clone()),
        (d, None) => d,
    };
    let kernel = match (build, ubr) {
        (Some(b), Some(u)) => Some(format!("NT 10.0.{b}.{u}")),
        (Some(b), None) => Some(format!("NT 10.0.{b}")),
        _ => None,
    };
    (name, version, kernel)
}

#[cfg(windows)]
fn machine_model() -> Option<String> {
    const BIOS: &str = r"HKLM\HARDWARE\DESCRIPTION\System\BIOS";
    match (
        reg_value(BIOS, "SystemManufacturer"),
        reg_value(BIOS, "SystemProductName"),
    ) {
        (Some(m), Some(p)) if !p.starts_with(&m) => Some(format!("{m} {p}")),
        (_, p) => p,
    }
}

#[cfg(windows)]
fn cpu_model() -> Option<String> {
    reg_value(
        r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0",
        "ProcessorNameString",
    )
}

#[cfg(windows)]
fn memory_bytes() -> Option<u64> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[cfg(windows)]
fn system_uptime_s() -> Option<u64> {
    None
}

// ---- anything else ----

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    windows
)))]
mod other {
    pub(super) fn os_version() -> (Option<String>, Option<String>, Option<String>) {
        (None, None, None)
    }
    pub(super) fn machine_model() -> Option<String> {
        None
    }
    pub(super) fn cpu_model() -> Option<String> {
        None
    }
    pub(super) fn memory_bytes() -> Option<u64> {
        None
    }
    pub(super) fn system_uptime_s() -> Option<u64> {
        None
    }
}
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    windows
)))]
use other::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_computer_says_something() {
        let h = host_info();
        assert_eq!(h.os, std::env::consts::OS);
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(h.os_version.is_some(), "{h:?}");
            assert!(h.memory_bytes.is_some_and(|m| m > 256 << 20), "{h:?}");
            assert!(h.system_uptime_s.is_some(), "{h:?}");
        }
        if cfg!(target_os = "macos") {
            // Not the user agent's frozen 10.15.7.
            assert!(!h.os_version.unwrap().starts_with("10.15.7"));
            assert!(h.machine_model.is_some());
        }
    }

    #[test]
    fn os_release_and_cpuinfo() {
        let t = "NAME=\"Ubuntu\"\nVERSION=\"24.04.1 LTS (Noble Numbat)\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nVERSION_ID=\"24.04\"\n";
        assert_eq!(os_release_name(t).as_deref(), Some("Ubuntu 24.04.1 LTS"));
        assert_eq!(
            os_release_name("NAME=Arch\nVERSION=rolling\n").as_deref(),
            Some("Arch rolling")
        );
        let c = "processor\t: 0\nmodel name\t: AMD Ryzen 7  5800X 8-Core Processor\n";
        assert_eq!(
            cpuinfo_model(c).as_deref(),
            Some("AMD Ryzen 7 5800X 8-Core Processor")
        );
        assert_eq!(
            cpuinfo_model("Hardware\t: Qualcomm SM8550\n").as_deref(),
            Some("Qualcomm SM8550")
        );
    }

    #[test]
    fn reg_query_output() {
        let out = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\r\n    ProductName    REG_SZ    Windows 10 Pro\r\n\r\n";
        assert_eq!(
            reg_query_value(out, "ProductName").as_deref(),
            Some("Windows 10 Pro")
        );
        assert_eq!(reg_query_value(out, "UBR"), None);
    }
}
