//! The engine's jobs (uploads, downloads, conversions): `jobs_list`, `jobs_get`, `jobs_cancel`,
//! `jobs_pause`, `jobs_resume`, `jobs_wait`.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::time::Instant;

use super::{object, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult};
use crate::mcp::encode_component;

/// How often `jobs_wait` looks.
const POLL_EVERY: Duration = Duration::from_secs(1);
/// Longest `jobs_wait`. Many clients give up on a tool call long before this.
const MAX_WAIT_S: u64 = 600;
/// A client with a 60 s limit on a tool call must get an answer from a call with no `timeout_s`.
const DEFAULT_WAIT_S: u64 = 30;

fn job_id_argument() -> Value {
    json!({ "type": "string", "description": "The job's id, from jobs_list or the call that started it." })
}

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "jobs_list",
            title: "List jobs",
            description: "The engine's jobs and how each stands: running, done or failed. Recent \
                          ones only: the engine forgets the oldest finished jobs.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "status": {
                        "type": "string",
                        "enum": ["running", "done", "failed"],
                        "description": "Only the jobs in this state.",
                    },
                }),
                &[],
            ),
            run: |ctx, args| Box::pin(list(ctx, args)),
        },
        ToolDef {
            name: "jobs_get",
            title: "Get a job",
            description: "One job's state now: progress while it runs, the result or the error \
                          after.",
            kind: Kind::Read,
            schema: object(json!({ "job_id": job_id_argument() }), &["job_id"]),
            run: |ctx, args| Box::pin(get(ctx, args)),
        },
        ToolDef {
            name: "jobs_cancel",
            title: "Cancel a job",
            description: "Stops a running transfer at its next safe point. What was sent stays, \
                          so the same transfer can resume later. `cancelled: false` means it was \
                          not running.",
            kind: Kind::Write { idempotent: true },
            schema: object(json!({ "job_id": job_id_argument() }), &["job_id"]),
            run: |ctx, args| Box::pin(cancel(ctx, args)),
        },
        ToolDef {
            name: "jobs_pause",
            title: "Pause a job",
            description: "Holds a running job that can wait (a conversion, for one): it uses no \
                          processor or disk until resumed. Refused for a job that is over or \
                          cannot be paused.",
            kind: Kind::Write { idempotent: true },
            schema: object(json!({ "job_id": job_id_argument() }), &["job_id"]),
            run: |ctx, args| Box::pin(pause(ctx, args)),
        },
        ToolDef {
            name: "jobs_resume",
            title: "Resume a job",
            description: "Lets a paused job carry on.",
            kind: Kind::Write { idempotent: true },
            schema: object(json!({ "job_id": job_id_argument() }), &["job_id"]),
            run: |ctx, args| Box::pin(resume(ctx, args)),
        },
        ToolDef {
            name: "jobs_wait",
            title: "Wait for a job",
            description: "Waits until a job ends, or for `timeout_s`, and answers with its state. \
                          `finished: false` means it still runs: call again. A job that failed is \
                          a finished job: its state says why.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "job_id": job_id_argument(),
                    "timeout_s": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_WAIT_S,
                        "description": "Longest to wait, in seconds. Default 30. Keep it under \
                                        your client's limit on a tool call.",
                    },
                }),
                &["job_id"],
            ),
            run: |ctx, args| Box::pin(wait(ctx, args)),
        },
    ]
}

fn job_path(id: &str, then: &str) -> String {
    format!("/api/jobs/{}{then}", encode_component(id))
}

