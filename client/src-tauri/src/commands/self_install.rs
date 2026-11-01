//! Install a downloaded update and start the new version, where that needs nothing paid.
//!
//! `update_download` saves the platform's bundle; this replaces the running copy with it. A
//! small script does the work after the app has quit (a running program cannot be replaced),
//! then starts the new version. Whatever fails, the script starts the app again, so a failed
//! install leaves the old version running and the download where it was.
//!
//! What can be replaced this way:
//!
//! * Windows, installed: the setup `.exe` is started directly in Tauri's update mode
//!   (`WINDOWS_SETUP_ARGS`): it closes this app itself (Restart Manager), shows its progress
//!   bar, installs over the same folder and starts the new version. No script: until 9.0.1 a
//!   hidden `powershell -ExecutionPolicy Bypass -File %TEMP%\….ps1` ran the setup with `/S`,
//!   a pattern security software and execution-policy group policy can block; the app had
//!   already quit by then, so nothing was installed, nothing restarted and nothing was shown
//!   (#533, Windows; the exact cause there is not known). If the setup fails now, it says so.
//! * Windows, portable: the zip holds one `PS5Upload.exe`, which replaces the running one. The
//!   app does this itself before it quits, with no script (`swap_in_zip_exe`): Windows lets a
//!   running program's file be renamed, though not overwritten, so the running exe is renamed
//!   aside, the new one put in its place, and the new version started; it waits for this
//!   process to end (`wait_for_previous_instance`) and deletes the old file. Until 10.5.1 a
//!   hidden `powershell -ExecutionPolicy Bypass -File %TEMP%\….ps1` did it after the app had
//!   quit, the same pattern the Windows setup dropped in 9.0.1 (#533): when that script did not
//!   run, the app closed, nothing was installed, nothing started again, and the next launch
//!   downloaded and "installed" the same update once more (#611). Now every failure happens
//!   while the app is still open, which says why and keeps running.
//! * macOS: the `.app` in the `.dmg` replaces the running bundle, if its folder is writable.
//! * Linux, portable: the zip's `PS5Upload.AppImage` (and launcher) replace the running
//!   AppImage (`$APPIMAGE`), if its folder is writable.
//!
//! * Linux, a `.deb` / `.rpm` install (since 10.2): `pkexec apt install -y <deb>` or
//!   `pkexec dnf install -y <rpm>` (the same tool `updates::install_command_for` names), so the
//!   desktop's own password prompt stands in for `sudo` in a terminal. It runs while the app
//!   is still open: the package manager replaces files of a running program safely on Linux
//!   (the old inodes live on until the process ends), and running it first is what lets the
//!   app see pkexec's exit status. Dismissed prompt (126), no polkit agent or no such program
//!   (127), no `pkexec` at all, or a failed install: the app stays open, nothing changed, and
//!   the renderer falls back to the command to copy. On success the app quits and a small
//!   script starts the new version once it has gone (starting it earlier would only hand
//!   its arguments to the old instance, tauri-plugin-single-instance).
//!
//! Not here: an rpm-ostree system (Bazzite, Silverblue: the swap applies at the next boot,
//! so `update_download` still hands over the command), Android (the system asks before every
//! install), iOS (re-signed by the user's own tool) and Docker (the image is pulled).
//!
//! Every install records what it installs (`update_files`, `install.json`) and logs its steps to
//! `update.log`; the next launch tells a success (the download is deleted) from a failure (said
//! once, and that version is not installed by itself again).
//!
//! Install on quit: the renderer arms `update_install_on_quit` with a downloaded update while
//! nothing transfers; when the app then quits normally, `install_armed_on_quit` (lib.rs,
//! `RunEvent::Exit`) starts the same script without the final restart, so quitting still
//! means quitting. Only plans that need no password: a polkit prompt popping up after the
//! window has gone would be a surprise, so a `.deb` / `.rpm` waits for the button.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::AppHandle;

/// How the Windows setup is started for an update: `/P` passive (a progress bar, no
/// questions; it closes the running app through the Restart Manager), `/R` start the app once
/// installed, `/UPDATE` install over the existing copy without uninstalling it first or
/// touching its shortcuts. The same flags tauri-plugin-updater passes to a Tauri NSIS setup.
pub const WINDOWS_SETUP_ARGS: [&str; 3] = ["/P", "/R", "/UPDATE"];

/// The Linux package manager a `.deb` / `.rpm` update goes through.
// Built only on Linux (`plan`); the tests cover it everywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Apt,
    Dnf,
}

/// The arguments handed to `pkexec`: the package manager and its install of `package`. No
/// shell sits between them, so the path needs no quoting.
pub fn pkexec_args(manager: PackageManager, package: &Path) -> Vec<OsString> {
    let tool = match manager {
        PackageManager::Apt => "apt",
        PackageManager::Dnf => "dnf",
    };
    vec![
        tool.into(),
        "install".into(),
        "-y".into(),
        package.as_os_str().to_os_string(),
    ]
}

/// What an install replaces, and with what.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum Plan {
    /// Run the Windows setup in update mode (`WINDOWS_SETUP_ARGS`); it starts `exe` again.
    WindowsSetup { installer: PathBuf, exe: PathBuf },
    /// Replace the portable `exe` with the zip's `PS5Upload.exe`.
    WindowsZip { zip: PathBuf, exe: PathBuf },
    /// Replace the `.app` bundle with the one in the `.dmg`.
    MacDmg { dmg: PathBuf, bundle: PathBuf },
    /// Replace the AppImage (and its launcher beside it) with the zip's.
    LinuxAppImage { zip: PathBuf, appimage: PathBuf },
    /// Install the `.deb` / `.rpm` through `pkexec` (a password prompt), then start `exe` again.
    LinuxPackage {
        package: PathBuf,
        manager: PackageManager,
        exe: PathBuf,
    },
}

impl Plan {
    /// The install asks for the user's password (pkexec). Such a plan is never run on quit or
    /// by the automatic install, only from the button.
    pub fn needs_password(&self) -> bool {
        matches!(self, Plan::LinuxPackage { .. })
    }
}

/// The plan for a packaged Linux install: `kind` is the package format that owns the running
/// program (`updates::linux_package_kind`), `ostree` whether this is an rpm-ostree system and
/// `pkexec` whether `pkexec` is on the PATH. None when any of them rules it out; the user
/// then gets the command to run instead.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn package_plan_for(
    download: &Path,
    exe: &Path,
    kind: Option<&str>,
    ostree: bool,
    pkexec: bool,
) -> Option<Plan> {
    if !pkexec {
        return None;
    }
    let name = download.file_name()?.to_str()?.to_ascii_lowercase();
    let manager = match kind? {
        "deb" if name.ends_with(".deb") => PackageManager::Apt,
        // rpm-ostree cannot `dnf install`; its swap needs a reboot (the command says so).
        "rpm" if name.ends_with(".rpm") && !ostree => PackageManager::Dnf,
        _ => return None,
    };
    Some(Plan::LinuxPackage {
        package: download.to_path_buf(),
        manager,
        exe: exe.to_path_buf(),
    })
}

