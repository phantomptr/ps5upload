//! The note a Windows user sees when they double-click the engine-only exe from the release page.
//!
//! Release builds of the engine have no console on Windows (`windows_subsystem = "windows"` in
//! main.rs), so a double-click used to start an invisible server and nothing else: people took it
//! for the app and reported that "the app doesn't run". When the engine was started by hand from
//! Explorer (or straight from a browser's download list or an archive tool), it now says what it
//! is and where the app is, and lets the person open its page or go to the release page.
//!
//! Never shown when anything set the engine up: the desktop app (it sets `PS5UPLOAD_PARENT_WATCH`
//! and `PS5UPLOAD_ENGINE_PORT`), a scheduled task or service (`scripts/autolaunch` sets
//! `PS5UPLOAD_ENGINE_PORT`; services are started by services.exe, tasks by svchost.exe), a
//! terminal (cmd.exe / powershell.exe are not launchers here), or any command-line argument. A
//! Startup-folder shortcut does run under explorer.exe: give it any argument and it stays quiet.

// The decision and the text are used only by the Windows code below, and by the tests everywhere.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

/// Processes that start a program because a person opened it by hand.
const HAND_LAUNCHERS: &[&str] = &[
    "explorer.exe",
    "msedge.exe",
    "chrome.exe",
    "firefox.exe",
    "brave.exe",
    "opera.exe",
    "vivaldi.exe",
    "7zfm.exe",
    "winrar.exe",
];

/// Whether this start looks like a double-click rather than a set-up launch. Pure, so it is
/// tested on every OS: `arg_count` excludes the program name, `env_names` are the variable names
/// of this process, and `parent_exe` is the parent process's executable name (a path is fine).
pub(crate) fn started_by_hand<'a>(
    arg_count: usize,
    env_names: impl IntoIterator<Item = &'a str>,
    parent_exe: Option<&str>,
) -> bool {
    if arg_count > 0 {
        return false;
    }
    let configured = env_names.into_iter().any(|name| {
        let name = name.to_ascii_uppercase();
        name.starts_with("PS5UPLOAD_") || name == "PS5_ADDR"
    });
    if configured {
        return false;
    }
    let Some(parent) = parent_exe else {
        return false;
    };
    let file = parent.rsplit(['\\', '/']).next().unwrap_or(parent);
    HAND_LAUNCHERS
        .iter()
        .any(|launcher| file.eq_ignore_ascii_case(launcher))
}

/// The installer to point at: this build's own architecture.
pub(crate) fn installer_suffix() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "-win-arm64-setup.exe"
    } else {
        "-win-x64-setup.exe"
    }
}

pub(crate) fn notice_text(port: u16) -> String {
    format!(
        "This is the PS5Upload engine on its own, for servers and advanced setups. It has no \
         window of its own.\n\n\
         To use the PS5Upload app, download the installer from the release page: the file \
         ending in {installer} (or the .zip to run it without installing).\n\n\
         Yes: open the engine's page (http://127.0.0.1:{port}/) and keep it running.\n\
         No: open the release page and close the engine.\n\
         Cancel: close the engine.",
        installer = installer_suffix(),
    )
}

pub(crate) const RELEASES_URL: &str = "https://github.com/phantomptr/ps5upload/releases/latest";

