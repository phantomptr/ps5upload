//! Sharing a PS5 save with another account, and moving one to another account on the same
//! console (#612).
//!
//! A PS5 save image is encrypted with a key sealed for the console that made it: the console
//! unseals it itself, no account involved. The account it belongs to is data: its param.sfo
//! (ACCOUNT_ID), the image's own parameters (user id, HMAC) and the console's save database. So a
//! save copied into another user's folder as it is (even with the account ID changed) does not
//! load as theirs, and an encrypted backup from another PS5 does not open at all. What does carry
//! over is what is inside it: the game's own files. So a save is shared as those files, opened decrypted on the console
//! with its sce_sys left out (that is the owner's account information), and used by opening the
//! other account's own save of the same game, putting those files in it and closing it, which the
//! console encrypts again as theirs.
//!
//! A game's save is often several images in its folder (`sdimg_STARBOARD`, `sdimg_sce_sdmemory`,
//! …), and the console keeps its own backup copy of each (`sdimg_sce_bu_<name>`). They go
//! together: every image of the game (never the console's backup copies) is in the one file, as a
//! folder of its own, and each goes into the target's image of the same name. A target missing
//! one of those images is refused before anything changes, naming what is missing: a game makes
//! the same images for every account once it has saved, so this asks for one more start of the
//! game at most, while putting in only some parts could leave a save the game can't read.
//!
//! - **Share** (`POST /api/save-backup/share`): the file.
//! - **Use a shared save** (`POST /api/save-backup/use-shared`): a file into a user's save.
//! - **Copy** (`POST /api/save-backup/copy`): share and use in one go, between two users of the
//!   same console, with no file through the computer's Downloads.
//! - **Restore to another user** (`POST /api/save-backup/restore-to-user`): a backup holds the
//!   encrypted images, which only the console can open. The target's save is backed up, the
//!   backup put into the target's folder as it is, shared from there, the target's own save put
//!   back, and the shared files used in it.
//!
//! The target's whole save folder is backed up first and put back, every image of it, if any
//! step fails; the game must not be running. Files shared before 10.6.0 (one image, the files at
//! the top of the folder) still load.
//!
//! The console work goes through [`Console`], so the order of the steps and every refusal can be
//! tested without a PS5; [`Live`] does it over the engine's own routes.

use std::path::{Path, PathBuf};

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::save_archive::{
    cleanup_temp_dir, make_temp_dir, restore_prepare, unzip_strict, zip_folder,
};
use ps5upload_core::saves::SaveEntry;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::save_backup::{
    self as local, is_console_backup_image, plain, BEFORE_SHARE_TAG, SHARE_MANIFEST, SYS_DIR,
};

/// The tag of the backup taken of a save before a backup of another user's replaced it.
pub const BEFORE_COPY_TAG: &str = "beforecopy";
/// The manifest `kind` of a shared save holding every image of the game, one folder each. A
/// version before 10.6.0 refuses it ("only PS5 saves can be shared") instead of misreading it.
pub const SHARE_KIND: &str = "ps5-saves";
/// The manifest `kind` of a shared save from before 10.6.0: one image's files.
const OLD_SHARE_KIND: &str = "ps5";

/// Why a step was refused; `missing` names the target's images that aren't there yet.
#[derive(Debug, PartialEq)]
pub struct Failed {
    pub msg: String,
    pub missing: Vec<String>,
}

impl From<String> for Failed {
    fn from(msg: String) -> Self {
        Failed {
            msg,
            missing: Vec::new(),
        }
    }
}

impl From<&str> for Failed {
    fn from(msg: &str) -> Self {
        msg.to_string().into()
    }
}

/// An image's name for people: `sdimg_STARBOARD` is "STARBOARD", and `sce_sdmemory` (the image
/// the console's save API makes for a game's one main save) "main save".
pub fn part_label(name: &str) -> String {
    let bare = name.strip_prefix("sdimg_").unwrap_or(name);
    if bare == "sce_sdmemory" {
        "main save".into()
    } else {
        bare.to_string()
    }
}

/// What the steps need from a console.
pub trait Console {
    /// Every save on the console.
    fn saves(&self) -> Result<Vec<SaveEntry>, String>;
    /// Whether `title_id` runs now.
    fn running(&self, title_id: &str) -> Result<bool, String>;
    /// A save is open in Save Data (one at a time).
    fn save_open(&self) -> bool;
    /// The save image names in a PS5 save folder, without the console's backup copies.
    fn images(&self, folder: &str) -> Result<Vec<String>, String>;
    /// The PS5's own identity (see `save_backup::console_identity`); None when it does not say.
    fn identity(&self) -> Option<String>;
    /// The size of the save image `image` (a file in a save folder): the most its files can
    /// ever hold, as the image never grows. None when it is not listed.
    fn image_size(&self, image: &str) -> Result<Option<u64>, String>;
    /// What the open save at `mount` holds in its sce_sys (kept when its files are replaced).
    fn sys_bytes(&self, mount: &str) -> Result<u64, String>;
    /// Err when the save there now can't be kept before it is replaced.
    fn can_back_up(&self) -> Result<(), String>;
    /// Backs up the save folder `entry`, every image of it (tagged `tag`); the backup zip.
    fn back_up(&self, entry: &SaveEntry, tag: &str) -> Result<PathBuf, String>;
    /// Whether a save is open now; Err when the console could not be asked.
    fn open_now(&self) -> Result<bool, String>;
    /// Opens the save image `image` decrypted (one at a time); its mount point.
    fn open_image(&self, image: &str) -> Result<String, String>;
    /// Copies the files of the open save at `mount` into the new local folder `dest`.
    fn copy_out(&self, mount: &str, dest: &Path) -> Result<(), String>;
    /// Empties the open save at `mount` but for sce_sys and fills it from `src`.
    fn fill(&self, mount: &str, src: &Path) -> Result<(), String>;
    /// One try at closing the open save (written back when it changed).
    fn close_once(&self) -> Result<Closed, String>;
    /// The wait between two tries at closing.
    fn pause(&self) {}
    /// Puts the folder `src` into the save folder `folder` as it is, without a backup first.
    fn put_raw(&self, title_id: &str, src: &Path, folder: &str, user_id: u32)
        -> Result<(), String>;
    /// Puts the backup zip `zip` back into `folder`, without a backup first.
    fn put_back(
        &self,
        title_id: &str,
        zip: &Path,
        folder: &str,
        user_id: u32,
    ) -> Result<(), String>;
}

/// What a close of the open save said.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Closed {
    /// A save was open (false: nothing was, it was closed already).
    pub was_open: bool,
}

/// How many times a close is tried before giving up (with [`Console::pause`] between).
const CLOSE_TRIES: usize = 5;

/// Closes the open save, trying again when a close fails: the helper can refuse one (still in
/// use, or the write-back failed and it keeps the edited copy for the next close) or the
/// connection can drop in the middle. Left open, the next open or close of any save would write
/// that copy over whatever is put in the folder meanwhile. `changed`: files were put in the save.
/// A close that worked only on a later try, finding nothing open, can't say the files were
/// written (the helper gives up on a second failed write-back and closes): an Err then.
fn close_save(c: &dyn Console, changed: bool) -> Result<(), String> {
    let mut first: Option<String> = None;
    let mut last = String::new();
    for i in 0..CLOSE_TRIES {
        if i > 0 {
            c.pause();
        }
        match c.close_once() {
            Ok(closed) => {
                return match first {
                    Some(e) if changed && !closed.was_open => Err(format!(
                        "closing the save failed ({e}), so its new files may not have been kept"
                    )),
                    _ => Ok(()),
                };
            }
            Err(e) => {
                crate::log_warn!("save copy: closing the save failed (try {}): {e}", i + 1);
                first.get_or_insert_with(|| e.clone());
                last = e;
            }
        }
    }
    Err(format!("the save could not be closed ({last})"))
}

/// Copies the files of the save image `image`, opened decrypted, into the new local folder
/// `dest`; closed again after.
fn read_image(c: &dyn Console, image: &str, dest: &Path) -> Result<(), String> {
    let mount = c.open_image(image)?;
    let copied = c.copy_out(&mount, dest);
    // Read only: closing it changes nothing on the console.
    let closed = close_save(c, false);
    copied?;
    closed
}

/// The bytes of every file under the local folder `dir`.
fn local_bytes(dir: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let e = e.map_err(|e| format!("{}: {e}", dir.display()))?;
        let t = e
            .file_type()
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        total += if t.is_dir() {
            local_bytes(&e.path())?
        } else {
            e.metadata().map(|m| m.len()).unwrap_or(0)
        };
    }
    Ok(total)
}

/// The refusal of a save too big for the image it goes into. A PS5 save image has a fixed size
/// (set when the game made it); a bigger one would fail part way through the upload.
fn too_small(title_id: &str, who: &str, need: u64, have: u64) -> String {
    use ps5upload_core::health::human_bytes;
    format!(
        "{title_id}'s save on {who} is too small for this save (needs {}, has {}). Start the game as {who}, make a bigger save if the game allows it, or use a save from the same game version.",
        human_bytes(need),
        human_bytes(have)
    )
}

