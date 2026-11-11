//! Bug-report bundle builder.
//!
//! The Bug Report page (`src/screens/BugReport`) assembles a manifest in the
//! renderer (user description, app/OS info, the diagnostic bundle, the PS5
//! snapshot) and hands it here together with a window size, the PS5's raw
//! kernel logs, and any attached screenshots. We zip the whole lot into one
//! timestamped `.zip` the user posts to Discord.
//!
//! Everything that helps debugging — and nothing that doesn't: no game/app
//! payloads, just logs, telemetry, and the user's own screenshots. Reads are
//! best-effort: a missing engine.log or an unreadable image degrades the
//! bundle, it doesn't fail it.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use zip::write::SimpleFileOptions;

/// Which sections to include. All default-on in the UI; a user can untick any.
#[derive(Deserialize)]
pub struct BugReportInclude {
    pub app_logs: bool,
    pub engine_log: bool,
    pub crash_reports: bool,
    pub ps5_logs: bool,
    pub images: bool,
}

/// One on-PS5 payload log fetched by the renderer snapshot (the helper's black
/// box). `name` is a safe leaf; `text` is the file body.
#[derive(Deserialize)]
pub struct PayloadLogFile {
    pub name: String,
    pub text: String,
}

/// A file the report builder assembled itself (timeline, MISSING.txt, report.md, screenshots):
/// the same shape as the engine's bundle entries. `text` is redacted; `base64` is written as is.
#[derive(Deserialize)]
pub struct ExtraEntry {
    pub path: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub base64: Option<String>,
}

