//! A console's hardware: `hw_info`, `hw_temps`, `hw_storage`, `hw_power`, `hw_fan_threshold`.

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
