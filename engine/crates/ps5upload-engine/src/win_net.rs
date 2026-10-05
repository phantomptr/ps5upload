//! Why a PS5 cannot reach this computer, on Windows (Discord 2026-10-04: F2.1, F2.3).
//!
//! The usual cause of "the PS5 never reached this computer" on Windows is the network category:
//! a cable straight to the console is an unidentified network, which Windows files as Public, and
//! the inbound rules the installer created allow Private only. "Firewall off" in the UI often
//! turns off a single profile. This module finds the local adapter on the console's subnet, reads
//! that adapter's category, and reads whether an inbound rule lets this engine in on it.
//!
//! Where it lives: in the engine. The engine is the process that listens on the port the console
//! connects to, wherever the UI runs (desktop sidecar, web UI, a remote browser), and it is the
//! one that knows the console's address when the stream fails. It needs no Windows crate: the
//! read is one PowerShell call (the same pattern `client/src-tauri/src/engine.rs` uses for port
//! owners). Everything that interprets the answer is plain Rust and is tested on every platform;
//! only the process spawning is `cfg(windows)`. Off Windows [`diagnose`] returns `None` and every
//! message stays as it was.
//!
//! The two fixes are never silent. "Make this network Private" only opens Windows Settings; the
//! person flips the switch. "Allow ps5upload on Public networks" adds a firewall rule through an
//! elevated `netsh`, which raises Windows' own consent prompt, and the engine refuses the request
//! unless it carries `confirm: true` (the UI asks first).
// The probe's interpretation is exercised on every platform; only Windows spawns the probe.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

/// A Windows network category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetCategory {
    Public,
    Private,
    Domain,
    Unknown,
}

impl NetCategory {
    fn from_windows(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "public" | "0" => Self::Public,
            "private" | "1" => Self::Private,
            "domainauthenticated" | "domain" | "2" => Self::Domain,
            _ => Self::Unknown,
        }
    }

    /// The name Windows' firewall profiles use.
    fn profile(self) -> Option<&'static str> {
        match self {
            Self::Public => Some("Public"),
            Self::Private => Some("Private"),
            Self::Domain => Some("Domain"),
            Self::Unknown => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Public => "Public",
            Self::Private => "Private",
            Self::Domain => "Domain",
            Self::Unknown => "unidentified",
        }
    }
}

/// What was found about the link to the console.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetDiag {
    /// The local adapter on the console's subnet ("Ethernet 3").
    pub adapter: String,
    /// This computer's address on that adapter.
    pub local_ip: String,
    pub category: NetCategory,
    /// Whether Windows Firewall is on for that category's profile (`None` = not read).
    pub firewall_enabled: Option<bool>,
    /// Whether an enabled inbound Allow rule for this engine (by program, or by its port) covers
    /// that category's profile (`None` = not read).
    pub allowed_by_rule: Option<bool>,
}

impl NetDiag {
    /// The category is Public and ps5upload is not allowed on it: the case both fixes target.
    pub fn blocked_by_category(&self) -> bool {
        self.category == NetCategory::Public
            && self.firewall_enabled != Some(false)
            && self.allowed_by_rule != Some(true)
    }

    /// The sentences that name the adapter and its category and say what Windows is doing about
    /// it. `None` when the category is unknown (the generic advice then stands alone).
    pub fn explain(&self) -> Option<String> {
        let p = self.category.profile()?;
        let net = format!(
            "\u{201c}{}\u{201d} ({}), the connection to the PS5,",
            self.adapter, self.local_ip
        );
        let cat = self.category.word();
        Some(if self.firewall_enabled == Some(false) {
            format!(
                "{net} is on a {cat} network and Windows Firewall is off for {p} networks, so Windows is not what blocks the connection: look for a third-party firewall or antivirus, or a VPN."
            )
        } else if self.allowed_by_rule == Some(true) {
            format!(
                "{net} is on a {cat} network and Windows Firewall allows ps5upload on it, so look for a third-party firewall or antivirus, or a VPN."
            )
        } else if self.category == NetCategory::Public {
            format!(
                "{net} is on a Public network, and Windows Firewall blocks ps5upload on Public networks. Make this network Private, or allow ps5upload on Public networks."
            )
        } else {
            format!(
                "{net} is on a {cat} network, but no Windows Firewall rule allows ps5upload in on {p} networks. Allow ps5upload on {p} networks."
            )
        })
    }
}