/// Err (`too_small`) when `need` bytes can't fit the image `image` at all: its whole size, the
/// most it can ever hold. Asked before anything changes, without opening it.
fn fits_image(
    c: &dyn Console,
    title_id: &str,
    who: &str,
    image: &str,
    need: u64,
) -> Result<(), String> {
    match c.image_size(image)? {
        Some(size) if need > size => Err(too_small(title_id, who, need, size)),
        _ => Ok(()),
    }
}

/// Replaces the files in the save image `image`: opened, emptied but for sce_sys, filled from
/// `src` and closed (which the console encrypts as the folder owner's). Refused, closed again
/// with nothing deleted, when `src` is bigger than the image can hold besides its sce_sys.
fn replace_files(
    c: &dyn Console,
    title_id: &str,
    who: &str,
    image: &str,
    src: &Path,
) -> Result<(), String> {
    let need = local_bytes(src)?;
    let mount = c.open_image(image)?;
    let room = (|| {
        let Some(size) = c.image_size(image)? else {
            return Ok(());
        };
        let have = size.saturating_sub(c.sys_bytes(&mount)?);
        if need > have {
            return Err(too_small(title_id, who, need, have));
        }
        Ok(())
    })();
    if let Err(e) = room {
        // Nothing was changed: closing it changes nothing either.
        return match close_save(c, false) {
            Ok(()) => Err(e),
            Err(why) => Err(format!("{e} ({why})")),
        };
    }
    let filled = c.fill(&mount, src);
    let closed = close_save(c, true);
    filled?;
    closed
}

/// Puts the backup `zip` back into `folder` after a failed step; Err is the sentence telling the
/// person what to do instead. Never while a save is still open (or the console can't say): the
/// helper would write its copy over the save put back at the next open or close of any save.
fn undo(
    c: &dyn Console,
    title_id: &str,
    zip: &Path,
    folder: &str,
    user_id: u32,
) -> Result<(), String> {
    let file = zip
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut open = c.open_now();
    for _ in 1..CLOSE_TRIES {
        if open.is_ok() {
            break;
        }
        c.pause();
        open = c.open_now();
    }
    if open != Ok(false) {
        crate::log_warn!(
            "save copy: not putting {file} back into {folder}: a save may still be open ({open:?})"
        );
        return Err(format!(
            "A save is still open on the PS5, so the save there before was not put back: once the PS5 can be reached, restore {file} from Save Data › Restore."
        ));
    }
    c.put_back(title_id, zip, folder, user_id).map_err(|e| {
        format!(
            "Putting the save back failed too ({e}); restore {file} from the game's save backups."
        )
    })
}

fn running_error(title_id: &str) -> String {
    format!("{title_id} is running on the PS5. Close the game, then try again.")
}

const OPEN_ERROR: &str = "A save is open in Save Data. Close it first.";

/// The title of a PS5 save folder, `/user/home/<8 hex>/savedata_prospero/<TITLE>`.
fn title_of_folder(folder: &str) -> Result<String, String> {
    local::ps5_save_image(&format!("{}/x", folder.trim_end_matches('/')))
        .map(|(t, _)| t)
        .ok_or_else(|| format!("{folder} is not a PS5 save folder"))
}

/// The user a PS5 save folder belongs to (`/user/home/<8 hex>/...`): the one the save is put
/// back as. `asked`: the user the request named, refused when it is someone else.
fn user_of_folder(folder: &str, asked: Option<u32>) -> Result<u32, String> {
    title_of_folder(folder)?;
    let hex = folder
        .trim_start_matches("/user/home/")
        .split('/')
        .next()
        .unwrap_or_default();
    let user =
        u32::from_str_radix(hex, 16).map_err(|_| format!("{folder} is not a PS5 save folder"))?;
    match asked {
        Some(a) if a != user => Err(format!("{folder} is not user {a:08x}'s save folder")),
        _ => Ok(user),
    }
}

fn checks(c: &dyn Console, title_id: &str) -> Result<(), String> {
    if c.running(title_id)? {
        return Err(running_error(title_id));
    }
    if c.save_open() {
        return Err(OPEN_ERROR.into());
    }
    Ok(())
}

