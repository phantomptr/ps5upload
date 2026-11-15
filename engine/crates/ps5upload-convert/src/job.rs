//! One job, asked for and reported through files in a folder (on the PS5
//! `/data/ps5upload/convert`). The helper's extractor works the same way
//! (`payload/extractor/README.md`); no port is opened, so nothing but the helper can ask.
//!
//! - `request.json`: written by the helper, read and deleted here at start.
//!   `{"id": "<the helper's job id>", "op": "inspect", "path": "<source>"}`
//! - `status.json`: rewritten here through a temporary file and a rename in the same folder, so
//!   the helper never reads half of one.
//!   `{"id", "pid", "op", "state": "running"|"done"|"failed", "cause"?, "detail"?, "result"?}`
//!   `cause` is a token the apps translate (`convert_bad_request`, `convert_unsupported_source`,
//!   `convert_source_unreadable`); `detail` is for the log; `result` is the op's answer.
//! - `cancel`: created by the helper to stop a job. An inspection is over before it could
//!   matter; the build ops will look for it between writes.

use std::path::Path;

use serde_json::json;

/// The most a request may be: a path and a few options. A larger file is not one of ours.
const MAX_REQUEST_BYTES: u64 = 64 << 10;

#[derive(Debug, serde::Deserialize)]
struct Request {
    id: String,
    op: String,
    #[serde(default)]
    path: String,
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
                "cause": "convert_bad_request", "detail": detail});
            write_status(dir, &status)?;
            return Ok(1);
        }
    };
    let status =
        |state: &str| json!({"id": request.id, "pid": pid, "op": request.op, "state": state});
    let fail = |cause: &str, detail: String| {
        crate::log(format_args!("{} failed: {cause}: {detail}", request.op));
        let mut s = status("failed");
        s["cause"] = json!(cause);
        s["detail"] = json!(detail);
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
                Err(refusal) => fail(refusal.cause, refusal.detail),
            }
        }
        other => fail("convert_bad_request", format!("unknown op {other:?}")),
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
        let s = run_with(Some(
            r#"{"id":"b","op":"inspect","path":"/nonexistent/game.exfat"}"#,
        ));
        assert_eq!(
            (s["id"].as_str(), s["cause"].as_str()),
            (Some("b"), Some("convert_source_unreadable"))
        );
    }
}