/// A zip path the builder may use: relative, no `..`, no backslashes, at most 200 characters.
fn extra_path_ok(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 200
        && !p.starts_with('/')
        && !p.contains('\\')
        && !p.contains(':')
        && p.split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

#[derive(Deserialize)]
pub struct BugReportArgs {
    /// User-picked destination `.zip` path. On Android the save dialog returns
    /// a `content://` SAF URI here, which `std::fs` can't create — the bundler
    /// redirects those to a real path under Downloads (see `save_dest`).
    pub dest: String,
    /// The filename the renderer offered in the save dialog (e.g.
    /// `ps5upload-bugreport-<stamp>.zip`). Used as the leaf name when `dest`
    /// is an Android `content://` URI and the write is redirected to Downloads.
    /// Optional for backward compatibility with older renderers.
    #[serde(default)]
    pub dest_filename: Option<String>,
    /// Pretty-printed manifest JSON the renderer already built.
    pub report_json: String,
    /// Apply privacy redaction to every textual archive entry, including the
    /// raw app/engine/payload logs (not just structured report fields).
    #[serde(default)]
    pub redact: bool,
    /// How many minutes of app log to include (filters `app.jsonl`). Superseded by `since_ms`.
    #[serde(default)]
    pub window_minutes: u64,
    /// The report's start time: app log lines from here on are included. 0 = use `window_minutes`.
    #[serde(default)]
    pub since_ms: u64,
    /// Raw PS5 kernel logs, if a console was connected (written as .txt).
    pub klog_text: Option<String>,
    pub syslog_text: Option<String>,
    /// The payload's own on-PS5 logs (startup trace, tx events, crash marker),
    /// written under `ps5/payload-logs/`. Empty when disconnected.
    #[serde(default)]
    pub payload_logs: Vec<PayloadLogFile>,
    /// Absolute paths of user-attached screenshots.
    pub image_paths: Vec<String>,
    pub include: BugReportInclude,
    /// Files the report builder assembled (see [`ExtraEntry`]).
    #[serde(default)]
    pub extra_entries: Vec<ExtraEntry>,
}

#[derive(Serialize)]
pub struct BugReportResult {
    /// Number of files written into the zip.
    pub entries: usize,
    /// Size of the finished zip on disk.
    pub bytes: u64,
    pub dest: String,
    /// Count of app-log lines included (for the success summary).
    pub log_lines: usize,
    pub crash_reports: usize,
    pub images: usize,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Report redaction (spec §3.4), the same rules as client/src/lib/redaction.ts and tested against
/// the same vectors (redaction.vectors.json). One per report: an address gets the same
/// placeholder (`<ip-1>`, `<ip-2>`) in every file. Secrets are removed whatever `redact` says.
pub(crate) struct Redactor {
    redact: bool,
    ips: Vec<String>,
}

const SECRET_KEYS: &[&str] = &[
    "pairing_key",
    "psk",
    "token",
    "access_token",
    "refresh_token",
    "rp_key",
    "regist_key",
    "account_id",
    "psn_account_id",
    "secret",
    "password",
    "wake_credential",
    "wake_regist_key",
    "wake_rp_key",
    "pin",
    "morning",
    "passcode",
    "credential",
    "launch_token",
    "stream_key",
    "pad_token",
];

use super::redact_json::{replace_escaped_json_values, replace_json_values};

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `key=value` (key not inside a longer word): the value, up to whitespace, `&` or `"`, goes.
fn replace_kv_values(s: &str, keys: &[&str], with: &str) -> String {
    let b = s.as_bytes();
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut cursor = 0;
    let mut i = 0;
    'scan: while i < b.len() {
        // A key starts with an ASCII letter, so `i` is a char boundary whenever it can match.
        if b[i].is_ascii_alphabetic() && (i == 0 || !is_word(b[i - 1])) {
            for k in keys {
                let pat = format!("{k}=");
                if lower[i..].starts_with(&pat) {
                    let vstart = i + pat.len();
                    let mut vend = vstart;
                    while vend < b.len()
                        && !b[vend].is_ascii_whitespace()
                        && b[vend] != b'&'
                        && b[vend] != b'"'
                    {
                        vend += 1;
                    }
                    if vend > vstart {
                        out.push_str(&s[cursor..vstart]);
                        out.push_str(with);
                        cursor = vend;
                        i = vend;
                        continue 'scan;
                    }
                }
            }
        }
        i += 1;
    }
    out.push_str(&s[cursor..]);
    out
}

/// A pairing PIN after the word `PIN` (not inside a longer word): `Remote Play PIN: 14928144`,
/// `"pin":"14928144"`, `"pin": 14928144`. One to eight of `: = " \`, space or tab, then four or
/// more digits, which become `<removed>`. Helpers before 11.5 wrote the PIN to stderr.log.
fn replace_pins(s: &str) -> String {
    let b = s.as_bytes();
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut cursor = 0;
    let mut from = 0;
    while let Some(rel) = lower[from..].find("pin") {
        let at = from + rel;
        from = at + 3;
        if (at > 0 && is_word(b[at - 1])) || b.get(at + 3).is_some_and(|c| is_word(*c)) {
            continue;
        }
        let mut i = at + 3;
        while i < b.len()
            && i - (at + 3) < 8
            && matches!(b[i], b':' | b'=' | b'"' | b'\\' | b' ' | b'\t')
        {
            i += 1;
        }
        let digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if i > at + 3 && digits >= 4 {
            out.push_str(&s[cursor..i]);
            out.push_str("<removed>");
            cursor = i + digits;
            from = cursor;
        }
    }
    out.push_str(&s[cursor..]);
    out
}

/// `/Users/<name>`, `/home/<name>` and `X:\Users\<name>` become `~`. The Windows form is also
/// matched with its backslashes escaped (`C:\\Users\\bob` in JSON text, `C:\\\\Users\\\\bob` in a
/// JSON string stored as a string) — report.json and app.jsonl are JSON, so that is how every
/// Windows path in them is spelled.
fn replace_home_dirs(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut cursor = 0;
    let mut i = 0;
    let end_of_name = |mut j: usize, sep: u8| {
        while j < b.len()
            && b[j] != sep
            && !b[j].is_ascii_whitespace()
            && b[j] != b'"'
            && b[j] != b'/'
            && b[j] != b'\\'
        {
            j += 1;
        }
        j
    };
    while i < b.len() {
        // Both patterns start with an ASCII byte, so only those positions (char boundaries) are tried.
        if !b[i].is_ascii() {
            i += 1;
            continue;
        }
        let rest = &s[i..];
        let posix = ["/Users/", "/home/"].iter().find(|p| rest.starts_with(**p));
        if let Some(p) = posix {
            let name_end = end_of_name(i + p.len(), b'/');
            if name_end > i + p.len() {
                out.push_str(&s[cursor..i]);
                out.push('~');
                cursor = name_end;
                i = name_end;
                continue;
            }
        }
        if i + 2 < b.len() && b[i].is_ascii_alphabetic() && b[i + 1] == b':' && b[i + 2] == b'\\' {
            // The separator is one backslash, or the same run of them on both sides of
            // `Users` when the path is JSON-escaped.
            let mut k = i + 2;
            while k < b.len() && b[k] == b'\\' {
                k += 1;
            }
            let sep = &s[i + 2..k];
            if s[k..].starts_with("Users") && s[k + "Users".len()..].starts_with(sep) {
                let name_start = k + "Users".len() + sep.len();
                let name_end = end_of_name(name_start, b'\\');
                if name_end > name_start {
                    out.push_str(&s[cursor..i]);
                    out.push('~');
                    cursor = name_end;
                    i = name_end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&s[cursor..]);
    out
}

/// `aa:bb:cc:dd:ee:ff` (not inside a longer word) becomes `<mac>`.
fn replace_macs(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut cursor = 0;
    let mut i = 0;
    while i + 17 <= b.len() {
        let w = &b[i..i + 17];
        let is_mac = (0..6)
            .all(|k| w[k * 3].is_ascii_hexdigit() && w[k * 3 + 1].is_ascii_hexdigit())
            && (0..5).all(|k| w[k * 3 + 2] == b':');
        let bounded = (i == 0 || !is_word(b[i - 1])) && (i + 17 == b.len() || !is_word(b[i + 17]));
        if is_mac && bounded {
            out.push_str(&s[cursor..i]);
            out.push_str("<mac>");
            cursor = i + 17;
            i += 17;
        } else {
            i += 1;
        }
    }
    out.push_str(&s[cursor..]);
    out
}

impl Redactor {
    pub(crate) fn new(redact: bool) -> Self {
        Redactor {
            redact,
            ips: Vec::new(),
        }
    }

    fn ip(&mut self, addr: &str) -> String {
        let n = match self.ips.iter().position(|a| a == addr) {
            Some(i) => i + 1,
            None => {
                self.ips.push(addr.to_string());
                self.ips.len()
            }
        };
        format!("<ip-{n}>")
    }

    fn replace_ipv6(&mut self, s: &str) -> String {
        let b = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut cursor = 0;
        let mut i = 0;
        while i < b.len() {
            if b[i] != b'[' {
                i += 1;
                continue;
            }
            let Some(close) = b[i + 1..]
                .iter()
                .position(|c| *c == b']')
                .map(|p| i + 1 + p)
            else {
                break;
            };
            let inner = &s[i + 1..close];
            let is_ipv6 =
                inner.contains(':') && inner.bytes().all(|c| c.is_ascii_hexdigit() || c == b':');
            if is_ipv6 {
                out.push_str(&s[cursor..i]);
                out.push('[');
                let p = self.ip(inner);
                out.push_str(&p);
                out.push(']');
                cursor = close + 1;
            }
            i = close + 1;
        }
        out.push_str(&s[cursor..]);
        out
    }

    /// A run of digits and dots with exactly four 1-3 digit parts is an address; longer dotted
    /// runs (versions, timestamps) are not.
    fn replace_ipv4(&mut self, s: &str) -> String {
        let b = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut cursor = 0;
        let mut i = 0;
        while i < b.len() {
            // Not inside a longer dotted number: a preceding dot only counts as a boundary after a
            // word ("Draft.192.168.0.5"), never after a digit ("1.2.3.4.5").
            let prev_ok = i == 0
                || (!b[i - 1].is_ascii_digit()
                    && (b[i - 1] != b'.'
                        || (i >= 2
                            && (b[i - 2].is_ascii_alphabetic()
                                || b[i - 2] == b'_'
                                || b[i - 2] == b'-'))));
            if !b[i].is_ascii_digit() || !prev_ok {
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            // A trailing dot ends a sentence or starts an extension ("100.json"), not the number.
            if i > start && b[i - 1] == b'.' {
                i -= 1;
            }
            let candidate = &s[start..i];
            let parts: Vec<&str> = candidate.split('.').collect();
            let is_ipv4 = parts.len() == 4
                && parts.iter().all(|p| {
                    !p.is_empty() && p.len() <= 3 && p.bytes().all(|c| c.is_ascii_digit())
                });
            if is_ipv4 {
                out.push_str(&s[cursor..start]);
                let p = self.ip(candidate);
                out.push_str(&p);
                cursor = i;
            }
        }
        out.push_str(&s[cursor..]);
        out
    }

    pub(crate) fn text(&mut self, s: &str) -> String {
        let out = replace_pins(s);
        let out = replace_json_values(&out, SECRET_KEYS, "<removed>");
        let out = replace_escaped_json_values(&out, SECRET_KEYS, "<removed>");
        let out = replace_kv_values(&out, SECRET_KEYS, "<removed>");
        if !self.redact {
            return out;
        }
        let out = replace_json_values(&out, &["serial"], "<serial>");
        let out = replace_home_dirs(&out);
        let out = replace_macs(&out);
        let out = self.replace_ipv6(&out);
        self.replace_ipv4(&out)
    }
}

/// Sanitize an arbitrary filename to a safe zip entry leaf (no path
/// components, no traversal). Splits on BOTH separators explicitly rather than
/// `Path::file_name` — the bundle is built on the desktop host, but an image
/// path may carry the other OS's separator (e.g. a Windows path inspected on a
/// dev mac), and a stray `..` must never escape the `images/` prefix.
fn safe_leaf(path: &str, fallback: &str) -> String {
    let leaf = path.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if leaf.is_empty() || leaf == "." || leaf == ".." {
        return fallback.to_string();
    }
    leaf.to_string()
}

/// Resolved on-disk locations the bundler reads from. Split out from the
/// `AppHandle` so the assembly can be integration-tested with temp dirs.
struct BundleDirs {
    /// `~/.ps5upload/logs/` — the renderer's rotating JSONL log.
    logs: std::path::PathBuf,
    /// `<app_local_data_dir>/engine/` — where `engine.log`(+`.old`) live.
    engine: std::path::PathBuf,
    /// `~/.ps5upload/crash-reports/` — auto-collected reports.
    reports: std::path::PathBuf,
}

/// Assemble the bundle. Thin: resolve the three source dirs from the app, then
/// delegate to `assemble_zip` (which is pure I/O over those dirs + the args,
/// so it's testable without a Tauri runtime).
#[tauri::command]
pub async fn bug_report_build(
    app: AppHandle,
    args: BugReportArgs,
) -> Result<BugReportResult, String> {
    let dirs = BundleDirs {
        logs: super::diag_log::logs_dir(&app)?,
        engine: app
            .path()
            .app_local_data_dir()
            .map(|d| d.join("engine"))
            .unwrap_or_default(),
        reports: super::crash_reports::reports_dir(&app).unwrap_or_default(),
    };
    let now = now_ms();
    tokio::task::spawn_blocking(move || assemble_zip(&args, &dirs, now))
        .await
        .map_err(|e| format!("bug_report task: {e}"))?
}

fn assemble_zip(
    args: &BugReportArgs,
    dirs: &BundleDirs,
    now_ms: u64,
) -> Result<BugReportResult, String> {
    // Resolve the destination to a real path. On Android the renderer's save
    // dialog hands us a `content://` URI that std::fs can't create; redirect it
    // to a real path under Downloads so the bundle actually writes (the failure
    // users saw as "Couldn't build the report"). Desktop paths pass through.
    let fallback_name = args
        .dest_filename
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("ps5upload-bugreport.zip");
    let dest_path = super::save_dest::resolve_save_dest(&args.dest, fallback_name)?;
    let res = assemble_zip_at(args, dirs, now_ms, &dest_path);
    if res.is_err() {
        // A half-written zip would otherwise sit where the report was meant to be.
        let _ = std::fs::remove_file(&dest_path);
    }
    res
}

fn assemble_zip_at(
    args: &BugReportArgs,
    dirs: &BundleDirs,
    now_ms: u64,
    dest_path: &Path,
) -> Result<BugReportResult, String> {
    let f = std::fs::File::create(dest_path)
        .map_err(|e| format!("create {}: {e}", dest_path.display()))?;
    let mut zw = zip::ZipWriter::new(f);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut entries = 0usize;
    let mut log_lines = 0usize;
    let mut crash_reports = 0usize;
    let mut images = 0usize;

    // A name already in the zip is skipped, not an error: the report builder sends files of
    // its own (its README.txt, its MISSING.txt) and one duplicate must not lose the report.
    let written = std::cell::RefCell::new(std::collections::HashSet::<String>::new());
    let write_entry = |zw: &mut zip::ZipWriter<std::fs::File>,
                       name: &str,
                       bytes: &[u8]|
     -> Result<bool, String> {
        if !written.borrow_mut().insert(name.to_string()) {
            return Ok(false);
        }
        zw.start_file(name, opts)
            .map_err(|e| format!("zip start_file {name}: {e}"))?;
        zw.write_all(bytes)
            .map_err(|e| format!("zip write {name}: {e}"))?;
        Ok(true)
    };

    // One redactor for the whole report: an address is <ip-N> with the same N in every file.
    let mut red = Redactor::new(args.redact);
    // Every log that was cut to fit, for MISSING.txt.
    let mut truncated: Vec<String> = Vec::new();
    let mut engine_bytes = 0u64;

    // 1. Manifest — always.
    let report_json = red.text(&args.report_json);
    write_entry(&mut zw, "report.json", report_json.as_bytes())?;
    entries += 1;

    // 2. README so a non-developer opening the zip knows what's inside.
    write_entry(&mut zw, "README.txt", README.as_bytes())?;
    entries += 1;

    // 3. Windowed app log.
    if args.include.app_logs {
        let since = if args.since_ms > 0 {
            args.since_ms
        } else {
            now_ms.saturating_sub(args.window_minutes.saturating_mul(60_000))
        };
        let mut lines = super::diag_log::window_lines(&dirs.logs, since);
        // The newest lines that fit APP_LOG_MAX_BYTES: a day at trace can be far more.
        let total: usize = lines.iter().map(|l| l.len() + 1).sum();
        if total > APP_LOG_MAX_BYTES {
            let mut kept = 0usize;
            let mut start = lines.len();
            while start > 0 && kept + lines[start - 1].len() < APP_LOG_MAX_BYTES {
                start -= 1;
                kept += lines[start].len() + 1;
            }
            truncated.push(format!(
                "logs/app.jsonl: kept the newest {} of {} lines ({} of {})",
                lines.len() - start,
                lines.len(),
                mib(kept as u64),
                mib(total as u64)
            ));
            lines.drain(..start);
        }
        log_lines = lines.len();
        let mut body = lines.join("\n");
        body.push('\n');
        let body = red.text(&body);
        write_entry(&mut zw, "logs/app.jsonl", body.as_bytes())?;
        entries += 1;
    }

    // 4. Engine sidecar log (full-fidelity, crash-survivable). Best-effort —
    //    may not exist on a fresh install that never started the engine.
    if args.include.engine_log {
        for (src, name) in [
            (dirs.engine.join("engine.log"), "logs/engine.log"),
            (dirs.engine.join("engine.log.old"), "logs/engine.log.old"),
            (
                dirs.engine.join("engine.log.old.2"),
                "logs/engine.log.old.2",
            ),
            (
                dirs.engine.join("engine.log.old.3"),
                "logs/engine.log.old.3",
            ),
            (
                dirs.engine.join("engine.log.old.4"),
                "logs/engine.log.old.4",
            ),
            // The desktop updater's own steps (#611), beside the engine folder.
            (
                dirs.engine.with_file_name("updates").join("update.log"),
                "logs/update.log",
            ),
        ] {
            // All the engine's logs together stay under ENGINE_LOGS_TOTAL_BYTES: the newest first,
            // so an older part is the one left out.
            let left = ENGINE_LOGS_TOTAL_BYTES.saturating_sub(engine_bytes);
            let size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
            if size > 0 && left < 256 * 1024 {
                truncated.push(format!(
                    "{name}: left out ({}), the newer engine logs filled the report's share",
                    mib(size)
                ));
                continue;
            }
            if let Some(data) = read_tail(&src, ENGINE_LOG_TAIL_BYTES.min(left)) {
                if size > data.len() as u64 {
                    truncated.push(format!(
                        "{name}: kept the newest {} of {}",
                        mib(data.len() as u64),
                        mib(size)
                    ));
                }
                engine_bytes += data.len() as u64;
                let text = red.text(&String::from_utf8_lossy(&data));
                write_entry(&mut zw, name, text.as_bytes())?;
                entries += 1;
            }
        }
    }

    // 5. Auto-collected crash reports.
    if args.include.crash_reports {
        let files = super::crash_reports::list_report_files(&dirs.reports);
        if files.len() > CRASH_REPORTS_MAX {
            truncated.push(format!(
                "crash-reports/: {} of {} reports (the rest left out)",
                CRASH_REPORTS_MAX,
                files.len()
            ));
        }
        for p in newest(files, CRASH_REPORTS_MAX) {
            let leaf = p
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("report-{crash_reports}.json"));
            let leaf = red.text(&leaf);
            if let Some(data) = read_tail(&p, CRASH_REPORT_MAX_BYTES) {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                if size > data.len() as u64 {
                    truncated.push(format!(
                        "crash-reports/{leaf}: kept the last {} of {}",
                        mib(data.len() as u64),
                        mib(size)
                    ));
                }
                let text = red.text(&String::from_utf8_lossy(&data));
                write_entry(&mut zw, &format!("crash-reports/{leaf}"), text.as_bytes())?;
                entries += 1;
                crash_reports += 1;
            }
        }
    }

    // 6. PS5 kernel logs (passed in from the renderer snapshot).
    if args.include.ps5_logs {
        for (t, name) in [
            (&args.klog_text, "ps5/klog.txt"),
            (&args.syslog_text, "ps5/syslog.txt"),
        ] {
            if let Some(t) = t.as_deref().filter(|t| !t.is_empty()) {
                let kept = keep_newest(t, CONSOLE_LOG_MAX_BYTES, name, &mut truncated);
                let text = red.text(kept);
                write_entry(&mut zw, name, text.as_bytes())?;
                entries += 1;
            }
        }
        // The helper's on-PS5 black box (startup trace, tx events, crash
        // marker) — the key to debugging a helper crash.
        for (i, pl) in args.payload_logs.iter().enumerate() {
            if pl.text.is_empty() {
                continue;
            }
            let leaf = red.text(&safe_leaf(&pl.name, "log"));
            let name = format!("ps5/payload-logs/{:02}_{leaf}", i + 1);
            let text = red.text(keep_newest(
                &pl.text,
                CONSOLE_LOG_MAX_BYTES,
                &name,
                &mut truncated,
            ));
            write_entry(&mut zw, &name, text.as_bytes())?;
            entries += 1;
        }
    }

    // 7. User-attached screenshots — index-prefixed to avoid collisions.
    if args.include.images {
        for (i, p) in args.image_paths.iter().enumerate() {
            let leaf = red.text(&safe_leaf(p, "image"));
            if let Ok(data) = std::fs::read(p) {
                write_entry(&mut zw, &format!("images/{:02}_{leaf}", i + 1), &data)?;
                entries += 1;
                images += 1;
            }
            // unreadable attachment → skip
        }
    }

    // 8. What the report builder assembled. A path that could leave the archive is refused and
    //    named in MISSING.txt, which the builder itself usually sends (then it is appended to).
    {
        use base64::Engine as _;
        let rejected: Vec<String> = args
            .extra_entries
            .iter()
            .filter(|e| !extra_path_ok(&e.path))
            .map(|e| format!("{}: invalid path", e.path))
            .collect();
        let mut missing_written = false;
        // MISSING.txt last: the other files' cuts are named in it.
        let ordered = args
            .extra_entries
            .iter()
            .filter(|e| extra_path_ok(&e.path) && e.path != "MISSING.txt")
            .chain(
                args.extra_entries
                    .iter()
                    .filter(|e| extra_path_ok(&e.path) && e.path == "MISSING.txt"),
            );
        for e in ordered {
            let body: Vec<u8> = if let Some(t) = &e.text {
                if e.path == "MISSING.txt" {
                    let notes: Vec<String> = rejected
                        .iter()
                        .cloned()
                        .chain(truncated.iter().map(|t| format!("cut to fit: {t}")))
                        .collect();
                    let mut t = red.text(t);
                    if !notes.is_empty() {
                        if t.trim() == "Nothing missing." {
                            t.clear();
                        } else {
                            t.push('\n');
                        }
                        t.push_str(&format!("{}\n", notes.join("\n")));
                    }
                    missing_written = true;
                    t.into_bytes()
                } else {
                    let kept = keep_newest(t, EXTRA_TEXT_MAX_BYTES, &e.path, &mut truncated);
                    red.text(kept).into_bytes()
                }
            } else if let Some(b) = &e.base64 {
                match base64::engine::general_purpose::STANDARD.decode(b) {
                    Ok(v) => v,
                    Err(_) => continue,
                }
            } else {
                continue;
            };
            if write_entry(&mut zw, &e.path, &body)? {
                entries += 1;
            }
        }
        if !missing_written && (!rejected.is_empty() || !truncated.is_empty()) {
            let notes: Vec<String> = rejected
                .iter()
                .cloned()
                .chain(truncated.iter().map(|t| format!("cut to fit: {t}")))
                .collect();
            write_entry(&mut zw, "MISSING.txt", notes.join("\n").as_bytes())?;
            entries += 1;
        }
    }

    zw.finish().map_err(|e| format!("zip finish: {e}"))?;

    let bytes = std::fs::metadata(dest_path).map(|m| m.len()).unwrap_or(0);
    Ok(BugReportResult {
        entries,
        bytes,
        // The path we actually wrote — equals args.dest on desktop, but is the
        // redirected Downloads path on Android, so the success UI points the
        // user at the real file.
        dest: dest_path.to_string_lossy().into_owned(),
        log_lines,
        crash_reports,
        images,
    })
}

/// How much of each engine log goes into the report. The sidecar rotates its log at 1 MiB, but
/// a rotation can fail (a lock on the .old file on Windows), and a report must not read a file
/// of hundreds of MB whole and then redact several copies of it.
const ENGINE_LOG_TAIL_BYTES: u64 = 4 * 1024 * 1024;
/// All the engine's logs together (engine.log, its older parts, update.log).
const ENGINE_LOGS_TOTAL_BYTES: u64 = 12 * 1024 * 1024;
/// The app's log for the window (a day at trace is far more).
const APP_LOG_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Each console log: klog, syslog, each of the helper's own logs.
const CONSOLE_LOG_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Crash reports: how many, and how much of each.
const CRASH_REPORTS_MAX: usize = 30;

/// The last `max` of a list sorted oldest-first: the newest reports are the ones about the
/// problem being reported. (The first `max` used to go in: the 30 oldest of up to 60.)
fn newest<T>(mut oldest_first: Vec<T>, max: usize) -> Vec<T> {
    let skip = oldest_first.len().saturating_sub(max);
    oldest_first.split_off(skip)
}
const CRASH_REPORT_MAX_BYTES: u64 = 1024 * 1024;
/// Each file the report builder assembled (the timeline is the big one).
const EXTRA_TEXT_MAX_BYTES: usize = 16 * 1024 * 1024;

fn mib(n: u64) -> String {
    format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
}

/// The newest `max` bytes of `text`, from a line start (and on a char boundary); a cut is noted
/// in `truncated` under `name`.
fn keep_newest<'a>(text: &'a str, max: usize, name: &str, truncated: &mut Vec<String>) -> &'a str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    if let Some(nl) = text[start..].find('\n').filter(|n| *n < 4096) {
        start += nl + 1;
    }
    truncated.push(format!(
        "{name}: kept the newest {} of {}",
        mib((text.len() - start) as u64),
        mib(text.len() as u64)
    ));
    &text[start..]
}

/// The last `max` bytes of `path` (the whole file when it is smaller), starting at a line
/// boundary when the file was cut. `None` when the file cannot be read.
fn read_tail(path: &Path, max: u64) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut data = Vec::new();
    if len > max {
        f.seek(SeekFrom::Start(len - max)).ok()?;
        f.read_to_end(&mut data).ok()?;
        if let Some(nl) = data.iter().position(|&b| b == b'\n') {
            data.drain(..=nl);
        }
    } else {
        f.read_to_end(&mut data).ok()?;
    }
    Some(data)
}

