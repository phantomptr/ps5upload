//! A console's power: `power_control`, `power_wake`, `power_telemetry`.

use serde_json::{json, Value};

use super::route::route;
use super::{
    console_optional, console_required, consoles, object, without_nulls, Args, Kind, ToolCtx,
    ToolDef, ToolOutput, ToolResult,
};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "power_control",
            title: "Reboot, shut down or rest a console",
            description: "Reboots a console, shuts it down, or puts it in rest mode (`standby`). \
                          Whatever runs on it stops, and the PS5Upload helper with it: after a \
                          reboot or shutdown it has to be loaded again before this server can \
                          reach the console. Only when the user asks; confirm the console first.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "action": {
                        "type": "string",
                        "enum": ["reboot", "shutdown", "standby"],
                        "description": "What to do: reboot, shutdown, or standby (rest mode).",
                    },
                }),
                &["console", "action"],
            ),
            run: |ctx, args| Box::pin(control(ctx, args)),
        },
        ToolDef {
            name: "power_wake",
            title: "Wake a console",
            description: "Wakes a console from rest mode over the network. The console does not \
                          answer, so success only means the request was sent: check with \
                          consoles_status after a while (the helper does not always survive rest \
                          mode). It needs \"Enable Remote Play\" on the console and its user \
                          credential.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "credential": {
                        "type": "string",
                        "description": "The console's user credential, from pairing it with the \
                                        PS Remote Play app. Left out, the one saved in the \
                                        self-hosted web UI's console list is used, if any.",
                    },
                }),
                &["console"],
            ),
            run: |ctx, args| Box::pin(wake(ctx, args)),
        },
        ToolDef {
            name: "power_telemetry",
            title: "Console power history",
            description: "A console's lifetime records: operating time, boot cycles, thermal \
                          alerts and what last powered it up. Do not poll it.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: route!(path: "/api/ps5/power/telemetry", done: "The console's power history."),
        },
    ]
}

async fn control(ctx: ToolCtx, args: Args) -> ToolResult {
    let console = args.required("console")?;
    let action = args.required("action")?;
    let answer = ctx
        .post(
            "/api/ps5/power/control",
            &json!({ "addr": console, "action": action }),
        )
        .await?;
    Ok(ToolOutput::ok(
        format!("Asked {console} to {action}."),
        answer,
    ))
}

async fn wake(ctx: ToolCtx, args: Args) -> ToolResult {
    let console = args.required("console")?;
    let credential = match args.text("credential") {
        Some(credential) => Some(credential.to_string()),
        None => saved_credential(&ctx, console).await,
    };
    let body = without_nulls(json!({ "host": console, "credential": credential }));
    let answer = ctx.post("/api/ps5/power/wake", &body).await?;
    Ok(ToolOutput::ok(
        format!(
            "The wake request went to {console}. It does not confirm: give it a moment, then \
             check with consoles_status."
        ),
        answer,
    ))
}

/// The credential the web UI's console list holds for this console. Used, never shown.
async fn saved_credential(ctx: &ToolCtx, console: &str) -> Option<String> {
    let wanted = crate::console_snapshot::host_key(console);
    consoles::saved_profiles(&ctx.state, &ctx.caller)
        .await
        .iter()
        .find(|profile| {
            profile
                .get("host")
                .and_then(Value::as_str)
                .is_some_and(|host| crate::console_snapshot::host_key(host) == wanted)
        })
        .and_then(|profile| profile.get("wake_credential")?.as_str())
        .filter(|credential| !credential.is_empty())
        .map(str::to_string)
}
