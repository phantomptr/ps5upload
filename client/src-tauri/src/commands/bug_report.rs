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

#[derive(Deserialize)]
pub struct BugReportArgs {
    /// User-picked destination `.zip` path.
    pub dest: String,
    /// Pretty-printed manifest JSON the renderer already built.
    pub report_json: String,
    /// How many minutes of app log to include (filters `app.jsonl`).
    pub window_minutes: u64,
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
    let f = std::fs::File::create(&args.dest).map_err(|e| format!("create {}: {e}", args.dest))?;
    let mut zw = zip::ZipWriter::new(f);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut entries = 0usize;
    let mut log_lines = 0usize;
    let mut crash_reports = 0usize;
    let mut images = 0usize;

    let write_entry =
        |zw: &mut zip::ZipWriter<std::fs::File>, name: &str, bytes: &[u8]| -> Result<(), String> {
            zw.start_file(name, opts)
                .map_err(|e| format!("zip start_file {name}: {e}"))?;
            zw.write_all(bytes)
                .map_err(|e| format!("zip write {name}: {e}"))?;
            Ok(())
        };

    // 1. Manifest — always.
    write_entry(&mut zw, "report.json", args.report_json.as_bytes())?;
    entries += 1;

    // 2. README so a non-developer opening the zip knows what's inside.
    write_entry(&mut zw, "README.txt", README.as_bytes())?;
    entries += 1;

    // 3. Windowed app log.
    if args.include.app_logs {
        let since = now_ms.saturating_sub(args.window_minutes.saturating_mul(60_000));
        let lines = super::diag_log::window_lines(&dirs.logs, since);
        log_lines = lines.len();
        let mut body = lines.join("\n");
        body.push('\n');
        write_entry(&mut zw, "logs/app.jsonl", body.as_bytes())?;
        entries += 1;
    }

    // 4. Engine sidecar log (full-fidelity, crash-survivable). Best-effort —
    //    may not exist on a fresh install that never started the engine.
    if args.include.engine_log {
        for (src, name) in [
            (dirs.engine.join("engine.log"), "logs/engine.log"),
            (dirs.engine.join("engine.log.old"), "logs/engine.log.old"),
        ] {
            if let Ok(data) = std::fs::read(&src) {
                write_entry(&mut zw, name, &data)?;
                entries += 1;
            }
        }
    }

    // 5. Auto-collected crash reports.
    if args.include.crash_reports {
        for p in super::crash_reports::list_report_files(&dirs.reports) {
            let leaf = p
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("report-{crash_reports}.json"));
            if let Ok(data) = std::fs::read(&p) {
                write_entry(&mut zw, &format!("crash-reports/{leaf}"), &data)?;
                entries += 1;
                crash_reports += 1;
            }
        }
    }

    // 6. PS5 kernel logs (passed in from the renderer snapshot).
    if args.include.ps5_logs {
        if let Some(t) = &args.klog_text {
            if !t.is_empty() {
                write_entry(&mut zw, "ps5/klog.txt", t.as_bytes())?;
                entries += 1;
            }
        }
        if let Some(t) = &args.syslog_text {
            if !t.is_empty() {
                write_entry(&mut zw, "ps5/syslog.txt", t.as_bytes())?;
                entries += 1;
            }
        }
        // The helper's on-PS5 black box (startup trace, tx events, crash
        // marker) — the key to debugging a helper crash.
        for (i, pl) in args.payload_logs.iter().enumerate() {
            if pl.text.is_empty() {
                continue;
            }
            let leaf = safe_leaf(&pl.name, "log");
            write_entry(
                &mut zw,
                &format!("ps5/payload-logs/{:02}_{leaf}", i + 1),
                pl.text.as_bytes(),
            )?;
            entries += 1;
        }
    }

    // 7. User-attached screenshots — index-prefixed to avoid collisions.
    if args.include.images {
        for (i, p) in args.image_paths.iter().enumerate() {
            let leaf = safe_leaf(p, "image");
            if let Ok(data) = std::fs::read(p) {
                write_entry(&mut zw, &format!("images/{:02}_{leaf}", i + 1), &data)?;
                entries += 1;
                images += 1;
            }
            // unreadable attachment → skip
        }
    }

    zw.finish().map_err(|e| format!("zip finish: {e}"))?;

    let bytes = std::fs::metadata(&args.dest).map(|m| m.len()).unwrap_or(0);
    Ok(BugReportResult {
        entries,
        bytes,
        dest: args.dest.clone(),
        log_lines,
        crash_reports,
        images,
    })
}

const README: &str = "ps5upload bug report bundle\n\
===========================\n\
\n\
This archive was generated by the ps5upload Bug Report page. It contains\n\
diagnostics to help debug an issue — no games or app data.\n\
\n\
  report.json          Summary: app version, OS, your description, the\n\
                       selected log level/window, the diagnostic bundle,\n\
                       and a snapshot of the connected PS5 (if any).\n\
  logs/app.jsonl       The app's unified log for the selected time window\n\
                       (one JSON object per line: ts, level, source, message).\n\
  logs/engine.log      Full transfer-engine log (crash-survivable).\n\
  crash-reports/       Auto-collected crash/error reports.\n\
  ps5/klog.txt         PS5 /dev/klog tail (kernel log).\n\
  ps5/syslog.txt       PS5 kern.msgbuf tail.\n\
  ps5/payload-logs/    The helper's own on-PS5 logs (startup trace, tx event\n\
                       log, tx state, crash marker) — best for helper crashes.\n\
  images/              Screenshots you attached.\n\
\n\
IP addresses and the console serial are redacted by default. Post this zip in\n\
the #bugs-report channel on Discord.\n";

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
        std::fs::write(engine.join("engine.log"), b"[engine:info] ts=1 boot\n").unwrap();
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
            report_json: r#"{"kind":"ps5upload-bug-report"}"#.to_string(),
            window_minutes: 30,
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
            .map(|i| zip.by_index(i).unwrap().name().to_string())
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
            report_json: "{}".to_string(),
            window_minutes: 30,
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
}
