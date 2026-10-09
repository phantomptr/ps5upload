//! One game across every saved console and every copy on the user's drives: what the game page
//! shows. Consoles come from `console_snapshot` (what was last read, and when); copies and the
//! packages a console could take come from the Collection, joined by `state_for`.

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::collection::console::{state_for, Offer};
use crate::collection::{Game, Location};
use crate::console_snapshot::{self, host_key, Snapshots};

/// One console's side of the game.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ConsoleEntry {
    pub host: String,
    pub read_at: u64,
    pub installed: bool,
    pub version: Option<String>,
    pub registered_from: Option<String>,
    pub base: Option<Offer>,
    pub update: Option<Offer>,
    pub dlc_missing: Vec<Offer>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GameView {
    pub title_id: String,
    pub title: String,
    pub platform: String,
    pub cover: Option<String>,
    pub copies: Vec<Location>,
    pub consoles: Vec<ConsoleEntry>,
}

fn platform_of(id: &str) -> &'static str {
    if id.starts_with("PPSA") {
        "PS5"
    } else if id.starts_with("CUSA") {
        "PS4"
    } else {
        ""
    }
}

/// The game `id` as the page shows it, or None when neither the Collection nor any console
/// knows it.
pub fn game_view(id: &str, game: Option<&Game>, snaps: &Snapshots) -> Option<GameView> {
    let id = id.to_ascii_uppercase();
    let empty = Game {
        game_id: id.clone(),
        ..Game::default()
    };
    let g = game.unwrap_or(&empty);
    let mut title = game.map(|g| g.title.clone()).unwrap_or_default();
    let mut consoles = Vec::new();
    for (host, titles) in snaps {
        let Some(facts) = titles.get(&id) else {
            continue;
        };
        if (title.is_empty() || title == id) && !facts.title.is_empty() {
            title = facts.title.clone();
        }
        let st = state_for(g, &facts.console_title());
        consoles.push(ConsoleEntry {
            host: host.clone(),
            read_at: facts.read_at,
            installed: facts.installed,
            version: facts.version.clone(),
            registered_from: facts.registered_from.clone(),
            base: st.base,
            update: st.update,
            dlc_missing: st.dlc_missing,
        });
    }
    if game.is_none() && consoles.is_empty() {
        return None;
    }
    let cover = game.and_then(|g| {
        if g.local_cover.is_some() {
            Some(format!("/api/collection/games/{id}/cover"))
        } else {
            g.cover_url.clone()
        }
    });
    let platform = match game.map(|g| g.platform.as_str()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => platform_of(&id).to_string(),
    };
    Some(GameView {
        title: if title.is_empty() { id.clone() } else { title },
        title_id: id,
        platform,
        cover,
        copies: game.map(|g| g.locations.clone()).unwrap_or_default(),
        consoles,
    })
}

fn json_error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

fn collection_game(id: &str) -> Option<Game> {
    crate::collection_api::library().and_then(|l| l.games.get(id).cloned())
}

/// `GET /api/games/{id}`
pub async fn get_game(Path(id): Path<String>) -> Response {
    let id = id.to_ascii_uppercase();
    let game = collection_game(&id);
    match game_view(&id, game.as_ref(), &console_snapshot::snapshot()) {
        Some(v) => Json(v).into_response(),
        None => json_error(
            StatusCode::NOT_FOUND,
            format!("{id} is not in the collection or on any console read so far"),
        ),
    }
}

#[derive(Deserialize)]
pub struct RefreshQuery {
    pub addr: String,
}

