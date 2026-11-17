//! Live probe for the community cheat-repo catalogue.
//!
//! Hits GitHub over the network, so it is `#[ignore]`d and never runs in
//! normal CI. Run:
//!
//!   cargo test -p ps5upload-core --test live_cheat_repos --features live -- --nocapture --ignored
//!
//! It guards the failure mode that silently emptied the repo browser:
//! a catalogue entry whose branch, content root, or enumeration strategy
//! does not match the repo as it actually exists on GitHub. Unit tests
//! cannot catch that — only asking GitHub can.

use ps5upload_core::cheats::{cheat_repos, cheats_repo_search};

#[test]
#[ignore = "hits the network (GitHub)"]
fn live_every_repo_returns_entries() {
    // An empty query matches everything, so each repo should contribute.
    let res = cheats_repo_search("").expect("search failed");
    assert!(!res.entries.is_empty(), "no entries from any repo");

    for repo in cheat_repos() {
        let n = res.entries.iter().filter(|e| e.repo_id == repo.id).count();
        println!("{:<10} {:>6} entries", repo.id, n);
        assert!(n > 0, "repo {} contributed no entries", repo.id);
    }
}

#[test]
#[ignore = "hits the network (GitHub)"]
fn live_henmix_title_id_search_resolves() {
    // Killzone: Shadow Fall — present in HEN-Cheats-Collection.
    let res = cheats_repo_search("CUSA00002").expect("search failed");
    let hit = res
        .entries
        .iter()
        .find(|e| e.repo_id == "henmix")
        .expect("no henmix entry for CUSA00002");
    println!("henmix hit: {} ({})", hit.filename, hit.format);
    assert!(hit.filename.starts_with("CUSA00002"));
    assert_eq!(hit.format, "json");
}

#[test]
#[ignore = "hits the network (GitHub)"]
fn live_ps_game_patch_lists_demons_souls() {
    // Demon's Souls (PPSA01339) has XML game patches in the PS-Game-Patch release.
    let res = cheats_repo_search("PPSA01339").expect("search failed");
    let hit = res
        .entries
        .iter()
        .find(|e| e.repo_id == "psgamepatch")
        .expect("no PS-Game-Patch entry for PPSA01339");
    println!(
        "psgamepatch hit: {} {} ({})",
        hit.filename, hit.game_title, hit.format
    );
    assert_eq!(hit.filename, "PPSA01339.xml");
    assert_eq!(hit.format, "xml");
    assert_eq!(hit.title_id, "PPSA01339");
    assert!(!hit.game_title.is_empty());
}
