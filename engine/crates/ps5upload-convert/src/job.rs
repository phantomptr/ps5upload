//! One job, asked for and reported through files in a folder (on the PS5
//! `/data/ps5upload/convert`). The helper's extractor works the same way
//! (`payload/extractor/README.md`); no port is opened, so nothing but the helper can ask. The
//! helper's side is `payload/src/console_convert.c`.
//!
//! - `request.json`: written by the helper, read and deleted here at start.
//!   `{"id": "<the helper's job id>", "op": "inspect"|"build", "path": "<source>"}`, and for a
//!   build `"dest"`, `"format"` (`exfat`, `ffpkg`, `ffpfs`, `folder`), `"zstd"`, `"verify"`.
//! - `status.json`: rewritten here through a temporary file and a rename in the same folder, so
//!   the helper never reads half of one; at most every [`STATUS_EVERY`] while a job runs.
//!   `{"id", "pid", "op", "state": "running"|"done"|"failed", "stage"?, "bytes_done"?,
//!   "bytes_total"?, "cause"?, "detail"?, "result"?}`
//!   `cause` is a token the apps translate (`convert_bad_request`, `convert_unsupported_source`,
//!   `convert_source_unreadable`, `convert_cancelled`, `convert_not_built_yet`); `detail` is for
//!   the log; `result` is the op's answer.
//! - `cancel`: created by the helper to stop a job; looked for at most every [`CANCEL_EVERY`].
//!
//! `build` does not build yet: the build code is still inside the engine. Until it moves here,
//! a build reads the whole source the way a real one will (through the same readers, reporting
//! progress, stopping when asked), writes nothing, and ends with `convert_not_built_yet`. That is
//! enough to try the whole chain on a console: start, progress, Stop, and how fast the console
//! reads a game.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::Refusal;

/// The most a request may be: a path and a few options. A larger file is not one of ours.
const MAX_REQUEST_BYTES: u64 = 64 << 10;
/// How often a running job rewrites its status. The helper polls every 200 ms.
const STATUS_EVERY: Duration = Duration::from_millis(250);
/// How often a running job looks for the cancel file.
const CANCEL_EVERY: Duration = Duration::from_millis(150);
/// The most of a failure's words kept in the status: the helper reads `detail` into 400 bytes
/// and drops a longer one.
const MAX_DETAIL_BYTES: usize = 300;
/// One read of the source. The image writers read 1 to 8 MiB at a time.
const READ_CHUNK: usize = 8 << 20;

#[derive(Debug, serde::Deserialize)]
struct Request {
    id: String,
    op: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    dest: String,
    #[serde(default)]
    format: String,
}

