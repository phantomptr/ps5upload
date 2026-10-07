//! Keep a console's elfldr from getting stuck.
//!
//! elfldr serves one connection at a time, and the stock build waits on a silent client
//! forever: after a dropped link or a sleep mid-send it stays in the process list but never
//! answers on :9021 again (users had to load elfldr again). ps5upload carries a patched build
//! (third_party/elfldr) and, once the helper is up, swaps it in for the console's stock one.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use ps5upload_core::payload_lifecycle::{self as pl, join_host_port, LoaderImage};
use ps5upload_core::process_mgr::process_list;
use serde::Serialize;

use crate::bundled_payload::{image_bytes, Image};

/// The loader port and the process name elfldr gives itself.
const LOADER_PORT: u16 = 9021;
const ELFLDR_NAME: &str = "elfldr.elf";

/// A request elfldr answers without running anything: it asks for a file that isn't there,
/// so the reply is an error line. (A bare connect-and-close must not be used: a loader handed a
/// connection and no bytes waits on it.)
const PROBE: &[u8] = b"GET /?uri=file:/data/ps5upload/.elfldr-probe HTTP/1.1\r\n\r\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    /// It answered.
    Healthy,
    /// It took the connection and never answered: the stuck state.
    Stuck,
    /// Nothing is listening.
    Absent,
}

