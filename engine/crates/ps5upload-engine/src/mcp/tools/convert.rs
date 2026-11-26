//! Convert's checks and its part on the console: `convert_archive_free_check`,
//! `convert_ampr_emu_to_console`.
//!
//! A conversion itself (`/api/fpkg/build`, `/api/fpkg/extract`, …) is a long job with many
//! choices: `api_describe` and `api_request` reach it.

use serde_json::json;

use super::route::{route, text};
use super::{console_required, object, Kind, ToolDef};

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "convert_archive_free_check",
            title: "Can an archive free its space",
            description: "Whether a .zip, .7z or .rar on the engine's computer can be unpacked \
                          with its own space given back as it unpacks (`free_archive` on \
                          /api/fpkg/extract, which DELETES the archive), and how: `mode` is \
                          entries (.zip, file by file), blocks (.7z, block by block), parts \
                          (multi-part .rar, part by part) or end (deleted once unpacked and \
                          checked). `needed` and `free` are bytes in the output folder. Changes \
                          nothing; an error says why it can't be done, including too little room.",
            kind: Kind::Read,
            schema: object(
                json!({
                    "source": text(
                        "The archive (for a multi-part .rar, its first part), on the computer \
                         the engine runs on.",
                    ),
                    "output_dir": text(
                        "Where Convert would unpack it. Default: Convert's output folder.",
                    ),
                    "password": text("A .rar's password, if it has one. Never logged."),
                }),
                &["source"],
            ),
            run: route!(
                method: "POST",
                path: "/api/fpkg/extract/free-check",
                done: "How the archive would be freed.",
            ),
        },
        ToolDef {
            name: "convert_ampr_emu_to_console",
            title: "Put the AMPR emulator on a console",
            description: "Writes the AMPR emulator (ampr_emu, the release the app installs into \
                          games, digest checked) to the console under \
                          /data/ps5upload/convert/runtime/, so Convert on the PS5 can keep an \
                          emulator when it unpacks asset packs. Only when the user asked for it. \
                          Answers `{version, path, written}`; `written: false` when that version \
                          was already there. Needs the helper running.",
            kind: Kind::Destructive { idempotent: true },
            schema: object(json!({ "console": console_required() }), &["console"]),
            run: route!(
                method: "POST",
                path: "/api/ps5/convert-ampr-emu",
                console: "host",
                done: "The AMPR emulator is on the console.",
            ),
        },
    ]
}
