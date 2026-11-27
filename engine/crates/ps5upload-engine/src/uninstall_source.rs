//! What an Uninstall may also delete: the folder or disk image a game we registered plays from.
//!
//! * `GET /api/ps5/app/uninstall-source?addr=&title_id=` -> `{title_id, offer, reason?, kind?,
//!   path?, mount_point?, bytes?, size_partial?}`. Read-only.
//! * `POST /api/ps5/app/unregister` with `delete_source: "<path>"` uninstalls, then deletes that
//!   path (unmounting an image first), only when it is still what this answer names.
//!
//! Only a title registered from a folder or one of our image mounts (`/mnt/ps5upload/*`) has
//! anything to offer. A package install, a system title, a ShadowMount+ or Kraken Mount title,
//! or a source that is already gone: `offer: false` and a `reason`, and the Uninstall is the
//! plain one. Never anything outside the game's own source: a drive root, a top folder of
//! `/data`, the console's own folders, a folder or image another registered game plays from,
//! or a folder that holds another game is refused.

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::fs_ops::{list_dir_with_timeout, ListDirOptions, RegisteredApp};
use ps5upload_core::volumes::Volume;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::disk_usage;

/// What a game plays from, which an Uninstall may delete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UninstallSource {
    /// `folder` or `image`.
    pub kind: &'static str,
    /// The folder, or the image file.
    pub path: String,
    /// An image's mount, unmounted before the image is deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount_point: Option<String>,
}

/// Image files our mount takes.
const IMAGE_EXTS: &[&str] = &[".ffpkg", ".exfat", ".ffpfs", ".ffpfsc", ".img"];
/// Top folders of a drive that are the console's or a tool's own, never a game's.
const RESERVED_TOP: &[&str] = &["user", "ps5upload", "system", "system_ex", "preinst"];

fn clean(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        "/".into()
    } else {
        t.into()
    }
}

/// `a` is `b` or inside it.
fn within(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}/"))
}

/// The drive a path is on and the components below it: `/data`, `/mnt/extN`, `/mnt/usbN`.
fn split_drive(p: &str) -> Option<(&str, Vec<&str>)> {
    let comps: Vec<&str> = p.split('/').skip(1).collect();
    if !p.starts_with('/')
        || comps
            .iter()
            .any(|c| c.is_empty() || *c == "." || *c == "..")
    {
        return None;
    }
    let numbered = |c: &str, pre: &str| {
        c.strip_prefix(pre)
            .is_some_and(|n| !n.is_empty() && n.len() <= 2 && n.bytes().all(|b| b.is_ascii_digit()))
    };
    match comps.as_slice() {
        ["data", rest @ ..] => Some(("/data", rest.to_vec())),
        ["mnt", d, rest @ ..] if numbered(d, "ext") || numbered(d, "usb") => {
            Some((&p[..5 + d.len()], rest.to_vec()))
        }
        _ => None,
    }
}

/// A path an Uninstall may delete as a game's own: on a storage drive, below its root, not in
/// the console's or a tool's own folders. `/data` needs two levels (`/data/homebrew/<game>`):
/// its top folders are tools' and scan roots.
pub(crate) fn deletable(p: &str) -> bool {
    let Some((drive, rest)) = split_drive(p) else {
        return false;
    };
    let Some(top) = rest.first() else {
        return false;
    };
    if RESERVED_TOP.iter().any(|r| top.eq_ignore_ascii_case(r)) {
        return false;
    }
    drive != "/data" || rest.len() >= 2
}

