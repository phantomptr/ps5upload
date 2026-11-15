//! `ps5upload-engine inspect` and `ps5upload-engine convert`: the Convert screen for scripts.
//!
//! No second converter lives here. Each command hands the same request the app would send to
//! the handler behind its route (`/api/fpkg/inspect`, `/api/fpkg/build`, `/api/exfat/build`,
//! `/api/folder/build`), in this process and without a socket, then watches the job that
//! handler started the way `GET /api/jobs/{id}` would report it. A job ends `done` only after
//! its builder read the output back and matched it against the source, so that is the only
//! way this exits 0.
//!
//! After ps5-dump-forge's `crates/ps5-dump-forge-cli/src/main.rs` (the commands and the
//! JSON-lines progress); the conversion itself is ours.

use std::io::Write;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use uuid::Uuid;

use crate::{fpkg_api, AppState, JobState};

pub(crate) const USAGE: &str = "\
usage: ps5upload-engine                      run the engine (what the desktop app starts)
       ps5upload-engine inspect <source> [--out <dir>]
       ps5upload-engine convert <source> --to <format> --out <dir>
                        [--name <stem>] [--level fast|balanced|smallest|stored]
                        [--zstd <level>] [--lz4] [--full-verify] [--json]
       ps5upload-engine mcp [...]            bridge an MCP client to the running engine
       ps5upload-engine --healthcheck        exit 0 when the running engine answers

  inspect   print what the source is and whether it can be converted, as the JSON the app's
            Convert screen reads (POST /api/fpkg/inspect). --out is only used to report the
            room left there.
  convert   convert the source and verify the result. It runs in this process: no engine has
            to be running, and a running one is left alone.

  <source>  a game folder (the one with eboot.bin and sce_sys in it), a game image (.exfat,
            .ffpkg, .ffpfs, .ffpfsc) or a debug-key .pkg
  --to      exfat, ffpkg or ffpfs (a game image ShadowMountPlus mounts), ffpfsc (a compressed
            image, exFAT inside), folder (an image or package unpacked into a game folder) or
            pkg (a debug package the console installs)
  --out     the folder the result goes into (made when missing)
  --name    the result's name without its extension (default: the source's name; for pkg,
            the content id)
  --level   how hard a pkg or ffpfsc is compressed: fast, balanced (default) or smallest;
            stored (a pkg with nothing compressed) is for pkg only
  --zstd    pack the game's files into zstd asset packs at this level (1 to 22) with the
            ampr-emu-ng emulator, which is downloaded as the app downloads it; for exfat,
            ffpkg, ffpfs, ffpfsc and folder
  --lz4     pack the game's files into AMPR LZ4 asset packs, read by the game's own ampr_emu;
            for exfat, ffpkg, ffpfs, ffpfsc and folder
  --full-verify
            read every byte of the result back. Without it the structure is checked in
            full and the data by a random sample.
  --json    progress as JSON lines on stdout, one event per line (start, progress, done or
            failed). Without it progress goes to stderr and stdout gets the result's path.

exit code: 0 the result was written and passed its verification; 1 it failed (nothing is
left behind); 2 the arguments were wrong; 130 it was interrupted.";

/// What `--to` names: which route builds it, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Exfat,
    Ffpkg,
    Ffpfs,
    Ffpfsc,
    Folder,
    Pkg,
}