/// The plan for installing `download` over this copy, or None when this copy cannot be
/// replaced by itself (see the module notes). `exe` is the running program and `appimage` the
/// `$APPIMAGE` path a Linux AppImage runs as.
pub fn plan_for(
    os: &str,
    download: &Path,
    exe: &Path,
    appimage: Option<&Path>,
    writable: &dyn Fn(&Path) -> bool,
) -> Option<Plan> {
    let name = download.file_name()?.to_str()?.to_ascii_lowercase();
    match os {
        "windows" if name.ends_with("-setup.exe") => Some(Plan::WindowsSetup {
            installer: download.to_path_buf(),
            exe: exe.to_path_buf(),
        }),
        "windows" if name.ends_with(".zip") && writable(exe.parent()?) => Some(Plan::WindowsZip {
            zip: download.to_path_buf(),
            exe: exe.to_path_buf(),
        }),
        "macos" if name.ends_with(".dmg") => {
            // …/PS5Upload.app/Contents/MacOS/PS5Upload
            let bundle = exe.parent()?.parent()?.parent()?;
            let is_bundle = bundle.extension().is_some_and(|e| e == "app");
            (is_bundle && writable(bundle.parent()?)).then(|| Plan::MacDmg {
                dmg: download.to_path_buf(),
                bundle: bundle.to_path_buf(),
            })
        }
        "linux" if name.ends_with(".zip") => {
            let appimage = appimage?;
            writable(appimage.parent()?).then(|| Plan::LinuxAppImage {
                zip: download.to_path_buf(),
                appimage: appimage.to_path_buf(),
            })
        }
        _ => None,
    }
}

/// A POSIX shell word.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// How the Windows setup is started: `WINDOWS_SETUP_ARGS`, without `/R` when the app is not
/// to start again (an install on quit).
pub fn windows_setup_args(relaunch: bool) -> Vec<&'static str> {
    WINDOWS_SETUP_ARGS
        .into_iter()
        .filter(|a| relaunch || *a != "/R")
        .collect()
}

/// The shell lines every script starts with: `say` appends a step to `update.log`
/// (`update_files`), which the next launch shows when the install did not take.
fn sh_log_prelude(log: &Path) -> String {
    format!(
        r#"log={}
say() {{ echo "$(date +%s) $*" >> "$log" 2>/dev/null; }}"#,
        sh_quote(&log.to_string_lossy())
    )
}