/// What `title_id` plays from, from the registered list and the volumes, or why there is
/// nothing to offer.
pub(crate) fn resolve(
    title_id: &str,
    apps: &[RegisteredApp],
    volumes: &[Volume],
) -> Result<UninstallSource, &'static str> {
    let app = apps
        .iter()
        .find(|a| a.title_id.eq_ignore_ascii_case(title_id))
        .ok_or("not_registered")?;
    if app.src.trim().is_empty() {
        return Err("not_registered");
    }
    let src = clean(&app.src);
    if within(&src, "/mnt/shadowmnt") {
        return Err("shadowmount");
    }
    let others: Vec<String> = apps
        .iter()
        .filter(|a| !a.title_id.eq_ignore_ascii_case(title_id) && !a.src.trim().is_empty())
        .map(|a| clean(&a.src))
        .collect();
    if within(&src, "/mnt/ps5upload") {
        let vol = volumes
            .iter()
            .map(|v| (clean(&v.path), v))
            .filter(|(p, _)| p != "/mnt/ps5upload" && within(&src, p))
            .max_by_key(|(p, _)| p.len())
            .ok_or("image_not_mounted")?;
        let (mount, v) = vol;
        if v.source_image.trim().is_empty() {
            return Err("image_unknown");
        }
        let image = clean(&v.source_image);
        let ext_ok = IMAGE_EXTS
            .iter()
            .any(|e| image.to_ascii_lowercase().ends_with(e));
        if !ext_ok || !deletable(&image) {
            return Err("unsafe_path");
        }
        if others.iter().any(|o| within(o, &mount)) {
            return Err("shared_image");
        }
        return Ok(UninstallSource {
            kind: "image",
            path: image,
            mount_point: Some(mount),
        });
    }
    if !deletable(&src) {
        return Err("unsafe_path");
    }
    if others.iter().any(|o| within(o, &src) || within(&src, o)) {
        return Err("shared_folder");
    }
    Ok(UninstallSource {
        kind: "folder",
        path: src,
        mount_point: None,
    })
}

/// A folder below the root (not the root's own) has a `sce_sys`: another game lives inside.
pub(crate) fn holds_other_game(root: &disk_usage::DuDir) -> bool {
    fn below(d: &disk_usage::DuDir) -> bool {
        d.children
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case("sce_sys") || below(c))
    }
    root.children
        .iter()
        .filter(|c| !c.name.eq_ignore_ascii_case("sce_sys"))
        .any(below)
}

/// The answer of `GET /api/ps5/app/uninstall-source`.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct UninstallPlan {
    pub title_id: String,
    /// The Uninstall may ask about deleting `path`.
    pub offer: bool,
    /// Why not: `not_registered` (a package install or a system title: Uninstall removes it
    /// all), `gone`, `shadowmount`, `image_not_mounted`, `image_unknown`, `unsafe_path`,
    /// `shared_image`, `shared_folder`, `not_a_game`, `holds_other_game`, `unreadable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub source: Option<UninstallSource>,
    /// What deleting frees (a folder's: everything below it the walk read).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// The folder walk stopped early: `bytes` is at least that much.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub size_partial: bool,
}

impl UninstallPlan {
    fn none(title_id: &str, reason: &'static str) -> Self {
        Self {
            title_id: title_id.into(),
            reason: Some(reason),
            ..Default::default()
        }
    }
}

/// How long a folder's size may take to read before the answer says "at least".
const WALK_WALL: Duration = Duration::from_secs(90);
const LIST_TIMEOUT: Duration = Duration::from_secs(20);

fn missing(e: &anyhow::Error) -> bool {
    ps5upload_core::fs_ops::is_not_found(&format!("{e:#}"))
}

/// [`resolve`] against the console now, with the folder's game check. No size walk.
pub(crate) fn current(addr: &str, title_id: &str) -> Result<UninstallSource, &'static str> {
    let apps = ps5upload_core::fs_ops::app_list_registered(addr)
        .map_err(|_| "unreadable")?
        .apps;
    let volumes = ps5upload_core::volumes::list_volumes(addr)
        .map(|l| l.volumes)
        .unwrap_or_default();
    let src = resolve(title_id, &apps, &volumes)?;
    match ps5upload_core::fs_ops::fs_stat(addr, &src.path) {
        Err(e) if missing(&e) => return Err("gone"),
        Err(_) => return Err("unreadable"),
        Ok(st) if src.kind == "image" && st.kind != "file" => return Err("unsafe_path"),
        Ok(st) if src.kind == "folder" && st.kind != "dir" => return Err("unsafe_path"),
        Ok(_) => {}
    }
    if src.kind == "folder" {
        match ps5upload_core::fs_ops::fs_stat(addr, &format!("{}/sce_sys/param.json", src.path)) {
            Ok(_) => {}
            Err(e) if missing(&e) => return Err("not_a_game"),
            Err(_) => return Err("unreadable"),
        }
    }
    Ok(src)
}

