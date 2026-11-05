//! Running and starting things on a console: `processes_list`, `processes_kill`, `shell_run`,
//! `games_installed`, `games_launch`, `games_stop`, `games_register`, `games_unregister`.

use serde_json::{json, Value};

use super::route::{flag, route, text, whole};
use super::{
    console_optional, console_required, object, Args, Kind, ToolCtx, ToolDef, ToolOutput,
    ToolResult,
};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "processes_list",
            title: "Console processes",
            description: "What runs on a console: pid, name, title id, memory and kind (`app` for \
                          a game or application, `payload`, `system`). A running game shows as \
                          kind `app` with its title id.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: |ctx, args| Box::pin(processes(ctx, args)),
        },
        ToolDef {
            name: "games_installed",
            title: "Installed games",
            description: "With `console`: what that console has installed now, read from it. \
                          Without: what each console this engine knows was last read to have, \
                          from its records, with no console asked.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "console": {
                        "type": "string",
                        "description": "The PS5's address, like 192.168.1.100. Left out, this \
                                        answers from the engine's records of every console.",
                    },
                }),
                &[],
            ),
            run: |ctx, args| Box::pin(installed(ctx, args)),
        },
        ToolDef {
            name: "games_launch",
            title: "Launch a game",
            description: "Starts an installed title. The answer only means the console accepted \
                          the request: a game takes a while to come up, so watch for it with \
                          processes_list, for a minute or more. Do not launch it again meanwhile: \
                          a second launch pushes a starting game to the background.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "title_id": {
                        "type": "string",
                        "description": "The title's id, like PPSA01234 (from games_installed).",
                    },
                }),
                &["console", "title_id"],
            ),
            run: |ctx, args| Box::pin(launch(ctx, args)),
        },
        ToolDef {
            name: "games_stop",
            title: "Close a game",
            description: "Closes a running title, as closing it on the console does, but with no \
                          chance to save: unsaved progress is lost. Confirm with the user first. \
                          Tries a clean close, then force-stops its process if that is refused.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "title_id": {
                        "type": "string",
                        "description": "The running title's id, like PPSA01234 (from \
                                        processes_list).",
                    },
                }),
                &["console", "title_id"],
            ),
            run: |ctx, args| Box::pin(stop(ctx, args)),
        },
        ToolDef {
            name: "games_register",
            title: "Register a game folder",
            description: "Registers a game folder already on a console, so it shows on the home \
                          screen and can launch.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "src_path": text("The game's folder on the console."),
                    "patch_drm_type": flag(
                        "Rewrite the DRM type in the folder's sce_sys/param.json, for a dump the \
                         launcher rejects. It changes the user's file: only when asked for.",
                    ),
                }),
                &["console", "src_path"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/app/register",
                done: "Registered the game.",
            ),
        },
        ToolDef {
            name: "games_unregister",
            title: "Uninstall a title",
            description: "Removes a title from a console through the system. Confirm with the \
                          user first. Read the answer: `sony_uninstall_refused` means the console \
                          said no, and `still_present` that its folders remain.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "title_id": text("The title's id, like PPSA01234."),
                }),
                &["console", "title_id"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/app/unregister",
                done: "Asked the console to remove the title.",
            ),
        },
        ToolDef {
            name: "shell_run",
            title: "Run a console command",
            description: "Runs a command in the PS5Upload helper's shell on a console. It can do \
                          anything the helper can, including what cannot be undone, and read any \
                          file there, sign-in keys in backup copies too: only what the user asked \
                          for, and show them the command first.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "cmd": text("The command."),
                    "cwd": text("The folder to run it in."),
                    "session_id": text("Keeps `cd` and the environment across calls."),
                    "timeout_s": whole("Longest to let it run, in seconds."),
                }),
                &["console", "cmd"],
            ),
            run: route!(method: "POST", path: "/api/ps5/shell/run", done: "Ran the command."),
        },
        ToolDef {
            name: "processes_kill",
            title: "Kill a console process",
            description: "Force-stops one process on a console. Unsaved work in it is lost, and \
                          the wrong system process can bring the console down: for a game use \
                          games_stop, and for anything else confirm with the user first.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "pid": whole("The process's id, from processes_list."),
                }),
                &["console", "pid"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/process/kill",
                done: "Force-stopped the process.",
            ),
        },
    ]
}