/// Every image of the save folder `folder`, as one shared-save zip: `<TITLE>/<image>/<files>`
/// for each, sce_sys left out, and a manifest naming them. The file's name with it.
pub fn share(c: &dyn Console, folder: &str) -> Result<(String, Vec<u8>), String> {
    let folder = folder.trim_end_matches('/');
    let title_id = title_of_folder(folder)?;
    checks(c, &title_id)?;
    let images = c.images(folder)?;
    if images.is_empty() {
        return Err(format!("{folder} has no save image"));
    }
    let tmp = make_temp_dir(&title_id)?;
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = (|| {
        let dir = tmp.join(&title_id);
        std::fs::create_dir_all(&dir).map_err(|e| format!("copy: {e}"))?;
        for name in &images {
            let dest = dir.join(name);
            read_image(c, &format!("{folder}/{name}"), &dest)?;
            let _ = std::fs::remove_dir_all(dest.join(SYS_DIR));
        }
        let manifest = json!({
            "kind": SHARE_KIND,
            "title_id": title_id,
            "images": images,
            "shared_at_ms": local::now_ms(),
            "app": env!("CARGO_PKG_VERSION"),
        });
        std::fs::write(dir.join(SHARE_MANIFEST), manifest.to_string())
            .map_err(|e| format!("manifest: {e}"))?;
        let zip = tmp.join("share.zip");
        zip_folder(&tmp, &title_id, &zip).map_err(|e| format!("zip: {e:#}"))?;
        let bytes = std::fs::read(&zip).map_err(|e| format!("zip: {e}"))?;
        let file = format!(
            "{title_id} save shared {}.zip",
            local::stamp(local::now_ms())
        );
        Ok((file, bytes))
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    result
}

/// One image's files in a shared save. `name` is None for a file from before 10.6.0 that did not
/// say which image it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub name: Option<String>,
    pub src: PathBuf,
    /// From a file of one image (before 10.6.0).
    pub old: bool,
}

/// An image name a shared save may carry: one folder, not a backup copy.
fn image_name_ok(n: &str) -> bool {
    !n.is_empty()
        && !n.starts_with('.')
        && !n.contains(['/', '\\'])
        && !n.ends_with(".bin")
        && !is_console_backup_image(n)
}

/// What a shared save unpacked under `unpacked` holds, checked before anything on the console
/// changes. Err says why it can't be used.
pub fn shared_parts(unpacked: &Path, title_id: &str) -> Result<Vec<Part>, String> {
    let dir = unpacked.join(title_id);
    let manifest: Value = std::fs::read(dir.join(SHARE_MANIFEST))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or(
            "this is not a shared save (no manifest): share it from Save Data on the other console",
        )?;
    if manifest.get("title_id").and_then(Value::as_str) != Some(title_id) {
        return Err(format!("this shared save is not for {title_id}"));
    }
    match manifest.get("kind").and_then(Value::as_str) {
        Some(OLD_SHARE_KIND) => {
            let name = manifest
                .get("image")
                .and_then(Value::as_str)
                .filter(|n| image_name_ok(n))
                .map(String::from);
            let src = local::shared_save_contents(unpacked, title_id)?;
            Ok(vec![Part {
                name,
                src,
                old: true,
            }])
        }
        Some(SHARE_KIND) => {
            let names: Vec<String> = manifest
                .get("images")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            if names.is_empty() {
                return Err("the shared save has no files".into());
            }
            let mut parts = Vec::new();
            for name in names {
                if !image_name_ok(&name) {
                    return Err(format!("the shared save names an odd part: {name}"));
                }
                let src = dir.join(&name);
                let _ = std::fs::remove_dir_all(src.join(SYS_DIR));
                let any = std::fs::read_dir(&src)
                    .map(|mut d| d.next().is_some())
                    .unwrap_or(false);
                if !any {
                    return Err(format!(
                        "the shared save's {} has no files",
                        part_label(&name)
                    ));
                }
                parts.push(Part {
                    name: Some(name),
                    src,
                    old: false,
                });
            }
            Ok(parts)
        }
        _ => Err("only PS5 saves can be shared".into()),
    }
}

/// Which of the target's `images` each part goes into: the one of the same name. A file from
/// before 10.6.0 (one image) also goes into `compat` (the image the caller picked) or the
/// target's only image. Err lists the parts the target doesn't have.
pub fn match_parts(
    parts: &[Part],
    images: &[String],
    compat: Option<&str>,
) -> Result<Vec<(String, PathBuf)>, Vec<String>> {
    let mut out = Vec::new();
    let mut missing = Vec::new();
    for p in parts {
        let same = p
            .name
            .as_deref()
            .and_then(|n| images.iter().find(|i| i.as_str() == n));
        let fallback = || {
            if !p.old {
                return None;
            }
            compat
                .and_then(|c| images.iter().find(|i| i.as_str() == c))
                .or(if images.len() == 1 {
                    images.first()
                } else {
                    None
                })
        };
        match same.or_else(fallback) {
            Some(i) => out.push((i.clone(), p.src.clone())),
            None => missing.push(p.name.clone().unwrap_or_else(|| "save".into())),
        }
    }
    if missing.is_empty() {
        Ok(out)
    } else {
        Err(missing)
    }
}

/// The error for parts a user's save doesn't have yet.
fn missing_error(title_id: &str, who: &str, missing: Vec<String>) -> Failed {
    let labels: Vec<String> = missing.iter().map(|m| part_label(m)).collect();
    Failed {
        msg: format!(
            "{title_id}'s {} isn't on {who} yet: start the game once as {who} and save, then try again.",
            labels.join(", ")
        ),
        missing,
    }
}

/// Put a shared save (`zip_bytes`) into the save folder `folder` of `user_id`: each image into
/// theirs of the same name, which the console then keeps as theirs. Their folder is backed up
/// first (tagged `beforeshare`), unless `backup` is one already taken, and put back, every image
/// of it, if a step fails. `compat`: the image a file from before 10.6.0 goes into. The images
/// replaced.
pub fn import(
    c: &dyn Console,
    folder: &str,
    user_id: u32,
    zip_bytes: &[u8],
    backup: Option<&Path>,
    compat: Option<&str>,
) -> Result<Vec<String>, Failed> {
    import_as(c, folder, user_id, "this user", zip_bytes, backup, compat)
}

/// `import`, `who` naming the user the folder belongs to in a refusal.
fn import_as(
    c: &dyn Console,
    folder: &str,
    user_id: u32,
    who: &str,
    zip_bytes: &[u8],
    backup: Option<&Path>,
    compat: Option<&str>,
) -> Result<Vec<String>, Failed> {
    let folder = folder.trim_end_matches('/');
    let title_id = title_of_folder(folder)?;
    if backup.is_none() {
        c.can_back_up()?;
    }
    let tmp = make_temp_dir(&title_id)?;
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = (|| {
        let zip = tmp.join("shared.zip");
        std::fs::write(&zip, zip_bytes).map_err(|e| format!("save the file: {e}"))?;
        let unpacked = tmp.join("x");
        std::fs::create_dir_all(&unpacked).map_err(|e| format!("unpack: {e}"))?;
        unzip_strict(&zip, &unpacked, &title_id)
            .map_err(|e| format!("this file is not a shared save of {title_id}: {e:#}"))?;
        let parts = shared_parts(&unpacked, &title_id)?;
        let pairs = match_parts(&parts, &c.images(folder)?, compat)
            .map_err(|m| missing_error(&title_id, who, m))?;
        checks(c, &title_id)?;
        // A part too big for its image is refused before anything changes.
        for (image, src) in &pairs {
            fits_image(
                c,
                &title_id,
                who,
                &format!("{folder}/{image}"),
                local_bytes(src)?,
            )?;
        }
        // Theirs first, so this can be undone.
        let backup = match backup {
            Some(b) => b.to_path_buf(),
            None => {
                let theirs = SaveEntry {
                    title_id: title_id.clone(),
                    user_id: user_id as i32,
                    path: folder.to_string(),
                    size: 0,
                    mtime: 0,
                    kind: "ps5".into(),
                };
                c.back_up(&theirs, BEFORE_SHARE_TAG)
                    .map_err(|e| format!("could not back up the save there first: {e}"))?
            }
        };
        let replaced = pairs.iter().try_for_each(|(image, src)| {
            replace_files(c, &title_id, who, &format!("{folder}/{image}"), src)
                .map_err(|e| format!("{}: {e}", part_label(image)))
        });
        match replaced {
            Ok(()) => {
                crate::log_info!(
                    "save backup: shared save of {title_id} put into {folder} ({} parts)",
                    pairs.len()
                );
                Ok(pairs.into_iter().map(|(i, _)| i).collect())
            }
            Err(why) => Err(match undo(c, &title_id, &backup, folder, user_id) {
                Ok(()) => format!("{why}. The save there before was put back."),
                Err(then) => format!("{why}. {then}"),
            }
            .into()),
        }
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    result
}

/// The PS5 save of `title_id` owned by `user`, from the console's list. Err says why there is
/// none (`who` names the user in it).
fn ps5_save_of<'a>(
    saves: &'a [SaveEntry],
    title_id: &str,
    user: u32,
    who: &str,
) -> Result<&'a SaveEntry, String> {
    let all: Vec<&SaveEntry> = saves
        .iter()
        .filter(|s| s.title_id.eq_ignore_ascii_case(title_id) && s.user_id as u32 == user)
        .collect();
    if let Some(s) = all.iter().find(|s| s.kind == "ps5") {
        return Ok(*s);
    }
    if !all.is_empty() {
        return Err(format!(
            "{who}'s save of {title_id} is not a PS5 save: only PS5 saves can be copied"
        ));
    }
    Err(format!("{who} has no save of {title_id}"))
}

/// The error when the user a save goes to has none of the game yet.
fn no_target_save(title_id: &str) -> String {
    format!(
        "The other user has no save of {title_id} yet. Start the game once as that user and save, then try again."
    )
}

fn target_save<'a>(
    saves: &'a [SaveEntry],
    title_id: &str,
    user: u32,
) -> Result<&'a SaveEntry, String> {
    ps5_save_of(saves, title_id, user, "The other user").map_err(|e| {
        if e.ends_with(&format!("has no save of {title_id}")) {
            no_target_save(title_id)
        } else {
            e
        }
    })
}

/// Parts in `from` that `to` lacks (names), each image matched by name.
fn lacking(from: &[String], to: &[String]) -> Vec<String> {
    from.iter().filter(|f| !to.contains(f)).cloned().collect()
}

#[derive(Debug, Clone, Deserialize)]
pub struct CopyBody {
    pub addr: String,
    pub title_id: String,
    pub from_user: u32,
    pub to_user: u32,
}

/// What a copy or restore put where: the source (a folder or a backup file), the target folder
/// and the images replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct Done {
    pub from: String,
    pub to: String,
    pub parts: Vec<String>,
}

/// Copies `from_user`'s PS5 save of `title_id`, every image of it, into `to_user`'s: shared, then
/// used (their folder backed up first, put back on failure).
pub fn copy_to_user(c: &dyn Console, b: &CopyBody) -> Result<Done, Failed> {
    let title_id = b.title_id.trim().to_ascii_uppercase();
    if !plain(&title_id) {
        return Err("unexpected title ID".into());
    }
    if b.from_user == b.to_user {
        return Err("Pick another user: the save is already theirs.".into());
    }
    c.can_back_up()?;
    let saves = c.saves()?;
    let from = ps5_save_of(&saves, &title_id, b.from_user, "This user")?;
    let to = target_save(&saves, &title_id, b.to_user)?;
    checks(c, &title_id)?;
    let missing = lacking(&c.images(&from.path)?, &c.images(&to.path)?);
    if !missing.is_empty() {
        return Err(missing_error(&title_id, "the other user", missing));
    }
    let (_, zip) = share(c, &from.path)?;
    let parts = import_as(c, &to.path, b.to_user, "the other user", &zip, None, None)?;
    Ok(Done {
        from: from.path.clone(),
        to: to.path.clone(),
        parts,
    })
}

/// Puts the backup zip `zip` (a backup of `title_id` by another user) into `to_user`'s save of
/// the game, every image of it: their save is backed up, the backup goes into their folder as it
/// is so the console can open it, it is shared from there, their save is put back, and the shared
/// files used in it.
pub fn restore_to_user(
    c: &dyn Console,
    title_id: &str,
    file: &str,
    zip: &Path,
    to_user: u32,
) -> Result<Done, Failed> {
    let title_id = title_id.trim().to_ascii_uppercase();
    if !plain(&title_id) || !plain(file) || !file.ends_with(".zip") {
        return Err("unexpected backup name".into());
    }
    let (from_user, _) = local::parse_backup_name(&title_id, file)
        .ok_or_else(|| format!("{file} is not a backup of {title_id}"))?;
    if from_user == to_user {
        return Err("This backup is already this user's: use Restore.".into());
    }
    // Encrypted for the PS5 that made it: one from another can't be opened here.
    local::same_console(zip, c.identity().as_deref())?;
    c.can_back_up()?;
    let saves = c.saves()?;
    let to = target_save(&saves, &title_id, to_user)?.clone();
    checks(c, &title_id)?;
    let tmp = make_temp_dir(&title_id)?;
    let tmp_s = tmp.to_string_lossy().into_owned();
    let result = (|| {
        // Check and unpack the backup before anything on the console changes.
        unzip_strict(zip, &tmp, &title_id).map_err(|e| format!("unzip: {e:#}"))?;
        let src = tmp.join(&title_id);
        restore_prepare(&src).map_err(|e| format!("prepare: {e:#}"))?;
        let in_backup = image_names(
            &std::fs::read_dir(&src)
                .map_err(|e| format!("the backup: {e}"))?
                .filter_map(|e| e.ok())
                .map(|e| {
                    let dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    json!({ "name": e.file_name().to_string_lossy(), "kind": if dir { "dir" } else { "file" } })
                })
                .collect::<Vec<_>>(),
        );
        if in_backup.is_empty() {
            return Err("the backup has no save image".into());
        }
        let missing = lacking(&in_backup, &c.images(&to.path)?);
        if !missing.is_empty() {
            return Err(missing_error(&title_id, "the other user", missing));
        }
        let theirs = c
            .back_up(&to, BEFORE_COPY_TAG)
            .map_err(|e| format!("could not back up the save there first: {e}"))?;
        // The backup in the target's folder, opened from there and taken out decrypted.
        let shared = (|| {
            c.put_raw(&title_id, &src, &to.path, to_user)?;
            share(c, &to.path).map(|(_, z)| z).map_err(|e| {
                format!("the backup could not be opened on this PS5 (was it made on another console?): {e}")
            })
        })();
        // Their own save back, whatever happened.
        let back = undo(c, &title_id, &theirs, &to.path, to_user);
        match (shared, back) {
            (Ok(zip), Ok(())) => import_as(
                c,
                &to.path,
                to_user,
                "the other user",
                &zip,
                Some(&theirs),
                None,
            ),
            (Err(why), Ok(())) => Err(format!("{why}. The save there before was put back.").into()),
            (shared, Err(then)) => {
                let why = shared.err().unwrap_or_else(|| "copy stopped".into());
                Err(format!("{why}. {then}").into())
            }
        }
    })();
    let _ = cleanup_temp_dir(&tmp_s);
    result.map(|parts| Done {
        from: file.to_string(),
        to: to.path.clone(),
        parts,
    })
}

