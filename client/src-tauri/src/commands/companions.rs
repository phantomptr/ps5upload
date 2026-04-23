//! Probe a PS5 for known scene-tool ports.
//!
//! A jailbroken PS5 typically hosts a few compositional tools —
//! etaHEN for homebrew launch, ftpsrv for file transfer — each
//! listening on a well-known TCP port. A cheap "what's currently
//! alive?" surface is just to connect-probe each port.
//!
//! This command is deliberately dumb: TCP connect with a short timeout,
//! report up/down. We don't attempt a handshake or version check — the
//! scene tools each have their own protocols we don't want to speak
//! from here. The probe is purely informational; features that actually
//! need injection go through ps5upload's own built-in path, not this.
//!
//! The UI filters for `reachable == true` before rendering, so the
//! strip shows **only the tools the user currently has loaded**, not a
//! hardcoded "here's what the ecosystem offers" list. The hardcoded
//! ports below are the known-vocabulary of what we can detect — not
//! what must be installed.

use std::time::Duration;

use serde::Serialize;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// A single well-known scene-tool listener.
///
/// Kept as `&'static str` fields so the table below is a `const` — no
/// allocation, no runtime setup. The table is the source of truth for
/// which tools we surface in the UI; adding a row here + a case in
/// About / Connection's companion strip is enough to surface a new
/// companion.
#[derive(Clone, Copy)]
struct Companion {
    /// UI-facing short name. Shown as-is in the Connection strip chip.
    name: &'static str,
    /// Purpose blurb. Deliberately terse — the About page's tier writeup
    /// is where the longer story lives.
    role: &'static str,
    /// TCP port the tool listens on when running.
    port: u16,
}

/// The companions we probe. Port values cross-checked against each
/// project's README; stable within a given tool release since they're
/// baked into the tool's source, not config.
///
/// Our own injection capability lives inside `ps5upload.elf` on the
/// management port, so it isn't surfaced as a separate companion —
/// if the payload is alive, injection is available.
///
/// NOTE: we dropped NineS and kldload. NineS is superseded by the
/// payload's built-in injector; kldload only applies to ≤6.x firmware
/// which isn't a target anymore. If a user still runs them, they know
/// those ports and don't need us to probe them. The UI hides
/// unreachable rows so keeping a long list here would just clutter the
/// code without affecting the user's strip.
const COMPANIONS: &[Companion] = &[
    Companion {
        name: "etaHEN",
        role: "Homebrew enabler + launcher",
        port: 2323,
    },
    Companion {
        name: "ftpsrv",
        role: "FTP server payload",
        port: 2121,
    },
];

/// Serialised as one row per probed companion. `reachable = null` means
/// we couldn't even try (e.g. DNS failure for the host); `false` means
/// TCP refused or timed out; `true` means the port accepted a
/// connection. Errors are surfaced with a short message so the UI can
/// render "NineS · not reachable · connection refused" when helpful.
#[derive(Serialize)]
pub struct CompanionStatus {
    name: &'static str,
    role: &'static str,
    port: u16,
    reachable: bool,
    error: Option<String>,
}

/// Per-probe timeout. Short on purpose — this runs every time the
/// Connection strip refreshes and a dead PS5 shouldn't wedge the UI
/// for several seconds per port.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Probe every companion port on the given host concurrently.
///
/// Returns one row per companion in the same order as the `COMPANIONS`
/// table so the UI can render a stable list even when some rows fail.
/// Host is passed in rather than read from a store so this command
/// stays pure: same input → same output, trivially cacheable in the UI
/// if we ever want to avoid re-probing on every tab switch.
#[tauri::command]
pub async fn companion_probe(host: String) -> Vec<CompanionStatus> {
    let host = host.trim().to_string();
    if host.is_empty() {
        // Return all rows as unreachable with a clear error so the UI
        // doesn't render "probed but empty" — this state is different
        // from "probed and everything was down".
        return COMPANIONS
            .iter()
            .map(|c| CompanionStatus {
                name: c.name,
                role: c.role,
                port: c.port,
                reachable: false,
                error: Some("No PS5 host configured".to_string()),
            })
            .collect();
    }

    // Launch all probes concurrently via tokio::spawn so the slowest
    // companion bounds the whole call rather than the sum. We collect
    // results in index order so the UI sees a stable list.
    let mut handles = Vec::with_capacity(COMPANIONS.len());
    for c in COMPANIONS.iter().copied() {
        let addr = format!("{}:{}", host, c.port);
        handles.push(tokio::spawn(async move {
            match timeout(PROBE_TIMEOUT, TcpStream::connect(&addr)).await {
                Ok(Ok(_)) => CompanionStatus {
                    name: c.name,
                    role: c.role,
                    port: c.port,
                    reachable: true,
                    error: None,
                },
                Ok(Err(e)) => CompanionStatus {
                    name: c.name,
                    role: c.role,
                    port: c.port,
                    reachable: false,
                    error: Some(e.to_string()),
                },
                Err(_) => CompanionStatus {
                    name: c.name,
                    role: c.role,
                    port: c.port,
                    reachable: false,
                    error: Some("timeout".to_string()),
                },
            }
        }));
    }

    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        // A spawn panic here would be a bug in our own probe code,
        // not a failed probe — if it happens we surface it as a row
        // rather than swallowing it.
        match h.await {
            Ok(row) => out.push(row),
            Err(e) => out.push(CompanionStatus {
                name: "probe",
                role: "internal error",
                port: 0,
                reachable: false,
                error: Some(format!("task join: {e}")),
            }),
        }
    }
    out
}