/// `POST /api/games/{id}/refresh?addr=` — reads this one title on that console now.
pub async fn refresh(Path(id): Path<String>, Query(q): Query<RefreshQuery>) -> Response {
    let id = id.to_ascii_uppercase();
    let addr = q.addr;
    let read = {
        let (addr, id) = (addr.clone(), id.clone());
        tokio::task::spawn_blocking(move || -> Result<_, String> {
            let installed = crate::collection_api::installed_titles(&addr)?;
            let roots = crate::pkg_install::installed_storage_roots(&addr);
            Ok(crate::collection_api::read_title(
                &addr, &id, &roots, &installed,
            ))
        })
        .await
    };
    let t = match read {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return json_error(StatusCode::BAD_GATEWAY, e),
        Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let game = collection_game(&id);
    let title = game.as_ref().map(|g| g.title.clone()).unwrap_or_default();
    let now = console_snapshot::now_unix();
    let snaps = {
        let (addr, id) = (addr.clone(), id.clone());
        // The save writes the file: off the async workers.
        match tokio::task::spawn_blocking(move || {
            console_snapshot::with(|s| {
                console_snapshot::merge_detailed(s, &addr, &id, &t, &title, now);
                s.clone()
            })
        })
        .await
        {
            Ok(s) => s,
            Err(e) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        }
    };
    let host = host_key(&addr);
    match game_view(&id, game.as_ref(), &snaps)
        .and_then(|v| v.consoles.into_iter().find(|c| c.host == host))
    {
        Some(entry) => Json(entry).into_response(),
        None => json_error(StatusCode::INTERNAL_SERVER_ERROR, "the read was not kept"),
    }
}

#[derive(Deserialize)]
pub struct KeepBody {
    pub hosts: Vec<String>,
}

/// `POST /api/console-snapshots/keep` — forgets consoles the app no longer has.
pub async fn keep(Json(body): Json<KeepBody>) -> Response {
    let kept = tokio::task::spawn_blocking(move || {
        console_snapshot::with(|s| {
            console_snapshot::keep_hosts(s, &body.hosts);
            s.len()
        })
    })
    .await;
    match kept {
        Ok(kept) => Json(serde_json::json!({ "kept": kept })).into_response(),
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::console::ConsoleTitle;
    use crate::collection::PkgInfo;
    use crate::console_snapshot::merge_detailed;

    fn pkg(path: &str, kind: &str, version: &str, content_id: &str) -> Location {
        Location {
            absolute_path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            kind: "pkg".into(),
            size_bytes: 10,
            pkg: Some(PkgInfo {
                kind: kind.into(),
                version: version.into(),
                content_id: content_id.into(),
                complete: true,
                ..PkgInfo::default()
            }),
            ..Location::default()
        }
    }

    fn game() -> Game {
        Game {
            game_id: "PPSA01234".into(),
            title: "Astro".into(),
            platform: "PS5".into(),
            locations: vec![
                pkg(
                    "/g/base.pkg",
                    "base",
                    "01.000",
                    "UP0000-PPSA01234_00-ASTRO0000000000",
                ),
                pkg(
                    "/g/patch.pkg",
                    "patch",
                    "01.004",
                    "UP0000-PPSA01234_00-ASTRO0000000000",
                ),
            ],
            ..Game::default()
        }
    }

    fn installed(version: &str) -> ConsoleTitle {
        ConsoleTitle {
            installed: true,
            version: Some(version.into()),
            ..ConsoleTitle::default()
        }
    }

    #[test]
    fn copies_only_when_no_console_has_read_it() {
        let v = game_view("ppsa01234", Some(&game()), &Snapshots::new()).unwrap();
        assert_eq!(v.title, "Astro");
        assert_eq!(v.copies.len(), 2);
        assert!(v.consoles.is_empty());
    }

    #[test]
    fn consoles_only_when_it_is_not_in_the_collection() {
        let mut s = Snapshots::new();
        merge_detailed(
            &mut s,
            "1.1.1.1",
            "PPSA09999",
            &installed("01.000"),
            "Other",
            3,
        );
        let v = game_view("PPSA09999", None, &s).unwrap();
        assert_eq!(v.title, "Other");
        assert_eq!(v.platform, "PS5");
        assert!(v.copies.is_empty());
        assert_eq!(v.consoles.len(), 1);
        assert!(v.consoles[0].installed);
        assert!(v.consoles[0].base.is_none() && v.consoles[0].update.is_none());
    }

    #[test]
    fn each_console_gets_what_the_drives_could_bring_it() {
        let mut s = Snapshots::new();
        merge_detailed(&mut s, "2.2.2.2", "PPSA01234", &installed("01.002"), "", 5);
        merge_detailed(
            &mut s,
            "1.1.1.1",
            "PPSA01234",
            &ConsoleTitle::default(),
            "",
            6,
        );
        let v = game_view("PPSA01234", Some(&game()), &s).unwrap();
        let hosts: Vec<_> = v.consoles.iter().map(|c| c.host.as_str()).collect();
        assert_eq!(hosts, vec!["1.1.1.1", "2.2.2.2"]);
        let missing = &v.consoles[0];
        assert!(!missing.installed);
        assert_eq!(missing.base.as_ref().unwrap().path, "/g/base.pkg");
        assert_eq!(missing.update.as_ref().unwrap().path, "/g/patch.pkg");
        let old = &v.consoles[1];
        assert!(old.base.is_none());
        assert_eq!(old.update.as_ref().unwrap().version, "01.004");
        assert_eq!(old.read_at, 5);
    }

    #[test]
    fn an_unknown_game_is_none() {
        assert!(game_view("PPSA00000", None, &Snapshots::new()).is_none());
    }

    #[test]
    fn a_collection_entry_without_a_title_id_still_shows_its_copies() {
        let g = Game {
            game_id: "MY-FOLDER-GAME".into(),
            title: "Homebrew".into(),
            locations: vec![Location {
                absolute_path: "/g/hb".into(),
                kind: "folder".into(),
                ..Location::default()
            }],
            ..Game::default()
        };
        let v = game_view("my-folder-game", Some(&g), &Snapshots::new()).unwrap();
        assert_eq!(v.copies.len(), 1);
        assert_eq!(v.platform, "");
        assert!(v.consoles.is_empty());
    }

    #[tokio::test]
    async fn the_handlers_answer_404_502_and_keep() {
        let r = get_game(Path("PPSA00000".into())).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);

        // A console that cannot be reached: the reason comes back and what was saved stays.
        console_snapshot::with(|s| {
            merge_detailed(s, "127.0.0.1", "PPSA05555", &installed("01.000"), "Kept", 1)
        });
        let r = refresh(
            Path("PPSA05555".into()),
            Query(RefreshQuery {
                addr: "127.0.0.1:1".into(),
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
        let snap = console_snapshot::snapshot();
        assert_eq!(
            snap["127.0.0.1"]["PPSA05555"].version.as_deref(),
            Some("01.000")
        );

        let r = get_game(Path("ppsa05555".into())).await;
        assert_eq!(r.status(), StatusCode::OK);

        console_snapshot::with(|s| {
            merge_detailed(s, "10.9.9.9", "PPSA05555", &installed("01.000"), "", 1)
        });
        let r = keep(Json(KeepBody {
            hosts: vec!["10.9.9.9:9114".into()],
        }))
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        let snap = console_snapshot::snapshot();
        assert!(snap.contains_key("10.9.9.9"));
        assert!(!snap.contains_key("127.0.0.1"));
    }
}
