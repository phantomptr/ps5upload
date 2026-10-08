//! Health checks for what sits between this computer and the console.
//!
//! The console checks in `ps5upload_core::health` ask the helper about the console. These ask
//! about the path: which of the console's ports answer, whether the console can reach this
//! engine (a Stream install needs that, and a firewall is the usual reason it cannot), how
//! long a reply takes, and whether this engine is set up to be reached at all (a container
//! on a bridge network hands the console an address it cannot route to).
//!
//! The same rule as the console checks: never guess. What could not be measured is `Skip`
//! with the reason. All verdicts are free functions over plain inputs so they can be tested
//! without a console.

use std::time::{Duration, Instant};

use ps5upload_core::health::{CheckCategory, CheckStatus, HealthCheck};
use ps5upload_core::payload_lifecycle::{probe_port, INSTALLER_PORT, PS5_LOADER_PORT};

const PORT_TIMEOUT: Duration = Duration::from_millis(1500);
const REACH_TIMEOUT_MS: u32 = 3000;
/// Replies slower than this are worth a line: on a wired LAN they take a few milliseconds.
pub(crate) const SLOW_REPLY_MS: u64 = 40;

fn check(id: &str, title: &str, status: CheckStatus, detail: impl Into<String>) -> HealthCheck {
    HealthCheck::new(id, title, CheckCategory::Network, status, detail)
}

/// The console's ELF loader port. Closed is a warning, not a failure: everything already
/// running keeps working, but the helper cannot be sent again and no payload can be loaded.
pub(crate) fn loader_port_check(open: Result<(), String>) -> HealthCheck {
    match open {
        Ok(()) => check(
            "loader_port",
            "PS5 payload loader (port 9021)",
            CheckStatus::Pass,
            "The loader is listening, so the helper and other payloads can be sent.",
        ),
        Err(e) => check(
            "loader_port",
            "PS5 payload loader (port 9021)",
            CheckStatus::Warn,
            format!("Nothing answered on port {PS5_LOADER_PORT} ({e})."),
        )
        .with_remedy(
            "The helper that is already running keeps working, but it cannot be sent again or \
             updated, and no other payload can be loaded, until a loader listens again. Run \
             your jailbreak's loader (or its autoloader) on the PS5.",
        ),
    }
}

/// What was found, or done, about ps5upload's installer on the console.
pub(crate) enum InstallerState {
    /// Already running, with its version.
    Running(String),
    /// Not running; the scan started it, and it answered with this state.
    Started,
    /// Not running, and starting it failed for this reason.
    CouldNotStart(String),
    /// Not running, and the scan did not try (the helper is not up, so nothing can install).
    NotTried,
}

/// ps5upload's installer on the console. Every install needs it, so a scan that finds it
/// missing starts it rather than leaving that to the first install, where a failure to
/// start looks like a failed install.
pub(crate) fn installer_check(state: InstallerState) -> HealthCheck {
    const TITLE: &str = "Package installer on the PS5 (port 9115)";
    match state {
        InstallerState::Running(version) => check(
            "installer_running",
            TITLE,
            CheckStatus::Pass,
            format!("Running, version {version}."),
        ),
        InstallerState::Started => check(
            "installer_running",
            TITLE,
            CheckStatus::Pass,
            "It was not running, so it was started just now. Installs are ready.",
        ),
        InstallerState::CouldNotStart(why) => check(
            "installer_running",
            TITLE,
            CheckStatus::Fail,
            format!("It is not running and could not be started: {why}."),
        )
        .with_remedy(
            "Installs will fail until it starts. It is sent through the PS5's ELF loader (port              9021) or Payload Manager (port 8084), so one of them must be running: load your              jailbreak's loader (elfldr) again, then press Scan again. kstuff must be loaded too.",
        ),
        InstallerState::NotTried => check(
            "installer_running",
            TITLE,
            CheckStatus::Skip,
            "Not running. It is started as soon as the helper is connected.",
        ),
    }
}

/// What the console said when asked to connect back to this engine.
pub(crate) enum Reach {
    Ok {
        ms: u64,
    },
    Refused {
        timed_out: bool,
        err: String,
    },
    /// The helper could not be asked (an older helper has no such probe).
    Unknown(String),
}

