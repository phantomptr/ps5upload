//! Share (`share_*`): an app's things to a friend through a link they open, each friend allowed
//! by the user (share/mod.rs). `share_status` only looks, and never shows a link (an agent that
//! may only look gets no way to hand one out); making a share or allowing a friend sends the
//! user's files to someone else, so those tools are marked to be confirmed.

use serde_json::json;

use super::route::{flag, route, text, whole};
use super::{object, Kind, ToolDef};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "share_status",
            title: "Shares",
            description: "What is being shared and received: each share's state (open, used, \
                          expired, cancelled), its items (names, sizes), when it expires, and \
                          its requests (who asked, for what, allowed or declined, bytes sent); \
                          what is being received; and the history (`dir`: sent or received). \
                          The links are left out (`link_hidden`): share_create gives the link \
                          when the user asks for one.",
            kind: Kind::Read,
            schema: object(json!({}), &[]),
            run: route!(path: "/api/share", done: "Shares."),
        },
        ToolDef {
            name: "share_create",
            title: "Share with a friend",
            description: "Makes a Share link for the user's friend: whoever opens it can ask \
                          for the items, and the user allows each one (share_decide). Apps are \
                          homebrew only: the engine refuses a retail game, its updates and \
                          add-ons in any format (403, code `retail`). Saves of any game, \
                          screenshots and clips, cheats, controls, stream settings and a note \
                          can go too. The answer has `url` (the link to give). Confirm with the \
                          user what is shared, and give the link only to whom they say.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "from": text("The user's name as the friend sees it."),
                    "title": text("The game or app the share is about."),
                    "title_id": text("Its Title ID."),
                    "expires_in_s": whole("Seconds the link works: 3600 to 604800; 86400 when left out."),
                    "one_use": flag("One friend only (the default)."),
                    "items": {
                        "type": "array",
                        "description": "What to share. Each: `kind` (app, save, media, cheats, \
                                        controls, stream, note) and, by kind, `console` with \
                                        `path` (an app folder or a cheat file on the console), \
                                        `folder` (a save folder), `paths` (screenshots and \
                                        clips), or a `path` on this computer; `data` (controls, \
                                        stream settings, which cheats are on); `text` (a note).",
                        "items": { "type": "object" },
                    },
                }),
                &["items"],
            ),
            run: route!(method: "POST", path: "/api/share", done: "Shared."),
        },
        ToolDef {
            name: "share_decide",
            title: "Answer a friend's request",
            description: "Allows or declines a friend asking for a share's items \
                          (share_status lists the requests: `name`, `wants_names`). Allowing \
                          sends those files to them at once: ask the user.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "id": text("The share's id, from share_status."),
                    "request": whole("The request's number, from share_status."),
                    "allow": flag("True to send it, false to decline."),
                }),
                &["id", "request", "allow"],
            ),
            run: route!(method: "POST", path: "/api/share/{id}/decide", done: "Answered."),
        },
        ToolDef {
            name: "share_cancel",
            title: "Cancel a share",
            description: "Stops a share: its link stops working and any transfer ends.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(
                json!({ "id": text("The share's id, from share_status.") }),
                &["id"],
            ),
            run: route!(method: "DELETE", path: "/api/share/{id}", done: "Cancelled."),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// share_status is for agents that may only look; making a share, allowing a friend and
    /// cancelling are asked of the user, and an agent that may only look doesn't get them.
    #[test]
    fn only_share_status_is_a_read() {
        let tools = tools();
        let tool = |name: &str| tools.iter().find(|t| t.name == name).unwrap();
        assert_eq!(
            tool("share_status").kind.annotations()["readOnlyHint"],
            true
        );
        for name in ["share_create", "share_decide", "share_cancel"] {
            assert_eq!(
                tool(name).kind.annotations()["destructiveHint"],
                true,
                "{name}"
            );
        }
        let looking = super::super::list(true);
        let names: Vec<&str> = looking["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"share_status"));
        for name in ["share_create", "share_decide", "share_cancel"] {
            assert!(
                !names.contains(&name),
                "{name} offered to an agent that may only look"
            );
        }
    }
}
