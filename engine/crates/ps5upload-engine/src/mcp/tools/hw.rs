//! A console's hardware: `hw_info`, `hw_temps`, `hw_storage`, `hw_power`, `hw_fps`,
//! `hw_fps_capture`, `hw_fan_threshold`.

use serde_json::{json, Value};

use super::route::{route, whole};
use super::{
    console_optional, console_required, gib, object, Args, Kind, ToolCtx, ToolDef, ToolOutput,
    ToolResult,
};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "hw_info",
            title: "Console hardware",
            description: "A console's model, serial, firmware, processor count and memory.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: |ctx, args| Box::pin(info(ctx, args)),
        },
        ToolDef {
            name: "hw_temps",
            title: "Console temperatures",
            description: "A console's temperatures and clocks. The basic read is safe to repeat; \
                          `extended` and `clocks` ask more of the console, so set them only when \
                          the user wants those readings, and do not poll them.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "console": console_optional(),
                    "extended": {
                        "type": "boolean",
                        "description": "Also read SoC power, processor use and fan duty.",
                    },
                    "clocks": {
                        "type": "boolean",
                        "description": "Also read the graphics and per-core processor clocks.",
                    },
                }),
                &[],
            ),
            run: |ctx, args| Box::pin(temps(ctx, args)),
        },
        ToolDef {
            name: "hw_storage",
            title: "Console storage",
            description: "A console's internal storage as PS5 Settings counts it: total, free and \
                          used bytes, with each partition. External drives are listed by the API's \
                          /api/ps5/volumes.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: |ctx, args| Box::pin(storage(ctx, args)),
        },
        ToolDef {
            name: "hw_power",
            title: "Console power use",
            description: "A console's power use now, load, how long it has run and how many times \
                          it has started.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: route!(path: "/api/ps5/hw/power", done: "The console's power readings."),
        },
        ToolDef {
            name: "hw_fps",
            title: "Game frame rate",
            description:
                "The running game's frame rate, sampled: the frames the console's display \
                          showed over the last second or so, not per-frame times. The first call \
                          starts sampling (`state` \"starting\"); ask again after two seconds. \
                          `state` \"no_game\" means no game picture to count. `capture` says \
                          whether a capture runs; its rows are GET /api/ps5/hw/fps/capture.",
            kind: Kind::Read,
            schema: object(json!({ "console": console_optional() }), &[]),
            run: |ctx, args| Box::pin(fps(ctx, args)),
        },
        ToolDef {
            name: "hw_fps_capture",
            title: "Start or stop a frame rate capture",
            description: "Starts a capture of the running game's sampled frame rate (one row a \
                          second, with the console's temperatures every few seconds; a new one \
                          replaces the last), or stops it. The result has min, average, max and \
                          1% low (of one-second samples). The CSV is GET \
                          /api/ps5/hw/fps/capture/csv. Only when the user asks for a capture.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "action": {
                        "type": "string",
                        "enum": ["start", "stop"],
                        "description": "start a new capture, or stop the running one.",
                    },
                }),
                &["console", "action"],
            ),
            run: |ctx, args| Box::pin(fps_capture(ctx, args)),
        },
        ToolDef {
            name: "hw_fan_threshold",
            title: "Set the fan threshold",
            description: "Sets the temperature at which a console's fan speeds up. Too high a \
                          value lets it run hot: only as the user asked.",
            kind: Kind::Write { idempotent: true },
            schema: object(
                json!({
                    "console": console_required(),
                    "threshold_c": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 255,
                        "description": "The temperature in °C.",
                    },
                    "reapply_sec": whole(
                        "How often to apply it again, 1 to 300 seconds. Left out, as it is.",
                    ),
                }),
                &["console", "threshold_c"],
            ),
            run: route!(
                method: "POST",
                path: "/api/ps5/hw/fan-threshold",
                done: "Set the fan threshold.",
            ),
        },
    ]
}

async fn info(ctx: ToolCtx, args: Args) -> ToolResult {
    let answer = ctx.get("/api/ps5/hw/info", &args.console_query()).await?;
    let text = |name: &str| {
        answer
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    };
    let summary = format!("Model {}, {}.", text("model"), text("os"));
    Ok(ToolOutput::ok(summary, answer))
}

