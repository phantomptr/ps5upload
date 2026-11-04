//! The messages PS5Upload put on a console's screen: `notifications_list`.

use serde_json::{json, Value};

use super::{console_optional, object, Args, Kind, ToolCtx, ToolDef, ToolOutput, ToolResult};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![ToolDef {
        name: "notifications_list",
        title: "Console notifications",
        description: "The messages PS5Upload's helper has put on a console's screen, kept on the \
                      console: `seq`, time, level and text. Not Sony's own notifications, which \
                      cannot be read.",
        kind: Kind::Read,
        schema: object(
            json!({
                "console": console_optional(),
                "since_seq": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Only messages newer than this `seq`.",
                },
            }),
            &[],
        ),
        run: |ctx, args| Box::pin(list(ctx, args)),
    }]
}

async fn list(ctx: ToolCtx, args: Args) -> ToolResult {
    let mut query = args.console_query();
    if let Some(since) = args.number("since_seq") {
        query.push(("since_seq", since.to_string()));
    }
    let answer = ctx.get("/api/ps5/notif/list", &query).await?;
    let count = answer
        .get("notifications")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    Ok(ToolOutput::ok(format!("{count} notification(s)."), answer))
}