const README: &str = "ps5upload bug report bundle\n\
===========================\n\
\n\
This archive was generated by the ps5upload Bug Report page. It contains\n\
diagnostics to help debug an issue — no games or app data.\n\
\n\
  report.json          Summary: app version, OS, your description, the\n\
                       selected log level/window, the diagnostic bundle,\n\
                       engine state (transfer jobs with why they failed, and\n\
                       live install sessions, and engine.job_summaries: the last\n\
                       20 per-job records of where each transfer's time went —\n\
                       a hash names the console, no address or path), and a\n\
                       snapshot of the connected\n\
                       PS5 (if any) — including per-volume free space with the\n\
                       safety reserve, the installed title list, and which\n\
                       service ports (loader :9021, DPI :9040, ours) were\n\
                       answering.\n\
                       Also what each feature was doing (engine.features):\n\
                       Remote Play (the session running and the last one's\n\
                       start, stages and end, e.g. a refused 80108b26), Play\n\
                       Together (open, guests' stages, round trip, skipped\n\
                       pictures), phone controllers and watch party (counts,\n\
                       whether a Tailscale address exists), Go live (service\n\
                       and server host per destination), saved remote\n\
                       connections (protocol, server, share), the mesh; the\n\
                       console's add-ons (ps5.features: Hush, cheats engine,\n\
                       the console's network shares); ps5upload:// links\n\
                       (diagnostic.app_links: registered?, the last kind);\n\
                       the log levels in force (log_levels); this computer\n\
                       (system: the OS's real version, model, CPU, memory, web\n\
                       view); what the web view can decode (media); the page's\n\
                       last Remote Play sessions (remote_play_client) beside\n\
                       the engine's (engine.features.remote_play.sessions);\n\
                       the console's payloads with versions (payloads).\n\
  logs/app.jsonl       The app's unified log for the selected time window\n\
                       (one JSON object per line: ts, level, source, message).\n\
  logs/engine.log      Full transfer-engine log (crash-survivable); .old and\n\
                       .old.2 to .old.4 are the earlier parts, newest first.\n\
  crash-reports/       Auto-collected crash/error reports.\n\
  ps5/klog.txt         PS5 /dev/klog tail (kernel log).\n\
  ps5/syslog.txt       PS5 kern.msgbuf tail.\n\
  ps5/payload-logs/    The helper's own on-PS5 logs (startup trace, tx event\n\
                       log, tx state, crash marker) — best for helper crashes.\n\
                       If the helper was down when this report was made, these\n\
                       are the copy taken when it last came up: see\n\
                       ps5.payload_logs_source and payload_logs_captured_at in\n\
                       report.json, and ps5.helper_lost_during for the request\n\
                       it stopped answering on.\n\
  images/              Screenshots you attached.\n\
  MISSING.txt          What could not be read, and every log cut to fit (each\n\
                       keeps its newest part, so the zip stays a size you can\n\
                       post).\n\
\n\
IP addresses (numbered, the same number in every file), MAC addresses, home\n\
folders and the console serial are redacted by default; pairing keys, PINs, tokens\n\
and account ids are always removed. Feature states are counts and names only:\n\
never stream keys or stream URLs, invite keys or codes, resume tokens, phone\n\
link keys, passcodes, share passwords or user names. Review screenshots before\n\
sharing.\n";

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn safe_leaf_strips_paths() {
        assert_eq!(safe_leaf("/a/b/shot.png", "x"), "shot.png");
        assert_eq!(safe_leaf("C:\\users\\me\\a.jpg", "x"), "a.jpg");
        assert_eq!(safe_leaf("", "fallback"), "fallback");
        // No traversal survives.
        assert!(!safe_leaf("../../etc/passwd", "x").contains('/'));
    }

    fn inc_all() -> BugReportInclude {
        BugReportInclude {
            app_logs: true,
            engine_log: true,
            crash_reports: true,
            ps5_logs: true,
            images: true,
        }
    }

    /// End-to-end: feed real-shaped inputs (windowed JSONL, engine.log, a crash
    /// report, real-ish kernel-log text with non-ASCII bytes, an attached
    /// image) and assert the produced zip has exactly the expected entries and
    /// that the app log is correctly time-windowed.
    #[test]
    fn assemble_zip_bundles_everything_and_windows_the_log() {
        let root = std::env::temp_dir().join(format!("ps5up-bugreport-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let logs = root.join("logs");
        let engine = root.join("engine");
        let reports = root.join("reports");
        for d in [&logs, &engine, &reports] {
            std::fs::create_dir_all(d).unwrap();
        }

        // now = fixed; window = 30 min. One line inside, one inside, one before.
        let now: u64 = 1_780_000_000_000;
        let recent = now - 10 * 60_000;
        let alsoin = now - 20 * 60_000;
        let oldone = now - 60 * 60_000;
        std::fs::write(
            logs.join("app-20260605.jsonl"),
            format!(
                "{{\"ts\":{oldone},\"level\":\"info\",\"message\":\"old\"}}\n\
                 {{\"ts\":{alsoin},\"level\":\"warn\",\"message\":\"mid\"}}\n\
                 {{\"ts\":{recent},\"level\":\"error\",\"message\":\"new\"}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            engine.join("engine.log"),
            b"[engine:warn] connect 192.0.2.99:9021 refused\n",
        )
        .unwrap();
        std::fs::write(
            reports.join("ps5upload-report-123-7.json"),
            br#"{"schema":2,"trigger":"test"}"#,
        )
        .unwrap();
        let img = root.join("shot.png");
        std::fs::write(&img, b"\x89PNG\r\n\x1a\nFAKE").unwrap();

        let dest = root.join("out.zip");
        let args = BugReportArgs {
            dest: dest.to_string_lossy().into_owned(),
            dest_filename: None,
            report_json: r#"{"kind":"ps5upload-bug-report","error":"connect 192.0.2.99:9021"}"#
                .to_string(),
            redact: true,
            window_minutes: 30,
            since_ms: 0,
            extra_entries: vec![],
            // Real kernel logs contain non-UTF8 / control bytes after lossy
            // decode; make sure they survive into the zip unmangled.
            klog_text: Some("klog line ⚠ 0x80f40030\nsecond\n".to_string()),
            syslog_text: Some("syslog\n".to_string()),
            payload_logs: vec![PayloadLogFile {
                name: "startup.log".to_string(),
                text: "1780601834.879 ENSURE_DIRECTORIES_DONE\n".to_string(),
            }],
            image_paths: vec![img.to_string_lossy().into_owned()],
            include: inc_all(),
        };

        let res = assemble_zip(
            &args,
            &BundleDirs {
                logs,
                engine,
                reports,
            },
            now,
        )
        .unwrap();
        // report.json, README, app.jsonl, engine.log, crash report, klog, syslog,
        // 1 payload-log, image
        assert_eq!(res.entries, 9, "unexpected entry count");
        assert_eq!(res.log_lines, 2, "app log should be windowed to 2 lines");
        assert_eq!(res.crash_reports, 1);
        assert_eq!(res.images, 1);

        // Re-open and verify names + that the OLD log line was excluded.
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().unwrap().into_owned())
            .collect();
        for expect in [
            "report.json",
            "README.txt",
            "logs/app.jsonl",
            "logs/engine.log",
            "crash-reports/ps5upload-report-123-7.json",
            "ps5/klog.txt",
            "ps5/syslog.txt",
            "ps5/payload-logs/01_startup.log",
            "images/01_shot.png",
        ] {
            assert!(
                names.contains(&expect.to_string()),
                "missing {expect} in {names:?}"
            );
        }
        let mut app_log = String::new();
        zip.by_name("logs/app.jsonl")
            .unwrap()
            .read_to_string(&mut app_log)
            .unwrap();
        assert!(app_log.contains("mid") && app_log.contains("new"));
        assert!(!app_log.contains("old"), "old line should be windowed out");
        let mut klog = String::new();
        zip.by_name("ps5/klog.txt")
            .unwrap()
            .read_to_string(&mut klog)
            .unwrap();
        assert!(klog.contains("0x80f40030") && klog.contains('⚠'));
        let mut engine_log = String::new();
        zip.by_name("logs/engine.log")
            .unwrap()
            .read_to_string(&mut engine_log)
            .unwrap();
        assert!(engine_log.contains("<ip-1>:9021"));
        assert!(!engine_log.contains("192.0.2.99"));
        let mut report = String::new();
        zip.by_name("report.json")
            .unwrap()
            .read_to_string(&mut report)
            .unwrap();
        assert!(!report.contains("192.0.2.99"));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Unticking sections drops them; report.json + README are always present.
    #[test]
    fn assemble_zip_respects_include_flags() {
        let root = std::env::temp_dir().join(format!("ps5up-bugreport2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("out.zip");
        let args = BugReportArgs {
            dest: dest.to_string_lossy().into_owned(),
            dest_filename: None,
            report_json: "{}".to_string(),
            redact: false,
            window_minutes: 30,
            since_ms: 0,
            extra_entries: vec![],
            klog_text: Some("x".to_string()),
            syslog_text: None,
            payload_logs: vec![],
            image_paths: vec![],
            include: BugReportInclude {
                app_logs: false,
                engine_log: false,
                crash_reports: false,
                ps5_logs: false,
                images: false,
            },
        };
        let dirs = BundleDirs {
            logs: root.join("nope-logs"),
            engine: root.join("nope-engine"),
            reports: root.join("nope-reports"),
        };
        let res = assemble_zip(&args, &dirs, 1_780_000_000_000).unwrap();
        assert_eq!(
            res.entries, 2,
            "only report.json + README when all unticked"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extras_the_bundle_already_has_do_not_fail_it() {
        // Every desktop report failed with "Duplicate filename: README.txt": the report
        // builder sends its own README.txt, and the bundle had already written one.
        let root = std::env::temp_dir().join(format!("ps5up-bugreport4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("out.zip");
        let text = |path: &str, t: &str| ExtraEntry {
            path: path.into(),
            text: Some(t.into()),
            base64: None,
        };
        let args = BugReportArgs {
            dest: dest.to_string_lossy().into_owned(),
            dest_filename: None,
            report_json: "{}".to_string(),
            redact: true,
            window_minutes: 0,
            since_ms: 0,
            klog_text: None,
            syslog_text: None,
            payload_logs: vec![],
            image_paths: vec![],
            include: BugReportInclude {
                app_logs: false,
                engine_log: false,
                crash_reports: false,
                ps5_logs: false,
                images: false,
            },
            extra_entries: vec![
                text("README.txt", "the builder's readme"),
                text("report.md", "what happened"),
                text("report.md", "again"),
            ],
        };
        let dirs = BundleDirs {
            logs: root.join("l"),
            engine: root.join("e"),
            reports: root.join("r"),
        };
        assemble_zip(&args, &dirs, 1_780_000_000_000).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("report.md").unwrap(), &mut s).unwrap();
        assert_eq!(s, "what happened");
        assert!(zip.by_name("README.txt").is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extra_entries_are_redacted_and_bad_paths_listed_as_missing() {
        let root = std::env::temp_dir().join(format!("ps5up-bugreport3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("out.zip");
        let args = BugReportArgs {
            dest: dest.to_string_lossy().into_owned(),
            dest_filename: None,
            report_json: "{}".to_string(),
            redact: true,
            window_minutes: 0,
            since_ms: 0,
            klog_text: None,
            syslog_text: None,
            payload_logs: vec![],
            image_paths: vec![],
            include: BugReportInclude {
                app_logs: false,
                engine_log: false,
                crash_reports: false,
                ps5_logs: false,
                images: false,
            },
            extra_entries: vec![
                ExtraEntry {
                    path: "timeline.txt".into(),
                    text: Some("18:27 [engine] 192.0.2.100 lost".into()),
                    base64: None,
                },
                ExtraEntry {
                    path: "screenshots/a.png".into(),
                    text: None,
                    base64: Some("iVBORw==".into()),
                },
                ExtraEntry {
                    path: "../escape.txt".into(),
                    text: Some("x".into()),
                    base64: None,
                },
            ],
        };
        let dirs = BundleDirs {
            logs: root.join("l"),
            engine: root.join("e"),
            reports: root.join("r"),
        };
        assemble_zip(&args, &dirs, 1_780_000_000_000).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        let mut read = |name: &str| {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut zip.by_name(name).unwrap(), &mut s).unwrap();
            s
        };
        assert_eq!(read("timeline.txt"), "18:27 [engine] <ip-1> lost");
        assert!(read("MISSING.txt").contains("../escape.txt: invalid path"));
        assert!(zip.by_name("screenshots/a.png").is_ok());
        assert!(zip.by_name("../escape.txt").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Each log keeps its newest part within its cap, and MISSING.txt names every cut; an older
    /// helper's PIN line is gone.
    #[test]
    fn logs_are_capped_and_cuts_named() {
        let root = std::env::temp_dir().join(format!("ps5up-bugreport-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("e")).unwrap();
        // Four engine logs of 5 MiB each: three fit at 4 MiB (the 12 MiB total), the oldest is left out.
        let line = "x".repeat(99) + "\n";
        let five = line.repeat(5 * 1024 * 1024 / 100);
        for f in [
            "engine.log",
            "engine.log.old",
            "engine.log.old.2",
            "engine.log.old.3",
        ] {
            std::fs::write(root.join("e").join(f), &five).unwrap();
        }
        let dest = root.join("out.zip");
        let mut klog = "old\n".repeat(2 * 1024 * 1024);
        klog.push_str("[remoteplay] [ps5upload] Remote Play PIN: 14928144\n");
        let args = BugReportArgs {
            dest: dest.to_string_lossy().into_owned(),
            dest_filename: None,
            report_json: "{}".to_string(),
            redact: true,
            window_minutes: 0,
            since_ms: 0,
            klog_text: Some(klog),
            syslog_text: None,
            payload_logs: vec![],
            image_paths: vec![],
            include: BugReportInclude {
                app_logs: false,
                engine_log: true,
                crash_reports: false,
                ps5_logs: true,
                images: false,
            },
            extra_entries: vec![ExtraEntry {
                path: "MISSING.txt".into(),
                text: Some("Nothing missing.".into()),
                base64: None,
            }],
        };
        let dirs = BundleDirs {
            logs: root.join("l"),
            engine: root.join("e"),
            reports: root.join("r"),
        };
        assemble_zip(&args, &dirs, 1_780_000_000_000).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        let mut read = |name: &str| {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut zip.by_name(name).unwrap(), &mut s).unwrap();
            s
        };
        let k = read("ps5/klog.txt");
        assert!(k.len() <= CONSOLE_LOG_MAX_BYTES);
        assert!(
            k.ends_with("Remote Play PIN: <removed>\n"),
            "{}",
            &k[k.len() - 60..]
        );
        assert!(read("logs/engine.log").len() as u64 <= ENGINE_LOG_TAIL_BYTES);
        let missing = read("MISSING.txt");
        assert!(!missing.contains("Nothing missing."), "{missing}");
        assert!(missing.contains("cut to fit: ps5/klog.txt"), "{missing}");
        assert!(
            missing.contains("cut to fit: logs/engine.log: kept the newest"),
            "{missing}"
        );
        assert!(
            missing.contains("logs/engine.log.old.3: left out"),
            "{missing}"
        );
        drop(read);
        assert!(zip.by_name("logs/engine.log.old.3").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[derive(serde::Deserialize)]
    struct Vector {
        name: String,
        redact: bool,
        #[serde(rename = "in")]
        input: String,
        out: String,
    }

    #[test]
    fn the_newest_crash_reports_are_the_ones_kept() {
        let names = |n: usize| {
            (0..n)
                .map(|i| format!("crash-{i:03}.json"))
                .collect::<Vec<_>>()
        };
        let kept = newest(names(45), CRASH_REPORTS_MAX);
        assert_eq!(kept.len(), CRASH_REPORTS_MAX);
        assert_eq!(kept.first().unwrap(), "crash-015.json");
        assert_eq!(kept.last().unwrap(), "crash-044.json");
        // Fewer than the limit: all of them, in order.
        assert_eq!(newest(names(3), CRASH_REPORTS_MAX), names(3));
        assert!(newest(Vec::<String>::new(), CRASH_REPORTS_MAX).is_empty());
    }

    /// A quote escaped earlier in a line shifted the old quote pairing, and the secret after it
    /// stayed in the report.
    #[test]
    fn a_secret_after_an_escaped_quote_is_removed() {
        let line = r#"{"message":"a 5\" drive","detail":{"token":"SECRET2"}}"#;
        let out = Redactor::new(false).text(line);
        assert!(!out.contains("SECRET2"), "{out}");
        assert_eq!(
            out,
            r#"{"message":"a 5\" drive","detail":{"token":"<removed>"}}"#
        );
    }

    /// The same vectors as client/src/lib/redaction.test.ts: web and desktop redact alike.
    #[test]
    fn shared_vectors() {
        let vectors: Vec<Vector> =
            serde_json::from_str(include_str!("../../../src/lib/redaction.vectors.json")).unwrap();
        for v in vectors {
            assert_eq!(Redactor::new(v.redact).text(&v.input), v.out, "{}", v.name);
        }
    }

    #[test]
    fn engine_log_tail_starts_on_a_line_boundary() {
        let root = std::env::temp_dir().join(format!("ps5upload-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let p = root.join("engine.log");
        std::fs::write(&p, b"first line\nsecond line\nthird\n").unwrap();
        // Small enough to be read whole.
        assert_eq!(
            read_tail(&p, 1024).unwrap(),
            b"first line\nsecond line\nthird\n"
        );
        // Cut inside "second": the partial line is dropped, the rest kept.
        assert_eq!(read_tail(&p, 10).unwrap(), b"third\n");
        assert!(read_tail(&root.join("missing.log"), 10).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn addresses_keep_their_number_across_files() {
        let mut r = Redactor::new(true);
        assert_eq!(r.text("a 10.0.0.1"), "a <ip-1>");
        assert_eq!(r.text("b 10.0.0.2 c 10.0.0.1"), "b <ip-2> c <ip-1>");
        assert_eq!(r.text("ts=1780601834.879"), "ts=1780601834.879");
    }
}