// ---- parsing the probe's answer (all platforms) ---------------------------------------------

/// One local IPv4 address with its adapter and category, as the probe reports it.
#[derive(Debug, Clone, Deserialize)]
struct AdapterRow {
    #[serde(default)]
    alias: String,
    #[serde(default)]
    ip: String,
    #[serde(default)]
    prefix: u32,
    #[serde(default)]
    category: String,
}

/// PowerShell's `ConvertTo-Json` writes a one-item array as the bare item and an empty one as null.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    Many(Vec<T>),
    One(T),
}

fn flat<T>(v: Option<OneOrMany<T>>) -> Vec<T> {
    match v {
        None => Vec::new(),
        Some(OneOrMany::Many(v)) => v,
        Some(OneOrMany::One(x)) => vec![x],
    }
}

#[derive(Debug, Deserialize)]
struct Probe {
    adapters: Option<OneOrMany<AdapterRow>>,
    /// The `Profile` of every enabled inbound Allow rule for this engine, as Windows prints it
    /// ("Any", "Public", "Private, Public", ...).
    rules: Option<OneOrMany<String>>,
    #[serde(default)]
    firewall: BTreeMap<String, bool>,
}

/// Whether `a` and `b` are on one IPv4 subnet of `prefix` bits.
fn same_subnet(a: Ipv4Addr, b: Ipv4Addr, prefix: u32) -> bool {
    if prefix == 0 || prefix > 32 {
        return false;
    }
    let mask = u32::MAX << (32 - prefix);
    u32::from(a) & mask == u32::from(b) & mask
}

/// Whether a rule's `Profile` text covers `profile` ("Any" covers all).
fn rule_covers(rule_profile: &str, profile: &str) -> bool {
    rule_profile
        .split(',')
        .map(|p| p.trim().to_ascii_lowercase())
        .any(|p| p == "any" || p == "all" || p == profile.to_ascii_lowercase())
}

/// Reads the probe's JSON into a diagnosis for the adapter that shares a subnet with `console`.
/// `None` when the answer is unreadable or no adapter shares the console's subnet (a routed
/// console: nothing to say about a cable).
pub(crate) fn parse_probe(json: &str, console: Ipv4Addr) -> Option<NetDiag> {
    let probe: Probe = serde_json::from_str(json.trim().trim_start_matches('\u{feff}')).ok()?;
    let rows = flat(probe.adapters);
    let row = rows
        .iter()
        .filter(|r| !r.alias.is_empty())
        .filter_map(|r| Some((r, r.ip.parse::<Ipv4Addr>().ok()?)))
        // the most specific match: a /24 to the console beats a /8 that happens to contain it
        .filter(|(r, ip)| *ip != console && same_subnet(*ip, console, r.prefix))
        .max_by_key(|(r, _)| r.prefix)?
        .0;
    let category = NetCategory::from_windows(&row.category);
    let profile = category.profile();
    let rules = flat(probe.rules);
    Some(NetDiag {
        adapter: row.alias.clone(),
        local_ip: row.ip.clone(),
        category,
        firewall_enabled: profile.and_then(|p| probe.firewall.get(p).copied()),
        allowed_by_rule: profile.map(|p| rules.iter().any(|r| rule_covers(r, p))),
    })
}

// ---- the Windows side -----------------------------------------------------------------------