impl Target {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw.to_ascii_lowercase().as_str() {
            "exfat" => Ok(Self::Exfat),
            "ffpkg" => Ok(Self::Ffpkg),
            "ffpfs" => Ok(Self::Ffpfs),
            "ffpfsc" => Ok(Self::Ffpfsc),
            "folder" => Ok(Self::Folder),
            "pkg" => Ok(Self::Pkg),
            other => Err(format!(
                "unknown format {other:?}: use exfat, ffpkg, ffpfs, ffpfsc, folder or pkg"
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Exfat => "exfat",
            Self::Ffpkg => "ffpkg",
            Self::Ffpfs => "ffpfs",
            Self::Ffpfsc => "ffpfsc",
            Self::Folder => "folder",
            Self::Pkg => "pkg",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConvertArgs {
    pub source: String,
    pub to: Target,
    pub out: String,
    pub name: Option<String>,
    pub level: Option<String>,
    pub zstd: Option<u8>,
    pub lz4: bool,
    pub full_verify: bool,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    Inspect { source: String, out: Option<String> },
    Convert(ConvertArgs),
}

/// The command line after the program's name, as the engine's `main` reads it: by hand, as it
/// reads `mcp` and `--healthcheck`. A flag's value follows it or comes after `=`; everything
/// after `--` is the source, so a path may start with a dash.
pub(crate) fn parse(command: &str, args: &[String]) -> Result<Command, String> {
    let mut source: Option<String> = None;
    let (mut to, mut out, mut name, mut level, mut zstd) = (None, None, None, None, None);
    let (mut lz4, mut full_verify, mut json) = (false, false, false);
    let mut plain = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if plain || !arg.starts_with('-') || arg == "-" {
            if source.replace(arg.clone()).is_some() {
                return Err(format!("one source at a time; {arg:?} is a second one"));
            }
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (arg.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        let switch = |on: &mut bool| match &inline {
            Some(_) => Err(format!("{flag} takes no value")),
            None => {
                *on = true;
                Ok(())
            }
        };
        match flag {
            "--" => plain = true,
            "-h" | "--help" => return Ok(Command::Help),
            "--out" | "-o" => out = Some(value()?),
            "--to" if command == "convert" => to = Some(Target::parse(&value()?)?),
            "--name" if command == "convert" => name = Some(value()?),
            "--level" if command == "convert" => {
                let v = value()?.to_ascii_lowercase();
                if !["fast", "balanced", "smallest", "stored"].contains(&v.as_str()) {
                    return Err(format!(
                        "unknown --level {v:?}: use fast, balanced, smallest or stored"
                    ));
                }
                level = Some(v);
            }
            "--zstd" if command == "convert" => {
                let v = value()?;
                match v.parse::<u8>() {
                    Ok(n) if (1..=22).contains(&n) => zstd = Some(n),
                    _ => return Err(format!("--zstd {v:?} is not a level from 1 to 22")),
                }
            }
            "--lz4" if command == "convert" => switch(&mut lz4)?,
            "--full-verify" if command == "convert" => switch(&mut full_verify)?,
            "--json" if command == "convert" => switch(&mut json)?,
            other => return Err(format!("unknown option {other:?} for {command}")),
        }
    }
    let source = source.ok_or("no source: name a game folder, image or package")?;
    if command == "inspect" {
        return Ok(Command::Inspect { source, out });
    }
    let to = to.ok_or("--to is missing: exfat, ffpkg, ffpfs, ffpfsc, folder or pkg")?;
    let out = out.ok_or("--out is missing: the folder the result goes into")?;
    // What a route has no field for is refused here; what it has one for, it judges itself
    // (an image format that takes no LZ4 packs, a source that is not a game).
    if level.is_some() && !matches!(to, Target::Pkg | Target::Ffpfsc) {
        return Err(format!(
            "--level is for pkg and ffpfsc; {} is not compressed",
            to.name()
        ));
    }
    // The package route takes `stored`; an ffpfsc is compressed or it is an exfat image.
    if to == Target::Ffpfsc && level.as_deref() == Some("stored") {
        return Err("--level stored is for pkg; an uncompressed ffpfsc is --to exfat".into());
    }
    if zstd.is_some() && lz4 {
        return Err("--zstd and --lz4 are two ways to pack a game: pick one".into());
    }
    if zstd.is_some() && to == Target::Pkg {
        return Err("--zstd is for images and folders, not for pkg".into());
    }
    if lz4 && to == Target::Pkg {
        return Err("--lz4 is for images and folders, not for pkg".into());
    }
    Ok(Command::Convert(ConvertArgs {
        source,
        to,
        out,
        name,
        level,
        zstd,
        lz4,
        full_verify,
        json,
    }))
}

/// The route a conversion goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// `POST /api/fpkg/build`
    Package,
    /// `POST /api/exfat/build`
    Image,
    /// `POST /api/folder/build`
    Folder,
}

impl ConvertArgs {
    /// The route and the JSON body the app would post to it for this conversion.
    pub(crate) fn request(&self) -> (Route, serde_json::Value) {
        let mut body = serde_json::json!({
            "source": self.source,
            "output_dir": self.out,
            "full_verify": self.full_verify,
        });
        let mut set = |key: &str, value: serde_json::Value| {
            body[key] = value;
        };
        if let Some(name) = &self.name {
            set("name", name.as_str().into());
        }
        if let Some(level) = self.zstd {
            set("ampr_zstd", serde_json::json!({ "level": level }));
        }
        if self.lz4 {
            set("ampr_lz4", serde_json::json!({}));
        }
        let route = match self.to {
            Target::Pkg => {
                if let Some(level) = &self.level {
                    set("compression", level.as_str().into());
                }
                Route::Package
            }
            Target::Folder => Route::Folder,
            Target::Exfat | Target::Ffpkg | Target::Ffpfs | Target::Ffpfsc => {
                if self.to == Target::Ffpfsc {
                    set("format", "exfat".into());
                    set("compress", true.into());
                    if let Some(level) = &self.level {
                        set("ffpfsc_level", level.as_str().into());
                    }
                } else {
                    set("format", self.to.name().into());
                }
                Route::Image
            }
        };
        (route, body)
    }
}

/// What a conversion reports while it runs. With `--json` each is one line on stdout.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum Event {
    Start {
        job_id: String,
        source: String,
        to: &'static str,
    },
    Progress {
        /// The stage of a staged build (`plan`, `write`, `verify`, …); absent for the rest.
        #[serde(skip_serializing_if = "Option::is_none")]
        stage: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        stage_index: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        stage_count: Option<u32>,
        /// Bytes of this stage (or of the whole job) done, and how many there are.
        done: u64,
        total: u64,
    },
    Done {
        /// The file or folder written.
        output: String,
        bytes: u64,
        files: u64,
        elapsed_ms: u64,
        /// `full`: every byte was read back. `sample`: the structure in full, the data by sample.
        verified: &'static str,
    },
    Failed {
        error: String,
        cancelled: bool,
    },
}

/// A handler's answer as `(status, JSON body)`.
async fn answer(response: impl IntoResponse) -> (StatusCode, serde_json::Value) {
    let response = response.into_response();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 64 << 20)
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(serde_json::Value::Null);
    (status, body)
}

/// The words of a refusal: the handler's `error`, or its status when it gave none.
fn refusal(status: StatusCode, body: &serde_json::Value) -> String {
    match body.get("error").and_then(|e| e.as_str()) {
        Some(e) => e.to_string(),
        None => format!("the engine answered {status}"),
    }
}

/// A state for one command: its own job table, no console, nobody listening for events.
fn state() -> AppState {
    let (events_tx, _) = tokio::sync::broadcast::channel(64);
    AppState {
        jobs: Default::default(),
        default_ps5_addr: String::new(),
        events_tx,
    }
}

/// `inspect`: the JSON `/api/fpkg/inspect` answers, or why it refused.
pub(crate) async fn inspect(source: &str, out: Option<&str>) -> Result<serde_json::Value, String> {
    let req = serde_json::from_value(serde_json::json!({ "source": source, "output_dir": out }))
        .map_err(|e| e.to_string())?;
    let (status, body) =
        answer(fpkg_api::fpkg_inspect_handler(State(state()), Json(req)).await).await;
    if status.is_success() {
        Ok(body)
    } else {
        Err(refusal(status, &body))
    }
}

/// How often a running job is looked at.
const POLL: Duration = Duration::from_millis(250);

/// `convert`: start the job through its route's handler and report it until it ends. `emit`
/// gets every [`Event`]; `interrupted` resolving (Ctrl-C) cancels the job, which then ends
/// failed after removing what it wrote. Returns whether the result was written and verified.
pub(crate) async fn convert(
    args: &ConvertArgs,
    emit: &mut dyn FnMut(&Event),
    interrupted: impl std::future::Future<Output = ()>,
) -> bool {
    let state = state();
    let (route, body) = args.request();
    let fail = |emit: &mut dyn FnMut(&Event), error: String| {
        emit(&Event::Failed {
            error,
            cancelled: false,
        });
        false
    };
    // Deserialized as a posted body is, into the route's own request type.
    macro_rules! start {
        ($handler:path) => {
            match serde_json::from_value(body) {
                Ok(req) => answer($handler(State(state.clone()), Json(req)).await).await,
                Err(e) => return fail(emit, e.to_string()),
            }
        };
    }
    let (status, started) = match route {
        Route::Package => start!(fpkg_api::fpkg_build_handler),
        Route::Image => start!(fpkg_api::exfat_build_handler),
        Route::Folder => start!(fpkg_api::folder_build_handler),
    };
    let job_id = started
        .get("job_id")
        .and_then(|j| j.as_str())
        .and_then(|j| Uuid::parse_str(j).ok());
    let Some(job_id) = job_id.filter(|_| status.is_success()) else {
        return fail(emit, refusal(status, &started));
    };
    emit(&Event::Start {
        job_id: job_id.to_string(),
        source: args.source.clone(),
        to: args.to.name(),
    });

    tokio::pin!(interrupted);
    let mut cancelled = false;
    let mut last: Option<Event> = None;
    loop {
        let job = state
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&job_id)
            .cloned();
        match job {
            Some(JobState::Running {
                stage,
                bytes_sent,
                total_bytes,
                ..
            }) => {
                let event = match stage {
                    Some(s) => Event::Progress {
                        stage: Some(s.id),
                        stage_index: Some(s.index),
                        stage_count: Some(s.count),
                        done: s.done,
                        total: s.total,
                    },
                    None => Event::Progress {
                        stage: None,
                        stage_index: None,
                        stage_count: None,
                        done: bytes_sent,
                        total: total_bytes,
                    },
                };
                if last.as_ref() != Some(&event) {
                    emit(&event);
                    last = Some(event);
                }
            }
            Some(JobState::Done {
                dest,
                bytes_sent,
                files_sent,
                elapsed_ms,
                ..
            }) => {
                emit(&Event::Done {
                    output: dest,
                    bytes: bytes_sent,
                    files: files_sent,
                    elapsed_ms,
                    verified: if args.full_verify { "full" } else { "sample" },
                });
                return true;
            }
            Some(JobState::Failed { error, .. }) => {
                emit(&Event::Failed { error, cancelled });
                return false;
            }
            None => return fail(emit, "the job vanished from the job table".into()),
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL) => {}
            _ = &mut interrupted, if !cancelled => {
                // The job's own cancel: the builder stops between chunks, removes its partial
                // output and ends failed, which the next look reports.
                cancelled = true;
                if let Some(flag) = crate::cancel_flag(job_id) {
                    flag.store(true, Ordering::Relaxed);
                }
            }
        }
    }
}

