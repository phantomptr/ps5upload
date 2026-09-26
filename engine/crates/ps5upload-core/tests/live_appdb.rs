//! Live hardware probe for the install launchability-verification path.
//!
//! Gated on `PS5UPLOAD_LIVE_ADDR` (e.g. "192.168.86.100:9114") so it never
//! runs in normal CI — a real, payload-loaded PS5 must be on the LAN. Run:
//!
//!   PS5UPLOAD_LIVE_ADDR=192.168.86.100:9114 \
//!     cargo test -p ps5upload-core --test live_appdb -- --nocapture --ignored
//!
//! It answers the FW-9.60 crux: does the dlsym-only `AppDbQuery` actually
//! read app.db on this firmware (→ verification is live), or degrade to
//! `sqlite_unavailable` (→ verification correctly no-ops, optimistic)?

use ps5upload_core::diagnostics::appdb_query;
use ps5upload_core::fs_ops::app_list_registered;
use ps5upload_core::pkg_install::{title_id_from_content_id, verify_launchable, LaunchCheck};

fn live_addr() -> Option<String> {
    std::env::var("PS5UPLOAD_LIVE_ADDR")
        .ok()
        .filter(|s| !s.is_empty())
}

#[test]
#[ignore = "requires a live PS5 on the LAN (set PS5UPLOAD_LIVE_ADDR)"]
fn live_verify_launchable_against_real_console() {
    let addr = match live_addr() {
        Some(a) => a,
        None => {
            eprintln!("PS5UPLOAD_LIVE_ADDR unset — skipping live probe");
            return;
        }
    };

    // 1. Does the sqlite-backed AppDbQuery read app.db on this firmware?
    let appdb = appdb_query(&addr).expect("AppDbQuery RPC should round-trip");
    println!("---- AppDbQuery (sqlite, dlsym-only) ----");
    println!("  err   = {:?}", appdb.err);
    println!("  count = {}", appdb.apps.len());
    for a in appdb.apps.iter().take(8) {
        println!("  app   = {} ({})", a.title_id, a.name);
    }

    // 2. Cross-check against the filesystem-scan list (always works).
    let fs_list = app_list_registered(&addr).expect("AppListRegistered RPC should round-trip");
    println!("---- AppListRegistered (filesystem scan) ----");
    println!("  count = {}", fs_list.apps.len());
    for a in fs_list.apps.iter().take(8) {
        println!("  app   = {} ({})", a.title_id, a.title_name);
    }

    let sqlite_live = appdb.err.is_none();
    let fs_live = !fs_list.apps.is_empty();
    println!(
        "\n==> sqlite app.db read: {}   |   filesystem /user/app scan: {}",
        if sqlite_live { "LIVE" } else { "unavailable" },
        if fs_live { "LIVE (primary)" } else { "empty" },
    );

    // 3. Drive verify_launchable end-to-end against a title we KNOW is
    //    registered (from the filesystem scan — the all-firmware primary).
    //    Synthesize a content_id embedding it, exactly as a PKG header would
    //    (region tag '-' title_id '_' label).
    if let Some(title_id) = fs_list.apps.first().map(|a| a.title_id.clone()) {
        let cid = format!("IV0000-{title_id}_00-PS5UPLOADLIVETEST");
        assert_eq!(
            title_id_from_content_id(&cid).as_deref(),
            Some(title_id.as_str()),
            "content_id should parse back to the title_id we built it from"
        );
        let check = verify_launchable(&addr, &cid);
        println!("\nverify_launchable(real title {title_id}) = {check:?}");
        // The filesystem scan finds it, so it MUST verify as Registered —
        // even on FW 9.60 where sqlite is unavailable. This is the whole
        // point: verification now actually works on the common firmwares.
        assert_eq!(
            check,
            LaunchCheck::Registered,
            "a registered title must verify as Registered via the filesystem scan, regardless of sqlite"
        );
    } else {
        println!("\n(no registered titles on this console — skipping positive verify)");
    }

    // 4. A title that cannot be installed must never verify as Registered.
    let bogus = "IV0000-CUSA99999_00-DEFINITELYNOTINSTALLED";
    let check = verify_launchable(&addr, bogus);
    println!("verify_launchable(bogus CUSA99999) = {check:?}");
    assert_ne!(
        check,
        LaunchCheck::Registered,
        "a non-existent title must never report Registered"
    );
    if fs_live {
        // Console enumerable, title in neither source → genuinely absent.
        assert_eq!(
            check,
            LaunchCheck::Absent,
            "reachable console, title absent → Absent"
        );
    }

    // 5. A FAKE-placeholder content_id is never verifiable (elf-arsenal parity).
    let fake = "IV0000-FAKE00000_00-PLACEHOLDER00000";
    assert_eq!(title_id_from_content_id(fake), None);
    assert_eq!(verify_launchable(&addr, fake), LaunchCheck::Unsupported);
    println!("\nlive probe OK");
}