/// Ask the loader on `ip:port` the harmless request and classify what comes back.
pub fn probe(ip: &str, port: u16, connect_timeout: Duration, reply_timeout: Duration) -> Health {
    let Ok(targets) = join_host_port(ip, port).to_socket_addrs() else {
        return Health::Absent;
    };
    let Some(mut c) = targets
        .into_iter()
        .find_map(|sa| TcpStream::connect_timeout(&sa, connect_timeout).ok())
    else {
        return Health::Absent;
    };
    let _ = c.set_write_timeout(Some(connect_timeout));
    let _ = c.set_read_timeout(Some(reply_timeout));
    if c.write_all(PROBE).is_err() {
        return Health::Stuck;
    }
    let mut buf = [0u8; 64];
    match c.read(&mut buf) {
        Ok(n) if n > 0 => Health::Healthy,
        // Closed without a word: something answered (it read, then hung up).
        Ok(_) => Health::Healthy,
        Err(_) => Health::Stuck,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Upgrade,
    /// elfldr is running but not answering, so it can't take the send that
    /// would replace it. Launch the patched build some other way (Payload
    /// Manager), which kills the wedged one by name and takes :9021.
    Recover(&'static str),
    Skip(&'static str),
}

/// Whether to replace the running elfldr. Only the standard `elfldr.elf` is touched (another
/// loader on :9021 is someone else's), only when it answers (a stuck one can't take the send),
/// and not when the one running is the one this engine installed.
///
/// And never when more than one process carries the loader's name. A new elfldr starts by
/// killing EVERY process named `elfldr.elf` (third_party/elfldr/socksrv.c: a loop over
/// `elfldr_find_pid`, by thread name), which is right when that is one old loader and fatal
/// when a loader that does not rename what it starts has left kstuff or ShadowMount+ running
/// under its own name: the swap takes them down with it (reported on FW 11.60: "the elfloader
/// is relaunched; ShadowMount and kstuff stop working").
pub fn decide(elfldr_pids: &[i32], installed: Option<i32>, health: Health) -> Decision {
    if elfldr_pids.is_empty() {
        return Decision::Skip("no elfldr");
    }
    if elfldr_pids.len() > 1 {
        return Decision::Skip("other payloads share the loader's name");
    }
    if installed.is_some_and(|pid| elfldr_pids == [pid]) {
        return Decision::Skip("current");
    }
    match health {
        Health::Healthy => Decision::Upgrade,
        Health::Stuck => Decision::Recover("stuck"),
        Health::Absent => Decision::Recover("not listening"),
    }
}

/// The new elfldr's pid once the swap is complete: exactly one `elfldr.elf`, not one of the
/// ones running before, and answering on :9021 (it is listed a few seconds before it listens).
pub fn took_over(before: &[i32], now: &[i32], health: Health) -> Option<i32> {
    match now {
        [pid] if !before.contains(pid) && health == Health::Healthy => Some(*pid),
        _ => None,
    }
}

/// The elfldr each console is running that this engine put there, by host.
fn installed() -> &'static Mutex<HashMap<String, i32>> {
    static M: OnceLock<Mutex<HashMap<String, i32>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// An elfldr this engine installed, as it is remembered across engine restarts: its pid and
/// when the console it runs on booted (on this computer's clock).
///
/// The in-memory table alone forgot every swap when the app closed, so each app start found
/// "an elfldr this engine did not install" and replaced the patched build with itself again:
/// a loader relaunch on every launch (reported on FW 11.60, where the phone app starts a new
/// engine each time). A pid means nothing after a reboot, when the stock loader may get the
/// same one, so the record only counts for the boot it was made in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, Serialize)]
pub struct Remembered {
    pub pid: i32,
    pub booted_at: i64,
}

/// Two readings of one boot differ by the time the two requests took, and by whatever this
/// computer's clock was corrected in between.
const SAME_BOOT_SLACK_SECS: i64 = 120;

/// The pid to treat as ours: the remembered one, if the console has not rebooted since.
pub fn remembered_pid(r: Option<Remembered>, booted_at: Option<i64>) -> Option<i32> {
    let (r, now) = (r?, booted_at?);
    ((r.booted_at - now).abs() <= SAME_BOOT_SLACK_SECS).then_some(r.pid)
}

fn store_path() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return None; // tests never touch the person's data folder
    }
    let data = std::env::var("PS5UPLOAD_DATA_DIR")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .filter(|v| !v.trim().is_empty());
    data.map(std::path::PathBuf::from)
        .or_else(|| home.map(|h| std::path::PathBuf::from(h).join(".ps5upload")))
        .map(|d| d.join("elfldr_installed.json"))
}

fn load_remembered(host: &str) -> Option<Remembered> {
    let all: HashMap<String, Remembered> =
        serde_json::from_slice(&std::fs::read(store_path()?).ok()?).ok()?;
    all.get(host).copied()
}

fn save_remembered(host: &str, r: Remembered) {
    let Some(p) = store_path() else { return };
    let mut all: HashMap<String, Remembered> = std::fs::read(&p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    all.insert(host.to_string(), r);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(b) = serde_json::to_vec(&all) {
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// When the console booted, on this computer's clock. `None` when it cannot be asked: the swap
/// is then remembered for this engine's lifetime only, as before.
fn console_booted_at(mgmt: &str) -> Option<i64> {
    let up = ps5upload_core::hw::hw_power(mgmt).ok()?.operating_time_sec;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    (up > 0).then(|| now as i64 - up as i64)
}

/// Records `pid` as the elfldr this engine put on `host`.
fn record_installed(host: &str, mgmt: &str, pid: i32) {
    installed()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(host.to_string(), pid);
    if let Some(booted_at) = console_booted_at(mgmt) {
        save_remembered(host, Remembered { pid, booted_at });
    }
}

fn elfldr_pids(mgmt: &str) -> Result<Vec<i32>, String> {
    let list = process_list(mgmt).map_err(|e| format!("{e:#}"))?;
    Ok(list
        .processes
        .iter()
        .filter(|p| p.name == ELFLDR_NAME)
        .map(|p| p.pid)
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    /// `upgraded`, `recovered`, `current` or `skipped`.
    pub action: &'static str,
    pub health: Health,
    /// Why it was skipped.
    pub reason: Option<&'static str>,
    /// The elfldr running afterwards, when known.
    pub pid: Option<i32>,
}

/// Make sure `host` runs the patched elfldr. Needs the helper up (it reads the process list).
pub fn ensure(host: &str) -> Result<Outcome, String> {
    let mgmt = crate::console_addr(host);
    let before = elfldr_pids(&mgmt)?;
    let mine = installed()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(host)
        .copied()
        .or_else(|| remembered_pid(load_remembered(host), console_booted_at(&mgmt)));
    let health = probe(
        host,
        LOADER_PORT,
        Duration::from_secs(3),
        Duration::from_secs(5),
    );
    match decide(&before, mine, health) {
        Decision::Skip(reason) => Ok(Outcome {
            action: if reason == "current" {
                "current"
            } else {
                "skipped"
            },
            health,
            reason: Some(reason),
            pid: before.first().copied(),
        }),
        Decision::Recover(reason) => {
            // A wedged elfldr (#344: the stock v0.26 that itsPLK's Payload
            // Manager carries) left :9021 dead, and with it every helper
            // redeploy — the user could not reconnect. Payload Manager
            // (:8084) can launch our patched elfldr instead. Without it, report
            // the skip as before.
            let bytes = image_bytes(Image::Elfldr)?;
            if let Err(e) =
                ps5upload_core::payload_manager::launch_elf(host, "ps5upload-elfldr.elf", &bytes)
            {
                crate::log_info!("elfldr on {host} is {reason}; no recovery route: {e}");
                return Ok(Outcome {
                    action: "skipped",
                    health,
                    reason: Some(reason),
                    pid: before.first().copied(),
                });
            }
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(500));
                let now = elfldr_pids(&mgmt).unwrap_or_default();
                let answering = probe(
                    host,
                    LOADER_PORT,
                    Duration::from_secs(2),
                    Duration::from_secs(3),
                );
                if let Some(pid) = took_over(&before, &now, answering) {
                    ps5upload_core::payload_manager::forget(host, "ps5upload-elfldr.elf");
                    record_installed(host, &mgmt, pid);
                    crate::log_info!("elfldr on {host} was {reason}; recovered through Payload Manager (pid {pid})");
                    return Ok(Outcome {
                        action: "recovered",
                        health: answering,
                        reason: None,
                        pid: Some(pid),
                    });
                }
            }
            ps5upload_core::payload_manager::forget(host, "ps5upload-elfldr.elf");
            Err("launched the patched elfldr through Payload Manager, but it was not answering on :9021 within 20 s".into())
        }
        Decision::Upgrade => {
            let bytes = image_bytes(Image::Elfldr)?;
            pl::send_elf_to_loader(host, LOADER_PORT, &bytes, LoaderImage::Companion)?;
            // The new elfldr kills the old one by name and takes the port; wait until it answers.
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(500));
                let now = elfldr_pids(&mgmt).unwrap_or_default();
                let answering = probe(
                    host,
                    LOADER_PORT,
                    Duration::from_secs(2),
                    Duration::from_secs(3),
                );
                if let Some(pid) = took_over(&before, &now, answering) {
                    record_installed(host, &mgmt, pid);
                    return Ok(Outcome {
                        action: "upgraded",
                        health: answering,
                        reason: None,
                        pid: Some(pid),
                    });
                }
            }
            Err("the patched elfldr was sent but was not answering on :9021 within 20 s".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    fn short() -> (Duration, Duration) {
        (Duration::from_millis(500), Duration::from_millis(700))
    }

    #[test]
    fn a_loader_that_answers_is_healthy() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = [0u8; 512];
            let _ = c.read(&mut buf);
            let _ =
                c.write_all(b"HTTP/1.1 200 OK\r\n\r\n[elfldr.elf] Error reading HTTP payload\n");
        });
        let (c, r) = short();
        assert_eq!(probe("127.0.0.1", port, c, r), Health::Healthy);
    }

    #[test]
    fn a_loader_that_accepts_and_says_nothing_is_stuck() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        // Never accepted: the connection sits in the backlog, as behind a wedged elfldr.
        let (c, r) = short();
        assert_eq!(probe("127.0.0.1", port, c, r), Health::Stuck);
        drop(l);
    }

    #[test]
    fn nothing_listening_is_absent() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let (c, r) = short();
        assert_eq!(probe("127.0.0.1", port, c, r), Health::Absent);
    }

    #[test]
    fn a_takeover_is_done_once_the_new_elfldr_answers() {
        // Measured: the new process is listed about 3 s before it listens on :9021.
        assert_eq!(took_over(&[84], &[683], Health::Absent), None);
        assert_eq!(took_over(&[84], &[683], Health::Healthy), Some(683));
        // The old one still running, or two at once: not yet.
        assert_eq!(took_over(&[84], &[84], Health::Healthy), None);
        assert_eq!(took_over(&[84], &[84, 683], Health::Healthy), None);
    }

    #[test]
    fn a_swap_is_remembered_across_engine_restarts_but_not_across_a_console_reboot() {
        use Decision::*;
        let r = Some(Remembered {
            pid: 97,
            booted_at: 1_000_000,
        });
        // A new engine (the app was reopened), same console boot: the elfldr is still ours.
        let mine = remembered_pid(r, Some(1_000_030));
        assert_eq!(mine, Some(97));
        assert_eq!(decide(&[97], mine, Health::Healthy), Skip("current"));
        // The console rebooted: a stock elfldr may hold the same pid, so it is replaced.
        let mine = remembered_pid(r, Some(1_086_400));
        assert_eq!(mine, None);
        assert_eq!(decide(&[97], mine, Health::Healthy), Upgrade);
        // Nothing remembered, or the console cannot say when it booted: as before.
        assert_eq!(remembered_pid(None, Some(1_000_000)), None);
        assert_eq!(remembered_pid(r, None), None);
    }

    #[test]
    fn only_a_healthy_stock_elfldr_is_replaced() {
        use Decision::*;
        // Stock elfldr, answering: replace it.
        assert_eq!(decide(&[84], None, Health::Healthy), Upgrade);
        // The one we put there: leave it.
        assert_eq!(decide(&[680], Some(680), Health::Healthy), Skip("current"));
        // A reboot (or an autoloader) started a new stock one: replace it again.
        assert_eq!(decide(&[84], Some(680), Health::Healthy), Upgrade);
        // Several processes carry the loader's name: a new elfldr would kill all of them, and
        // the others are payloads (kstuff, ShadowMount+) an older loader never renamed.
        let shared = Skip("other payloads share the loader's name");
        assert_eq!(decide(&[84, 91, 93], None, Health::Healthy), shared);
        assert_eq!(decide(&[680, 91], Some(680), Health::Healthy), shared);
        assert_eq!(decide(&[84, 91], None, Health::Stuck), shared);
        // No elfldr.elf at all: the loader on :9021 is someone else's (etaHEN, …).
        assert_eq!(decide(&[], None, Health::Healthy), Skip("no elfldr"));
        // Stuck or gone: nothing can be sent through it.
        // #344: elfldr listed but :9021 dead. It can't be replaced through
        // itself, so it is recovered another way.
        assert_eq!(decide(&[84], None, Health::Stuck), Recover("stuck"));
        assert_eq!(
            decide(&[84], None, Health::Absent),
            Recover("not listening")
        );
    }
}