// ── on a real console ───────────────────────────────────────────────────

/// A console, through the engine's own routes.
pub struct Live<'a> {
    host: &'a str,
}

impl<'a> Live<'a> {
    pub fn new(host: &'a str) -> Self {
        Self { host }
    }
}

impl Console for Live<'_> {
    fn saves(&self) -> Result<Vec<SaveEntry>, String> {
        local::list_saves(self.host)
    }
    fn running(&self, title_id: &str) -> Result<bool, String> {
        local::title_running(self.host, title_id)
    }
    fn save_open(&self) -> bool {
        local::a_save_is_open(self.host)
    }
    fn images(&self, folder: &str) -> Result<Vec<String>, String> {
        Ok(image_names(&local::folder_entries(self.host, folder)?))
    }
    fn identity(&self) -> Option<String> {
        local::console_identity(self.host)
    }
    fn image_size(&self, image: &str) -> Result<Option<u64>, String> {
        let name = image.rsplit('/').next().unwrap_or_default();
        Ok(local::folder_entries(self.host, local::folder_of(image))?
            .iter()
            .find(|e| e.get("name").and_then(Value::as_str) == Some(name))
            .and_then(|e| e.get("size").and_then(Value::as_u64))
            .filter(|n| *n > 0))
    }
    fn sys_bytes(&self, mount: &str) -> Result<u64, String> {
        // sce_sys holds files only (param.sfo, icon0.png, …).
        Ok(
            local::folder_entries(self.host, &format!("{mount}/{SYS_DIR}"))?
                .iter()
                .filter(|e| e.get("kind").and_then(Value::as_str) != Some("dir"))
                .filter_map(|e| e.get("size").and_then(Value::as_u64))
                .sum(),
        )
    }
    fn can_back_up(&self) -> Result<(), String> {
        if local::settings().dir.trim().is_empty() {
            return Err("Choose a folder for save backups first (Save Data › Automatic backups): the save there now is kept there before its files are replaced.".into());
        }
        Ok(())
    }
    fn back_up(&self, entry: &SaveEntry, tag: &str) -> Result<PathBuf, String> {
        let s = local::settings();
        local::back_up_one(self.host, entry, &s.dir, s.keep, Some(tag), &|| Ok(()))
    }
    fn open_now(&self) -> Result<bool, String> {
        local::save_open_state(self.host)
    }
    fn open_image(&self, image: &str) -> Result<String, String> {
        local::open_save(self.host, image)
    }
    fn copy_out(&self, mount: &str, dest: &Path) -> Result<(), String> {
        let dl = make_temp_dir("share")?;
        let dl_s = dl.to_string_lossy().into_owned();
        let r = (|| {
            let start = local::post(
                "/api/transfer/download",
                json!({ "addr": self.host, "src_path": mount, "dest_dir": dl_s, "kind": "folder" }),
            )?;
            let job = start
                .get("job_id")
                .and_then(Value::as_str)
                .ok_or("download did not start")?;
            local::wait_job(job)?;
            let leaf = mount.rsplit('/').next().unwrap_or("save_mount");
            std::fs::rename(dl.join(leaf), dest).map_err(|e| format!("copy: {e}"))
        })();
        let _ = cleanup_temp_dir(&dl_s);
        r
    }
    fn fill(&self, mount: &str, src: &Path) -> Result<(), String> {
        let host = self.host;
        let listing = local::get(&format!(
            "/api/ps5/list-dir?addr={}&path={}&offset=0&limit=4096",
            local::enc(host),
            local::enc(mount)
        ))?;
        for e in listing
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let Some(name) = e.get("name").and_then(Value::as_str) else {
                continue;
            };
            if name == "." || name == ".." || name == SYS_DIR {
                continue;
            }
            local::post(
                "/api/ps5/fs/delete",
                json!({ "addr": host, "path": format!("{mount}/{name}") }),
            )?;
        }
        let start = local::post(
            "/api/transfer/dir",
            json!({ "addr": host, "src_dir": src.to_string_lossy(), "dest_root": mount, "excludes": [] }),
        )?;
        let job = start
            .get("job_id")
            .and_then(Value::as_str)
            .ok_or("upload did not start")?;
        local::wait_job(job)
    }
    fn close_once(&self) -> Result<Closed, String> {
        local::close_save(self.host).map(|was_open| Closed { was_open })
    }
    fn pause(&self) {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    fn put_raw(
        &self,
        title_id: &str,
        src: &Path,
        folder: &str,
        user_id: u32,
    ) -> Result<(), String> {
        let s = local::settings();
        local::put_in_place(self.host, title_id, src, folder, user_id, &s, None, false)
    }
    fn put_back(
        &self,
        title_id: &str,
        zip: &Path,
        folder: &str,
        user_id: u32,
    ) -> Result<(), String> {
        local::restore_zip_with(self.host, title_id, zip, folder, user_id, false)
    }
}

/// The save images among a PS5 save folder's entries: its files, without the `.bin` keys,
/// anything hidden and the console's own backup copies (`sdimg_sce_bu_*`).
pub fn image_names(entries: &[Value]) -> Vec<String> {
    let mut out: Vec<String> = entries
        .iter()
        .filter(|e| e.get("kind").and_then(Value::as_str) != Some("dir"))
        .filter_map(|e| e.get("name").and_then(Value::as_str))
        .filter(|n| *n != "." && *n != ".." && image_name_ok(n))
        .map(String::from)
        .collect();
    out.sort();
    out
}

// ── HTTP ────────────────────────────────────────────────────────────────

fn failed_json(f: Failed) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({ "error": f.msg, "missing": f.missing })),
    )
        .into_response()
}

fn done_json(d: Done) -> Response {
    Json(json!({ "ok": true, "from": d.from, "to": d.to, "parts": d.parts })).into_response()
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Failed> + Send + 'static,
    ok: impl FnOnce(T) -> Response,
) -> Response {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => ok(v),
        Ok(Err(e)) => failed_json(e),
        Err(e) => local::json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct ShareBody {
    pub addr: String,
    /// The save folder (every image of it).
    #[serde(default)]
    pub folder: Option<String>,
    /// One image (an app before 10.6.0): its whole folder is shared.
    #[serde(default)]
    pub image: Option<String>,
}

/// The folder a request names, or the folder of the image it names.
fn folder_asked(folder: Option<&str>, image: Option<&str>) -> Result<String, String> {
    match (
        folder.map(str::trim).filter(|f| !f.is_empty()),
        image.map(str::trim).filter(|i| !i.is_empty()),
    ) {
        (Some(f), _) => Ok(f.trim_end_matches('/').to_string()),
        (None, Some(i)) => Ok(local::folder_of(i).to_string()),
        _ => Err("name the save".into()),
    }
}

/// `POST /api/save-backup/share` `{addr, folder}` (or `{addr, image}`): every image of the save
/// as one zip (see `share`).
pub async fn post_share(Json(b): Json<ShareBody>) -> Response {
    blocking(
        move || {
            let folder = folder_asked(b.folder.as_deref(), b.image.as_deref())?;
            let host = b.addr.trim().to_string();
            Ok(share(&Live::new(&host), &folder)?)
        },
        |(file, bytes)| {
            (
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        "application/zip".to_string(),
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{}\"", file.replace('"', "")),
                    ),
                ],
                bytes,
            )
                .into_response()
        },
    )
    .await
}

#[derive(Deserialize)]
pub struct UseSharedQuery {
    pub addr: String,
    #[serde(default)]
    pub folder: Option<String>,
    /// An app before 10.6.0 names the image a one-image file goes into.
    #[serde(default)]
    pub image: Option<String>,
    /// Checked against the folder's own user, which is the one used.
    #[serde(default)]
    pub user_id: Option<u32>,
}

