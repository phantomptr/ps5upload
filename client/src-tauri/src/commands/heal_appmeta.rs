//! Game metadata healing — restore missing /user/appmeta/<TID>/ entries.
//!
//! Why: when SMP mounts a game image, the PS5 home-screen looks for
//! the title's icon0.png, param.json, and snd0.at9 under
//! `/user/appmeta/<TITLE_ID>/`. If those aren't there (older SMP
//! versions, deleted by user, never staged), the tile shows a blank
//! square or no name. SMP's own `update_snd0info` fix addresses the
//! music side; this command does the visual side.
//!
//! Strictly idempotent — we only copy files that aren't already in
//! place; existing appmeta entries are left untouched (their content
//! might be deliberately customised).
//!
//! Read-only-ish: we write under /user/appmeta but only what the
//! PS5 expects there for that title. We never touch the game
//! source folder or any other on-console state.

use std::time::Duration;

use serde::Serialize;

use ps5upload_core::fs_ops::{
    fs_copy_with_timeout, fs_mkdir, fs_read_with_timeout, list_dir_with_timeout, ListDirOptions,
};

/// Per-call deadline. FS_COPY of an icon0.png (~256 KB typical) plus
/// its accompanying param.json (~4 KB) finishes in well under 5s on
/// a healthy LAN; this caps the worst case where the PS5 is
/// thrashing under another workload.
const RPC_TIMEOUT: Duration = Duration::from_secs(15);

/// Files we ever copy. Order matches the home-screen's load order —
/// icon0 first so the tile renders before the metadata kicks in.
/// snd0.at9 is optional (silent menu loops are fine if absent), but
/// we copy it when present so the menu plays the game's theme.
const HEAL_FILES: &[&str] = &["icon0.png", "param.json", "param.sfo", "snd0.at9"];

/// Result row per file we considered.
#[derive(Serialize)]
pub struct HealOutcome {
    file: String,
    /// One of: "copied", "already_present", "missing_from_source",
    /// "skipped_no_source", "error".
    status: String,
    /// Set when status == "error".
    error: Option<String>,
}

#[derive(Serialize)]
pub struct HealResult {
    title_id: String,
    appmeta_dir: String,
    source_dir: String,
    /// Per-file outcomes in HEAL_FILES order. UI renders them as a
    /// table so the user can see exactly what changed.
    outcomes: Vec<HealOutcome>,
    /// Pre-computed counts so the renderer doesn't have to filter the
    /// outcomes vec.
    copied: u32,
    already_present: u32,
    errors: u32,
}

/// Heal /user/appmeta/<TITLE_ID>/ for a single title.
///
/// `addr` — management-port address ("ip:9114").
/// `title_id` — e.g. "CUSA12345" or "PPSA01234".
/// `source_path` — absolute path on the PS5 to the game folder
///                 containing `sce_sys/`. Caller can pass either
///                 the root (we'll look for sce_sys/) or directly
///                 the sce_sys/ path.
#[tauri::command]
pub async fn heal_appmeta(
    addr: String,
    title_id: String,
    source_path: String,
) -> Result<HealResult, String> {
    tokio::task::spawn_blocking(move || run_heal(&addr, &title_id, &source_path))
        .await
        .map_err(|e| format!("heal_appmeta task: {e}"))?
}

/// Strict shape check for PS5 title IDs. Sony's allocator hands out
/// 9-character codes like CUSAxxxxx (Sony first-party), PPSAxxxxx
/// (homebrew/dev), CUSWxxxxx (regional variants). Length 9 is the
/// canonical case. We allow 4-16 chars of the same alphabet so future
/// shapes don't trip us up — but the alphabet itself is locked down so
/// renderer-supplied `../downloads` (or `..`) can't escape into a path
/// like `/user/appmeta/../downloads/icon0.png`.
fn is_valid_title_id(s: &str) -> bool {
    let len = s.len();
    if !(4..=16).contains(&len) {
        return false;
    }
    s.bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn run_heal(addr: &str, title_id: &str, source_path: &str) -> Result<HealResult, String> {
    if !is_valid_title_id(title_id) {
        return Err("title_id must be 4-16 chars of [A-Z0-9_] (e.g. CUSA00001)".into());
    }
    if source_path.is_empty() {
        return Err("source_path is empty".into());
    }
    // Normalise source: if the caller handed us the game root, append
    // sce_sys; if they handed us sce_sys directly, leave it. This
    // makes both `app_register`'s convention and a raw drag-drop work.
    let sce_sys = if source_path.trim_end_matches('/').ends_with("sce_sys") {
        source_path.trim_end_matches('/').to_string()
    } else {
        format!("{}/sce_sys", source_path.trim_end_matches('/'))
    };
    let appmeta_dir = format!("/user/appmeta/{title_id}");

    // Make sure /user/appmeta/<TID>/ exists. The payload's FS_MKDIR
    // handler silently treats EEXIST as success and returns
    // FS_MKDIR_ACK, so any Err here is a real failure (permission,
    // out of inodes, etc.) that the caller needs to see.
    //
    // The previous code did a `s.contains("EEXIST") || s.contains(
    // "File exists")` check that was dead — payload-side already
    // swallowed EEXIST so we never actually saw it in this path. If
    // the payload ever drops the EEXIST suppression, callers will
    // start failing visibly, which is the correct behavior.
    fs_mkdir(addr, &appmeta_dir).map_err(|e| format!("mkdir {appmeta_dir}: {e}"))?;

    // List what's already in /user/appmeta/<TID>/ so we don't
    // overwrite the user's manual customisations.
    let existing: std::collections::HashSet<String> = match list_dir_with_timeout(
        addr,
        &appmeta_dir,
        ListDirOptions::default(),
        Some(RPC_TIMEOUT),
    ) {
        Ok(listing) => listing
            .entries
            .iter()
            .map(|e| e.name.to_ascii_lowercase())
            .collect(),
        Err(_) => std::collections::HashSet::new(),
    };

    let mut outcomes: Vec<HealOutcome> = Vec::with_capacity(HEAL_FILES.len());
    let mut copied = 0u32;
    let mut already_present = 0u32;
    let mut errors = 0u32;

    for file in HEAL_FILES {
        let dst = format!("{appmeta_dir}/{file}");
        if existing.contains(&file.to_ascii_lowercase()) {
            outcomes.push(HealOutcome {
                file: (*file).to_string(),
                status: "already_present".into(),
                error: None,
            });
            already_present += 1;
            continue;
        }
        let src = format!("{sce_sys}/{file}");
        // Probe the source via fs_read with limit=1 — cheaper than
        // listing the parent dir per file, gives us "exists?" in one
        // round-trip.
        if fs_read_with_timeout(addr, &src, 0, 1, Some(RPC_TIMEOUT)).is_err() {
            outcomes.push(HealOutcome {
                file: (*file).to_string(),
                status: "missing_from_source".into(),
                error: None,
            });
            continue;
        }
        match fs_copy_with_timeout(addr, &src, &dst, Some(RPC_TIMEOUT)) {
            Ok(()) => {
                outcomes.push(HealOutcome {
                    file: (*file).to_string(),
                    status: "copied".into(),
                    error: None,
                });
                copied += 1;
            }
            Err(e) => {
                outcomes.push(HealOutcome {
                    file: (*file).to_string(),
                    status: "error".into(),
                    error: Some(e.to_string()),
                });
                errors += 1;
            }
        }
    }

    Ok(HealResult {
        title_id: title_id.to_string(),
        appmeta_dir,
        source_dir: sce_sys,
        outcomes,
        copied,
        already_present,
        errors,
    })
}