/// Can the console open a connection to this engine? A Stream install, a link streamed
/// through this computer and the second try of an Upload & install all need it.
pub(crate) fn reach_check(
    origin: &str,
    reach: Reach,
    pinned: bool,
    in_container: bool,
) -> HealthCheck {
    const ID: &str = "ps5_reaches_computer";
    const TITLE: &str = "PS5 can reach this computer";
    match reach {
        Reach::Ok { ms } => check(
            ID,
            TITLE,
            CheckStatus::Pass,
            format!("The PS5 connected to {origin} in {ms} ms. Stream & install can work."),
        ),
        Reach::Refused { timed_out, err } => {
            let why = if timed_out {
                "no answer (a firewall usually drops it silently)".to_string()
            } else if err.is_empty() {
                "the connection was refused".to_string()
            } else {
                err
            };
            let mut remedy = String::from(
                "Stream & install and links streamed through this computer will fail until \
                 this works; Upload & install does not need it. Allow ps5upload through this \
                 computer's firewall (on Windows for both Private and Public networks), keep \
                 the PS5 and this computer on the same network with any VPN off, and set the \
                 PS5's Proxy Server to \"Do Not Use\".",
            );
            if in_container && !pinned {
                remedy.push_str(
                    " This engine runs in a container: use host networking, or set \
                     PS5UPLOAD_PKG_HOST_IP to the host's LAN address and publish port 19113.",
                );
            } else if pinned {
                remedy.push_str(
                    " The address comes from PS5UPLOAD_PKG_HOST_IP: check it is this \
                     machine's LAN address.",
                );
            }
            check(
                ID,
                TITLE,
                CheckStatus::Fail,
                format!("The PS5 could not connect to {origin}: {why}."),
            )
            .with_remedy(remedy)
        }
        Reach::Unknown(e) => check(
            ID,
            TITLE,
            CheckStatus::Skip,
            format!("The helper could not be asked to try ({e}). Update the helper to check this."),
        ),
    }
}

/// A container on a bridge network gives the console an address it cannot route to.
pub(crate) fn container_address_check(
    in_container: bool,
    bridged_without_pin: bool,
) -> Option<HealthCheck> {
    if !in_container {
        return None;
    }
    Some(if bridged_without_pin {
        check(
            "container_address",
            "Engine address for the PS5 (container)",
            CheckStatus::Fail,
            "This engine runs in a container on a bridge network and no address is pinned, so \
             the PS5 is handed a container address it cannot reach.",
        )
        .with_remedy(
            "Run the container with host networking, or set PS5UPLOAD_PKG_HOST_IP to the \
             host's LAN address and publish port 19113.",
        )
    } else {
        check(
            "container_address",
            "Engine address for the PS5 (container)",
            CheckStatus::Pass,
            "This engine runs in a container and gives the PS5 a LAN address.",
        )
    })
}

/// Middle of the measured reply times, in milliseconds. `None` when nothing was measured.
pub(crate) fn median_ms(mut samples: Vec<u64>) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_unstable();
    Some(samples[samples.len() / 2])
}

/// How quickly the helper answers. Slow is a warning with the likely cause, never a failure:
/// everything still works, only slower.
pub(crate) fn reply_time_check(median: Option<u64>, samples: usize) -> HealthCheck {
    const ID: &str = "reply_time";
    const TITLE: &str = "Reply time";
    match median {
        None => check(ID, TITLE, CheckStatus::Skip, "No reply could be timed."),
        Some(ms) if ms > SLOW_REPLY_MS => check(
            ID,
            TITLE,
            CheckStatus::Warn,
            format!("The helper answers in about {ms} ms (middle of {samples} tries)."),
        )
        .with_remedy(
            "That is slow for a home network and usually means Wi-Fi, a busy link or a VPN in \
             the path. Transfers will be slower than the PS5 can take; a network cable to the \
             PS5 fixes it. Run the speed test below for real numbers.",
        ),
        Some(ms) => check(
            ID,
            TITLE,
            CheckStatus::Pass,
            format!("The helper answers in about {ms} ms (middle of {samples} tries)."),
        ),
    }
}

/// This computer's data folder must take writes: pairing keys, saved connections and job
/// journals live there, and a read-only one fails in ways that look like network trouble.
pub(crate) fn data_dir_check(
    dir: Option<&std::path::Path>,
    write: Result<(), String>,
) -> HealthCheck {
    const ID: &str = "data_dir_writable";
    const TITLE: &str = "ps5upload's data folder on this computer";
    let Some(dir) = dir else {
        return HealthCheck::new(
            ID,
            TITLE,
            CheckCategory::Runtime,
            CheckStatus::Fail,
            "No data folder is set, so pairing and saved settings cannot be kept.",
        )
        .with_remedy(
            "Set PS5UPLOAD_DATA_DIR to a writable folder (in Docker, mount a volume there).",
        );
    };
    match write {
        Ok(()) => HealthCheck::new(
            ID,
            TITLE,
            CheckCategory::Runtime,
            CheckStatus::Pass,
            format!("{} can be written.", dir.display()),
        ),
        Err(e) => HealthCheck::new(
            ID,
            TITLE,
            CheckCategory::Runtime,
            CheckStatus::Fail,
            format!("{} cannot be written: {e}", dir.display()),
        )
        .with_remedy(
            "Make the folder writable for the account running ps5upload (in Docker, mount a \
             writable volume and set PS5UPLOAD_DATA_DIR to it).",
        ),
    }
}