/// `POST /api/save-backup/use-shared?addr=&folder=&user_id=` with the shared zip as the body.
pub async fn post_use_shared(Query(q): Query<UseSharedQuery>, body: axum::body::Bytes) -> Response {
    blocking(
        move || {
            let folder = folder_asked(q.folder.as_deref(), q.image.as_deref())?;
            let compat = q
                .image
                .as_deref()
                .and_then(|i| i.rsplit('/').next())
                .map(String::from);
            let user_id = user_of_folder(&folder, q.user_id)?;
            let host = q.addr.trim().to_string();
            import(
                &Live::new(&host),
                &folder,
                user_id,
                &body,
                None,
                compat.as_deref(),
            )
        },
        |parts| Json(json!({ "ok": true, "parts": parts })).into_response(),
    )
    .await
}

/// `POST /api/save-backup/copy` `{addr, title_id, from_user, to_user}`: one user's PS5 save into
/// another user's save of the same game on the same console.
pub async fn post_copy(Json(b): Json<CopyBody>) -> Response {
    blocking(
        move || {
            let host = b.addr.trim().to_string();
            let d = copy_to_user(&Live::new(&host), &b)?;
            crate::log_info!("save backup: copied {} to {}", d.from, d.to);
            Ok(d)
        },
        done_json,
    )
    .await
}

#[derive(Debug, Clone, Deserialize)]
pub struct RestoreToUserBody {
    pub addr: String,
    pub title_id: String,
    /// The backup's file name (`<title>_user<8 hex>_<stamp>.zip`).
    pub file: String,
    pub to_user: u32,
    /// The backup is on the network share (#592), not in the backup folder.
    #[serde(default)]
    pub server: bool,
}

