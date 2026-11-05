//! Game saves: `saves_list`, `saves_backups_list`, `saves_backup_now`, `saves_restore`. Backups
//! are kept by the engine, on its computer.

use serde_json::json;

use super::files::{op_id_argument, wait_argument, OWN_OP_ID};
use super::route::{route, text, whole};
use super::{console_optional, console_required, object, Kind, ToolDef};

const USER_ID: &str = "The console user's `user_id`, from saves_list.";

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "saves_list",
            title: "Saves on a console",
            description: "The game saves on a console: `title_id`, `user_id`, `path`, `size` and \
                          `kind` of each.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "console": console_optional(),
                    "user_id": whole("Only this user's saves. Left out or 0, every user's."),
                }),
                &[],
            ),
            run: route!(path: "/api/ps5/saves/list", done: "The saves on the console."),
        },
        ToolDef {
            name: "saves_backups_list",
            title: "Save backups of a game",
            description: "The backups this engine keeps of one game's saves, newest first. \
                          Nothing here touches a console.",
            kind: Kind::Read,
            schema: object(
                json!({ "title_id": text("The title's id, like PPSA01234.") }),
                &["title_id"],
            ),
            run: route!(path: "/api/save-backup/list", done: "The game's backups."),
        },
        ToolDef {
            name: "saves_backup_now",
            title: "Back up a save",
            description: "Backs up one user's save of a game now, to the engine's computer. It \
                          can take a while: past `wait_s` this answers with an op_id for \
                          files_op_status.",
            kind: Kind::Write { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "title_id": text("The title's id, like PPSA01234."),
                    "user_id": whole(USER_ID),
                    "op_id": op_id_argument(OWN_OP_ID),
                    "wait_s": wait_argument(),
                }),
                &["console", "title_id", "user_id"],
            ),
            run: route!(
                method: "POST",
                path: "/api/save-backup/now",
                long: true,
                done: "Backed up the save.",
            ),
        },
        ToolDef {
            name: "saves_restore",
            title: "Restore a save backup",
            description: "Puts a backup back on a console, replacing that save as it is now. The \
                          game must not be running. Confirm with the user first. It can take a \
                          while: past `wait_s` this answers with an op_id for files_op_status.",
            kind: Kind::Destructive { idempotent: false },
            schema: object(
                json!({
                    "console": console_required(),
                    "title_id": text("The title's id, like PPSA01234."),
                    "file": text("The backup's file, as saves_backups_list shows it."),
                    "save_path": text("The save's `path` on the console, from saves_list."),
                    "user_id": whole(USER_ID),
                    "op_id": op_id_argument(OWN_OP_ID),
                    "wait_s": wait_argument(),
                }),
                &["console", "title_id", "file", "save_path", "user_id"],
            ),
            run: route!(
                method: "POST",
                path: "/api/save-backup/restore",
                long: true,
                done: "Restored the save.",
            ),
        },
    ]
}