/// [`current`], plus the size: an image's file size, or a walk of the folder (which also
/// refuses a folder holding another game).
pub(crate) fn plan(addr: &str, title_id: &str) -> UninstallPlan {
    let src = match current(addr, title_id) {
        Ok(s) => s,
        Err(r) => return UninstallPlan::none(title_id, r),
    };
    let mut out = UninstallPlan {
        title_id: title_id.into(),
        offer: true,
        ..Default::default()
    };
    if src.kind == "image" {
        out.bytes = ps5upload_core::fs_ops::fs_stat(addr, &src.path)
            .ok()
            .map(|s| s.size);
    } else {
        let list = |p: &str| {
            disk_usage::list_all(|offset| {
                list_dir_with_timeout(
                    addr,
                    p,
                    ListDirOptions { offset, limit: 256 },
                    Some(LIST_TIMEOUT),
                )
                .map(|l| (l.entries, l.truncated))
            })
        };
        let mounts: Vec<String> = ps5upload_core::volumes::list_volumes(addr)
            .map(|l| l.volumes.into_iter().map(|v| clean(&v.path)).collect())
            .unwrap_or_default();
        let skip = disk_usage::skip_rule(&src.path, mounts);
        let limits = disk_usage::Limits {
            max_wall: WALK_WALL,
            ..Default::default()
        };
        match disk_usage::walk(
            &src.path,
            &list,
            &skip,
            &limits,
            &disk_usage::Progress::default(),
            &|| false,
        ) {
            Ok(w) => {
                if holds_other_game(&w.tree) {
                    return UninstallPlan::none(title_id, "holds_other_game");
                }
                out.bytes = Some(w.tree.bytes);
                out.size_partial = w.partial.is_some();
            }
            Err(_) => return UninstallPlan::none(title_id, "unreadable"),
        }
    }
    out.source = Some(src);
    out
}

#[derive(Debug, Deserialize)]
pub(crate) struct UninstallSourceQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    title_id: String,
}