/// `POST /api/save-backup/restore-to-user` `{addr, title_id, file, to_user, server?}`: a backup
/// of one user's save into another user's save of the game.
pub async fn post_restore_to_user(Json(b): Json<RestoreToUserBody>) -> Response {
    blocking(
        move || {
            let title_id = b.title_id.trim().to_ascii_uppercase();
            let file = b.file.trim().to_string();
            if !plain(&title_id) || !plain(&file) || !file.ends_with(".zip") {
                return Err("unexpected backup name".into());
            }
            let host = b.addr.trim().to_string();
            let live = Live::new(&host);
            let d = if b.server {
                let tmp = make_temp_dir(&title_id)?;
                let zip = tmp.join("restore.zip");
                let r = crate::save_backup_server::fetch_to(&title_id, &file, &zip)
                    .map_err(Failed::from)
                    .and_then(|()| restore_to_user(&live, &title_id, &file, &zip, b.to_user));
                let _ = cleanup_temp_dir(&tmp.to_string_lossy());
                r?
            } else {
                let zip = Path::new(&local::settings().dir)
                    .join(&title_id)
                    .join(&file);
                if !zip.is_file() {
                    return Err(format!("no backup {}", zip.display()).into());
                }
                restore_to_user(&live, &title_id, &file, &zip, b.to_user)?
            };
            crate::log_info!("save backup: {} restored into {}", d.from, d.to);
            Ok(d)
        },
        done_json,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    const T: &str = "PPSA01650";
    const A: u32 = 0x1111_1111;
    const B: u32 = 0x2222_2222;
    const MAIN: &str = "sdimg_sce_sdmemory";
    const STAR: &str = "sdimg_STARBOARD";

    fn folder(user: u32) -> String {
        format!("/user/home/{user:08x}/savedata_prospero/{T}")
    }

    fn entry(user: u32, kind: &str) -> SaveEntry {
        SaveEntry {
            title_id: T.into(),
            user_id: user as i32,
            path: if kind == "ps5" {
                folder(user)
            } else {
                format!("/user/home/{user:08x}/savedata/{T}")
            },
            size: 1,
            mtime: 1,
            kind: kind.into(),
        }
    }

    /// A zip of `T` made of `files` (paths under `<T>/`).
    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let tmp = make_temp_dir("copytest").unwrap();
        for (p, b) in files {
            let f = tmp.join(T).join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, b).unwrap();
        }
        let zip = tmp.join("s.zip");
        zip_folder(&tmp, T, &zip).unwrap();
        let bytes = std::fs::read(&zip).unwrap();
        let _ = cleanup_temp_dir(&tmp.to_string_lossy());
        bytes
    }

    /// A shared save from before 10.6.0: one image's files at the top.
    fn old_zip(image: Option<&str>) -> Vec<u8> {
        let mut m = json!({ "kind": "ps5", "title_id": T });
        if let Some(i) = image {
            m["image"] = json!(i);
        }
        let m = m.to_string();
        zip_of(&[("GAME.DAT", b"old"), (SHARE_MANIFEST, m.as_bytes())])
    }

    /// A backup zip of `T`: encrypted images, keys and the console's backup copies.
    fn backup_zip(dir: &Path, images: &[&str]) -> PathBuf {
        backup_zip_from(dir, images, None)
    }

    /// `backup_zip`, noting the PS5 it was made on.
    fn backup_zip_from(dir: &Path, images: &[&str], console: Option<&str>) -> PathBuf {
        let src = dir.join("b");
        std::fs::create_dir_all(src.join(T)).unwrap();
        for i in images {
            std::fs::write(src.join(T).join(i), b"sealed").unwrap();
        }
        std::fs::write(src.join(T).join("sdimg_sce_bu_STARBOARD"), b"sealed").unwrap();
        let zip = dir.join(format!("{T}_user{A:08x}_2026-11-01_101500.zip"));
        ps5upload_core::save_archive::zip_folder_noted(&src, T, &zip, &local::backup_note(console))
            .unwrap();
        zip
    }

    struct Fake {
        saves: Vec<SaveEntry>,
        running: bool,
        open: bool,
        no_backup_dir: bool,
        /// Each folder's images.
        images: BTreeMap<String, Vec<String>>,
        /// Replacing this image fails.
        fail_replace: Option<String>,
        fail_read: bool,
        /// The image open now.
        mounted: RefCell<Option<String>>,
        /// What the next closes say before one works: "reset" (the connection dropped before
        /// the close), "gone" (it dropped after the console closed it), anything else (the
        /// helper refused: still open).
        close_errors: RefCell<Vec<&'static str>>,
        /// Every close fails (still open).
        close_stuck: bool,
        /// Asking whether a save is open fails.
        probe_fails: bool,
        /// Tries at closing.
        closes: Cell<usize>,
        /// The PS5's identity.
        identity: Option<String>,
        /// Image (name) → its size; not listed: unknown.
        sizes: BTreeMap<String, u64>,
        /// What every save's sce_sys holds.
        sys: u64,
        calls: RefCell<Vec<String>>,
        /// Image → the game file put in it.
        replaced: RefCell<BTreeMap<String, Vec<u8>>>,
    }

    impl Fake {
        fn two_users(a: &[&str], b: &[&str]) -> Self {
            let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect();
            Fake {
                saves: vec![entry(A, "ps5"), entry(B, "ps5")],
                running: false,
                open: false,
                no_backup_dir: false,
                images: BTreeMap::from([(folder(A), v(a)), (folder(B), v(b))]),
                fail_replace: None,
                fail_read: false,
                mounted: RefCell::default(),
                close_errors: RefCell::default(),
                close_stuck: false,
                probe_fails: false,
                closes: Cell::default(),
                identity: None,
                sizes: BTreeMap::new(),
                sys: 0,
                calls: RefCell::default(),
                replaced: RefCell::default(),
            }
        }
        fn both(images: &[&str]) -> Self {
            Self::two_users(images, images)
        }
        fn log(&self, s: String) {
            self.calls.borrow_mut().push(s);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
        /// Anything that changes the console.
        fn wrote(&self) -> bool {
            self.calls().iter().any(|c| {
                c.starts_with("replace") || c.starts_with("put_") || c.starts_with("back_up")
            })
        }
    }

    impl Console for Fake {
        fn saves(&self) -> Result<Vec<SaveEntry>, String> {
            Ok(self.saves.clone())
        }
        fn running(&self, _: &str) -> Result<bool, String> {
            Ok(self.running)
        }
        fn save_open(&self) -> bool {
            self.open
        }
        fn images(&self, folder: &str) -> Result<Vec<String>, String> {
            Ok(self.images.get(folder).cloned().unwrap_or_default())
        }
        fn identity(&self) -> Option<String> {
            self.identity.clone()
        }
        fn image_size(&self, image: &str) -> Result<Option<u64>, String> {
            Ok(self.sizes.get(image.rsplit('/').next().unwrap()).copied())
        }
        fn sys_bytes(&self, mount: &str) -> Result<u64, String> {
            assert_eq!(mount, "/mnt/save");
            Ok(self.sys)
        }
        fn can_back_up(&self) -> Result<(), String> {
            if self.no_backup_dir {
                Err("no backup folder".into())
            } else {
                Ok(())
            }
        }
        fn back_up(&self, entry: &SaveEntry, tag: &str) -> Result<PathBuf, String> {
            self.log(format!("back_up {} {tag}", entry.path));
            Ok(PathBuf::from(format!(
                "/backups/{T}_user{:08x}_x_{tag}.zip",
                entry.user_id as u32
            )))
        }
        fn open_now(&self) -> Result<bool, String> {
            if self.probe_fails {
                return Err("connection refused".into());
            }
            Ok(self.mounted.borrow().is_some())
        }
        fn open_image(&self, image: &str) -> Result<String, String> {
            // One at a time, as the helper.
            assert!(
                self.mounted.borrow().is_none(),
                "{image} opened over another"
            );
            *self.mounted.borrow_mut() = Some(image.to_string());
            Ok("/mnt/save".into())
        }
        fn copy_out(&self, mount: &str, dest: &Path) -> Result<(), String> {
            assert_eq!(mount, "/mnt/save");
            let image = self.mounted.borrow().clone().unwrap();
            self.log(format!("read {image}"));
            if self.fail_read {
                return Err("mount failed".into());
            }
            std::fs::create_dir_all(dest.join(SYS_DIR)).unwrap();
            std::fs::write(dest.join(SYS_DIR).join("param.sfo"), b"owner").unwrap();
            std::fs::write(dest.join("GAME.DAT"), image.as_bytes()).unwrap();
            Ok(())
        }
        fn fill(&self, mount: &str, src: &Path) -> Result<(), String> {
            assert_eq!(mount, "/mnt/save");
            let image = self.mounted.borrow().clone().unwrap();
            self.log(format!("replace {image}"));
            // The owner's account details and the manifest never go across.
            assert!(!src.join(SYS_DIR).exists());
            assert!(!src.join(SHARE_MANIFEST).exists());
            self.replaced.borrow_mut().insert(
                image.rsplit('/').next().unwrap().to_string(),
                std::fs::read(src.join("GAME.DAT")).unwrap_or_default(),
            );
            if self.fail_replace.as_deref() == image.rsplit('/').next() {
                Err("upload failed".into())
            } else {
                Ok(())
            }
        }
        fn close_once(&self) -> Result<Closed, String> {
            self.closes.set(self.closes.get() + 1);
            if self.close_stuck {
                return Err("save_unmount_failed: the save is still in use".into());
            }
            let next = {
                let mut q = self.close_errors.borrow_mut();
                (!q.is_empty()).then(|| q.remove(0))
            };
            match next {
                Some("reset") => Err("connection reset by peer".into()),
                Some("gone") => {
                    *self.mounted.borrow_mut() = None;
                    Err("connection reset by peer".into())
                }
                Some(e) => Err(e.into()),
                None => Ok(Closed {
                    was_open: self.mounted.borrow_mut().take().is_some(),
                }),
            }
        }
        fn put_raw(&self, _: &str, src: &Path, folder: &str, _: u32) -> Result<(), String> {
            assert!(src.join(STAR).is_file());
            self.log(format!("put_raw {folder}"));
            Ok(())
        }
        fn put_back(&self, _: &str, zip: &Path, folder: &str, _: u32) -> Result<(), String> {
            self.log(format!(
                "put_back {} {folder}",
                zip.file_name().unwrap().to_string_lossy()
            ));
            Ok(())
        }
    }

    fn body(from: u32, to: u32) -> CopyBody {
        CopyBody {
            addr: "1.2.3.4".into(),
            title_id: T.to_lowercase(),
            from_user: from,
            to_user: to,
        }
    }

    #[test]
    fn parts_have_readable_names() {
        assert_eq!(part_label("sdimg_STARBOARD"), "STARBOARD");
        assert_eq!(part_label("sdimg_sce_sdmemory"), "main save");
        assert_eq!(part_label("SAVEDATA00"), "SAVEDATA00");
    }

    #[test]
    fn image_names_leave_out_keys_and_the_consoles_backup_copies() {
        let entries = vec![
            json!({"name": ".", "kind": "dir"}),
            json!({"name": "sce_sys", "kind": "dir"}),
            json!({"name": "sdimg_STARBOARD", "kind": "file"}),
            json!({"name": "sdimg_sce_sdmemory", "kind": "file"}),
            json!({"name": "sdimg_sce_bu_sce_sdmemory", "kind": "file"}),
            json!({"name": "sdimg_sce_bu_STARBOARD", "kind": "file"}),
            json!({"name": "STARBOARD.bin", "kind": "file"}),
            json!({"name": ".hidden", "kind": "file"}),
        ];
        assert_eq!(
            image_names(&entries),
            vec![STAR.to_string(), MAIN.to_string()]
        );
    }

    /// Every image of the game in one file, each its own folder, sce_sys left out; it loads
    /// back as the same parts.
    #[test]
    fn share_puts_every_image_in_one_file() {
        let c = Fake::both(&[MAIN, STAR]);
        let (name, zip) = share(&c, &folder(A)).unwrap();
        assert!(name.starts_with(T) && name.ends_with(".zip"));
        assert_eq!(
            c.calls(),
            vec![
                format!("read {}/{MAIN}", folder(A)),
                format!("read {}/{STAR}", folder(A)),
            ]
        );
        let tmp = make_temp_dir("copytest").unwrap();
        let path = tmp.join("z.zip");
        std::fs::write(&path, &zip).unwrap();
        unzip_strict(&path, &tmp, T).unwrap();
        let parts = shared_parts(&tmp, T).unwrap();
        let names: Vec<_> = parts.iter().map(|p| p.name.clone().unwrap()).collect();
        assert_eq!(names, vec![MAIN.to_string(), STAR.to_string()]);
        assert!(parts
            .iter()
            .all(|p| !p.old && !p.src.join(SYS_DIR).exists()));
        let _ = cleanup_temp_dir(&tmp.to_string_lossy());
    }

    #[test]
    fn share_is_refused_while_the_game_runs() {
        let c = Fake {
            running: true,
            ..Fake::both(&[MAIN])
        };
        assert!(share(&c, &folder(A)).unwrap_err().contains("is running"));
        assert!(c.calls().is_empty());
    }

    #[test]
    fn parts_go_into_the_images_of_the_same_name() {
        let p = |n: Option<&str>, old: bool| Part {
            name: n.map(String::from),
            src: PathBuf::from(n.unwrap_or("x")),
            old,
        };
        let imgs = vec![MAIN.to_string(), STAR.to_string()];
        let got = match_parts(&[p(Some(STAR), false), p(Some(MAIN), false)], &imgs, None).unwrap();
        assert_eq!(got[0].0, STAR);
        assert_eq!(got[1].0, MAIN);
        // A part the target lacks is named, nothing matched.
        assert_eq!(
            match_parts(&[p(Some(STAR), false)], &[MAIN.to_string()], None).unwrap_err(),
            vec![STAR.to_string()]
        );
        // A new file never falls back to another name, even with one image there.
        assert!(match_parts(&[p(Some("sdimg_X"), false)], &[MAIN.to_string()], None).is_err());
        // An old file (one image) goes into the target's only image, or the one picked.
        let old = p(Some("sdimg_X"), true);
        assert_eq!(
            match_parts(std::slice::from_ref(&old), &[MAIN.to_string()], None).unwrap()[0].0,
            MAIN
        );
        assert_eq!(
            match_parts(std::slice::from_ref(&old), &imgs, Some(STAR)).unwrap()[0].0,
            STAR
        );
        assert!(match_parts(&[old], &imgs, None).is_err());
    }

    /// Copy shares A's whole save and uses it in B's: B backed up once, each image replaced by
    /// A's image of the same name.
    #[test]
    fn copy_moves_every_image_into_the_same_names() {
        let c = Fake::both(&[MAIN, STAR]);
        let d = copy_to_user(&c, &body(A, B)).unwrap();
        assert_eq!(d.to, folder(B));
        assert_eq!(d.parts, vec![MAIN.to_string(), STAR.to_string()]);
        let calls = c.calls();
        assert_eq!(
            calls.iter().filter(|x| x.starts_with("back_up")).count(),
            1,
            "{calls:?}"
        );
        assert!(calls.contains(&format!("back_up {} beforeshare", folder(B))));
        let r = c.replaced.borrow();
        assert_eq!(r[MAIN], format!("{}/{MAIN}", folder(A)).into_bytes());
        assert_eq!(r[STAR], format!("{}/{STAR}", folder(A)).into_bytes());
    }

    /// A part the target lacks refuses the copy before anything is read or written.
    #[test]
    fn copy_refuses_a_missing_part_before_changing_anything() {
        let c = Fake::two_users(&[MAIN, STAR], &[MAIN]);
        let e = copy_to_user(&c, &body(A, B)).unwrap_err();
        assert_eq!(e.missing, vec![STAR.to_string()]);
        assert!(
            e.msg.contains("STARBOARD isn't on the other user yet"),
            "{}",
            e.msg
        );
        assert!(c.calls().is_empty());
        // The target having more parts is fine: theirs that the source lacks stay.
        let c = Fake::two_users(&[MAIN], &[MAIN, STAR]);
        assert_eq!(
            copy_to_user(&c, &body(A, B)).unwrap().parts,
            vec![MAIN.to_string()]
        );
    }

    #[test]
    fn copy_refuses_the_same_user_a_missing_save_and_other_kinds() {
        let c = Fake::both(&[MAIN]);
        assert!(copy_to_user(&c, &body(A, A))
            .unwrap_err()
            .msg
            .contains("already theirs"));
        let c = Fake {
            saves: vec![entry(A, "ps5")],
            ..Fake::both(&[MAIN])
        };
        let e = copy_to_user(&c, &body(A, B)).unwrap_err().msg;
        assert!(e.contains("Start the game once as that user"), "{e}");
        let c = Fake {
            saves: vec![entry(B, "ps5")],
            ..Fake::both(&[MAIN])
        };
        assert!(copy_to_user(&c, &body(A, B))
            .unwrap_err()
            .msg
            .contains("has no save"));
        let c = Fake {
            saves: vec![entry(A, "ps5"), entry(B, "ps4")],
            ..Fake::both(&[MAIN])
        };
        assert!(copy_to_user(&c, &body(A, B))
            .unwrap_err()
            .msg
            .contains("only PS5 saves"));
        let mut other = entry(B, "ps5");
        other.title_id = "PPSA09999".into();
        let c = Fake {
            saves: vec![entry(A, "ps5"), other],
            ..Fake::both(&[MAIN])
        };
        assert!(copy_to_user(&c, &body(A, B))
            .unwrap_err()
            .msg
            .contains("Start the game once"));
        let mut bad = body(A, B);
        bad.title_id = "../x".into();
        assert!(copy_to_user(&Fake::both(&[MAIN]), &bad).is_err());
    }

    #[test]
    fn copy_is_refused_while_the_game_runs_or_a_save_is_open() {
        let c = Fake {
            running: true,
            ..Fake::both(&[MAIN])
        };
        assert!(copy_to_user(&c, &body(A, B))
            .unwrap_err()
            .msg
            .contains("is running"));
        assert!(c.calls().is_empty());
        let c = Fake {
            open: true,
            ..Fake::both(&[MAIN])
        };
        assert!(copy_to_user(&c, &body(A, B))
            .unwrap_err()
            .msg
            .contains("open in Save Data"));
        assert!(c.calls().is_empty());
        let c = Fake {
            no_backup_dir: true,
            ..Fake::both(&[MAIN])
        };
        assert!(copy_to_user(&c, &body(A, B)).is_err());
        assert!(c.calls().is_empty());
    }

    /// The second image failing puts back the folder backup: every image, the first one too.
    #[test]
    fn a_failed_part_puts_the_whole_save_back() {
        let c = Fake {
            fail_replace: Some(STAR.into()),
            ..Fake::both(&[MAIN, STAR])
        };
        let e = copy_to_user(&c, &body(A, B)).unwrap_err().msg;
        assert!(
            e.contains("STARBOARD: upload failed") && e.contains("put back"),
            "{e}"
        );
        let calls = c.calls();
        assert!(calls.contains(&format!("replace {}/{MAIN}", folder(B))));
        assert_eq!(
            calls.last().unwrap(),
            &format!("put_back {T}_user{B:08x}_x_beforeshare.zip {}", folder(B))
        );
    }

    /// A shared save of `T` from A's save (every image in `images`).
    fn shared_zip(images: &[&str]) -> Vec<u8> {
        share(&Fake::both(images), &folder(A)).unwrap().1
    }

    /// A close the helper refuses once (the write-back failed, "the next Close tries again")
    /// is tried again: the save is used, nothing put back.
    #[test]
    fn a_close_that_fails_once_is_tried_again() {
        let c = Fake::both(&[MAIN, STAR]);
        c.close_errors.borrow_mut().push(
            "save_writeback_failed: the edited copy could not be written over the save; it is kept and the next Close tries again",
        );
        let parts = import(&c, &folder(B), B, &shared_zip(&[MAIN, STAR]), None, None).unwrap();
        assert_eq!(parts, vec![MAIN.to_string(), STAR.to_string()]);
        assert_eq!(c.closes.get(), 3);
        assert!(c.mounted.borrow().is_none());
        assert!(!c.calls().iter().any(|x| x.starts_with("put_back")));
    }

    /// The connection dropping in the middle of a close: tried again on a new one.
    #[test]
    fn a_connection_drop_during_close_is_tried_again() {
        let c = Fake::both(&[MAIN]);
        c.close_errors.borrow_mut().extend(["reset", "reset"]);
        assert!(import(&c, &folder(B), B, &shared_zip(&[MAIN]), None, None).is_ok());
        assert_eq!(c.closes.get(), 3);
        assert!(!c.calls().iter().any(|x| x.starts_with("put_back")));
        // Dropped after the console closed it: the retry finds nothing open and can't say the
        // files were kept, so the folder is put back (safe: nothing is open).
        let c = Fake::both(&[MAIN]);
        c.close_errors.borrow_mut().push("gone");
        let e = import(&c, &folder(B), B, &shared_zip(&[MAIN]), None, None)
            .unwrap_err()
            .msg;
        assert!(
            e.contains("may not have been kept") && e.contains("put back"),
            "{e}"
        );
        assert!(c.calls().last().unwrap().starts_with("put_back"));
    }

    /// A save still open after every try: the folder is not put back (the helper would write its
    /// copy over it at the next open or close), and the error names the backup to restore.
    #[test]
    fn a_save_left_open_is_never_put_back_over() {
        let c = Fake {
            close_stuck: true,
            ..Fake::both(&[MAIN, STAR])
        };
        let e = import(&c, &folder(B), B, &shared_zip(&[MAIN, STAR]), None, None)
            .unwrap_err()
            .msg;
        let kept = format!("{T}_user{B:08x}_x_beforeshare.zip");
        assert!(
            e.contains("could not be closed")
                && e.contains("not put back")
                && e.contains(&kept)
                && e.contains("Save Data › Restore"),
            "{e}"
        );
        assert_eq!(c.closes.get(), CLOSE_TRIES);
        assert!(!c.calls().iter().any(|x| x.starts_with("put_back")));
        // Nor when the console can't be asked whether one is open.
        let c = Fake {
            fail_replace: Some(MAIN.into()),
            probe_fails: true,
            ..Fake::both(&[MAIN])
        };
        let e = import(&c, &folder(B), B, &shared_zip(&[MAIN]), None, None)
            .unwrap_err()
            .msg;
        assert!(
            e.contains("not put back") && e.contains("beforeshare"),
            "{e}"
        );
        assert!(!c.calls().iter().any(|x| x.starts_with("put_back")));
    }

    /// Restore to another user: the backup opened from the target's folder and left open is
    /// not covered by the target's own save either.
    #[test]
    fn restore_to_user_never_puts_back_under_an_open_save() {
        let tmp = make_temp_dir("restoretest").unwrap();
        let zip = backup_zip(&tmp, &[MAIN, STAR]);
        let file = zip.file_name().unwrap().to_string_lossy().into_owned();
        let c = Fake {
            close_stuck: true,
            ..Fake::both(&[MAIN, STAR])
        };
        let e = restore_to_user(&c, T, &file, &zip, B).unwrap_err().msg;
        assert!(
            e.contains("not put back") && e.contains("beforecopy"),
            "{e}"
        );
        assert!(!c.calls().iter().any(|x| x.starts_with("put_back")));
        let _ = cleanup_temp_dir(&tmp.to_string_lossy());
    }

    /// The user a shared save goes in as is the folder's own; another one named is refused.
    #[test]
    fn the_user_comes_from_the_folder() {
        assert_eq!(user_of_folder(&folder(B), None), Ok(B));
        assert_eq!(user_of_folder(&format!("{}/", folder(B)), Some(B)), Ok(B));
        assert!(user_of_folder(&folder(B), Some(A))
            .unwrap_err()
            .contains("not user 11111111's"));
        assert!(user_of_folder("/user/home/zzzzzzzz/savedata_prospero/PPSA01650", None).is_err());
        assert!(user_of_folder("/data/x", None).is_err());
    }

    /// A save bigger than the target's image is refused before anything changes: by the image's
    /// whole size without opening it, and once open by what is left besides its sce_sys, closed
    /// again with nothing deleted.
    #[test]
    fn a_save_too_big_for_the_image_is_refused_before_anything_changes() {
        // shared_zip's GAME.DAT is the source image's path (63 bytes for STARBOARD).
        let zip = shared_zip(&[MAIN, STAR]);
        let c = Fake {
            sizes: BTreeMap::from([(MAIN.to_string(), 1 << 20), (STAR.to_string(), 10)]),
            ..Fake::both(&[MAIN, STAR])
        };
        let e = import(&c, &folder(B), B, &zip, None, None).unwrap_err().msg;
        assert!(
            e.contains(
                "PPSA01650's save on this user is too small for this save (needs 63 B, has 10 B)"
            ) && e.contains("Start the game as this user"),
            "{e}"
        );
        assert!(c.calls().is_empty(), "{:?}", c.calls());
        assert_eq!(c.closes.get(), 0);
        // Fits the image, but not beside its sce_sys: refused once open, nothing filled, closed.
        let c = Fake {
            sizes: BTreeMap::from([(MAIN.to_string(), 70)]),
            sys: 40,
            ..Fake::both(&[MAIN])
        };
        let e = import(&c, &folder(B), B, &shared_zip(&[MAIN]), None, None)
            .unwrap_err()
            .msg;
        assert!(e.contains("too small") && e.contains("has 30 B"), "{e}");
        assert!(!c.calls().iter().any(|x| x.starts_with("replace")));
        assert!(c.mounted.borrow().is_none());
        // Copy names the other user; sizes that fit go ahead.
        let c = Fake {
            sizes: BTreeMap::from([(MAIN.to_string(), 10)]),
            ..Fake::both(&[MAIN])
        };
        let e = copy_to_user(&c, &body(A, B)).unwrap_err().msg;
        assert!(e.contains("save on the other user is too small"), "{e}");
        let c = Fake {
            sizes: BTreeMap::from([(MAIN.to_string(), 1 << 20)]),
            sys: 1000,
            ..Fake::both(&[MAIN])
        };
        assert!(copy_to_user(&c, &body(A, B)).is_ok());
    }

    /// A file shared before 10.6.0 (one image, files at the top) still loads.
    #[test]
    fn an_old_one_image_file_still_loads() {
        let c = Fake::both(&[MAIN]);
        let parts = import(
            &c,
            &folder(B),
            B,
            &old_zip(Some("sdimg_SAVEDATA00")),
            None,
            None,
        )
        .unwrap();
        assert_eq!(parts, vec![MAIN.to_string()]);
        assert_eq!(c.replaced.borrow()[MAIN], b"old".to_vec());
        // Into the image an old app named, when there are several.
        let c = Fake::both(&[MAIN, STAR]);
        let parts = import(&c, &folder(B), B, &old_zip(None), None, Some(STAR)).unwrap();
        assert_eq!(parts, vec![STAR.to_string()]);
        // Several and none named: refused, nothing changed.
        let c = Fake::both(&[MAIN, STAR]);
        assert!(import(&c, &folder(B), B, &old_zip(None), None, None).is_err());
        assert!(!c.wrote());
    }

    #[test]
    fn a_shared_file_must_be_for_this_game_and_well_formed() {
        let c = Fake::both(&[MAIN]);
        let m = |v: Value| v.to_string();
        let bad = [
            zip_of(&[("GAME.DAT", b"x")]),
            zip_of(&[(
                SHARE_MANIFEST,
                m(json!({"kind": SHARE_KIND, "title_id": "PPSA09999", "images": [MAIN]}))
                    .as_bytes(),
            )]),
            zip_of(&[(
                SHARE_MANIFEST,
                m(json!({"kind": "ps4", "title_id": T})).as_bytes(),
            )]),
            // A part that is the console's backup copy, or steps out.
            zip_of(&[
                ("sdimg_sce_bu_X/GAME.DAT", b"x"),
                (
                    SHARE_MANIFEST,
                    m(json!({"kind": SHARE_KIND, "title_id": T, "images": ["sdimg_sce_bu_X"]}))
                        .as_bytes(),
                ),
            ]),
            zip_of(&[(
                SHARE_MANIFEST,
                m(json!({"kind": SHARE_KIND, "title_id": T, "images": ["../x"]})).as_bytes(),
            )]),
            // A part with no files.
            zip_of(&[
                (&format!("{MAIN}/{SYS_DIR}/param.sfo"), b"x"),
                (
                    SHARE_MANIFEST,
                    m(json!({"kind": SHARE_KIND, "title_id": T, "images": [MAIN]})).as_bytes(),
                ),
            ]),
        ];
        for z in bad {
            assert!(import(&c, &folder(B), B, &z, None, None).is_err());
        }
        assert!(!c.wrote());
    }

    /// Restore to another user: B kept, the backup put in B's folder and shared from there (every
    /// image, never the console's backup copies), B put back, then the shared files used in B
    /// with that same backup.
    #[test]
    fn restore_to_user_round_trips_through_the_targets_folder() {
        let tmp = make_temp_dir("restoretest").unwrap();
        let zip = backup_zip(&tmp, &[MAIN, STAR]);
        let file = zip.file_name().unwrap().to_string_lossy().into_owned();
        let c = Fake::both(&[MAIN, STAR]);
        let d = restore_to_user(&c, T, &file, &zip, B).unwrap();
        assert_eq!(d.parts, vec![MAIN.to_string(), STAR.to_string()]);
        let kept = format!("{T}_user{B:08x}_x_beforecopy.zip");
        assert_eq!(
            c.calls(),
            vec![
                format!("back_up {} beforecopy", folder(B)),
                format!("put_raw {}", folder(B)),
                format!("read {}/{MAIN}", folder(B)),
                format!("read {}/{STAR}", folder(B)),
                format!("put_back {kept} {}", folder(B)),
                format!("replace {}/{MAIN}", folder(B)),
                format!("replace {}/{STAR}", folder(B)),
            ]
        );
        let _ = cleanup_temp_dir(&tmp.to_string_lossy());
    }

    #[test]
    fn restore_to_user_puts_the_save_back_on_any_failure() {
        let tmp = make_temp_dir("restoretest").unwrap();
        let zip = backup_zip(&tmp, &[MAIN, STAR]);
        let file = zip.file_name().unwrap().to_string_lossy().into_owned();
        let c = Fake {
            fail_read: true,
            ..Fake::both(&[MAIN, STAR])
        };
        let e = restore_to_user(&c, T, &file, &zip, B).unwrap_err().msg;
        assert!(
            e.contains("another console") && e.contains("put back"),
            "{e}"
        );
        assert!(c.calls().last().unwrap().starts_with("put_back"));
        assert!(!c.calls().iter().any(|x| x.starts_with("replace")));
        let c = Fake {
            fail_replace: Some(STAR.into()),
            ..Fake::both(&[MAIN, STAR])
        };
        assert!(restore_to_user(&c, T, &file, &zip, B).is_err());
        assert!(c.calls().last().unwrap().starts_with("put_back"));
        let _ = cleanup_temp_dir(&tmp.to_string_lossy());
    }

    /// A backup made on another PS5 is refused before anything changes; one made here, or one
    /// that does not say (older, or a console with Remote Play off), goes ahead.
    #[test]
    fn a_backup_from_another_ps5_is_refused() {
        let tmp = make_temp_dir("restoretest").unwrap();
        let zip = backup_zip_from(&tmp, &[MAIN, STAR], Some("D4F7D5D36123"));
        let file = zip.file_name().unwrap().to_string_lossy().into_owned();
        let c = Fake {
            identity: Some("5C843CA8AE72".into()),
            ..Fake::both(&[MAIN, STAR])
        };
        let e = restore_to_user(&c, T, &file, &zip, B).unwrap_err().msg;
        assert_eq!(e, local::OTHER_CONSOLE_ERROR);
        assert!(c.calls().is_empty());
        for here in [Some("d4f7d5d36123"), None] {
            let c = Fake {
                identity: here.map(String::from),
                ..Fake::both(&[MAIN, STAR])
            };
            assert!(restore_to_user(&c, T, &file, &zip, B).is_ok());
        }
        let old = backup_zip(&tmp.join("old"), &[MAIN, STAR]);
        let c = Fake {
            identity: Some("5C843CA8AE72".into()),
            ..Fake::both(&[MAIN, STAR])
        };
        assert!(restore_to_user(&c, T, &file, &old, B).is_ok());
        let _ = cleanup_temp_dir(&tmp.to_string_lossy());
    }

    #[test]
    fn restore_to_user_refusals_change_nothing() {
        let tmp = make_temp_dir("restoretest").unwrap();
        let zip = backup_zip(&tmp, &[MAIN, STAR]);
        let file = zip.file_name().unwrap().to_string_lossy().into_owned();
        let c = Fake::both(&[MAIN, STAR]);
        assert!(restore_to_user(&c, T, &file, &zip, A)
            .unwrap_err()
            .msg
            .contains("use Restore"));
        let c = Fake {
            running: true,
            ..Fake::both(&[MAIN, STAR])
        };
        assert!(restore_to_user(&c, T, &file, &zip, B)
            .unwrap_err()
            .msg
            .contains("is running"));
        assert!(!c.wrote());
        let c = Fake {
            saves: vec![entry(A, "ps5")],
            ..Fake::both(&[MAIN, STAR])
        };
        assert!(restore_to_user(&c, T, &file, &zip, B)
            .unwrap_err()
            .msg
            .contains("Start the game once"));
        assert!(!c.wrote());
        // The target lacks a part the backup has.
        let c = Fake::two_users(&[MAIN, STAR], &[MAIN]);
        let e = restore_to_user(&c, T, &file, &zip, B).unwrap_err();
        assert_eq!(e.missing, vec![STAR.to_string()]);
        assert!(!c.wrote());
        assert!(restore_to_user(&Fake::both(&[MAIN]), T, "x.zip", &zip, B).is_err());
        let _ = cleanup_temp_dir(&tmp.to_string_lossy());
    }
}