/// Runs the network checks against `addr` (the console's management address).
/// `helper_up`: the helper answered the console scan; without it only the ports and this
/// computer's own state can be measured.
pub(crate) fn network_checks(addr: &str, helper_up: bool) -> Vec<HealthCheck> {
    let host = crate::pkg_install::strip_host_port(addr);
    let mut out = Vec::new();

    out.push(loader_port_check(probe_port(
        &host,
        PS5_LOADER_PORT,
        PORT_TIMEOUT,
    )));

    let in_container = crate::pkg_install::in_container();
    if let Some(c) = container_address_check(
        in_container,
        crate::pkg_install::bridged_container_without_pkg_host_ip(),
    ) {
        out.push(c);
    }

    if helper_up {
        // Reply time: a handful of the lightest call the helper has.
        let mut samples = Vec::new();
        for _ in 0..5 {
            let t = Instant::now();
            if ps5upload_core::fs_ops::fs_stat(addr, "/data").is_ok() {
                samples.push(t.elapsed().as_millis() as u64);
            }
        }
        let n = samples.len();
        out.push(reply_time_check(median_ms(samples), n));

        let pinned = std::env::var("PS5UPLOAD_PKG_HOST_IP").is_ok_and(|v| !v.trim().is_empty());
        match crate::pkg_install::engine_origin_for_ps5(addr) {
            Ok(origin) => {
                let (h, p) = split_origin(&origin);
                let reach = match ps5upload_core::diagnostics::net_reach(addr, &h, p, REACH_TIMEOUT_MS) {
                    Ok(r) if r.ok => Reach::Ok { ms: r.ms },
                    Ok(r) => Reach::Refused {
                        timed_out: r.timed_out,
                        err: r.err,
                    },
                    Err(e) => Reach::Unknown(format!("{e:#}")),
                };
                out.push(reach_check(&origin, reach, pinned, in_container));
            }
            Err(e) => out.push(
                check(
                    "ps5_reaches_computer",
                    "PS5 can reach this computer",
                    CheckStatus::Fail,
                    format!("This computer's address for the PS5 could not be worked out: {e}"),
                )
                .with_remedy(
                    "Set PS5UPLOAD_PKG_HOST_IP to this computer's LAN address and restart the engine.",
                ),
            ),
        }
    }

    let running = probe_port(&host, INSTALLER_PORT, PORT_TIMEOUT)
        .and_then(|()| ps5upload_core::installer_client::hello(&host).map(|h| h.version));
    out.push(installer_check(match running {
        Ok(version) => InstallerState::Running(version),
        Err(_) if !helper_up => InstallerState::NotTried,
        Err(_) => {
            let elf =
                crate::bundled_payload::image_bytes(crate::bundled_payload::Image::Installer).ok();
            // protect_running: a daemon that is serving an install is never replaced here.
            let e = ps5upload_core::installer_client::ensure(&host, elf.as_deref(), true);
            if e.listening {
                InstallerState::Started
            } else {
                InstallerState::CouldNotStart(
                    e.error
                        .or(e.reason.map(str::to_string))
                        .unwrap_or_else(|| "no reason given".into()),
                )
            }
        }
    }));

    let dir = crate::remote::store::data_dir();
    let write = match &dir {
        Some(d) => {
            let probe = d.join(format!(".health-write-{}", std::process::id()));
            std::fs::create_dir_all(d)
                .and_then(|()| std::fs::write(&probe, b"ok"))
                .map(|()| {
                    let _ = std::fs::remove_file(&probe);
                })
                .map_err(|e| e.to_string())
        }
        None => Err(String::new()),
    };
    out.push(data_dir_check(dir.as_deref(), write));
    out
}