fn process_rows(answer: &Value) -> &[Value] {
    answer
        .get("processes")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// The processes that belong to a title: games only, never the helper itself. The app's way of
/// telling (`client/src/lib/runningGames.ts`).
fn game_processes<'a>(
    processes: &'a [Value],
    title_id: &str,
) -> impl Iterator<Item = &'a Value> + 'a {
    let title_id = title_id.to_string();
    processes.iter().filter(move |process| {
        process.get("kind").and_then(Value::as_str) == Some("app")
            && process.get("is_self").and_then(Value::as_bool) != Some(true)
            && process
                .get("title_id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty() && id.eq_ignore_ascii_case(&title_id))
    })
}

async fn processes(ctx: ToolCtx, args: Args) -> ToolResult {
    let answer = ctx
        .get("/api/ps5/process/list", &args.console_query())
        .await?;
    let list = process_rows(&answer);
    let mut titles: Vec<&str> = list
        .iter()
        .filter(|process| process.get("kind").and_then(Value::as_str) == Some("app"))
        .filter_map(|process| process.get("title_id").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .collect();
    titles.sort_unstable();
    titles.dedup();
    let summary = if titles.is_empty() {
        format!("{} processes; no game or application runs.", list.len())
    } else {
        format!("{} processes; running: {}.", list.len(), titles.join(", "))
    };
    Ok(ToolOutput::ok(summary, answer))
}

async fn installed(ctx: ToolCtx, args: Args) -> ToolResult {
    if args.text("console").is_none() {
        let answer = ctx.get("/api/games/installed", &[]).await?;
        let count = answer.as_array().map_or(0, Vec::len);
        return Ok(ToolOutput::ok(
            format!("{count} title(s) on record, across the consoles this engine knows."),
            answer,
        ));
    }
    let answer = ctx
        .get("/api/ps5/apps/installed", &args.console_query())
        .await?;
    let count = answer
        .get("titles")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let maybe_more = if answer.get("incomplete").and_then(Value::as_bool) == Some(true) {
        " There may be more: the console could not list everything."
    } else {
        ""
    };
    Ok(ToolOutput::ok(
        format!("{count} title(s) installed.{maybe_more}"),
        answer,
    ))
}

async fn launch(ctx: ToolCtx, args: Args) -> ToolResult {
    let console = args.required("console")?;
    let title_id = args.required("title_id")?;
    let answer = ctx
        .post(
            "/api/ps5/app/launch",
            &json!({ "addr": console, "title_id": title_id }),
        )
        .await?;
    Ok(ToolOutput::ok(
        format!(
            "{console} accepted the launch of {title_id}. Watch for it with processes_list; do \
             not launch it again meanwhile."
        ),
        answer,
    ))
}

async fn stop(ctx: ToolCtx, args: Args) -> ToolResult {
    let console = args.required("console")?;
    let title_id = args.required("title_id")?;
    let answer = ctx
        .get("/api/ps5/process/list", &[("addr", console.to_string())])
        .await?;
    let list = process_rows(&answer);
    let number =
        |process: &Value, name: &str| process.get(name).and_then(Value::as_u64).unwrap_or(0);
    // A game runs as many processes; a clean close needs one that carries its app id.
    let target = game_processes(list, title_id)
        .find(|process| number(process, "app_id") != 0)
        .or_else(|| game_processes(list, title_id).next());
    let Some(target) = target else {
        return Ok(ToolOutput::ok(
            format!("{title_id} is not running on {console}."),
            json!({ "title_id": title_id, "stopped": false, "running": false }),
        ));
    };
    let (app_id, pid) = (number(target, "app_id"), number(target, "pid"));

    // The way the app closes a game (`client/src/lib/killGame.ts`): Sony's app kill, then a
    // SIGKILL when that is refused, as some firmwares do.
    let mut refused = None;
    if app_id != 0 {
        let body = json!({ "addr": console, "action": "kill", "app_id": app_id });
        match ctx.post("/api/ps5/app/lifecycle", &body).await {
            Ok(_) => {
                return Ok(ToolOutput::ok(
                    format!("Closed {title_id} on {console}."),
                    json!({
                        "title_id": title_id,
                        "stopped": true,
                        "how": "app_kill",
                        "app_id": app_id,
                    }),
                ));
            }
            Err(failure) => refused = Some(failure),
        }
    }
    if pid != 0 {
        let body = json!({ "addr": console, "pid": pid });
        ctx.post("/api/ps5/process/kill", &body).await?;
        return Ok(ToolOutput::ok(
            format!("Force-stopped {title_id} on {console} (process {pid})."),
            json!({ "title_id": title_id, "stopped": true, "how": "sigkill", "pid": pid }),
        ));
    }
    Err(refused.unwrap_or_else(|| {
        ToolOutput::error(format!(
            "{title_id} runs on {console}, but its process gives nothing to close it by."
        ))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_titles_own_game_processes_are_targets() {
        let list = vec![
            json!({"pid": 1, "kind": "app", "title_id": "PPSA00001", "app_id": 7}),
            json!({"pid": 2, "kind": "app", "title_id": "PPSA00002"}),
            json!({"pid": 3, "kind": "system", "title_id": "PPSA00001"}),
            json!({"pid": 4, "kind": "app", "title_id": "PPSA00001", "is_self": true}),
            json!({"pid": 5, "kind": "app", "title_id": ""}),
        ];
        let pids = |id: &str| -> Vec<u64> {
            game_processes(&list, id)
                .map(|p| p["pid"].as_u64().unwrap())
                .collect()
        };
        assert_eq!(pids("PPSA00001"), [1]);
        assert_eq!(pids("ppsa00001"), [1]);
        assert_eq!(pids("PPSA00002"), [2]);
        assert!(pids("").is_empty());
    }
}
