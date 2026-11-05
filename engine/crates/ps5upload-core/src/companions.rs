//! Probe a PS5 for known third-party tool ports.
//!
//! A jailbroken PS5 typically hosts a few tools (a homebrew enabler, an FTP server, ...), each on
//! a well-known TCP port. This is deliberately dumb: TCP connect with a short timeout, report up
//! or down, no handshake, since each tool has its own protocol. A caller shows the tools that
//! answer; the ports below are what we can detect, not what must be installed.

use std::sync::mpsc;
use std::time::Duration;

use serde::Serialize;

use crate::payload_lifecycle::probe_port;

/// One well-known scene-tool listener.
#[derive(Clone, Copy)]
struct Companion {
    /// Short name in English: the Connection strip words it from the port and falls back to this.
    name: &'static str,
    /// Purpose blurb, the same way.
    role: &'static str,
    /// TCP port the tool listens on when running.
    port: u16,
    /// The tool serves its own web UI on that port, which the strip can open.
    web: bool,
}

/// Port values cross-checked against each project's README. Our own injection lives inside
/// `ps5upload.elf`, so it isn't a companion here.
const COMPANIONS: &[Companion] = &[
    Companion {
        name: "etaHEN",
        role: "Homebrew enabler + launcher",
        port: 9028,
        web: false,
    },
    // Both etaHEN and shsrv serve a telnet shell here, so this port alone says nothing about
    // which is loaded (it used to read "Homebrew enabler" for a console with only shsrv).
    Companion {
        name: "Telnet shell",
        role: "Telnet shell (etaHEN or shsrv)",
        port: 2323,
        web: false,
    },
    Companion {
        name: "FTP",
        role: "FTP server payload",
        port: 2121,
        web: false,
    },
    Companion {
        name: "Payload Manager",
        role: "Keeps payloads on the console and loads them at boot (itsPLK)",
        port: 8084,
        web: true,
    },
    Companion {
        name: "LZ4 Compression",
        role: "Compresses games on the PS5 itself (LZ4 packs for AMPR titles)",
        port: 5920,
        web: true,
    },
];

/// One row per companion. `error` is the reason it is not reachable.
#[derive(Debug, Clone, Serialize)]
pub struct CompanionStatus {
    pub name: &'static str,
    pub role: &'static str,
    pub port: u16,
    pub web: bool,
    pub reachable: bool,
    pub error: Option<String>,
}

/// Short on purpose: this runs every time the Connection strip refreshes, and a dead PS5 shouldn't
/// hold the UI for seconds per port.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// A host name gets its own, longer budget: a slow first lookup used to read as "this tool isn't
/// running" (#272). No row waits longer than this.
const ROW_DEADLINE: Duration = Duration::from_secs(12);

fn row(c: &Companion, result: Result<(), String>) -> CompanionStatus {
    CompanionStatus {
        name: c.name,
        role: c.role,
        port: c.port,
        web: c.web,
        reachable: result.is_ok(),
        error: result.err(),
    }
}

/// Probe every companion port on `host` at once; the slowest bounds the call, not the sum. One row
/// per companion, in table order, so a list renders stably.
pub fn probe_companions(host: &str) -> Vec<CompanionStatus> {
    let host = host.trim();
    if host.is_empty() {
        return COMPANIONS
            .iter()
            .map(|c| row(c, Err("No PS5 host configured".to_string())))
            .collect();
    }
    let (tx, rx) = mpsc::channel();
    for (i, c) in COMPANIONS.iter().enumerate() {
        let (row_tx, host, port) = (tx.clone(), host.to_string(), c.port);
        let spawned = std::thread::Builder::new()
            .name("companion-probe".into())
            .spawn(move || {
                let _ = row_tx.send((i, probe_port(&host, port, PROBE_TIMEOUT)));
            });
        if let Err(e) = spawned {
            let _ = tx.send((i, Err(format!("probe thread: {e}"))));
        }
    }
    drop(tx);
    let mut results: Vec<Option<Result<(), String>>> = vec![None; COMPANIONS.len()];
    let deadline = std::time::Instant::now() + ROW_DEADLINE;
    while let Ok((i, r)) =
        rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
    {
        results[i] = Some(r);
    }
    COMPANIONS
        .iter()
        .zip(results)
        .map(|(c, r)| row(c, r.unwrap_or_else(|| Err("timeout".to_string()))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_host_is_said_on_every_row() {
        let rows = probe_companions("  ");
        assert_eq!(rows.len(), COMPANIONS.len());
        assert!(rows
            .iter()
            .all(|r| !r.reachable && r.error.as_deref() == Some("No PS5 host configured")));
    }

    #[test]
    fn etahen_is_probed_on_its_own_port_and_2323_is_just_a_shell() {
        // shsrv also listens on 2323, so that port must not claim to be the homebrew enabler.
        let by_port = |p: u16| COMPANIONS.iter().find(|c| c.port == p).unwrap();
        assert_eq!(by_port(9028).name, "etaHEN");
        assert_eq!(by_port(2323).name, "Telnet shell");
        assert!(!by_port(2323).role.starts_with("Homebrew"));
    }

    #[test]
    fn rows_keep_table_order_and_a_name_that_never_resolves_fails_on_each() {
        let rows = probe_companions("no-such-host.invalid");
        let want: Vec<u16> = COMPANIONS.iter().map(|c| c.port).collect();
        let got: Vec<u16> = rows.iter().map(|r| r.port).collect();
        assert_eq!(got, want);
        assert!(rows.iter().all(|r| !r.reachable && r.error.is_some()));
    }
}