fn status_of(job: &Value) -> &str {
    job.get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

async fn list(ctx: ToolCtx, args: Args) -> ToolResult {
    let answer = ctx.get("/api/jobs", &[]).await?;
    let jobs = answer.as_array().cloned().unwrap_or_default();
    let jobs: Vec<Value> = match args.text("status") {
        Some(wanted) => jobs
            .into_iter()
            .filter(|job| status_of(job) == wanted)
            .collect(),
        None => jobs,
    };
    let running = jobs
        .iter()
        .filter(|job| status_of(job) == "running")
        .count();
    Ok(ToolOutput::ok(
        format!("{} job(s), {running} running.", jobs.len()),
        Value::Array(jobs),
    ))
}

async fn get(ctx: ToolCtx, args: Args) -> ToolResult {
    let id = args.required("job_id")?;
    let job = ctx.get(&job_path(id, ""), &[]).await?;
    Ok(ToolOutput::ok(describe(id, &job), job))
}

async fn cancel(ctx: ToolCtx, args: Args) -> ToolResult {
    let id = args.required("job_id")?;
    let answer = ctx.post_empty(&job_path(id, "/cancel")).await?;
    let summary = if answer.get("cancelled").and_then(Value::as_bool) == Some(true) {
        format!("Asked job {id} to stop. jobs_wait shows when it has.")
    } else {
        format!("Job {id} is not running, so there was nothing to stop.")
    };
    Ok(ToolOutput::ok(summary, answer))
}

async fn pause(ctx: ToolCtx, args: Args) -> ToolResult {
    let id = args.required("job_id")?;
    let answer = ctx.post_empty(&job_path(id, "/pause")).await?;
    Ok(ToolOutput::ok(format!("Job {id} is paused."), answer))
}

async fn resume(ctx: ToolCtx, args: Args) -> ToolResult {
    let id = args.required("job_id")?;
    let answer = ctx.post_empty(&job_path(id, "/resume")).await?;
    Ok(ToolOutput::ok(format!("Job {id} carries on."), answer))
}

async fn wait(ctx: ToolCtx, args: Args) -> ToolResult {
    let id = args.required("job_id")?;
    let limit = Duration::from_secs(
        args.number("timeout_s")
            .unwrap_or(DEFAULT_WAIT_S)
            .min(MAX_WAIT_S),
    );
    let path = job_path(id, "");
    let started = Instant::now();
    loop {
        let job = ctx.get(&path, &[]).await?;
        let waited = started.elapsed();
        let finished = status_of(&job) != "running";
        if finished || waited >= limit {
            let summary = if finished {
                describe(id, &job)
            } else {
                format!(
                    "Job {id} is still running after {} s. Call jobs_wait again to keep waiting.",
                    waited.as_secs()
                )
            };
            return Ok(ToolOutput::ok(
                summary,
                json!({
                    "job_id": id,
                    "finished": finished,
                    "waited_s": waited.as_secs(),
                    "job": job,
                }),
            ));
        }
        tokio::time::sleep(POLL_EVERY.min(limit - waited)).await;
    }
}

/// One line about a job.
fn describe(id: &str, job: &Value) -> String {
    match status_of(job) {
        "failed" => format!(
            "Job {id} failed: {}",
            job.get("error")
                .and_then(Value::as_str)
                .unwrap_or("no error given")
        ),
        "done" => format!("Job {id} is done."),
        "running" => {
            let number = |name: &str| job.get(name).and_then(Value::as_u64).unwrap_or(0);
            let (sent, total) = (number("bytes_sent"), number("total_bytes"));
            match sent.saturating_mul(100).checked_div(total) {
                Some(percent) => format!("Job {id} is running: {percent} % done."),
                None => format!("Job {id} is running."),
            }
        }
        other => format!("Job {id} is {other}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_reads_in_one_line() {
        assert_eq!(
            describe("j", &json!({"status": "failed", "error": "disk full"})),
            "Job j failed: disk full"
        );
        assert_eq!(describe("j", &json!({"status": "done"})), "Job j is done.");
        assert_eq!(
            describe(
                "j",
                &json!({"status": "running", "bytes_sent": 25, "total_bytes": 100})
            ),
            "Job j is running: 25 % done."
        );
        assert_eq!(
            describe("j", &json!({"status": "running"})),
            "Job j is running."
        );
    }

    #[test]
    fn a_job_id_cannot_leave_its_path_segment() {
        assert_eq!(job_path("a/../b", "/cancel"), "/api/jobs/a%2F..%2Fb/cancel");
    }
}