/// `GET /api/ps5/app/uninstall-source?addr=&title_id=`: whether Uninstall can also delete the
/// folder or disk image the title plays from, what that is and its size. Read-only. A folder's
/// size is walked (up to 90 s; `size_partial` past that). `offer: false` with a `reason` when
/// there is nothing to delete: a package install (Uninstall removes it all), a system title,
/// a ShadowMount+ title, a source already gone, or one that is not safe to delete.
pub(crate) async fn plan_handler(Query(q): Query<UninstallSourceQuery>) -> Response {
    let Some(addr) = crate::required_console_addr(q.addr) else {
        return crate::missing_console_addr();
    };
    let title_id = q.title_id.trim().to_ascii_uppercase();
    if !crate::looks_like_title_id(&title_id) {
        return crate::json_err(StatusCode::BAD_REQUEST, "title_id is not a title id")
            .into_response();
    }
    match tokio::task::spawn_blocking(move || plan(&addr, &title_id)).await {
        Ok(p) => Json(p).into_response(),
        Err(e) => crate::json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, src: &str) -> RegisteredApp {
        RegisteredApp {
            title_id: id.into(),
            title_name: id.into(),
            src: src.into(),
            image_backed: src.starts_with("/mnt/ps5upload/"),
        }
    }

    fn vol(path: &str, image: &str) -> Volume {
        serde_json::from_value(serde_json::json!({
            "path": path, "fs_type": "exfatfs", "total_bytes": 1, "free_bytes": 0,
            "writable": true, "source_image": image,
        }))
        .unwrap()
    }

    #[test]
    fn a_folder_game_offers_its_folder() {
        let apps = [app("PPSA00001", "/data/homebrew/Game/")];
        assert_eq!(
            resolve("ppsa00001", &apps, &[]),
            Ok(UninstallSource {
                kind: "folder",
                path: "/data/homebrew/Game".into(),
                mount_point: None
            })
        );
        let usb = [app("PPSA00001", "/mnt/usb0/Game")];
        assert_eq!(
            resolve("PPSA00001", &usb, &[]).unwrap().path,
            "/mnt/usb0/Game"
        );
    }

    #[test]
    fn an_image_game_offers_the_image_and_its_mount() {
        let apps = [app("PPSA00002", "/mnt/ps5upload/Game/PPSA00002-app")];
        let vols = [
            vol("/data", ""),
            vol("/mnt/ps5upload/Game", "/mnt/ext1/games/Game.ffpkg"),
        ];
        assert_eq!(
            resolve("PPSA00002", &apps, &vols),
            Ok(UninstallSource {
                kind: "image",
                path: "/mnt/ext1/games/Game.ffpkg".into(),
                mount_point: Some("/mnt/ps5upload/Game".into()),
            })
        );
        // Not mounted, or mounted before the tracker: nothing to name.
        assert_eq!(resolve("PPSA00002", &apps, &[]), Err("image_not_mounted"));
        let untracked = [vol("/mnt/ps5upload/Game", "")];
        assert_eq!(
            resolve("PPSA00002", &apps, &untracked),
            Err("image_unknown")
        );
        let odd = [vol("/mnt/ps5upload/Game", "/data/homebrew/Game.zip")];
        assert_eq!(resolve("PPSA00002", &apps, &odd), Err("unsafe_path"));
    }

    #[test]
    fn package_installs_and_other_mounters_offer_nothing() {
        assert_eq!(resolve("PPSA00003", &[], &[]), Err("not_registered"));
        assert_eq!(
            resolve("PPSA00003", &[app("PPSA00003", "")], &[]),
            Err("not_registered")
        );
        let smp = [app("PPSA00003", "/mnt/shadowmnt/Game.ffpkg_1a2b/x")];
        assert_eq!(resolve("PPSA00003", &smp, &[]), Err("shadowmount"));
    }

    #[test]
    fn never_a_drive_root_a_top_folder_or_the_consoles_own() {
        for p in [
            "/data",
            "/data/homebrew",
            "/mnt/usb0",
            "/mnt/ext1/user/app/PPSA00004",
            "/data/ps5upload/x",
            "/user/app/PPSA00004",
            "/system_ex/app/PPSA00004",
            "/mnt/sandbox/x",
            "/data/homebrew/../x",
            "relative/x",
        ] {
            assert!(!deletable(p), "{p}");
            let apps = [app("PPSA00004", p)];
            assert!(resolve("PPSA00004", &apps, &[]).is_err(), "{p}");
        }
        assert!(deletable("/mnt/ext0/Game"));
        assert!(deletable("/data/homebrew/Game"));
    }

    #[test]
    fn a_source_another_game_shares_is_refused() {
        let nested = [
            app("PPSA00005", "/mnt/usb0/Games"),
            app("PPSA00006", "/mnt/usb0/Games/Other"),
        ];
        assert_eq!(resolve("PPSA00005", &nested, &[]), Err("shared_folder"));
        assert_eq!(resolve("PPSA00006", &nested, &[]), Err("shared_folder"));
        let image = [
            app("PPSA00005", "/mnt/ps5upload/Pack/A"),
            app("PPSA00006", "/mnt/ps5upload/Pack/B"),
        ];
        let vols = [vol("/mnt/ps5upload/Pack", "/data/homebrew/Pack.exfat")];
        assert_eq!(resolve("PPSA00005", &image, &vols), Err("shared_image"));
    }

    #[test]
    fn a_folder_holding_another_game_is_seen() {
        let dir = |name: &str, children: Vec<disk_usage::DuDir>| disk_usage::DuDir {
            name: name.into(),
            children,
            ..Default::default()
        };
        let game = dir(
            "Game",
            vec![
                dir("sce_sys", vec![dir("about", vec![])]),
                dir("data", vec![]),
            ],
        );
        assert!(!holds_other_game(&game));
        let pack = dir(
            "Game",
            vec![
                dir("sce_sys", vec![]),
                dir("extra", vec![dir("Other", vec![dir("sce_sys", vec![])])]),
            ],
        );
        assert!(holds_other_game(&pack));
    }

    /// A console with one registered title and the paths `present` (kind by trailing `/`):
    /// records every unmount and delete it is asked for, in order.
    struct Console {
        src: String,
        volumes: serde_json::Value,
        present: Vec<&'static str>,
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl ps5upload_core::mgmt::MgmtTransport for Console {
        fn call(
            &self,
            _addr: &str,
            method: ps5upload_core::mgmt::Method,
            label: &str,
            body: &[u8],
            _timeout: Duration,
        ) -> anyhow::Result<Option<Vec<u8>>> {
            use ps5upload_core::mgmt::m;
            let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
            let reply = if method == m::APP_LIST {
                serde_json::json!({"apps": [
                    {"title_id": "PPSA00001", "title_name": "G", "src": self.src}
                ]})
            } else if method == m::FS_VOLUMES {
                self.volumes.clone()
            } else if method == m::FS_STAT {
                let p = v["path"].as_str().unwrap_or_default();
                match self.present.iter().find(|x| x.trim_end_matches('/') == p) {
                    Some(x) if x.ends_with('/') => {
                        serde_json::json!({"kind": "dir", "size": 0, "mtime": 0, "mode": 0})
                    }
                    Some(_) => {
                        serde_json::json!({"kind": "file", "size": 4096, "mtime": 0, "mode": 0})
                    }
                    None => {
                        return Err(anyhow::Error::new(ps5upload_core::mgmt::MgmtError {
                            label: label.into(),
                            status: 404,
                            cause: "fs_stat_failed_errno_2".into(),
                        }))
                    }
                }
            } else if method == m::FS_UNMOUNT {
                self.asked.lock().unwrap().push(format!(
                    "unmount {}",
                    v["mount_point"].as_str().unwrap_or_default()
                ));
                serde_json::json!({})
            } else {
                serde_json::json!({})
            };
            Ok(Some(serde_json::to_vec(&reply).unwrap()))
        }

        fn run_job(
            &self,
            _addr: &str,
            op: ps5upload_core::mgmt::JobOp,
            _label: &str,
            body: &[u8],
            _call: &ps5upload_core::mgmt::JobCall<'_>,
        ) -> anyhow::Result<Option<Vec<u8>>> {
            let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
            self.asked.lock().unwrap().push(format!(
                "{} {}",
                op.label,
                v["path"].as_str().unwrap_or_default()
            ));
            Ok(Some(b"{}".to_vec()))
        }
    }

    fn console(
        src: &str,
        volumes: serde_json::Value,
        present: Vec<&'static str>,
    ) -> std::sync::Arc<Console> {
        std::sync::Arc::new(Console {
            src: src.into(),
            volumes,
            present,
            asked: Default::default(),
        })
    }

    const ADDR: &str = "192.0.2.10";

    #[test]
    fn a_folder_is_offered_only_while_it_is_there_and_holds_a_game() {
        let c = console(
            "/mnt/usb0/Game",
            serde_json::json!({"volumes": []}),
            vec!["/mnt/usb0/Game/", "/mnt/usb0/Game/sce_sys/param.json"],
        );
        let _t = ps5upload_core::mgmt::scoped_transport(c);
        assert_eq!(current(ADDR, "PPSA00001").unwrap().path, "/mnt/usb0/Game");

        let gone = console("/mnt/usb0/Game", serde_json::json!({"volumes": []}), vec![]);
        let _t = ps5upload_core::mgmt::scoped_transport(gone);
        assert_eq!(current(ADDR, "PPSA00001"), Err("gone"));

        let no_game = console(
            "/mnt/usb0/Game",
            serde_json::json!({"volumes": []}),
            vec!["/mnt/usb0/Game/"],
        );
        let _t = ps5upload_core::mgmt::scoped_transport(no_game);
        assert_eq!(current(ADDR, "PPSA00001"), Err("not_a_game"));
    }

    #[test]
    fn an_image_is_unmounted_then_deleted_and_only_after_the_uninstall() {
        let vols = serde_json::json!({"volumes": [{
            "path": "/mnt/ps5upload/G", "fs_type": "exfatfs", "total_bytes": 1,
            "free_bytes": 0, "writable": true, "source_image": "/data/homebrew/G.exfat"
        }]});
        let c = console("/mnt/ps5upload/G/app", vols, vec!["/data/homebrew/G.exfat"]);
        let _t = ps5upload_core::mgmt::scoped_transport(c.clone());
        let src = current(ADDR, "PPSA00001").unwrap();
        assert_eq!(src.mount_point.as_deref(), Some("/mnt/ps5upload/G"));

        // Still listed after Sony accepted: nothing is touched.
        let listed =
            ps5upload_core::fs_ops::UnregisterOutcome::from_ack(br#"{"still_present":true}"#);
        let d = crate::delete_uninstalled_source(ADDR, &src, &listed);
        assert!(!d.deleted);
        assert_eq!(d.error.as_deref(), Some("uninstall_not_finished"));
        assert!(c.asked.lock().unwrap().is_empty());

        let done =
            ps5upload_core::fs_ops::UnregisterOutcome::from_ack(br#"{"still_present":false}"#);
        let d = crate::delete_uninstalled_source(ADDR, &src, &done);
        assert!(d.deleted, "{:?}", d.error);
        assert_eq!(
            *c.asked.lock().unwrap(),
            vec![
                "unmount /mnt/ps5upload/G".to_string(),
                "FS_DELETE /data/homebrew/G.exfat".to_string()
            ]
        );
    }
}