/// Quotes `s` as a PowerShell single-quoted string.
pub(crate) fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The probe script: one JSON object with every local IPv4 address (adapter, prefix, category),
/// the profiles of the enabled inbound Allow rules for `exe` or for TCP `port`, and whether the
/// firewall is on per profile.
pub(crate) fn probe_script(exe: &str, port: u16) -> String {
    format!(
        r#"$ErrorActionPreference = 'SilentlyContinue'
$exe = {exe}
$port = '{port}'
$adapters = @(Get-NetIPAddress -AddressFamily IPv4 | ForEach-Object {{
  $p = Get-NetConnectionProfile -InterfaceIndex $_.InterfaceIndex
  [pscustomobject]@{{ alias = [string]$_.InterfaceAlias; ip = [string]$_.IPAddress; prefix = [int]$_.PrefixLength; category = $(if ($p) {{ [string]$p.NetworkCategory }} else {{ '' }}) }}
}})
$allow = {{ $_.Direction -eq 'Inbound' -and [string]$_.Enabled -eq 'True' -and $_.Action -eq 'Allow' }}
$byProgram = @(Get-NetFirewallApplicationFilter -Program $exe | Get-NetFirewallRule | Where-Object $allow | ForEach-Object {{ [string]$_.Profile }})
$byPort = @(Get-NetFirewallPortFilter -Protocol TCP | Where-Object {{ $_.LocalPort -contains $port }} | Get-NetFirewallRule | Where-Object $allow | ForEach-Object {{ [string]$_.Profile }})
$fw = @{{}}
foreach ($n in 'Domain', 'Private', 'Public') {{ $fw[$n] = ([string](Get-NetFirewallProfile -Name $n).Enabled -eq 'True') }}
[pscustomobject]@{{ adapters = $adapters; rules = @($byProgram + $byPort); firewall = $fw }} | ConvertTo-Json -Depth 4 -Compress"#,
        exe = ps_quote(exe),
    )
}

/// The `netsh` arguments of the rule "Allow ps5upload on `profile` networks": inbound TCP for
/// this engine's program only (not the whole firewall, not every program).
pub(crate) fn allow_rule_args(exe: &str, profile: &str) -> String {
    format!(
        "advfirewall firewall add rule name=\"ps5upload engine ({profile})\" dir=in action=allow program=\"{exe}\" profile={} protocol=TCP enable=yes",
        profile.to_ascii_lowercase()
    )
}

/// The script that runs `netsh` elevated (Windows' own consent prompt) and exits with its code.
pub(crate) fn elevate_script(exe: &str, profile: &str) -> String {
    format!(
        "$p = Start-Process -FilePath 'netsh.exe' -ArgumentList {} -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
        ps_quote(&allow_rule_args(exe, profile))
    )
}

/// The profile names the engine will add a rule for. Anything else is refused.
pub fn allowed_profile(p: &str) -> Option<&'static str> {
    match p.trim().to_ascii_lowercase().as_str() {
        "public" => Some("Public"),
        "private" => Some("Private"),
        _ => None,
    }
}

/// Which Settings page lists the adapter's network profile.
pub(crate) fn settings_uri(adapter: &str) -> &'static str {
    let a = adapter.to_ascii_lowercase();
    if a.contains("wi-fi") || a.contains("wifi") || a.contains("wireless") || a.contains("wlan") {
        "ms-settings:network-wifi"
    } else {
        "ms-settings:network-ethernet"
    }
}

