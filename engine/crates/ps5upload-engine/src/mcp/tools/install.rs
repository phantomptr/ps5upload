//! Installing packages: `install_pkg`, `install_status`, `install_wait`, `install_stop`,
//! `install_preflight`, `install_sessions`, `install_history`.
//!
//! An install is not a job of `/api/jobs`: it has its own status route and states, and the engine
//! allows one per console at a time.

use std::time::Duration;

use axum::http::Method;
use serde_json::{json, Value};
use tokio::time::Instant;

use super::route::{flag, route, text, whole};
use super::{
    console_required, engine_failure, object, success_json, without_nulls, Args, Kind, ToolCtx,
    ToolDef, ToolOutput, ToolResult,
};

const STATUS_ROUTE: &str = "/api/pkg/install/status";
/// How often `install_wait` looks.
const POLL_EVERY: Duration = Duration::from_secs(2);
const MAX_WAIT_S: u64 = 600;
/// A client with a 60 s limit on a tool call must get an answer from a call with no `timeout_s`.
const DEFAULT_WAIT_S: u64 = 30;

fn job_argument() -> Value {
    text("The install's id, from install_pkg.")
}

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "install_pkg",
            title: "Install a package",
            description: "Installs a .pkg on a console, from the console's own storage, from the \
                          computer the engine runs on, from a link, or from a saved server. Call \
                          consoles_readiness first. It answers at once with a `job`: install_wait \
                          follows it. One install per console at a time; a second is refused and \
                          names the one running.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "source_type": {
                        "type": "string",
                        "enum": ["console_path", "host_file", "url", "remote"],
                        "description": "Where the package is: on the console, on the engine's \
                                        computer, at a link, or on a saved server \
                                        (sources_list).",
                    },
                    "source": text(
                        "The package's path, or its link. For `remote`, its path on that server.",
                    ),
                    "connection": text("For `remote`: the saved server's id, from sources_list."),
                    "title_id": text("The title's id, when known, like PPSA01234."),
                    "content_id": text("The package's content id, when known."),
                    "category": text("The package's category, when known (`gd` for a game, `gp` \
                                      for an update, `ac` for an add-on)."),
                    "package_app_ver": text("The package's version, when known."),
                    "force_stream": flag(
                        "For `console_path`: have this engine serve the package to the console, \
                         after the console refused it from its own storage.",
                    ),
                    "proxy_link": flag(
                        "For `url`: have this engine download the link and serve it, for a link \
                         only this computer can reach.",
                    ),
                    "insecure_tls": flag(
                        "For `url` with `proxy_link`: skip this engine's certificate checks. Only \
                         when the user asked for it.",
                    ),
                    "console_path_fallback": flag(
                        "For `console_path`: as a last resort, when the console cannot reach this \
                         computer, install by its plain file path.",
                    ),
                    "allow_destructive_reinstall": flag(
                        "Allow a reinstall that first removes what is installed. It can lose \
                         data: only when the user asked for it.",
                    ),
                    "delete_source_copy_after": flag(
                        "Delete the console's copy of the package after a verified install. Only \
                         when the user asked for it.",
                    ),
                }),
                &["console", "source_type", "source"],
            ),
            run: |ctx, args| Box::pin(install(ctx, args)),
        },
        ToolDef {
            name: "install_status",
            title: "Follow an install",
            description: "How an install stands: its `phase` (resolve, deliver, install, verify, \
                          done, failed), `verdict`, and `hint` when it failed.",
            kind: Kind::Read,
            schema: object(json!({ "job": job_argument() }), &["job"]),
            run: |ctx, args| Box::pin(status(ctx, args)),
        },
        ToolDef {
            name: "install_wait",
            title: "Wait for an install",
            description: "Waits until an install ends, or for `timeout_s`, and answers with how \
                          it stands. `finished: false` means it still runs: call again. A failed \
                          install is a finished one: `verdict` and `hint` say why.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "job": job_argument(),
                    "timeout_s": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_WAIT_S,
                        "description": "Longest to wait, in seconds. Default 30. Keep it under \
                                        your client's limit on a tool call.",
                    },
                }),
                &["job"],
            ),
            run: |ctx, args| Box::pin(wait(ctx, args)),
        },
        ToolDef {
            name: "install_stop",
            title: "Stop an install",
            description: "Stops a running install within moments. The install ends as cancelled.",
            kind: Kind::Write { idempotent: true },
            schema: object(json!({ "job": job_argument() }), &["job"]),
            run: route!(
                method: "POST",
                path: "/api/pkg/install/stop",
                done: "Asked the install to stop.",
            ),
        },
        ToolDef {
            name: "install_preflight",
            title: "Check before an install",
            description: "How a package stands against a console before installing it: `state` \
                          is installed, different_version_installed, base_missing, not_installed \
                          or unknown (the console could not be read: never act on that).",
            kind: Kind::Read,
            schema: object(
                json!({
                    "console": console_required(),
                    "content_id": text("The package's content id."),
                    "package_type": text("The package's type, when known."),
                    "expected_size": whole("The package's size in bytes, when known."),
                    "package_fingerprint": text("The package's fingerprint, when known."),
                }),
                &["console", "content_id"],
            ),
            run: route!(path: "/api/pkg/install/preflight", done: "The console's answer."),
        },
        ToolDef {
            name: "install_sessions",
            title: "Package serving sessions",
            description: "The packages this engine is serving to consoles now.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: route!(path: "/api/pkg/install/sessions", done: "The serving sessions."),
        },
        ToolDef {
            name: "install_history",
            title: "Install history",
            description: "The installs this engine has run on a console, and how each ended.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_required() }), &["console"]),
            run: route!(
                path: "/api/pkg/install/history",
                console: "ps5_addr",
                done: "The install history.",
            ),
        },
    ]
}