/// Shows the note when the engine was double-clicked on Windows. Returns `false` when the engine
/// should exit instead of starting. Anywhere else, and for every set-up launch, it returns `true`
/// at once.
pub fn double_click_notice() -> bool {
    #[cfg(target_os = "windows")]
    {
        win::notice()
    }
    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

#[cfg(target_os = "windows")]
mod win {
    use std::ffi::c_void;

    const TH32CS_SNAPPROCESS: u32 = 0x2;
    const INVALID_HANDLE_VALUE: isize = -1;
    const MB_YESNOCANCEL: u32 = 0x3;
    const MB_ICONINFORMATION: u32 = 0x40;
    const MB_SETFOREGROUND: u32 = 0x1_0000;
    const IDYES: i32 = 6;
    const IDNO: i32 = 7;
    const SW_SHOWNORMAL: i32 = 1;
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const COINIT_DISABLE_OLE1DDE: u32 = 0x4;

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; 260],
    }

    // PROCESSENTRY32W is 568 bytes on 64-bit Windows (556 on 32-bit); a wrong layout here would
    // make Process32FirstW fail or read garbage.
    const _: () = assert!(
        std::mem::size_of::<ProcessEntry32W>()
            == if cfg!(target_pointer_width = "64") {
                568
            } else {
                556
            }
    );

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> isize;
        fn Process32FirstW(snapshot: isize, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: isize, entry: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(handle: isize) -> i32;
        fn GetCurrentProcessId() -> u32;
    }

    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(hwnd: *mut c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> *mut c_void;
    }

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Every (pid, parent pid, exe name) in a process snapshot.
    fn processes() -> Vec<(u32, u32, String)> {
        let mut out = Vec::new();
        // SAFETY: plain Win32 calls; the entry is sized as the API requires and the handle is
        // closed on every path.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE || snap == 0 {
                return out;
            }
            let mut entry: ProcessEntry32W = std::mem::zeroed();
            entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut entry);
            while ok != 0 {
                let len = entry
                    .sz_exe_file
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.sz_exe_file.len());
                out.push((
                    entry.th32_process_id,
                    entry.th32_parent_process_id,
                    String::from_utf16_lossy(&entry.sz_exe_file[..len]),
                ));
                ok = Process32NextW(snap, &mut entry);
            }
            CloseHandle(snap);
        }
        out
    }

    fn parent_exe() -> Option<String> {
        // SAFETY: no arguments, always succeeds.
        let me = unsafe { GetCurrentProcessId() };
        let list = processes();
        let parent = list.iter().find(|(pid, _, _)| *pid == me)?.1;
        list.into_iter()
            .find(|(pid, _, _)| *pid == parent)
            .map(|(_, _, name)| name)
    }

    fn open_url(url: &str) {
        let op = wide("open");
        let file = wide(url);
        // SAFETY: null-terminated UTF-16 strings that outlive the call. COM is initialised as
        // ShellExecute asks; a second initialisation on the same thread is harmless.
        unsafe {
            CoInitializeEx(
                std::ptr::null_mut(),
                COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
            );
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
        }
    }

    pub(super) fn notice() -> bool {
        let env_names: Vec<String> = std::env::vars_os()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        let hand = super::started_by_hand(
            std::env::args_os().len().saturating_sub(1),
            env_names.iter().map(String::as_str),
            parent_exe().as_deref(),
        );
        if !hand {
            return true;
        }
        // No PS5UPLOAD_* variable is set here, so this is the port run_cli binds.
        let port: u16 = 19113;
        let text = wide(&super::notice_text(port));
        let caption = wide("PS5Upload engine");
        // SAFETY: null-terminated UTF-16 strings that outlive the call; no owner window.
        let choice = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                caption.as_ptr(),
                MB_YESNOCANCEL | MB_ICONINFORMATION | MB_SETFOREGROUND,
            )
        };
        match choice {
            IDYES => {
                // Open the page once the engine is listening, so the browser does not land on a
                // refused connection.
                std::thread::spawn(move || {
                    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
                    for _ in 0..60 {
                        let up = std::net::TcpStream::connect_timeout(
                            &addr,
                            std::time::Duration::from_millis(500),
                        )
                        .is_ok();
                        if up {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(250));
                    }
                    open_url(&format!("http://127.0.0.1:{port}/"));
                });
                true
            }
            IDNO => {
                open_url(super::RELEASES_URL);
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPLORER: Option<&str> = Some("explorer.exe");

    #[test]
    fn double_click_from_explorer_shows_the_note() {
        assert!(started_by_hand(0, ["PATH", "USERPROFILE"], EXPLORER));
        assert!(started_by_hand(0, [], Some(r"C:\Windows\Explorer.EXE")));
    }

    #[test]
    fn opened_from_a_browser_or_archive_shows_the_note() {
        assert!(started_by_hand(0, [], Some("msedge.exe")));
        assert!(started_by_hand(0, [], Some("Chrome.exe")));
        assert!(started_by_hand(0, [], Some("7zFM.exe")));
    }

    #[test]
    fn the_desktop_app_spawn_stays_quiet() {
        // client/src-tauri/src/engine.rs sets these; the parent is PS5Upload.exe.
        assert!(!started_by_hand(
            0,
            [
                "PS5UPLOAD_PARENT_WATCH",
                "PS5UPLOAD_ENGINE_PORT",
                "RUST_BACKTRACE"
            ],
            Some("PS5Upload.exe"),
        ));
        // Even if the app were ever started via Explorer, its env says it set the engine up.
        assert!(!started_by_hand(0, ["PS5UPLOAD_PARENT_WATCH"], EXPLORER));
    }

    #[test]
    fn server_setups_stay_quiet() {
        // scripts/autolaunch/install-windows.ps1: a scheduled task with PS5UPLOAD_ENGINE_PORT.
        assert!(!started_by_hand(
            0,
            ["PS5UPLOAD_ENGINE_PORT"],
            Some("svchost.exe")
        ));
        assert!(!started_by_hand(0, ["ps5upload_allow_ip"], EXPLORER));
        assert!(!started_by_hand(0, ["PS5_ADDR"], EXPLORER));
        // A service or a terminal, with nothing set.
        assert!(!started_by_hand(0, [], Some("services.exe")));
        assert!(!started_by_hand(0, [], Some("svchost.exe")));
        assert!(!started_by_hand(0, [], Some("cmd.exe")));
        assert!(!started_by_hand(0, [], Some("powershell.exe")));
        // Parent unknown (gone, or the snapshot failed).
        assert!(!started_by_hand(0, [], None));
    }

    #[test]
    fn any_argument_stays_quiet() {
        // --healthcheck, mcp, inspect/convert, --help, or a shortcut with an argument.
        assert!(!started_by_hand(1, [], EXPLORER));
        assert!(!started_by_hand(3, [], EXPLORER));
    }

    #[test]
    fn the_note_names_the_installer_and_the_port() {
        let text = notice_text(19113);
        assert!(text.contains(installer_suffix()));
        assert!(text.contains("http://127.0.0.1:19113/"));
        assert!(installer_suffix().ends_with("-setup.exe"));
    }
}