/// Writes `status` as `dir/status.json`. The temporary file is in `dir` too: a rename never
/// leaves the folder (a rename across filesystems panics the PS5's kernel).
fn write_status(dir: &Path, status: &serde_json::Value) -> std::io::Result<()> {
    let tmp = dir.join(format!("status.json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, format!("{status}\n"))?;
    std::fs::rename(&tmp, dir.join("status.json"))
}

fn read_request(dir: &Path) -> Result<Request, String> {
    let path = dir.join("request.json");
    let size = std::fs::metadata(&path)
        .map_err(|e| format!("request.json: {e}"))?
        .len();
    if size > MAX_REQUEST_BYTES {
        return Err(format!("request.json is {size} bytes"));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("request.json: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("request.json: {e}"))
}

/// `detail` cut to [`MAX_DETAIL_BYTES`] at a character boundary.
fn short(detail: &str) -> &str {
    let mut end = detail.len().min(MAX_DETAIL_BYTES);
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    &detail[..end]
}

/// A running job's view of its folder: where it reports and where it is told to stop.
struct Running<'a> {
    dir: &'a Path,
    base: serde_json::Value,
    last_status: Instant,
    last_cancel: Instant,
    cancelled: bool,
}

impl Running<'_> {
    /// Has the helper asked to stop? The file is looked for now and then, and once seen stays seen.
    fn cancelled(&mut self) -> bool {
        if !self.cancelled && self.last_cancel.elapsed() >= CANCEL_EVERY {
            self.last_cancel = Instant::now();
            self.cancelled = self.dir.join("cancel").exists();
        }
        self.cancelled
    }

    /// Reports progress, at most every [`STATUS_EVERY`]. A status that cannot be written is not
    /// worth ending the job for: the next one may go through, and the final one is checked.
    fn progress(&mut self, stage: &str, done: u64, total: u64) {
        if self.last_status.elapsed() < STATUS_EVERY {
            return;
        }
        self.last_status = Instant::now();
        let mut s = self.base.clone();
        s["state"] = json!("running");
        s["stage"] = json!(stage);
        s["bytes_done"] = json!(done);
        s["bytes_total"] = json!(total);
        let _ = write_status(self.dir, &s);
    }
}

/// The stand-in for a build (see the module docs): reads every byte of the source.
fn read_through(request: &Request, run: &mut Running<'_>) -> Result<(), Refusal> {
    let bad = |detail: String| Refusal {
        cause: "convert_bad_request",
        detail,
    };
    if request.dest.is_empty() {
        return Err(bad("a build needs a dest".to_string()));
    }
    if !matches!(
        request.format.as_str(),
        "exfat" | "ffpkg" | "ffpfs" | "folder"
    ) {
        return Err(bad(format!("unknown format {:?}", request.format)));
    }
    let path = Path::new(&request.path);
    // The same checks an inspection makes: what this program reads, and that it opens.
    crate::inspect(path)?;
    let unreadable = |e: ps5upload_fpkg::Error| Refusal {
        cause: "convert_source_unreadable",
        detail: format!("{}: {e}", request.path),
    };
    let mut tree = ps5upload_fpkg::source::open(path).map_err(unreadable)?;
    let files = tree.files().to_vec();
    let total: u64 = files.iter().map(|f| f.size).sum();
    let started = Instant::now();
    let mut done = 0u64;
    for file in &files {
        let mut at = 0u64;
        while at < file.size {
            if run.cancelled() {
                return Err(Refusal {
                    cause: "convert_cancelled",
                    detail: format!("stopped after {done} of {total} bytes"),
                });
            }
            let bytes = tree
                .read_range(&file.path, at, READ_CHUNK)
                .map_err(unreadable)?;
            if bytes.is_empty() {
                return Err(Refusal {
                    cause: "convert_source_unreadable",
                    detail: format!("{} ends at {at} of {} bytes", file.path, file.size),
                });
            }
            at += bytes.len() as u64;
            done += bytes.len() as u64;
            run.progress("read", done, total);
        }
    }
    let secs = started.elapsed().as_secs_f64().max(0.001);
    crate::log(format_args!(
        "read {} files, {done} bytes in {secs:.1} s ({:.0} MiB/s)",
        files.len(),
        done as f64 / secs / (1 << 20) as f64
    ));
    Err(Refusal {
        cause: "convert_not_built_yet",
        detail: format!(
            "read all {done} bytes of the source; writing {} is not in this build",
            request.dest
        ),
    })
}

/// Runs the request in `dir` and leaves its outcome in `dir/status.json`. Returns the process's
/// exit code: 0 done, 1 failed. A failure to write the status itself is the only error.
pub fn run(dir: &Path) -> std::io::Result<i32> {
    let pid = std::process::id();
    let request = read_request(dir);
    // Read once: a request left behind would run again at the next start.
    let _ = std::fs::remove_file(dir.join("request.json"));
    let request = match request {
        Ok(r) => r,
        Err(detail) => {
            crate::log(format_args!("bad request: {detail}"));
            let status = json!({"id": "", "pid": pid, "op": "", "state": "failed",
                "cause": "convert_bad_request", "detail": short(&detail)});
            write_status(dir, &status)?;
            return Ok(1);
        }
    };
    let base = json!({"id": request.id, "pid": pid, "op": request.op});
    let status = |state: &str| {
        let mut s = base.clone();
        s["state"] = json!(state);
        s
    };
    let fail = |refusal: Refusal| {
        crate::log(format_args!("{} failed: {refusal}", request.op));
        let mut s = status("failed");
        s["cause"] = json!(refusal.cause);
        s["detail"] = json!(short(&refusal.detail));
        write_status(dir, &s).map(|()| 1)
    };
    // The helper waits for the first status to know the program started and learn its pid.
    write_status(dir, &status("running"))?;
    match request.op.as_str() {
        "inspect" => {
            crate::log(format_args!("inspect {}", request.path));
            match crate::inspect(Path::new(&request.path)) {
                Ok(inspection) => {
                    let mut s = status("done");
                    s["result"] = json!(inspection);
                    write_status(dir, &s)?;
                    Ok(0)
                }
                Err(refusal) => fail(refusal),
            }
        }
        "build" => {
            crate::log(format_args!(
                "build {} -> {} ({})",
                request.path, request.dest, request.format
            ));
            let mut running = Running {
                dir,
                base: base.clone(),
                last_status: Instant::now(),
                last_cancel: Instant::now(),
                cancelled: false,
            };
            match read_through(&request, &mut running) {
                Ok(()) => {
                    write_status(dir, &status("done"))?;
                    Ok(0)
                }
                Err(refusal) => fail(refusal),
            }
        }
        other => fail(Refusal {
            cause: "convert_bad_request",
            detail: format!("unknown op {other:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{game_folder, TempDir};

    fn status(dir: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir.join("status.json")).unwrap()).unwrap()
    }

    #[test]
    fn an_inspect_request_ends_in_a_done_status_with_the_result() {
        let tmp = TempDir::new("job");
        let game = game_folder(&tmp.0);
        let dir = tmp.0.join("convert");
        std::fs::create_dir_all(&dir).unwrap();
        let request = json!({"id": "j1", "op": "inspect", "path": game});
        std::fs::write(dir.join("request.json"), request.to_string()).unwrap();

        assert_eq!(run(&dir).unwrap(), 0);
        let s = status(&dir);
        assert_eq!(
            (s["id"].as_str(), s["state"].as_str()),
            (Some("j1"), Some("done"))
        );
        assert_eq!(s["pid"], std::process::id());
        let result: crate::Inspection = serde_json::from_value(s["result"].clone()).unwrap();
        assert_eq!(result, crate::inspect(&game).unwrap());
        // The request is gone and no temporary status is left behind.
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(left, ["status.json"]);
    }

    #[test]
    fn failures_carry_a_cause() {
        let tmp = TempDir::new("jobfail");
        let run_with = |request: Option<&str>| {
            if let Some(r) = request {
                std::fs::write(tmp.0.join("request.json"), r).unwrap();
            }
            assert_eq!(run(&tmp.0).unwrap(), 1);
            assert!(!tmp.0.join("request.json").exists());
            status(&tmp.0)
        };
        assert_eq!(run_with(None)["cause"], "convert_bad_request");
        assert_eq!(run_with(Some("{not json"))["cause"], "convert_bad_request");
        assert_eq!(
            run_with(Some(r#"{"id":"a","op":"build"}"#))["cause"],
            "convert_bad_request"
        );
        assert_eq!(
            run_with(Some(r#"{"id":"a","op":"mount"}"#))["cause"],
            "convert_bad_request"
        );
        let s = run_with(Some(
            r#"{"id":"b","op":"inspect","path":"/nonexistent/game.exfat"}"#,
        ));
        assert_eq!(
            (s["id"].as_str(), s["cause"].as_str()),
            (Some("b"), Some("convert_source_unreadable"))
        );
    }

    /// The stand-in build reads the source and says it is not built yet; with a cancel file
    /// there it stops instead. Neither writes the destination.
    #[test]
    fn a_build_reads_the_source_and_stops_when_asked() {
        let tmp = TempDir::new("jobbuild");
        let game = game_folder(&tmp.0);
        let dir = tmp.0.join("convert");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = tmp.0.join("out.exfat");
        let request =
            json!({"id": "b1", "op": "build", "path": game, "dest": dest, "format": "exfat"});

        std::fs::write(dir.join("request.json"), request.to_string()).unwrap();
        assert_eq!(run(&dir).unwrap(), 1);
        let s = status(&dir);
        assert_eq!(s["cause"], "convert_not_built_yet");
        assert!(s["detail"].as_str().unwrap().starts_with("read all 100"));

        std::fs::write(dir.join("request.json"), request.to_string()).unwrap();
        std::fs::write(dir.join("cancel"), b"").unwrap();
        // The cancel file is looked for after CANCEL_EVERY: a slow source gives it that long.
        let mut running = Running {
            dir: &dir,
            base: json!({}),
            last_status: Instant::now(),
            last_cancel: Instant::now() - CANCEL_EVERY,
            cancelled: false,
        };
        let request: Request = serde_json::from_value(request).unwrap();
        assert_eq!(
            read_through(&request, &mut running).unwrap_err().cause,
            "convert_cancelled"
        );
        assert!(!dest.exists());
    }

    #[test]
    fn a_long_detail_is_cut_at_a_character() {
        let long = "é".repeat(400);
        let cut = short(&long);
        assert!(cut.len() <= MAX_DETAIL_BYTES && cut.len() >= MAX_DETAIL_BYTES - 1);
        assert_eq!(short("short"), "short");
    }
}