/// The script that carries `plan` out once process `pid` has quit, logging its steps to `log`.
/// With `relaunch`, every path ends by starting the app, new or old; without it (an install on
/// quit) nothing is started. None for the Windows plans: the setup is started directly and the
/// zip is swapped in by the app itself (`swap_in_zip_exe`).
///
/// For a `.deb` / `.rpm` the package is already installed when this runs (`update_install`
/// ran pkexec first): the script only waits for the old process and starts the new one.
pub fn script_for(plan: &Plan, pid: u32, relaunch: bool, log: &Path) -> Option<String> {
    let prelude = sh_log_prelude(log);
    Some(match plan {
        Plan::MacDmg { dmg, bundle } => {
            let dmg = sh_quote(&dmg.to_string_lossy());
            let app = sh_quote(&bundle.to_string_lossy());
            let start = if relaunch { r#"open "$app""# } else { "" };
            format!(
                r#"#!/bin/sh
# PS5Upload update: replace the app bundle with the one in the .dmg, then start it.
{prelude}
pid={pid}
dmg={dmg}
app={app}
n=0
while kill -0 "$pid" 2>/dev/null && [ "$n" -lt 60 ]; do sleep 0.5; n=$((n+1)); done
# Still running after 30 s: it hung on the way out. Replacing a running app and opening it
# would only bring the old one forward.
if kill -0 "$pid" 2>/dev/null; then say "the app did not quit in 30 s: ended it"; kill -9 "$pid"; sleep 1; fi
mnt=$(mktemp -d /tmp/ps5upload-update.XXXXXX)
# hdiutil attach is deprecated from macOS 27 (still works); diskutil image is its successor.
if hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$mnt" "$dmg" >/dev/null 2>&1 \
  || diskutil image attach --mountOptions nobrowse --readOnly --mountPoint "$mnt" "$dmg" >/dev/null 2>&1; then
  src=$(ls -d "$mnt"/*.app 2>/dev/null | head -n 1)
  new="$app.new.$$"
  old="$app.old.$$"
  rm -rf "$new"
  if [ -z "$src" ]; then
    say "the .dmg holds no app"
  elif ! ditto "$src" "$new"; then
    say "could not copy the new app next to $app"
  elif ! mv "$app" "$old"; then
    say "could not move $app aside"
  elif mv "$new" "$app"; then
    rm -rf "$old"
    say "replaced $app"
  else
    mv "$old" "$app"
    say "could not put the new app in place of $app"
  fi
  rm -rf "$new"
  hdiutil detach "$mnt" -quiet || hdiutil detach "$mnt" -force -quiet
else
  say "could not open the .dmg"
fi
rmdir "$mnt" 2>/dev/null
xattr -dr com.apple.quarantine "$app" 2>/dev/null
{start}
"#
            )
        }
        Plan::LinuxAppImage { zip, appimage } => {
            let zip = sh_quote(&zip.to_string_lossy());
            let target = sh_quote(&appimage.to_string_lossy());
            // Through the launcher beside it when it starts this AppImage, so its FUSE and
            // WebKit rescues apply to the new version too.
            let start = if relaunch {
                r#"launcher="$(dirname "$target")/PS5Upload.sh"
if [ "$(basename "$target")" = PS5Upload.AppImage ] && [ -x "$launcher" ]; then
  nohup "$launcher" >/dev/null 2>&1 &
else
  nohup "$target" >/dev/null 2>&1 &
fi"#
            } else {
                ""
            };
            format!(
                r#"#!/bin/sh
# PS5Upload update: replace the AppImage (and its launcher) with the zip's, then start it.
{prelude}
pid={pid}
zip={zip}
target={target}
n=0
while kill -0 "$pid" 2>/dev/null && [ "$n" -lt 60 ]; do sleep 0.5; n=$((n+1)); done
# Still running after 30 s: it hung on the way out.
if kill -0 "$pid" 2>/dev/null; then say "the app did not quit in 30 s: ended it"; kill -9 "$pid"; sleep 1; fi
dir=$(mktemp -d /tmp/ps5upload-update.XXXXXX)
if unzip -oq "$zip" -d "$dir" 2>/dev/null || python3 -c 'import sys,zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$zip" "$dir"; then
  if [ ! -f "$dir/PS5Upload.AppImage" ]; then
    say "the zip holds no PS5Upload.AppImage"
  else
    chmod +x "$dir/PS5Upload.AppImage"
    if cp -f "$dir/PS5Upload.AppImage" "$target.new.$$" && mv -f "$target.new.$$" "$target"; then
      say "replaced $target"
    else
      rm -f "$target.new.$$"
      say "could not replace $target"
    fi
    if [ -f "$dir/PS5Upload.sh" ] && [ -f "$(dirname "$target")/PS5Upload.sh" ]; then
      cp -f "$dir/PS5Upload.sh" "$(dirname "$target")/PS5Upload.sh"
      chmod +x "$(dirname "$target")/PS5Upload.sh"
    fi
  fi
else
  say "could not unpack the zip (no unzip or python3?)"
fi
rm -rf "$dir"
{start}
"#
            )
        }
        Plan::WindowsSetup { .. } | Plan::WindowsZip { .. } => return None,
        Plan::LinuxPackage { exe, .. } => {
            if !relaunch {
                return None;
            }
            let exe = sh_quote(&exe.to_string_lossy());
            format!(
                r#"#!/bin/sh
# PS5Upload update: the package is installed; start the new version once the old one has quit.
{prelude}
pid={pid}
exe={exe}
n=0
while kill -0 "$pid" 2>/dev/null && [ "$n" -lt 60 ]; do sleep 0.5; n=$((n+1)); done
# Still running after 30 s: it hung on the way out.
if kill -0 "$pid" 2>/dev/null; then say "the app did not quit in 30 s: ended it"; kill -9 "$pid"; sleep 1; fi
nohup "$exe" >/dev/null 2>&1 &
"#
            )
        }
    })
}

/// The file the running exe is renamed to while the new one takes its name.
fn aside_path(exe: &Path, pid: u32) -> PathBuf {
    let name = exe.file_name().map_or_else(
        || "PS5Upload.exe".into(),
        |n| n.to_string_lossy().into_owned(),
    );
    exe.with_file_name(format!("{name}.old-{pid}"))
}

/// The file the new exe is unpacked to, beside the running one (the same volume, so putting it
/// in place is a rename).
fn staged_path(exe: &Path, pid: u32) -> PathBuf {
    let name = exe.file_name().map_or_else(
        || "PS5Upload.exe".into(),
        |n| n.to_string_lossy().into_owned(),
    );
    exe.with_file_name(format!("{name}.new-{pid}"))
}

/// A leftover of an earlier swap beside `exe_name`: `<exe>.old-<pid>` or `<exe>.new-<pid>`.
fn is_swap_leftover(name: &str, exe_name: &str) -> bool {
    name.strip_prefix(exe_name)
        .and_then(|r| r.strip_prefix(".old-").or_else(|| r.strip_prefix(".new-")))
        .is_some_and(|pid| !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()))
}

/// The update's exe is far smaller; this only stops a damaged zip from filling the disk.
const EXE_MAX_BYTES: u64 = 500 * 1024 * 1024;

/// Unpack the zip's `PS5Upload.exe` to `to`. Its first bytes must be an executable's ("MZ").
fn unpack_exe(zip: &Path, to: &Path) -> Result<(), String> {
    use std::io::{Read, Write};
    let file = std::fs::File::open(zip).map_err(|e| format!("open {}: {e}", zip.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("read {}: {e}", zip.display()))?;
    let index = (0..archive.len())
        .find(|&i| {
            archive
                .name_for_index(i)
                .and_then(|r| r.ok())
                .is_some_and(|n| {
                    n.rsplit(['/', '\\'])
                        .next()
                        .is_some_and(|b| b.eq_ignore_ascii_case("PS5Upload.exe"))
                })
        })
        .ok_or_else(|| format!("{} holds no PS5Upload.exe", zip.display()))?;
    let mut entry = archive
        .by_index(index)
        .map_err(|e| format!("read {}: {e}", zip.display()))?;
    let mut out = std::fs::File::create(to).map_err(|e| format!("create {}: {e}", to.display()))?;
    let mut head = [0u8; 2];
    entry
        .read_exact(&mut head)
        .map_err(|e| format!("unpack PS5Upload.exe: {e}"))?;
    if &head != b"MZ" {
        return Err("the PS5Upload.exe in the zip is not a Windows program".into());
    }
    out.write_all(&head).map_err(|e| format!("write: {e}"))?;
    let copied = std::io::copy(&mut entry.take(EXE_MAX_BYTES), &mut out)
        .map_err(|e| format!("unpack PS5Upload.exe to {}: {e}", to.display()))?;
    if copied + 2 >= EXE_MAX_BYTES {
        return Err("the PS5Upload.exe in the zip is too large".into());
    }
    out.sync_all().map_err(|e| format!("write: {e}"))?;
    Ok(())
}

/// Rename with a few retries: a virus scanner looking at a fresh file holds it for a moment.
fn rename_retrying(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last = None;
    for i in 0..10 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
        if i < 9 {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("rename failed")))
}

/// Windows portable: put the zip's `PS5Upload.exe` in place of `exe` (the running program),
/// while it runs. Returns where the old exe was moved aside; `undo_swap` puts it back. On any
/// error nothing has changed. `say` logs each step.
pub fn swap_in_zip_exe(
    zip: &Path,
    exe: &Path,
    pid: u32,
    say: &dyn Fn(&str),
) -> Result<PathBuf, String> {
    let staged = staged_path(exe, pid);
    let aside = aside_path(exe, pid);
    let _ = std::fs::remove_file(&staged);
    say(&format!(
        "unpacking {} to {}",
        zip.display(),
        staged.display()
    ));
    if let Err(e) = unpack_exe(zip, &staged) {
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }
    // Downloaded by the app, so it carries no mark of the web; a zip saved by a browser and
    // put here by hand could, and its "unblock" is this stream.
    #[cfg(windows)]
    let _ = std::fs::remove_file(format!("{}:Zone.Identifier", staged.display()));
    let _ = std::fs::remove_file(&aside);
    if let Err(e) = rename_retrying(exe, &aside) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!("could not move {} aside: {e}", exe.display()));
    }
    if let Err(e) = rename_retrying(&staged, exe) {
        let back = std::fs::rename(&aside, exe);
        let _ = std::fs::remove_file(&staged);
        return Err(format!(
            "could not put the new PS5Upload.exe in place of {}: {e}{}",
            exe.display(),
            match back {
                Ok(()) => String::new(),
                Err(b) => format!(
                    " (and the old one could not be put back from {}: {b})",
                    aside.display()
                ),
            }
        ));
    }
    say(&format!(
        "replaced {} (old copy at {})",
        exe.display(),
        aside.display()
    ));
    Ok(aside)
}

/// Put the old exe back after `swap_in_zip_exe` (the new one could not be started).
pub fn undo_swap(exe: &Path, aside: &Path) -> std::io::Result<()> {
    std::fs::remove_file(exe)?;
    std::fs::rename(aside, exe)
}

/// The environment variable a new version started by an update reads: the PID of the old one,
/// to wait for before anything else (its single-instance lock would otherwise send the new one
/// away, and its engine is still stopping).
pub const WAIT_PID_ENV: &str = "PS5UPLOAD_UPDATE_WAIT_PID";

/// Start the new `exe` (Windows portable), detached; it waits for this process to end.
fn spawn_new_exe(exe: &Path) -> Result<(), String> {
    let mut cmd = std::process::Command::new(exe);
    cmd.env(WAIT_PID_ENV, std::process::id().to_string());
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("start the new version {}: {e}", exe.display()))
}

/// Started by an update (`WAIT_PID_ENV`): wait up to 30 s for the old version to end, then end
/// it. Called first thing in `run()`, before the single-instance check.
pub fn wait_for_previous_instance() {
    let Some(pid) = std::env::var(WAIT_PID_ENV)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
    else {
        return;
    };
    // Still single-threaded here; nothing this process starts should wait again.
    std::env::remove_var(WAIT_PID_ENV);
    if pid != std::process::id() {
        wait_for_pid(pid, std::time::Duration::from_secs(30));
    }
}

#[cfg(windows)]
fn wait_for_pid(pid: u32, limit: std::time::Duration) {
    use std::ffi::c_void;
    #[allow(non_snake_case)]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, ms: u32) -> u32;
        fn TerminateProcess(handle: *mut c_void, code: u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const PROCESS_TERMINATE: u32 = 0x0001;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;
    // SAFETY: plain kernel32 calls on a handle this function owns and closes.
    unsafe {
        let h = OpenProcess(SYNCHRONIZE | PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return; // Already gone.
        }
        if WaitForSingleObject(h, limit.as_millis().min(u32::MAX as u128) as u32) == WAIT_TIMEOUT {
            TerminateProcess(h, 1);
            WaitForSingleObject(h, 5_000);
        }
        CloseHandle(h);
    }
}

#[cfg(unix)]
fn wait_for_pid(pid: u32, limit: std::time::Duration) {
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    let alive = || {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    };
    let until = std::time::Instant::now() + limit;
    while alive() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if alive() {
        // SAFETY: as above; the old version hung on its way out.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn wait_for_pid(_pid: u32, _limit: std::time::Duration) {}

/// Delete what earlier updates left: the old exe a swap moved aside (once that version has
/// ended, which `wait_for_previous_instance` saw to) and the scripts in the temp folder. Only
/// names this module creates.
pub fn clean_leftovers() {
    if let Ok(exe) = std::env::current_exe() {
        if let (Some(dir), Some(name)) = (exe.parent(), exe.file_name().and_then(|n| n.to_str())) {
            clean_swap_leftovers(dir, name);
        }
    }
    if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let ours = name
                .strip_prefix("ps5upload-update-")
                .and_then(|r| r.strip_suffix(".sh").or_else(|| r.strip_suffix(".ps1")))
                .is_some_and(|pid| {
                    !pid.is_empty()
                        && pid.chars().all(|c| c.is_ascii_digit())
                        && pid != std::process::id().to_string()
                });
            if ours {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn clean_swap_leftovers(dir: &Path, exe_name: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name()
            .to_str()
            .is_some_and(|n| is_swap_leftover(n, exe_name))
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".ps5upload-write-test-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Whether `prog` is an executable file in a `$PATH` directory.
#[cfg(target_os = "linux")]
fn on_path(prog: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|d| {
            std::fs::metadata(d.join(prog))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

/// This copy's plan for `download`, from the running program and the platform.
pub fn plan(download: &Path) -> Option<Plan> {
    let exe = std::env::current_exe().ok()?;
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    let plain = plan_for(
        std::env::consts::OS,
        download,
        &exe,
        appimage.as_deref(),
        &dir_writable,
    );
    #[cfg(target_os = "linux")]
    if plain.is_none() && appimage.is_none() {
        // The path the package database knows (/usr/bin/x may be a link into /opt): it is
        // what starts the new version, since /proc/self/exe reads "(deleted)" once replaced.
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        return package_plan_for(
            download,
            &exe,
            super::updates::linux_package_kind(),
            Path::new("/run/ostree-booted").exists(),
            on_path("pkexec"),
        );
    }
    plain
}

/// What the AppImage's `AppRun` (linuxdeploy, plus its GTK and GStreamer hooks) points into the
/// bundle. A system program started with these loads the bundle's libraries instead of its own:
/// bash on Arch (readline 8.3) dies at once on the bundled older `libreadline.so.8`
/// ("undefined symbol: rl_print_keybinding", exit 127), so the update script never ran (#603).
const APPIMAGE_BUNDLE_ENV: &[&str] = &[
    "LD_LIBRARY_PATH",
    "PYTHONHOME",
    "PYTHONPATH",
    "PERLLIB",
    "QT_PLUGIN_PATH",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GST_PLUGIN_PATH_1_0",
    "GST_PLUGIN_SCANNER_1_0",
    "GST_PTP_HELPER_1_0",
    "GST_REGISTRY_REUSE_PLUGIN_SCANNER",
    "GTK_PATH",
    "GTK_EXE_PREFIX",
    "GTK_DATA_PREFIX",
    "GTK_THEME",
    "GTK_IM_MODULE_FILE",
    "GDK_PIXBUF_MODULE_FILE",
    "GIO_MODULE_DIR",
    "GI_TYPELIB_PATH",
    "GSETTINGS_SCHEMA_DIR",
];

/// Starts `cmd` (the update script) with the system's own libraries when this copy runs as an
/// AppImage. `LD_PRELOAD`, `APPIMAGE_EXTRACT_AND_RUN` and the `WEBKIT_*` switches stay: the
/// launcher (PS5Upload.sh) set them, and the app the script starts again needs them too.
fn without_appimage_env(cmd: &mut std::process::Command, appimage: bool) {
    if appimage {
        for var in APPIMAGE_BUNDLE_ENV {
            cmd.env_remove(var);
        }
    }
}

/// Start `script` (a shell script) detached from this process, so it outlives the app.
fn spawn_detached(script: &str) -> Result<(), String> {
    let path = std::env::temp_dir().join(format!("ps5upload-update-{}.sh", std::process::id()));
    std::fs::write(&path, script).map_err(|e| format!("write {}: {e}", path.display()))?;
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg(&path);
    without_appimage_env(&mut cmd, std::env::var_os("APPIMAGE").is_some());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: nothing that ends the app's group ends the update.
        cmd.process_group(0);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("start the update: {e}"))
}

/// Start the Windows setup on its own, detached, so it outlives the app.
fn spawn_setup(installer: &Path, relaunch: bool) -> Result<(), String> {
    let mut cmd = std::process::Command::new(installer);
    cmd.args(windows_setup_args(relaunch));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("start the update setup {}: {e}", installer.display()))
}

/// What `update_download` tells the renderer about installing `path` on this copy:
/// (`update_install` can, it asks for a password).
pub fn install_ability(path: &str) -> (bool, bool) {
    match plan(Path::new(path)) {
        Some(p) => (true, p.needs_password()),
        None => (false, false),
    }
}

/// `path` as a downloaded update: a file `update_download` saved in the updates folder, never
/// an arbitrary path. Returns it and the updates folder.
fn downloaded_update(app: &AppHandle, path: &str) -> Result<(PathBuf, PathBuf), String> {
    let download = PathBuf::from(path);
    let dir = super::update_files::updates_dir(app)?;
    if download.parent() != Some(dir.as_path()) || !download.is_file() {
        return Err(format!("{path} is not a downloaded update"));
    }
    Ok((download, dir))
}

/// The version `download` installs: the one the renderer names, else the one in the release
/// file name (`PS5Upload-<version>-…`).
fn target_version(download: &Path, named: Option<&str>) -> String {
    named
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.trim_start_matches('v').to_string())
        .or_else(|| {
            let name = download.file_name()?.to_str()?;
            super::update_files::release_file_version(name).map(str::to_string)
        })
        .unwrap_or_default()
}

/// Start carrying out `plan` (for a package, its pkexec step has already run), logging to the
/// updates folder `dir`.
fn start(plan: &Plan, relaunch: bool, dir: &Path) -> Result<(), String> {
    let say = |line: &str| super::update_files::log(dir, line);
    if let Plan::WindowsZip { zip, exe } = plan {
        let aside = swap_in_zip_exe(zip, exe, std::process::id(), &say)?;
        if relaunch {
            if let Err(e) = spawn_new_exe(exe) {
                // Put the old exe back: the app stays open as it was.
                let undone = undo_swap(exe, &aside);
                say(&format!("{e}; old version put back: {undone:?}"));
                return Err(e);
            }
            say("started the new version");
        }
        return Ok(());
    }
    let log = super::update_files::log_path(dir);
    match script_for(plan, std::process::id(), relaunch, &log) {
        Some(script) => spawn_detached(&script),
        None => match plan {
            Plan::WindowsSetup { installer, .. } => spawn_setup(installer, relaunch),
            Plan::LinuxPackage { .. } => Ok(()),
            _ => Err("no way to install this update".into()),
        },
    }
}

/// Record the install (`update_files`), then start it. A failure to start is recorded as the
/// reason, so that version is not tried again by itself (`update_last_install`).
fn record_and_start(
    plan: &Plan,
    relaunch: bool,
    dir: &Path,
    record: &super::update_files::InstallRecord,
) -> Result<(), String> {
    use super::update_files as files;
    let _ = files::write_record(dir, record);
    files::log(
        dir,
        &format!(
            "installing {} over {} from {}",
            record.to,
            record.from,
            record.file.display()
        ),
    );
    start(plan, relaunch, dir).inspect_err(|e| {
        files::log(dir, &format!("failed: {e}"));
        let mut failed = record.clone();
        failed.error = Some(e.clone());
        // The app says so itself right now: the next launch need not say it again.
        failed.reported = true;
        let _ = files::write_record(dir, &failed);
    })
}

/// What a pkexec exit status means. The renderer matches the two fixed strings to fall back
/// to the command to copy.
pub fn pkexec_outcome(code: Option<i32>, stderr: &str) -> Result<(), String> {
    match code {
        Some(0) => Ok(()),
        // The password prompt was dismissed, or the user is not allowed.
        Some(126) => Err("pkexec-cancelled".into()),
        // No polkit agent to ask with, or the program was not found.
        Some(127) => Err("pkexec-unavailable".into()),
        other => {
            let tail: String = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .chars()
                .take(300)
                .collect();
            Err(format!(
                "the package manager did not install the update (exit {}){}",
                other.map_or_else(|| "signal".to_string(), |c| c.to_string()),
                if tail.is_empty() {
                    String::new()
                } else {
                    format!(": {tail}")
                }
            ))
        }
    }
}

/// Run `pkexec <manager> install -y <package>` and wait for it; the desktop shows its password
/// prompt. The app stays open throughout, so a refusal changes nothing.
async fn run_pkexec(manager: PackageManager, package: &Path) -> Result<(), String> {
    let out = tokio::process::Command::new("pkexec")
        .args(pkexec_args(manager, package))
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "pkexec-unavailable".to_string()
            } else {
                format!("start pkexec: {e}")
            }
        })?;
    pkexec_outcome(out.status.code(), &String::from_utf8_lossy(&out.stderr))
}

/// The update armed to install when the app quits (`update_install_on_quit`).
struct Armed {
    download: PathBuf,
    dir: PathBuf,
    record: super::update_files::InstallRecord,
}

static ON_QUIT: Mutex<Option<Armed>> = Mutex::new(None);

fn on_quit_slot() -> std::sync::MutexGuard<'static, Option<Armed>> {
    ON_QUIT.lock().unwrap_or_else(|p| p.into_inner())
}

fn record_for(
    app: &AppHandle,
    download: &Path,
    version: Option<&str>,
) -> super::update_files::InstallRecord {
    super::update_files::InstallRecord {
        from: app.package_info().version.to_string(),
        to: target_version(download, version),
        file: download.to_path_buf(),
        error: None,
        reported: false,
    }
}

/// Arm (a path) or disarm (None) installing a downloaded update when the app quits. Returns
/// whether it is armed: only an update this copy installs without a password can be. The
/// renderer arms it only while nothing transfers and disarms it when a transfer starts.
#[tauri::command]
pub fn update_install_on_quit(
    app: AppHandle,
    path: Option<String>,
    version: Option<String>,
) -> Result<bool, String> {
    let Some(path) = path else {
        *on_quit_slot() = None;
        return Ok(false);
    };
    let (download, dir) = downloaded_update(&app, &path)?;
    let armed = plan(&download).is_some_and(|p| !p.needs_password());
    let record = record_for(&app, &download, version.as_deref());
    *on_quit_slot() = armed.then_some(Armed {
        download,
        dir,
        record,
    });
    Ok(armed)
}

/// The app is quitting (lib.rs, `RunEvent::Exit`): start the armed update, if any, without
/// starting the app again. Best effort: a failure leaves the old version and the download.
pub fn install_armed_on_quit() {
    let Some(armed) = on_quit_slot().take() else {
        return;
    };
    let result = match plan(&armed.download) {
        Some(p) if !p.needs_password() && armed.download.is_file() => {
            record_and_start(&p, false, &armed.dir, &armed.record)
        }
        _ => Err("the armed update can no longer be installed".into()),
    };
    if let Err(e) = result {
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("[self_install] install on quit: {e}\n"),
        );
    }
}

/// Install the downloaded update at `path` (version `version`) and restart into it. The app
/// quits once the install has started; the new version (or, after a failure the script saw,
/// the old one) starts again when it is done. An error means the app stays open, unchanged.
#[tauri::command]
pub async fn update_install(
    app: AppHandle,
    path: String,
    version: Option<String>,
) -> Result<(), String> {
    let (download, dir) = downloaded_update(&app, &path)?;
    let plan = plan(&download).ok_or_else(|| {
        "this copy cannot install updates by itself; install the download by hand".to_string()
    })?;
    if let Plan::LinuxPackage {
        package, manager, ..
    } = &plan
    {
        run_pkexec(*manager, package).await?;
    }
    // This install replaces any armed for the quit.
    *on_quit_slot() = None;
    let record = record_for(&app, &download, version.as_deref());
    record_and_start(&plan, true, &dir, &record)?;
    // Let the reply reach the window before the app goes.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // The update waits for this process to end: it must end. The orderly exit is bounded
    // (exit_guard.rs); this ends the process even if the event loop never gets to it.
    #[cfg(desktop)]
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(15));
        crate::exit_guard::kill_all();
        std::process::exit(0);
    });
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const YES: &dyn Fn(&Path) -> bool = &|_: &Path| true;
    const NO: &dyn Fn(&Path) -> bool = &|_: &Path| false;

    #[test]
    fn the_update_script_does_not_inherit_the_appimage_loader_env() {
        let removed = |appimage: bool| {
            let mut cmd = std::process::Command::new("/bin/sh");
            without_appimage_env(&mut cmd, appimage);
            cmd.get_envs()
                .map(|(k, v)| {
                    assert!(v.is_none(), "{k:?} is set, not removed");
                    k.to_string_lossy().into_owned()
                })
                .collect::<Vec<_>>()
        };
        // An AppImage: every bundle path goes, LD_LIBRARY_PATH first of all (#603); what the
        // launcher set for the app (LD_PRELOAD and the rest) stays.
        let gone = removed(true);
        assert!(gone.iter().any(|k| k == "LD_LIBRARY_PATH"));
        assert_eq!(gone.len(), APPIMAGE_BUNDLE_ENV.len());
        for kept in [
            "LD_PRELOAD",
            "APPIMAGE_EXTRACT_AND_RUN",
            "WEBKIT_DISABLE_DMABUF_RENDERER",
        ] {
            assert!(!gone.iter().any(|k| k == kept), "{kept} must be kept");
        }
        // Not an AppImage (a .deb / .rpm install, macOS): the environment is left alone.
        assert!(removed(false).is_empty());
    }

    #[test]
    fn each_platform_gets_its_own_plan_or_none() {
        let mac_exe = Path::new("/Applications/PS5Upload.app/Contents/MacOS/PS5Upload");
        assert_eq!(
            plan_for(
                "macos",
                Path::new("/d/PS5Upload-7.6.0-mac-arm64.dmg"),
                mac_exe,
                None,
                YES
            ),
            Some(Plan::MacDmg {
                dmg: "/d/PS5Upload-7.6.0-mac-arm64.dmg".into(),
                bundle: "/Applications/PS5Upload.app".into(),
            })
        );
        // /Applications not writable (a standard account): by hand.
        assert_eq!(
            plan_for("macos", Path::new("/d/x-mac-arm64.dmg"), mac_exe, None, NO),
            None
        );
        // Run from inside the mounted .dmg, not a bundle we can swap.
        assert_eq!(
            plan_for(
                "macos",
                Path::new("/d/x.dmg"),
                Path::new("/usr/local/bin/ps5upload"),
                None,
                YES
            ),
            None
        );
        let win_exe = Path::new(r"C:\Users\me\AppData\Local\PS5Upload\PS5Upload.exe");
        assert!(matches!(
            plan_for(
                "windows",
                Path::new("/d/PS5Upload-7.6.0-win-x64-setup.exe"),
                win_exe,
                None,
                NO
            ),
            Some(Plan::WindowsSetup { .. })
        ));
        assert!(matches!(
            plan_for(
                "windows",
                Path::new("/d/PS5Upload-7.6.0-win-x64.zip"),
                win_exe,
                None,
                YES
            ),
            Some(Plan::WindowsZip { .. })
        ));
        assert_eq!(
            plan_for(
                "windows",
                Path::new("/d/PS5Upload-7.6.0-win-x64.zip"),
                win_exe,
                None,
                NO
            ),
            None
        );
        let linux_exe = Path::new("/tmp/.mount_PS5UpXY/usr/bin/ps5upload");
        assert!(matches!(
            plan_for(
                "linux",
                Path::new("/d/PS5Upload-7.6.0-linux-x64.zip"),
                linux_exe,
                Some(Path::new("/home/me/Apps/PS5Upload.AppImage")),
                YES
            ),
            Some(Plan::LinuxAppImage { .. })
        ));
        // A packaged install (no $APPIMAGE) or a package download: by hand.
        assert_eq!(
            plan_for(
                "linux",
                Path::new("/d/x-linux-x64.zip"),
                linux_exe,
                None,
                YES
            ),
            None
        );
        assert_eq!(
            plan_for(
                "linux",
                Path::new("/d/x-linux-x64.rpm"),
                linux_exe,
                None,
                YES
            ),
            None
        );
        assert_eq!(
            plan_for(
                "android",
                Path::new("/d/x-android.apk"),
                linux_exe,
                None,
                YES
            ),
            None
        );
    }

    const LOG: &str = "/Users/o'neil/Library/Application Support/PS5Upload/updates/update.log";

    #[test]
    fn scripts_quote_paths_and_always_restart_the_app() {
        let log = Path::new(LOG);
        let plan = Plan::MacDmg {
            dmg: "/Users/o'neil/Downloads/PS5Upload.dmg".into(),
            bundle: "/Applications/PS5Upload.app".into(),
        };
        let s = script_for(&plan, 42, true, log).unwrap();
        assert!(s.contains("pid=42"));
        // An app that hangs on the way out is ended, not waited on forever.
        assert!(s.contains(r#"kill -9 "$pid""#));
        assert!(s.contains(r"dmg='/Users/o'\''neil/Downloads/PS5Upload.dmg'"));
        assert!(s.contains(
            r"log='/Users/o'\''neil/Library/Application Support/PS5Upload/updates/update.log'"
        ));
        assert!(s.trim_end().ends_with(r#"open "$app""#));

        let plan = Plan::LinuxAppImage {
            zip: "/d/x.zip".into(),
            appimage: "/home/me/PS5Upload.AppImage".into(),
        };
        assert!(script_for(&plan, 9, true, log)
            .unwrap()
            .trim_end()
            .ends_with("nohup \"$target\" >/dev/null 2>&1 &\nfi"));

        // #533 / #611: neither Windows plan runs a script (no PowerShell at all).
        let setup = Plan::WindowsSetup {
            installer: r"C:\d\setup.exe".into(),
            exe: r"C:\Apps\PS5Upload.exe".into(),
        };
        let zip = Plan::WindowsZip {
            zip: r"C:\d\PS5Upload-10.5.1-win-x64.zip".into(),
            exe: r"C:\Apps\PS5Upload.exe".into(),
        };
        for relaunch in [true, false] {
            assert_eq!(script_for(&setup, 7, relaunch, log), None);
            assert_eq!(script_for(&zip, 7, relaunch, log), None);
        }
        assert_eq!(WINDOWS_SETUP_ARGS, ["/P", "/R", "/UPDATE"]);
    }

    #[test]
    fn every_failure_a_script_can_see_is_logged() {
        let log = Path::new("/u/updates/update.log");
        let mac = script_for(
            &Plan::MacDmg {
                dmg: "/d/x.dmg".into(),
                bundle: "/Applications/PS5Upload.app".into(),
            },
            1,
            true,
            log,
        )
        .unwrap();
        for step in [
            "say \"could not open the .dmg\"",
            "say \"the .dmg holds no app\"",
            "say \"could not move $app aside\"",
            "say \"replaced $app\"",
        ] {
            assert!(mac.contains(step), "{step}");
        }
        let img = script_for(
            &Plan::LinuxAppImage {
                zip: "/d/x.zip".into(),
                appimage: "/a/PS5Upload.AppImage".into(),
            },
            1,
            true,
            log,
        )
        .unwrap();
        for step in [
            "say \"could not unpack the zip",
            "say \"the zip holds no PS5Upload.AppImage\"",
            "say \"could not replace $target\"",
        ] {
            assert!(img.contains(step), "{step}");
        }
    }

    /// A zip like the release's (publish.yml: Compress-Archive of one PS5Upload.exe).
    fn release_zip(path: &Path, entry: &str, body: &[u8]) {
        use std::io::Write;
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        z.start_file(
            entry,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        z.write_all(body).unwrap();
        z.finish().unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ps5upload-swap-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_windows_zip_is_swapped_in_by_the_app_itself_and_can_be_undone() {
        let d = scratch("ok");
        // A user folder with a space and a non-ASCII letter (no script, so no quoting at all).
        let app_dir = d.join("José Apps");
        std::fs::create_dir_all(&app_dir).unwrap();
        let exe = app_dir.join("PS5Upload.exe");
        std::fs::write(&exe, b"MZold").unwrap();
        let zip = d.join("PS5Upload-10.5.1-win-x64.zip");
        release_zip(&zip, "PS5Upload.exe", b"MZnew version");
        let lines = std::cell::RefCell::new(Vec::new());
        let say = |l: &str| lines.borrow_mut().push(l.to_string());
        let aside = swap_in_zip_exe(&zip, &exe, 4242, &say).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"MZnew version");
        assert_eq!(aside, app_dir.join("PS5Upload.exe.old-4242"));
        assert_eq!(std::fs::read(&aside).unwrap(), b"MZold");
        assert!(!app_dir.join("PS5Upload.exe.new-4242").exists());
        assert!(lines.borrow().iter().any(|l| l.starts_with("replaced ")));
        // The new version could not be started: the old one goes back.
        undo_swap(&exe, &aside).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"MZold");
        assert!(!aside.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_bad_zip_changes_nothing() {
        let d = scratch("bad");
        let exe = d.join("PS5Upload.exe");
        std::fs::write(&exe, b"MZold").unwrap();
        let say = |_: &str| {};
        let cases: [(&str, &str, &[u8], &str); 3] = [
            ("no-exe.zip", "README.txt", b"hi", "holds no PS5Upload.exe"),
            (
                "not-pe.zip",
                "PS5Upload.exe",
                b"<html>",
                "not a Windows program",
            ),
            ("nested.zip", "x/PS5Upload.exe", b"MZnested", ""),
        ];
        for (name, entry, body, err) in cases {
            let zip = d.join(name);
            release_zip(&zip, entry, body);
            let r = swap_in_zip_exe(&zip, &exe, 1, &say);
            if err.is_empty() {
                // One folder down is still found.
                let aside = r.unwrap();
                undo_swap(&exe, &aside).unwrap();
            } else {
                assert!(r.unwrap_err().contains(err), "{name}");
            }
            assert_eq!(std::fs::read(&exe).unwrap(), b"MZold", "{name}");
            let left: Vec<_> = std::fs::read_dir(&d)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains(".old-") || n.contains(".new-"))
                .collect();
            assert!(left.is_empty(), "{name}: {left:?}");
        }
        // Not a zip at all.
        let junk = d.join("junk.zip");
        std::fs::write(&junk, b"not a zip").unwrap();
        assert!(swap_in_zip_exe(&junk, &exe, 1, &say).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"MZold");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_our_own_swap_leftovers_are_cleaned() {
        assert!(is_swap_leftover("PS5Upload.exe.old-1234", "PS5Upload.exe"));
        assert!(is_swap_leftover("PS5Upload.exe.new-9", "PS5Upload.exe"));
        assert!(!is_swap_leftover("PS5Upload.exe", "PS5Upload.exe"));
        assert!(!is_swap_leftover("PS5Upload.exe.old-", "PS5Upload.exe"));
        assert!(!is_swap_leftover("PS5Upload.exe.old-12x", "PS5Upload.exe"));
        assert!(!is_swap_leftover("notes.old-1", "PS5Upload.exe"));
        let d = scratch("clean");
        for n in [
            "PS5Upload.exe",
            "PS5Upload.exe.old-77",
            "PS5Upload.exe.new-78",
            "settings.json",
        ] {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        clean_swap_leftovers(&d, "PS5Upload.exe");
        let mut left: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["PS5Upload.exe", "settings.json"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_target_version_comes_from_the_renderer_or_the_file_name() {
        let f = Path::new("/u/updates/PS5Upload-10.5.1-win-x64.zip");
        assert_eq!(target_version(f, Some("v10.6.0")), "10.6.0");
        assert_eq!(target_version(f, None), "10.5.1");
        assert_eq!(target_version(f, Some("  ")), "10.5.1");
        assert_eq!(target_version(Path::new("/u/x.zip"), None), "");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_version_waits_for_the_old_one_to_end() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("0.3")
            .spawn()
            .unwrap();
        let pid = child.id();
        // Reaped on another thread, or it would linger as a zombie (signal 0 still sees one).
        let reaper = std::thread::spawn(move || child.wait());
        let t = std::time::Instant::now();
        wait_for_pid(pid, std::time::Duration::from_secs(10));
        assert!(t.elapsed() >= std::time::Duration::from_millis(200));
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
        reaper.join().unwrap().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_shell_scripts_parse() {
        for plan in [
            Plan::MacDmg {
                dmg: PathBuf::from("/Users/o'neil/Downloads/PS5Upload 8.dmg"),
                bundle: PathBuf::from("/Applications/PS5Upload.app"),
            },
            Plan::LinuxAppImage {
                zip: PathBuf::from("/home/o'neil/PS5Upload.zip"),
                appimage: PathBuf::from("/home/o'neil/Apps/PS5Upload.AppImage"),
            },
            Plan::LinuxPackage {
                package: PathBuf::from("/home/o'neil/Downloads/PS5Upload.deb"),
                manager: PackageManager::Apt,
                exe: PathBuf::from("/usr/bin/ps5 upload"),
            },
        ] {
            for relaunch in [true, false] {
                let Some(script) = script_for(&plan, 42, relaunch, Path::new(LOG)) else {
                    continue;
                };
                let out = std::process::Command::new("/bin/sh")
                    .arg("-n")
                    .arg("-c")
                    .arg(script)
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "{plan:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
    }

    #[test]
    fn a_packaged_linux_install_goes_through_pkexec() {
        let exe = Path::new("/usr/bin/ps5-upload");
        let deb = Path::new("/home/me/Downloads/PS5Upload-10.2.0-linux-x64.deb");
        let rpm = Path::new("/home/me/Downloads/PS5Upload-10.2.0-linux-x64.rpm");
        assert_eq!(
            package_plan_for(deb, exe, Some("deb"), false, true),
            Some(Plan::LinuxPackage {
                package: deb.into(),
                manager: PackageManager::Apt,
                exe: exe.into(),
            })
        );
        assert_eq!(
            package_plan_for(rpm, exe, Some("rpm"), false, true),
            Some(Plan::LinuxPackage {
                package: rpm.into(),
                manager: PackageManager::Dnf,
                exe: exe.into(),
            })
        );
        // rpm-ostree needs a reboot: the command, not the button.
        assert_eq!(package_plan_for(rpm, exe, Some("rpm"), true, true), None);
        // No pkexec: the command.
        assert_eq!(package_plan_for(deb, exe, Some("deb"), false, false), None);
        // The download must be the format that owns this copy, and a portable copy has none.
        assert_eq!(package_plan_for(rpm, exe, Some("deb"), false, true), None);
        assert_eq!(package_plan_for(deb, exe, None, false, true), None);
        assert_eq!(
            package_plan_for(
                Path::new("/d/PS5Upload-linux-x64.zip"),
                exe,
                Some("deb"),
                false,
                true
            ),
            None
        );
        let plan = package_plan_for(deb, exe, Some("deb"), false, true).unwrap();
        assert!(plan.needs_password());
        assert!(!Plan::LinuxAppImage {
            zip: "/d/x.zip".into(),
            appimage: "/a/x.AppImage".into()
        }
        .needs_password());
    }

    #[test]
    fn pkexec_gets_the_path_as_one_argument_and_the_tool_install_command_for_names() {
        // The same tools updates::install_command_for shows (sudo apt / sudo dnf install).
        let odd = Path::new("/home/o'neil/Down loads/PS5Upload $(x).deb");
        assert_eq!(
            pkexec_args(PackageManager::Apt, odd),
            [
                "apt",
                "install",
                "-y",
                "/home/o'neil/Down loads/PS5Upload $(x).deb"
            ]
            .map(OsString::from)
            .to_vec()
        );
        assert_eq!(
            pkexec_args(PackageManager::Dnf, Path::new("/d/x.rpm")),
            ["dnf", "install", "-y", "/d/x.rpm"]
                .map(OsString::from)
                .to_vec()
        );
    }

    #[test]
    fn pkexec_refusals_fall_back_to_the_command() {
        assert_eq!(pkexec_outcome(Some(0), ""), Ok(()));
        assert_eq!(
            pkexec_outcome(Some(126), ""),
            Err("pkexec-cancelled".to_string())
        );
        assert_eq!(
            pkexec_outcome(
                Some(127),
                "Error executing command as another user: No authentication agent found."
            ),
            Err("pkexec-unavailable".to_string())
        );
        let err = pkexec_outcome(Some(100), "E: Unable to locate package\n\n").unwrap_err();
        assert!(
            err.contains("exit 100") && err.contains("Unable to locate"),
            "{err}"
        );
        assert!(pkexec_outcome(None, "").unwrap_err().contains("signal"));
    }

    #[test]
    fn the_package_script_only_restarts_the_new_version() {
        let plan = Plan::LinuxPackage {
            package: "/d/x.deb".into(),
            manager: PackageManager::Apt,
            exe: "/opt/PS5 Upload/ps5-upload".into(),
        };
        let s = script_for(&plan, 9, true, Path::new(LOG)).unwrap();
        assert!(s.contains("pid=9"));
        assert!(s.contains("exe='/opt/PS5 Upload/ps5-upload'"));
        // pkexec already ran inside the app: no root, no package manager here.
        assert!(!s.contains("apt") && !s.contains("pkexec"));
        assert!(s.trim_end().ends_with(r#"nohup "$exe" >/dev/null 2>&1 &"#));
        assert_eq!(script_for(&plan, 9, false, Path::new(LOG)), None);
    }

    #[test]
    fn an_install_on_quit_does_not_start_the_app_again() {
        let mac = Plan::MacDmg {
            dmg: "/d/x.dmg".into(),
            bundle: "/Applications/PS5Upload.app".into(),
        };
        let s = script_for(&mac, 1, false, Path::new(LOG)).unwrap();
        assert!(s.contains("ditto"));
        assert!(!s.contains(r#"open "$app""#));
        let img = Plan::LinuxAppImage {
            zip: "/d/x.zip".into(),
            appimage: "/a/PS5Upload.AppImage".into(),
        };
        let s = script_for(&img, 1, false, Path::new(LOG)).unwrap();
        assert!(s.contains("PS5Upload.AppImage") && !s.contains("nohup"));
        assert_eq!(windows_setup_args(true), ["/P", "/R", "/UPDATE"]);
        assert_eq!(windows_setup_args(false), ["/P", "/UPDATE"]);
    }
}