/// `http://host:port` -> (host, port). The origin is one this engine just built.
fn split_origin(origin: &str) -> (String, u16) {
    let rest = origin.trim_start_matches("http://");
    match rest.rsplit_once(':') {
        Some((h, p)) => (
            h.trim_start_matches('[').trim_end_matches(']').to_string(),
            p.parse().unwrap_or(19113),
        ),
        None => (rest.to_string(), 19113),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_loader_port_is_a_warning_that_says_what_still_works() {
        let c = loader_port_check(Err("connection refused".into()));
        assert_eq!(c.status, CheckStatus::Warn);
        assert!(c.remedy.contains("keeps working"));
        assert_eq!(loader_port_check(Ok(())).status, CheckStatus::Pass);
    }

    #[test]
    fn an_installer_that_could_not_be_started_fails_and_says_why() {
        let up = installer_check(InstallerState::Running("1.3.9".into()));
        assert_eq!(up.status, CheckStatus::Pass);
        assert!(up.detail.contains("1.3.9"));
        assert_eq!(
            installer_check(InstallerState::Started).status,
            CheckStatus::Pass
        );
        let bad = installer_check(InstallerState::CouldNotStart(
            "nothing answered on the ELF loader port :9021".into(),
        ));
        assert_eq!(bad.status, CheckStatus::Fail);
        assert!(bad.detail.contains(":9021"));
        assert!(bad.remedy.contains("elfldr"));
        // Without the helper nothing can install, so the scan does not send anything.
        assert_eq!(
            installer_check(InstallerState::NotTried).status,
            CheckStatus::Skip
        );
    }

    #[test]
    fn a_ps5_that_cannot_reach_us_fails_and_names_the_address_and_the_firewall() {
        let c = reach_check(
            "http://192.168.1.20:19113",
            Reach::Refused {
                timed_out: true,
                err: String::new(),
            },
            false,
            false,
        );
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.detail.contains("192.168.1.20:19113"));
        assert!(c.remedy.contains("firewall"));
        assert!(c.remedy.contains("Upload & install does not need it"));
        assert!(!c.remedy.contains("container"));
    }

    #[test]
    fn the_reach_remedy_follows_how_the_address_was_chosen() {
        let refused = || Reach::Refused {
            timed_out: false,
            err: "ECONNREFUSED".into(),
        };
        let boxed = reach_check("http://172.17.0.2:19113", refused(), false, true);
        assert!(boxed.remedy.contains("host networking"));
        let pinned = reach_check("http://10.0.0.9:19113", refused(), true, true);
        assert!(pinned.remedy.contains("PS5UPLOAD_PKG_HOST_IP: check"));
        assert!(!pinned.remedy.contains("host networking"));
    }

    #[test]
    fn reach_that_worked_passes_and_an_old_helper_is_not_blamed() {
        let ok = reach_check("http://h:1", Reach::Ok { ms: 4 }, false, false);
        assert_eq!(ok.status, CheckStatus::Pass);
        assert!(ok.detail.contains("4 ms"));
        let old = reach_check(
            "http://h:1",
            Reach::Unknown("unknown frame".into()),
            false,
            false,
        );
        assert_eq!(old.status, CheckStatus::Skip);
    }

    #[test]
    fn a_bridged_container_with_no_pinned_address_fails_and_others_do_not() {
        assert!(container_address_check(false, false).is_none());
        assert_eq!(
            container_address_check(true, true).map(|c| c.status),
            Some(CheckStatus::Fail)
        );
        assert_eq!(
            container_address_check(true, false).map(|c| c.status),
            Some(CheckStatus::Pass)
        );
    }

    #[test]
    fn reply_time_warns_when_slow_and_never_fails() {
        assert_eq!(median_ms(vec![]), None);
        assert_eq!(median_ms(vec![9, 2, 400, 3, 4]), Some(4));
        assert_eq!(reply_time_check(Some(4), 5).status, CheckStatus::Pass);
        let slow = reply_time_check(Some(SLOW_REPLY_MS + 1), 5);
        assert_eq!(slow.status, CheckStatus::Warn);
        assert!(slow.remedy.contains("Wi-Fi"));
        assert_eq!(reply_time_check(None, 0).status, CheckStatus::Skip);
    }

    #[test]
    fn a_data_folder_that_cannot_be_written_fails() {
        let p = std::path::Path::new("/tmp/x");
        assert_eq!(data_dir_check(Some(p), Ok(())).status, CheckStatus::Pass);
        assert_eq!(
            data_dir_check(Some(p), Err("read-only".into())).status,
            CheckStatus::Fail
        );
        assert_eq!(
            data_dir_check(None, Err(String::new())).status,
            CheckStatus::Fail
        );
    }

    #[test]
    fn an_origin_splits_into_host_and_port() {
        assert_eq!(
            split_origin("http://192.168.1.20:19113"),
            ("192.168.1.20".into(), 19113)
        );
        assert_eq!(
            split_origin("http://[fe80::1]:19199"),
            ("fe80::1".into(), 19199)
        );
    }
}