/// A line for a terminal, or nothing when the event repeats what the last line said.
fn plain_line(event: &Event, last_percent: &mut Option<(Option<String>, u64)>) -> Option<String> {
    const MIB: f64 = 1024.0 * 1024.0;
    match event {
        Event::Start { source, to, .. } => Some(format!("converting {source} to {to}")),
        Event::Progress {
            stage,
            stage_index,
            stage_count,
            done,
            total,
        } => {
            // One line per stage and per whole percent, not per poll.
            let percent = (*done)
                .min(*total)
                .saturating_mul(100)
                .checked_div(*total)
                .unwrap_or(0);
            let key = (stage.clone(), percent);
            if last_percent.as_ref() == Some(&key) {
                return None;
            }
            *last_percent = Some(key);
            let what = match (stage, stage_index, stage_count) {
                (Some(s), Some(i), Some(n)) => format!("{s} ({}/{n})", i + 1),
                _ => "working".to_string(),
            };
            Some(if *total > 0 {
                format!(
                    "{what}: {percent}% ({:.1} of {:.1} MiB)",
                    *done as f64 / MIB,
                    *total as f64 / MIB
                )
            } else {
                what
            })
        }
        Event::Done {
            output,
            bytes,
            files,
            elapsed_ms,
            verified,
        } => Some(format!(
            "done: {output} ({bytes} bytes, {files} files, {:.1} s; {})",
            *elapsed_ms as f64 / 1000.0,
            match *verified {
                "full" => "every byte read back and matched",
                _ => "structure checked in full, data by sample",
            }
        )),
        Event::Failed { error, cancelled } => Some(match cancelled {
            true => format!("interrupted: {error}"),
            false => format!("failed: {error}"),
        }),
    }
}