/// `-EncodedCommand` text for `script`: base64 of its UTF-16LE bytes. It reaches PowerShell with
/// no command-line quoting to get wrong (the scripts hold quotes of both kinds).
pub(crate) fn encoded_command(script: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(windows)]
mod win {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// No console window flashes when the engine runs under the GUI shell.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// Runs `script` in Windows PowerShell and returns its stdout and exit code; `None` when it
    /// cannot start or does not finish within `limit`.
    pub(super) fn powershell(script: &str, limit: Duration) -> Option<(String, i32)> {
        let mut child = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-EncodedCommand",
                &super::encoded_command(script),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok()?;
        let started = Instant::now();
        loop {
            match child.try_wait().ok()? {
                Some(status) => {
                    let mut out = String::new();
                    if let Some(mut s) = child.stdout.take() {
                        use std::io::Read;
                        let _ = s.read_to_string(&mut out);
                    }
                    return Some((out, status.code().unwrap_or(-1)));
                }
                None if started.elapsed() > limit => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    pub(super) fn open(uri: &str) -> bool {
        Command::new("explorer.exe")
            .arg(uri)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .is_ok()
    }
}

/// Diagnoses the link to `console` (an IPv4 address). Blocking: it runs PowerShell, a second or
/// two; call it from a blocking task and only once something has already failed. `None` off
/// Windows, for a console that is not IPv4 or not on a local subnet, or when the read failed.
#[cfg(windows)]
pub fn diagnose(console: &str, engine_port: u16) -> Option<NetDiag> {
    let console: Ipv4Addr = console.trim().parse().ok()?;
    let exe = std::env::current_exe().ok()?.to_string_lossy().into_owned();
    let (out, code) = win::powershell(
        &probe_script(&exe, engine_port),
        std::time::Duration::from_secs(12),
    )?;
    if code != 0 {
        return None;
    }
    parse_probe(&out, console)
}

#[cfg(not(windows))]
pub fn diagnose(_console: &str, _engine_port: u16) -> Option<NetDiag> {
    None
}

/// Opens Windows Settings at the network page for `adapter`. The person changes the category;
/// nothing is changed here.
#[cfg(windows)]
pub fn open_network_settings(adapter: &str) -> Result<(), String> {
    if win::open(settings_uri(adapter)) {
        Ok(())
    } else {
        Err("could not open Windows Settings".into())
    }
}

#[cfg(not(windows))]
pub fn open_network_settings(_adapter: &str) -> Result<(), String> {
    Err("this computer is not running Windows".into())
}

/// Adds the inbound rule through an elevated `netsh` (Windows' consent prompt appears; declining
/// it is an `Err`). Only ever called after the person confirmed in the UI.
#[cfg(windows)]
pub fn allow_on_profile(profile: &str) -> Result<(), String> {
    let profile = allowed_profile(profile).ok_or("unknown network profile")?;
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    match win::powershell(
        &elevate_script(&exe, profile),
        // the prompt waits for a person
        std::time::Duration::from_secs(120),
    ) {
        Some((_, 0)) => Ok(()),
        Some((_, _)) => {
            Err("the firewall rule was not added (the prompt was declined, or netsh failed)".into())
        }
        None => Err("Windows did not answer in time".into()),
    }
}

#[cfg(not(windows))]
pub fn allow_on_profile(_profile: &str) -> Result<(), String> {
    Err("this computer is not running Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONSOLE: Ipv4Addr = Ipv4Addr::new(192, 168, 88, 2);

    fn probe(adapters: &str, rules: &str, fw: &str) -> String {
        format!(r#"{{"adapters":{adapters},"rules":{rules},"firewall":{fw}}}"#)
    }

    const WIFI: &str = r#"{"alias":"Wi-Fi","ip":"192.168.1.20","prefix":24,"category":"Private"}"#;
    const CABLE_PUBLIC: &str =
        r#"{"alias":"Ethernet 3","ip":"192.168.88.1","prefix":24,"category":"Public"}"#;
    const FW_ON: &str = r#"{"Domain":true,"Private":true,"Public":true}"#;

    #[test]
    fn the_cable_on_the_consoles_subnet_is_named_with_its_category() {
        let j = probe(&format!("[{WIFI},{CABLE_PUBLIC}]"), r#"["Private"]"#, FW_ON);
        let d = parse_probe(&j, CONSOLE).unwrap();
        assert_eq!(d.adapter, "Ethernet 3");
        assert_eq!(d.local_ip, "192.168.88.1");
        assert_eq!(d.category, NetCategory::Public);
        assert_eq!(d.allowed_by_rule, Some(false), "the rule is Private only");
        assert_eq!(d.firewall_enabled, Some(true));
        assert!(d.blocked_by_category());
        let e = d.explain().unwrap();
        assert!(
            e.contains("Ethernet 3") && e.contains("192.168.88.1"),
            "{e}"
        );
        assert!(e.contains("Public network"), "{e}");
        assert!(e.contains("Make this network Private"), "{e}");
        assert!(e.contains("allow ps5upload on Public networks"), "{e}");
    }

    #[test]
    fn powershell_writes_a_single_item_as_a_bare_object_and_nothing_as_null() {
        let j = probe(CABLE_PUBLIC, "null", FW_ON);
        let d = parse_probe(&j, CONSOLE).unwrap();
        assert_eq!(d.allowed_by_rule, Some(false));
        let j = probe(CABLE_PUBLIC, r#""Any""#, FW_ON);
        assert_eq!(
            parse_probe(&j, CONSOLE).unwrap().allowed_by_rule,
            Some(true)
        );
        // a byte-order mark from a redirected console is tolerated
        let j = format!("\u{feff}{}", probe(CABLE_PUBLIC, "[]", FW_ON));
        assert!(parse_probe(&j, CONSOLE).is_some());
    }

    #[test]
    fn a_rule_covers_the_profiles_windows_lists() {
        assert!(rule_covers("Any", "Public"));
        assert!(rule_covers("Private, Public", "Public"));
        assert!(rule_covers("public", "Public"));
        assert!(!rule_covers("Private", "Public"));
        assert!(!rule_covers("Domain, Private", "Public"));
    }

    #[test]
    fn a_firewall_that_is_off_for_the_profile_is_not_blamed() {
        let j = probe(
            CABLE_PUBLIC,
            "[]",
            r#"{"Domain":true,"Private":true,"Public":false}"#,
        );
        let d = parse_probe(&j, CONSOLE).unwrap();
        assert!(!d.blocked_by_category());
        let e = d.explain().unwrap();
        assert!(e.contains("off for Public networks"), "{e}");
        assert!(e.contains("third-party firewall"), "{e}");
        assert!(!e.contains("Make this network Private"), "{e}");
    }

    #[test]
    fn a_private_network_with_no_rule_offers_the_rule_and_not_the_category_change() {
        let j = probe(
            r#"{"alias":"Ethernet","ip":"192.168.88.1","prefix":24,"category":"Private"}"#,
            "[]",
            FW_ON,
        );
        let d = parse_probe(&j, CONSOLE).unwrap();
        assert!(!d.blocked_by_category());
        let e = d.explain().unwrap();
        assert!(e.contains("Private network"), "{e}");
        assert!(e.contains("Allow ps5upload on Private networks"), "{e}");
    }

    #[test]
    fn a_private_network_with_a_rule_points_elsewhere() {
        let j = probe(
            r#"{"alias":"Ethernet","ip":"192.168.88.1","prefix":24,"category":"Private"}"#,
            r#"["Private"]"#,
            FW_ON,
        );
        let e = parse_probe(&j, CONSOLE).unwrap().explain().unwrap();
        assert!(e.contains("allows ps5upload on it"), "{e}");
    }

    #[test]
    fn the_most_specific_subnet_wins_and_a_routed_console_has_no_adapter() {
        let wide = r#"{"alias":"VPN","ip":"192.168.0.9","prefix":16,"category":"Public"}"#;
        let j = probe(&format!("[{wide},{CABLE_PUBLIC}]"), "[]", FW_ON);
        assert_eq!(parse_probe(&j, CONSOLE).unwrap().adapter, "Ethernet 3");
        let far = probe(WIFI, "[]", FW_ON);
        assert!(parse_probe(&far, CONSOLE).is_none());
        assert!(parse_probe("not json", CONSOLE).is_none());
    }

    #[test]
    fn an_unidentified_category_says_nothing_rather_than_guess() {
        let j = probe(
            r#"{"alias":"Ethernet","ip":"192.168.88.1","prefix":24,"category":""}"#,
            "[]",
            FW_ON,
        );
        let d = parse_probe(&j, CONSOLE).unwrap();
        assert_eq!(d.category, NetCategory::Unknown);
        assert!(d.explain().is_none());
        assert!(!d.blocked_by_category());
        // PowerShell 5.1 writes the enum as a number when it is not cast to a string
        assert_eq!(NetCategory::from_windows("0"), NetCategory::Public);
        assert_eq!(
            NetCategory::from_windows("DomainAuthenticated"),
            NetCategory::Domain
        );
    }

    #[test]
    fn the_subnet_arithmetic_is_exact() {
        let a = Ipv4Addr::new(192, 168, 88, 1);
        assert!(same_subnet(a, CONSOLE, 24));
        assert!(!same_subnet(a, Ipv4Addr::new(192, 168, 89, 2), 24));
        assert!(same_subnet(a, Ipv4Addr::new(192, 168, 89, 2), 16));
        assert!(!same_subnet(a, CONSOLE, 0));
        assert!(!same_subnet(a, CONSOLE, 33));
        assert!(same_subnet(a, a, 32));
    }

    #[test]
    fn quoting_survives_an_apostrophe_in_the_path() {
        assert_eq!(
            ps_quote(r"C:\Users\O'Neil\x.exe"),
            r"'C:\Users\O''Neil\x.exe'"
        );
        let s = probe_script(r"C:\Users\O'Neil\ps5upload-engine.exe", 19113);
        assert!(
            s.contains(r"$exe = 'C:\Users\O''Neil\ps5upload-engine.exe'"),
            "{s}"
        );
        assert!(s.contains("$port = '19113'"));
    }

    #[test]
    fn the_rule_is_for_this_program_and_one_profile_only() {
        let a = allow_rule_args(r"C:\App\ps5upload-engine.exe", "Public");
        assert!(
            a.contains(r#"program="C:\App\ps5upload-engine.exe""#),
            "{a}"
        );
        assert!(a.contains("profile=public") && a.contains("dir=in") && a.contains("action=allow"));
        assert!(a.contains("protocol=TCP"));
        assert!(
            !a.contains("profile=any") && !a.contains("profile=public,"),
            "{a}"
        );
        let e = elevate_script(r"C:\App\ps5upload-engine.exe", "Public");
        assert!(e.contains("-Verb RunAs") && e.contains("-Wait") && e.contains("exit $p.ExitCode"));
    }

    #[test]
    fn the_script_travels_as_utf16_base64() {
        use base64::Engine as _;
        let enc = encoded_command("a'\"é");
        let raw = base64::engine::general_purpose::STANDARD
            .decode(enc)
            .unwrap();
        let units: Vec<u16> = raw
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "a'\"é");
    }

    #[test]
    fn only_public_and_private_may_be_allowed() {
        assert_eq!(allowed_profile("public"), Some("Public"));
        assert_eq!(allowed_profile(" Private "), Some("Private"));
        assert_eq!(allowed_profile("any"), None);
        assert_eq!(allowed_profile("Public,Private"), None);
        assert_eq!(allowed_profile("public\" & calc"), None);
    }

    #[test]
    fn settings_opens_at_the_adapters_page() {
        assert_eq!(settings_uri("Wi-Fi"), "ms-settings:network-wifi");
        assert_eq!(settings_uri("Ethernet 3"), "ms-settings:network-ethernet");
    }

    #[cfg(not(windows))]
    #[test]
    fn off_windows_there_is_no_diagnosis_and_no_fix() {
        assert!(diagnose("192.168.88.2", 19113).is_none());
        assert!(open_network_settings("Ethernet").is_err());
        assert!(allow_on_profile("Public").is_err());
    }
}