async fn temps(ctx: ToolCtx, args: Args) -> ToolResult {
    let mut query = args.console_query();
    for name in ["extended", "clocks"] {
        if args.flag(name) == Some(true) {
            query.push((name, "1".to_string()));
        }
    }
    let answer = ctx.get("/api/ps5/hw/temps", &query).await?;
    let number = |name: &str| answer.get(name).and_then(Value::as_i64).unwrap_or(0);
    let summary = format!(
        "Processor {} °C, SoC {} °C.",
        number("cpu_temp"),
        number("soc_temp")
    );
    Ok(ToolOutput::ok(summary, answer))
}

/// One line for a frame rate answer (live or capture).
fn fps_summary(answer: &Value) -> String {
    let state = answer.get("state").and_then(Value::as_str).unwrap_or("");
    let live = match (state, answer.get("fps").and_then(Value::as_f64)) {
        (_, Some(fps)) => format!("{fps:.0} FPS (sampled)."),
        ("no_game", _) => "No game picture to count.".to_string(),
        ("unsupported", _) => "The helper on this console is too old for this.".to_string(),
        ("offline", _) => "The console did not answer.".to_string(),
        _ => "Sampling has started; ask again in two seconds.".to_string(),
    };
    let capture = &answer["capture"];
    let rows = capture.get("samples").and_then(Value::as_u64).unwrap_or(0);
    let stat = |name: &str| capture["stats"][name].as_f64().unwrap_or(0.0);
    if capture["active"].as_bool() == Some(true) {
        format!("{live} Capturing: {rows} samples.")
    } else if rows > 0 {
        format!(
            "{live} Last capture: {rows} samples, min {:.0}, average {:.1}, 1% low {:.1}, max {:.0}.",
            stat("min_fps"),
            stat("avg_fps"),
            stat("low_1pct_fps"),
            stat("max_fps")
        )
    } else {
        live
    }
}

async fn fps(ctx: ToolCtx, args: Args) -> ToolResult {
    let answer = ctx.get("/api/ps5/hw/fps", &args.console_query()).await?;
    Ok(ToolOutput::ok(fps_summary(&answer), answer))
}

async fn fps_capture(ctx: ToolCtx, args: Args) -> ToolResult {
    let console = args.required("console")?;
    let path = match args.required("action")? {
        "start" => "/api/ps5/hw/fps/capture/start",
        "stop" => "/api/ps5/hw/fps/capture/stop",
        other => return Err(ToolOutput::error(format!("Unknown action `{other}`."))),
    };
    let answer = ctx.post(path, &json!({ "addr": console })).await?;
    Ok(ToolOutput::ok(fps_summary(&answer), answer))
}

async fn storage(ctx: ToolCtx, args: Args) -> ToolResult {
    let answer = ctx
        .get("/api/ps5/hw/storage", &args.console_query())
        .await?;
    let number = |name: &str| answer.get(name).and_then(Value::as_u64).unwrap_or(0);
    let summary = format!(
        "{} free of {}.",
        gib(number("free_bytes")),
        gib(number("total_bytes"))
    );
    Ok(ToolOutput::ok(summary, answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_rate_summary_says_sampled_and_what_is_missing() {
        let live = json!({"state": "ok", "fps": 59.6, "capture": {"active": false, "samples": 0}});
        assert_eq!(fps_summary(&live), "60 FPS (sampled).");
        let none = json!({"state": "no_game", "fps": null, "capture": {"active": false}});
        assert_eq!(fps_summary(&none), "No game picture to count.");
        let busy = json!({"state": "ok", "fps": 30.0, "capture": {"active": true, "samples": 12}});
        assert_eq!(
            fps_summary(&busy),
            "30 FPS (sampled). Capturing: 12 samples."
        );
        let done = json!({"state": "starting", "fps": null, "capture": {"active": false, "samples": 3,
            "stats": {"min_fps": 48.0, "avg_fps": 57.25, "low_1pct_fps": 48.0, "max_fps": 60.0}}});
        assert_eq!(
            fps_summary(&done),
            "Sampling has started; ask again in two seconds. Last capture: 3 samples, min 48, \
             average 57.2, 1% low 48.0, max 60."
        );
    }
}