/// The route's `source` field.
fn source_field(kind: &str, source: &str, connection: Option<&str>) -> Result<Value, String> {
    match kind {
        "console_path" | "host_file" | "url" => Ok(json!({ kind: source })),
        "remote" => match connection {
            Some(connection) => {
                Ok(json!({ "remote": { "connection": connection, "path": source } }))
            }
            None => {
                Err("`remote` needs `connection`: sources_list shows the saved servers.".into())
            }
        },
        other => Err(format!("`source_type` cannot be {other}.")),
    }
}

async fn install(ctx: ToolCtx, args: Args) -> ToolResult {
    let console = args.required("console")?;
    let source = source_field(
        args.required("source_type")?,
        args.required("source")?,
        args.text("connection"),
    )
    .map_err(ToolOutput::error)?;
    let mut options = serde_json::Map::new();
    for name in [
        "force_stream",
        "proxy_link",
        "insecure_tls",
        "console_path_fallback",
        "allow_destructive_reinstall",
        "delete_source_copy_after",
    ] {
        if let Some(on) = args.flag(name) {
            options.insert(name.to_string(), json!(on));
        }
    }
    let body = without_nulls(json!({
        "ps5_addr": console,
        "source": source,
        "content_id": args.text("content_id"),
        "title_id": args.text("title_id"),
        "category": args.text("category"),
        "package_app_ver": args.text("package_app_ver"),
        "options": options,
    }));
    let label = "POST /api/pkg/install";
    let reply = ctx
        .reply(Method::POST, "/api/pkg/install", &[], Some(&body))
        .await?;
    let answer = reply.json().unwrap_or(Value::Null);
    if answer.get("error").and_then(Value::as_str) == Some("busy") {
        let running = answer.get("job").and_then(Value::as_str).unwrap_or("?");
        return Err(ToolOutput::error_with(
            format!("{console} already runs an install, job {running}: install_wait follows it."),
            json!({ "error": "busy", "job": running }),
        ));
    }
    if !reply.is_success() {
        return Err(engine_failure(label, &reply));
    }
    let answer = success_json(label, &reply)?;
    let job = answer
        .get("job")
        .and_then(Value::as_str)
        .unwrap_or("(no id given)");
    Ok(ToolOutput::ok(
        format!("The install started, job {job}: install_wait follows it."),
        answer,
    ))
}

fn is_over(state: &Value) -> bool {
    matches!(
        state.get("phase").and_then(Value::as_str),
        Some("done" | "failed")
    )
}

/// One line about an install.
fn describe(job: &str, state: &Value) -> String {
    let field = |name: &str| state.get(name).and_then(Value::as_str);
    let phase = field("phase").unwrap_or("unknown");
    let mut line = format!("Install {job}: {phase}.");
    if let Some(verdict) = field("verdict") {
        line.push_str(&format!(" Verdict: {verdict}."));
    }
    if let Some(hint) = field("hint") {
        line.push(' ');
        line.push_str(hint);
    }
    line
}

async fn read_state(ctx: &ToolCtx, job: &str) -> Result<Value, ToolOutput> {
    ctx.get(STATUS_ROUTE, &[("job", job.to_string())]).await
}

async fn status(ctx: ToolCtx, args: Args) -> ToolResult {
    let job = args.required("job")?;
    let state = read_state(&ctx, job).await?;
    Ok(ToolOutput::ok(describe(job, &state), state))
}

async fn wait(ctx: ToolCtx, args: Args) -> ToolResult {
    let job = args.required("job")?;
    let limit = Duration::from_secs(
        args.number("timeout_s")
            .unwrap_or(DEFAULT_WAIT_S)
            .min(MAX_WAIT_S),
    );
    let started = Instant::now();
    loop {
        let state = read_state(&ctx, job).await?;
        let waited = started.elapsed();
        let finished = is_over(&state);
        if finished || waited >= limit {
            return Ok(ToolOutput::ok(
                describe(job, &state),
                json!({
                    "job": job,
                    "finished": finished,
                    "waited_s": waited.as_secs(),
                    "install": state,
                }),
            ));
        }
        tokio::time::sleep(POLL_EVERY.min(limit - waited)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_source_goes_as_the_route_reads_it() {
        use crate::install::deliver::Source;
        let parse = |value: Value| serde_json::from_value::<Source>(value).unwrap();
        assert_eq!(
            parse(source_field("console_path", "/data/a.pkg", None).unwrap()),
            Source::ConsolePath("/data/a.pkg".into())
        );
        assert_eq!(
            parse(source_field("host_file", "/tmp/a.pkg", None).unwrap()),
            Source::HostFile("/tmp/a.pkg".into())
        );
        assert_eq!(
            parse(source_field("url", "http://x/a.pkg", None).unwrap()),
            Source::Url("http://x/a.pkg".into())
        );
        assert_eq!(
            parse(source_field("remote", "/a.pkg", Some("nas")).unwrap()),
            Source::Remote {
                connection: "nas".into(),
                path: "/a.pkg".into()
            }
        );
        assert!(source_field("remote", "/a.pkg", None)
            .unwrap_err()
            .contains("sources_list"));
        assert!(source_field("carrier_pigeon", "/a.pkg", None).is_err());
    }

    #[test]
    fn an_install_reads_in_one_line() {
        assert!(!is_over(&json!({"phase": "deliver"})));
        assert!(is_over(&json!({"phase": "done"})));
        assert!(is_over(&json!({"phase": "failed"})));
        assert_eq!(
            describe(
                "j",
                &json!({"phase": "failed", "verdict": "failed", "hint": "Try again."})
            ),
            "Install j: failed. Verdict: failed. Try again."
        );
    }
}