/// Never `println!`/`eprintln!` here: they panic when the pipe is closed (`| head`, a parent
/// that went away), and a panic would leave the job's partial output behind.
fn say(mut to: impl Write, line: &str) {
    let _ = writeln!(to, "{line}");
    let _ = to.flush();
}

/// The `inspect` and `convert` subcommands of the engine binary: `args` is what follows the
/// command's name. Returns the process's exit code.
pub async fn run_cli(command: &str, args: Vec<String>) -> i32 {
    let parsed = match parse(command, &args) {
        Ok(p) => p,
        Err(e) => {
            say(
                std::io::stderr(),
                &format!("ps5upload-engine {command}: {e}\n\n{USAGE}"),
            );
            return 2;
        }
    };
    match parsed {
        Command::Help => {
            say(std::io::stdout(), USAGE);
            0
        }
        Command::Inspect { source, out } => match inspect(&source, out.as_deref()).await {
            Ok(v) => {
                let text = serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string());
                say(std::io::stdout(), &text);
                0
            }
            Err(e) => {
                say(std::io::stderr(), &format!("ps5upload-engine inspect: {e}"));
                1
            }
        },
        Command::Convert(args) => {
            let mut last_percent = None;
            let mut output = None;
            let mut cancelled = false;
            let mut emit = |event: &Event| {
                match event {
                    Event::Done { output: o, .. } => output = Some(o.clone()),
                    Event::Failed { cancelled: c, .. } => cancelled = *c,
                    _ => {}
                }
                if args.json {
                    if let Ok(line) = serde_json::to_string(event) {
                        say(std::io::stdout(), &line);
                    }
                } else if let Some(line) = plain_line(event, &mut last_percent) {
                    say(std::io::stderr(), &line);
                }
            };
            let interrupted = async {
                if tokio::signal::ctrl_c().await.is_err() {
                    // No signal handler on this system: never interrupted.
                    std::future::pending::<()>().await;
                }
            };
            let ok = convert(&args, &mut emit, interrupted).await;
            match (ok, cancelled) {
                (true, _) => {
                    // The result's path alone on stdout, for `out=$(ps5upload-engine convert …)`.
                    if let (false, Some(path)) = (args.json, output) {
                        say(std::io::stdout(), &path);
                    }
                    0
                }
                (false, true) => 130,
                (false, false) => 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    fn convert_args(args: &[&str]) -> Result<ConvertArgs, String> {
        match parse("convert", &argv(args))? {
            Command::Convert(c) => Ok(c),
            other => Err(format!("not a conversion: {other:?}")),
        }
    }

    #[test]
    fn a_full_command_line_parses() {
        let c = convert_args(&[
            "/games/PPSA01234",
            "--to",
            "ffpkg",
            "--out=/out dir",
            "--name",
            "My Game",
            "--zstd",
            "7",
            "--full-verify",
            "--json",
        ])
        .unwrap();
        assert_eq!(
            c,
            ConvertArgs {
                source: "/games/PPSA01234".into(),
                to: Target::Ffpkg,
                out: "/out dir".into(),
                name: Some("My Game".into()),
                level: None,
                zstd: Some(7),
                lz4: false,
                full_verify: true,
                json: true,
            }
        );
    }

    #[test]
    fn flags_may_come_before_the_source_and_a_source_may_start_with_a_dash() {
        let c = convert_args(&["--to", "PKG", "-o", "out", "--", "-odd name"]).unwrap();
        assert_eq!((c.source.as_str(), c.to), ("-odd name", Target::Pkg));
    }

    #[test]
    fn what_is_missing_or_wrong_is_named() {
        let err = |args: &[&str]| convert_args(args).unwrap_err();
        assert!(err(&["--to", "exfat", "--out", "o"]).contains("no source"));
        assert!(err(&["src", "--out", "o"]).contains("--to is missing"));
        assert!(err(&["src", "--to", "exfat"]).contains("--out is missing"));
        assert!(err(&["src", "--to", "iso", "--out", "o"]).contains("unknown format"));
        assert!(err(&["src", "--to"]).contains("--to needs a value"));
        assert!(err(&["a", "b", "--to", "exfat", "--out", "o"]).contains("second one"));
        assert!(err(&["src", "--to", "exfat", "--out", "o", "--fast"]).contains("unknown option"));
        assert!(err(&["src", "--to", "exfat", "--out", "o", "--json=1"]).contains("no value"));
        for bad in ["0", "23", "high"] {
            assert!(err(&["s", "--to", "exfat", "--out", "o", "--zstd", bad]).contains("1 to 22"));
        }
        assert!(
            err(&["s", "--to", "pkg", "--out", "o", "--level", "max"]).contains("unknown --level")
        );
    }

    #[test]
    fn options_a_format_has_no_use_for_are_refused() {
        let err = |extra: &[&str]| {
            let mut args = vec!["src", "--out", "o", "--to"];
            args.extend_from_slice(extra);
            convert_args(&args).unwrap_err()
        };
        assert!(err(&["exfat", "--level", "fast"]).contains("--level is for pkg and ffpfsc"));
        assert!(err(&["pkg", "--zstd", "5"]).contains("not for pkg"));
        assert!(err(&["pkg", "--lz4"]).contains("--lz4 is for images and folders"));
        assert!(err(&["exfat", "--lz4", "--zstd", "5"]).contains("pick one"));
        assert!(err(&["ffpfsc", "--level", "stored"]).contains("--to exfat"));
        assert_eq!(
            convert_args(&["s", "--out", "o", "--to", "pkg", "--level", "Stored"])
                .unwrap()
                .level
                .as_deref(),
            Some("stored")
        );
    }

    #[test]
    fn inspect_takes_a_source_and_nothing_of_convert() {
        assert_eq!(
            parse("inspect", &argv(&["game", "--out", "o"])),
            Ok(Command::Inspect {
                source: "game".into(),
                out: Some("o".into()),
            })
        );
        assert!(parse("inspect", &argv(&["game", "--to", "exfat"]))
            .unwrap_err()
            .contains("unknown option"));
        assert_eq!(parse("inspect", &argv(&["--help"])), Ok(Command::Help));
        assert_eq!(parse("convert", &argv(&["-h"])), Ok(Command::Help));
    }

    #[test]
    fn each_format_goes_to_its_route_with_the_body_the_app_sends() {
        let request = |args: &[&str]| convert_args(args).unwrap().request();
        let (route, body) = request(&["g", "--to", "pkg", "--out", "o", "--level", "smallest"]);
        assert_eq!(route, Route::Package);
        assert_eq!(
            body,
            serde_json::json!({
                "source": "g", "output_dir": "o", "full_verify": false,
                "compression": "smallest",
            })
        );
        let (route, body) = request(&[
            "g", "--to", "ffpfsc", "--out", "o", "--level", "fast", "--name", "n",
        ]);
        assert_eq!(route, Route::Image);
        assert_eq!(
            body,
            serde_json::json!({
                "source": "g", "output_dir": "o", "full_verify": false, "name": "n",
                "format": "exfat", "compress": true, "ffpfsc_level": "fast",
            })
        );
        let (route, body) = request(&["g", "--to", "exfat", "--out", "o", "--lz4"]);
        assert_eq!(route, Route::Image);
        assert_eq!(body["ampr_lz4"], serde_json::json!({}));
        assert_eq!(body["format"], "exfat");
        let (route, body) = request(&[
            "g.exfat",
            "--to",
            "folder",
            "--out",
            "o",
            "--zstd",
            "9",
            "--full-verify",
        ]);
        assert_eq!(route, Route::Folder);
        assert_eq!(body["ampr_zstd"], serde_json::json!({ "level": 9 }));
        assert_eq!(body["full_verify"], true);
        // Every body is one its route's request type takes.
        for to in ["exfat", "ffpkg", "ffpfs", "ffpfsc"] {
            let (_, body) = request(&["g", "--to", to, "--out", "o", "--zstd", "3"]);
            serde_json::from_value::<fpkg_api::ExfatBuildReq>(body).unwrap();
        }
        let (_, body) = request(&["g", "--to", "pkg", "--out", "o", "--name", "n"]);
        serde_json::from_value::<fpkg_api::BuildReq>(body).unwrap();
        let (_, body) = request(&["g", "--to", "folder", "--out", "o", "--zstd", "3"]);
        serde_json::from_value::<fpkg_api::FolderBuildReq>(body).unwrap();
        // LZ4 packs go into a folder and every image format too.
        let (route, body) = request(&["g", "--to", "folder", "--out", "o", "--lz4"]);
        assert_eq!(route, Route::Folder);
        assert_eq!(body["ampr_lz4"], serde_json::json!({}));
        serde_json::from_value::<fpkg_api::FolderBuildReq>(body).unwrap();
        for to in ["exfat", "ffpkg", "ffpfs"] {
            let (_, body) = request(&["g", "--to", to, "--out", "o", "--lz4"]);
            assert_eq!(body["format"], to);
            serde_json::from_value::<fpkg_api::ExfatBuildReq>(body).unwrap();
        }
    }

    #[test]
    fn progress_is_one_line_per_stage_and_percent() {
        let mut last = None;
        let progress = |done| Event::Progress {
            stage: Some("write".into()),
            stage_index: Some(1),
            stage_count: Some(3),
            done,
            total: 200 << 20,
        };
        assert_eq!(
            plain_line(&progress(50 << 20), &mut last).unwrap(),
            "write (2/3): 25% (50.0 of 200.0 MiB)"
        );
        assert_eq!(plain_line(&progress((50 << 20) + 1), &mut last), None);
        assert!(plain_line(&progress(52 << 20), &mut last).is_some());
        let line = serde_json::to_string(&Event::Progress {
            stage: None,
            stage_index: None,
            stage_count: None,
            done: 1,
            total: 2,
        })
        .unwrap();
        assert_eq!(line, r#"{"event":"progress","done":1,"total":2}"#);
    }

    /// A folder with the least a game needs: a module and its title's parameters.
    fn tiny_game(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("sce_sys")).unwrap();
        std::fs::create_dir_all(root.join("data/levels")).unwrap();
        std::fs::write(
            root.join("eboot.bin"),
            [0x4F, 0x15, 0x3D, 0x1D, 0, 1, 1, 0x12],
        )
        .unwrap();
        std::fs::write(
            root.join("sce_sys/param.json"),
            br#"{"contentId":"UP0000-PPSA01234_00-CONVERTCLITEST00","contentVersion":"01.000.000","titleId":"PPSA01234","applicationCategoryType":0,"attributePub":0,"applicationDrmType":"standard"}"#,
        )
        .unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(root.join("data/levels/big.bin"), big).unwrap();
        std::fs::write(root.join("data/small.txt"), b"hello\n").unwrap();
    }

    /// End to end, as the binary runs it: a game folder becomes an exFAT image through the
    /// image route's own handler and job, and the image holds the folder's files.
    #[tokio::test]
    async fn a_game_folder_converts_to_a_verified_image() {
        let root = std::env::temp_dir().join(format!("convert-cli-{}", Uuid::new_v4()));
        let (game, out) = (root.join("PPSA01234"), root.join("out"));
        tiny_game(&game);

        let seen = inspect(&game.to_string_lossy(), None).await.unwrap();
        assert_eq!(
            seen["content_id"], "UP0000-PPSA01234_00-CONVERTCLITEST00",
            "{seen}"
        );
        assert_eq!(seen["files"], 4);

        let args = convert_args(&[
            &game.to_string_lossy(),
            "--to",
            "exfat",
            "--out",
            &out.to_string_lossy(),
            "--name",
            "tiny",
            "--full-verify",
        ])
        .unwrap();
        let mut events = Vec::new();
        let ok = convert(
            &args,
            &mut |e| events.push(e.clone()),
            std::future::pending(),
        )
        .await;
        assert!(ok, "{events:?}");
        assert!(matches!(
            events.first(),
            Some(Event::Start { to: "exfat", .. })
        ));
        let Some(Event::Done {
            output,
            files,
            verified,
            ..
        }) = events.last()
        else {
            panic!("no done event: {events:?}");
        };
        assert_eq!((*files, *verified), (4, "full"));
        let image = out.join("tiny.exfat");
        assert_eq!(std::path::Path::new(output), image);
        let mut tree = ps5upload_fpkg::source::open(&image).unwrap();
        assert_eq!(
            tree.read("data/small.txt").unwrap(),
            b"hello\n",
            "the image reads back"
        );
        assert_eq!(tree.files().len(), 4);

        // A source that is not a game is refused by the route, in its words, with nothing run.
        let mut events = Vec::new();
        let args = ConvertArgs {
            source: out.to_string_lossy().into_owned(),
            name: Some("second".into()),
            ..args
        };
        assert!(
            !convert(
                &args,
                &mut |e| events.push(e.clone()),
                std::future::pending()
            )
            .await
        );
        assert!(
            matches!(&events[..], [Event::Failed { error, cancelled: false }] if error.contains("param.json")),
            "{events:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
